use crate::error::GatewayError;
use axum::{
    extract::{Path, Query, State},
    http::HeaderMap,
    response::sse::{Event, KeepAlive, Sse},
    Json,
};
use eay_gateway_core::engine::{ChatOptions, GatewayEngine};
use futures::StreamExt;
use serde::Deserialize;
use serde_json::{json, Value};

pub async fn handle_activity_snapshot(State(engine): State<GatewayEngine>) -> Json<Value> {
    Json(json!({
        "events": engine.activity.recent_events(),
        "subagents": engine.activity.list_runs(),
    }))
}

pub async fn handle_activity_events(
    State(engine): State<GatewayEngine>,
) -> Sse<impl futures::Stream<Item = Result<Event, std::convert::Infallible>>> {
    let recent = engine.activity.recent_events();
    let rx = engine.activity.subscribe();
    let initial = futures::stream::iter(
        recent.into_iter().map(|ev| Ok(Event::default().event(&ev.phase).data(serde_json::to_string(&ev).unwrap_or_default()))),
    );
    let live = futures::stream::unfold(rx, |mut rx| async move {
        loop {
            match rx.recv().await {
                Ok(ev) => {
                    let data = serde_json::to_string(&ev).unwrap_or_default();
                    return Some((Ok(Event::default().event(ev.phase).data(data)), rx));
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
            }
        }
    });
    Sse::new(initial.chain(live)).keep_alive(KeepAlive::default())
}

#[derive(Deserialize)]
pub struct SpawnSubagentPayload {
    role: String,
    #[serde(default = "default_auto")]
    model: String,
    prompt: String,
    system: Option<String>,
    #[serde(default)]
    skills: Vec<String>,
    session_id: Option<String>,
    /// MCP servers the subagent may use (`["*"]` = all enabled).
    #[serde(default)]
    mcp: Vec<String>,
    /// Run or request id of the agent that spawned this one.
    parent_id: Option<String>,
    /// Gateway key (`gw-sk-…`) whose budget and limits the subagent runs under.
    api_key: Option<String>,
}

/// Subagents as a parent/child tree (`children` nested), for team views.
pub async fn handle_subagent_tree(State(engine): State<GatewayEngine>) -> Json<Value> {
    Json(json!({ "tree": engine.activity.run_tree() }))
}

pub async fn handle_list_subagents(State(engine): State<GatewayEngine>) -> Json<Value> {
    Json(json!({ "subagents": engine.activity.list_runs() }))
}

pub async fn handle_spawn_subagent(
    State(engine): State<GatewayEngine>,
    headers: HeaderMap,
    Json(p): Json<SpawnSubagentPayload>,
) -> Result<Json<Value>, GatewayError> {
    if p.role.trim().is_empty() || p.prompt.trim().is_empty() {
        return Err(GatewayError::BadRequest("Both 'role' and 'prompt' are required".into()));
    }
    // Key from the body, else from `x-eay-key` (Authorization carries the admin token here).
    let key = match p.api_key.as_deref().or_else(|| headers.get("x-eay-key").and_then(|v| v.to_str().ok())).map(str::trim) {
        Some(secret) if !secret.is_empty() => match engine.store.find_virtual_key(secret) {
            Some(k) if k.enabled => Some(k),
            Some(_) => return Err(GatewayError::Unauthorized("API key is disabled".into())),
            None => return Err(GatewayError::Unauthorized("Invalid API key".into())),
        },
        _ => None,
    };
    let opts = ChatOptions {
        skills: p.skills,
        session_id: p.session_id,
        mcp_servers: p.mcp,
        mcp_allowed: super::is_loopback_host(&headers),
        ..Default::default()
    };
    let run = engine.spawn_subagent(key, p.role, p.model, p.prompt, p.system, p.parent_id, opts);
    Ok(Json(json!({ "subagent": run })))
}

#[derive(Deserialize)]
pub struct GetSubagentQuery {
    /// Seconds to wait for the run to finish before answering (max 300).
    wait: Option<u64>,
}

/// With `?wait=N`, long-polls until the run finishes so an orchestrator can join on a child.
pub async fn handle_get_subagent(
    State(engine): State<GatewayEngine>,
    Path(id): Path<String>,
    Query(q): Query<GetSubagentQuery>,
) -> Result<Json<Value>, GatewayError> {
    let run = match q.wait.filter(|w| *w > 0) {
        Some(w) => engine.activity.wait_run(&id, std::time::Duration::from_secs(w.min(300))).await,
        None => engine.activity.get_run(&id),
    };
    let run = run.ok_or_else(|| GatewayError::NotFound("Subagent not found".into()))?;
    Ok(Json(json!({ "subagent": run })))
}

/// Tool calls waiting for a person (`approval: manual` or `manual_for_writes`).
pub async fn handle_list_approvals(State(engine): State<GatewayEngine>) -> Json<Value> {
    Json(json!({ "approvals": engine.activity.list_approvals() }))
}

#[derive(Deserialize)]
pub struct ApprovalDecision {
    approve: bool,
}

pub async fn handle_resolve_approval(
    State(engine): State<GatewayEngine>,
    Path(id): Path<String>,
    Json(d): Json<ApprovalDecision>,
) -> Result<Json<Value>, GatewayError> {
    if !engine.activity.resolve_approval(&id, d.approve) {
        return Err(GatewayError::NotFound("No pending approval with that id (already answered or expired)".into()));
    }
    Ok(Json(json!({ "status": if d.approve { "approved" } else { "denied" } })))
}

pub async fn handle_cancel_subagent(State(engine): State<GatewayEngine>, Path(id): Path<String>) -> Result<Json<Value>, GatewayError> {
    if engine.activity.get_run(&id).is_none() {
        return Err(GatewayError::NotFound("Subagent not found".into()));
    }
    let cancelled = engine.cancel_subagent(&id);
    Ok(Json(json!({ "status": if cancelled { "cancelled" } else { "not_running" } })))
}

pub async fn handle_delete_subagent(State(engine): State<GatewayEngine>, Path(id): Path<String>) -> Json<Value> {
    // Stop the branch first, so deleting a lead does not leave its workers running.
    let _ = engine.cancel_subagent(&id);
    let deleted = engine.activity.delete_run(&id);
    engine.store.delete_subagent_run(&id);
    Json(json!({ "status": if deleted { "deleted" } else { "not_found" } }))
}

fn default_auto() -> String {
    "auto".into()
}
