use crate::error::GatewayError;
use axum::{
    extract::{Path, Query, State},
    Json,
};
use eay_gateway_core::engine::GatewayEngine;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
pub struct SaveAgentPayload {
    name: Option<String>,
    #[serde(default)]
    description: String,
    #[serde(default = "default_auto")]
    model: String,
    #[serde(default)]
    skills: Vec<String>,
    #[serde(default)]
    personality: String,
    temperature: Option<f64>,
    max_tokens: Option<u64>,
    /// `{"mode":"read_only","allow":["fs/*"],"deny":["write_file"]}` for gateway-run MCP tools.
    #[serde(default)]
    tool_policy: eay_gateway_core::mcp::ToolPolicy,
    /// MCP servers this agent uses on a loopback request (or a subagent of one). Example: `["filesystem"]`.
    #[serde(default)]
    mcp: Vec<String>,
    #[serde(default = "default_true")]
    enabled: bool,
}

fn default_auto() -> String {
    "auto".into()
}

pub async fn handle_list_agents(State(engine): State<GatewayEngine>) -> Json<Value> {
    Json(json!({ "agents": engine.store.list_agents() }))
}

pub async fn handle_save_agent(State(engine): State<GatewayEngine>, Json(p): Json<SaveAgentPayload>) -> Result<Json<Value>, GatewayError> {
    let name = p.name.as_deref().unwrap_or("");
    let agent = engine
        .store
        .save_agent(
            name,
            &p.description,
            &p.model,
            &p.skills,
            &p.personality,
            p.temperature,
            p.max_tokens,
            &p.tool_policy,
            p.enabled,
            &p.mcp,
        )
        .map_err(GatewayError::BadRequest)?;
    Ok(Json(json!({ "agent": agent })))
}

pub async fn handle_put_agent(
    State(engine): State<GatewayEngine>,
    Path(name): Path<String>,
    Json(p): Json<SaveAgentPayload>,
) -> Result<Json<Value>, GatewayError> {
    let agent = engine
        .store
        .save_agent(
            &name,
            &p.description,
            &p.model,
            &p.skills,
            &p.personality,
            p.temperature,
            p.max_tokens,
            &p.tool_policy,
            p.enabled,
            &p.mcp,
        )
        .map_err(GatewayError::BadRequest)?;
    Ok(Json(json!({ "agent": agent })))
}

pub async fn handle_delete_agent(State(engine): State<GatewayEngine>, Path(name): Path<String>) -> Json<Value> {
    let deleted = engine.store.delete_agent(&name).unwrap_or(false);
    Json(json!({ "status": if deleted { "deleted" } else { "not_found" } }))
}

#[derive(Deserialize)]
pub struct SaveTeamPayload {
    /// Set to update this team. Omit to create a new id.
    id: Option<String>,
    name: Option<String>,
    #[serde(default)]
    lead: String,
    #[serde(default)]
    members: Vec<String>,
}

pub async fn handle_list_teams(State(engine): State<GatewayEngine>) -> Json<Value> {
    Json(json!({ "teams": engine.store.list_teams() }))
}

pub async fn handle_save_team(State(engine): State<GatewayEngine>, Json(p): Json<SaveTeamPayload>) -> Result<Json<Value>, GatewayError> {
    let team =
        engine.store.save_team(p.id.as_deref(), p.name.as_deref().unwrap_or(""), &p.lead, &p.members).map_err(GatewayError::BadRequest)?;
    Ok(Json(json!({ "team": team })))
}

pub async fn handle_delete_team(State(engine): State<GatewayEngine>, Path(id): Path<String>) -> Json<Value> {
    let deleted = engine.store.delete_team(&id).unwrap_or(false);
    Json(json!({ "status": if deleted { "deleted" } else { "not_found" } }))
}

#[derive(Deserialize)]
pub struct ListQuery {
    limit: Option<usize>,
}

/// Sessions with their message counts, newest first.
pub async fn handle_list_sessions(State(engine): State<GatewayEngine>, Query(q): Query<ListQuery>) -> Json<Value> {
    Json(json!({ "sessions": engine.store.list_sessions(q.limit.unwrap_or(100).min(1000)) }))
}

/// One session: its messages and the subagents that ran in it. `id` is the full storage id from
/// the list (`key:<key-id>:<session>` or `local:<session>`); a bare id means the local session.
pub async fn handle_get_session(State(engine): State<GatewayEngine>, Path(id): Path<String>) -> Result<Json<Value>, GatewayError> {
    let storage = if engine.store.session_updated_at(&id).is_some() { id } else { eay_gateway_core::engine::session_storage_id(None, &id) };
    let updated_at = engine.store.session_updated_at(&storage).ok_or_else(|| GatewayError::NotFound("Session not found".into()))?;
    let name = storage.rsplit(':').next().unwrap_or(&storage).to_string();
    let subagents: Vec<Value> = engine
        .activity
        .list_runs()
        .into_iter()
        .filter(|r| r.parent_session.as_deref() == Some(name.as_str()))
        .filter_map(|r| serde_json::to_value(r).ok())
        .collect();
    Ok(Json(json!({
        "id": storage, "session": name, "updated_at": updated_at,
        "team": engine.store.session_team(&storage),
        "messages": engine.store.get_session_messages(&storage),
        "subagents": subagents,
    })))
}

pub async fn handle_delete_session(State(engine): State<GatewayEngine>, Path(id): Path<String>) -> Json<Value> {
    // Admin can pass a full storage id (`key:<id>:<sid>`) or a bare id for local (no-key) sessions.
    let raw = engine.store.delete_session(&id).unwrap_or(false);
    let local = engine.store.delete_session(&eay_gateway_core::engine::session_storage_id(None, &id)).unwrap_or(false);
    let deleted = raw || local;
    Json(json!({ "status": if deleted { "deleted" } else { "not_found" } }))
}

fn default_true() -> bool {
    true
}
