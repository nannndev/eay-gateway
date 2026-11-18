use crate::error::GatewayError;
use axum::{
    body::{Body, Bytes},
    extract::{Path, Query, State},
    http::HeaderMap,
    http::{header, HeaderValue, StatusCode},
    response::sse::{Event, KeepAlive, Sse},
    response::IntoResponse,
    response::Response,
    Json,
};
use eay_gateway_core::engine::{ChatError, ChatOptions, ChatReply, GatewayEngine, PassthroughPayload, PassthroughReply};
use eay_gateway_core::providers::{OAUTH_PROVIDERS, PROVIDERS};
use eay_gateway_core::store::{VirtualKeyRecord, KEY_PREFIX};
use eay_gateway_core::translate::{
    anthropic_to_openai, openai_completion_to_anthropic, openai_completion_to_responses, responses_to_openai, OpenAiToAnthropicSse,
};
use futures::StreamExt;
use serde::Deserialize;
use serde_json::{json, Value};

fn resolve_virtual_key(engine: &GatewayEngine, headers: &HeaderMap) -> Result<Option<VirtualKeyRecord>, GatewayError> {
    let presented = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer ").or(v.strip_prefix("bearer ")))
        .or_else(|| headers.get("x-api-key").and_then(|v| v.to_str().ok()))
        .map(str::trim)
        .filter(|s| !s.is_empty());
    match presented {
        // Agents often send a dummy key; only gateway-issued keys are checked.
        Some(secret) if secret.starts_with(KEY_PREFIX) => match engine.store.find_virtual_key(secret) {
            Some(k) if k.enabled => Ok(Some(k)),
            Some(_) => Err(GatewayError::Unauthorized("API key is disabled".into())),
            None => Err(GatewayError::Unauthorized("Invalid API key".into())),
        },
        _ => Ok(None),
    }
}

fn passthrough_response(res: Result<PassthroughReply, ChatError>) -> Response {
    match res {
        Ok(reply) => {
            let mut resp = Response::new(Body::from(reply.body));
            *resp.status_mut() = StatusCode::from_u16(reply.status).unwrap_or(StatusCode::OK);
            if let Ok(ct) = HeaderValue::from_str(&reply.content_type) {
                resp.headers_mut().insert(header::CONTENT_TYPE, ct);
            }
            resp
        }
        Err(e) => (StatusCode::from_u16(e.status).unwrap_or(StatusCode::BAD_GATEWAY), Json(e.body)).into_response(),
    }
}

/// Scans a `multipart/form-data` payload for a simple text field (such as `model`).
fn extract_multipart_field(body: &[u8], field_name: &str) -> Option<String> {
    let text = String::from_utf8_lossy(body);
    let needle = format!("name=\"{field_name}\"");
    let pos = text.find(&needle)?;
    let after = &text[pos + needle.len()..];
    let body_start = after.find("\r\n\r\n").map(|i| i + 4).or_else(|| after.find("\n\n").map(|i| i + 2))?;
    let rest = &after[body_start..];
    let end = rest.find("\r\n--").or_else(|| rest.find("\n--")).unwrap_or(rest.len());
    let val = rest[..end].trim();
    if val.is_empty() {
        None
    } else {
        Some(val.to_string())
    }
}

fn chat_options_from_headers(headers: &HeaderMap) -> ChatOptions {
    let skills = headers
        .get("x-eay-skills")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.split(',').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect())
        .unwrap_or_default();
    let session_id =
        headers.get("x-eay-session-id").and_then(|v| v.to_str().ok()).map(str::trim).filter(|s| !s.is_empty()).map(String::from);
    let max_context_chars =
        headers.get("x-eay-max-context-chars").and_then(|v| v.to_str().ok()).and_then(|s| s.trim().parse().ok()).unwrap_or(0);
    let redact_secrets = headers.get("x-eay-redact").and_then(|v| v.to_str().ok()).is_some_and(|v| v == "true" || v == "1");
    let mcp_servers = headers
        .get("x-eay-mcp")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.split(',').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect())
        .unwrap_or_default();
    ChatOptions { skills, session_id, max_context_chars, redact_secrets, mcp_servers, ..Default::default() }
}

/// A presented key must be a valid, enabled virtual key; no key means the local app (anonymous).
pub async fn handle_chat_completions(
    State(engine): State<GatewayEngine>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> axum::response::Response {
    let key = match resolve_virtual_key(&engine, &headers) {
        Ok(k) => k,
        Err(e) => return e.into_response(),
    };
    let opts = chat_options_from_headers(&headers);
    match engine.chat_completions_with_options(key, payload, opts).await {
        Ok(ChatReply::Stream(frames)) => {
            let mut resp = Response::new(Body::from_stream(frames));
            let h = resp.headers_mut();
            h.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/event-stream"));
            h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
            resp
        }
        Ok(ChatReply::Json(v)) => Json(v).into_response(),
        Err(e) => (StatusCode::from_u16(e.status).unwrap_or(StatusCode::BAD_GATEWAY), Json(e.body)).into_response(),
    }
}

pub async fn handle_images_generations(State(engine): State<GatewayEngine>, headers: HeaderMap, Json(payload): Json<Value>) -> Response {
    let key = match resolve_virtual_key(&engine, &headers) {
        Ok(k) => k,
        Err(e) => return e.into_response(),
    };
    let req_model = payload.get("model").and_then(|m| m.as_str()).unwrap_or("openai/dall-e-3").to_string();
    passthrough_response(engine.passthrough_openai_post(key, "images/generations", &req_model, PassthroughPayload::Json(payload)).await)
}

pub async fn handle_audio_speech(State(engine): State<GatewayEngine>, headers: HeaderMap, Json(payload): Json<Value>) -> Response {
    let key = match resolve_virtual_key(&engine, &headers) {
        Ok(k) => k,
        Err(e) => return e.into_response(),
    };
    let req_model = payload.get("model").and_then(|m| m.as_str()).unwrap_or("openai/tts-1").to_string();
    passthrough_response(engine.passthrough_openai_post(key, "audio/speech", &req_model, PassthroughPayload::Json(payload)).await)
}

#[derive(Deserialize)]
pub struct AudioQuery {
    model: Option<String>,
}

async fn handle_audio_multipart(engine: GatewayEngine, headers: HeaderMap, query: AudioQuery, body: Bytes, endpoint: &str) -> Response {
    let key = match resolve_virtual_key(&engine, &headers) {
        Ok(k) => k,
        Err(e) => return e.into_response(),
    };
    let content_type = headers.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("multipart/form-data").to_string();
    let req_model = query.model.or_else(|| extract_multipart_field(&body, "model")).unwrap_or_else(|| "openai/whisper-1".to_string());
    passthrough_response(engine.passthrough_openai_post(key, endpoint, &req_model, PassthroughPayload::Raw { content_type, body }).await)
}

pub async fn handle_audio_transcriptions(
    State(engine): State<GatewayEngine>,
    headers: HeaderMap,
    Query(query): Query<AudioQuery>,
    body: Bytes,
) -> Response {
    handle_audio_multipart(engine, headers, query, body, "audio/transcriptions").await
}

pub async fn handle_audio_translations(
    State(engine): State<GatewayEngine>,
    headers: HeaderMap,
    Query(query): Query<AudioQuery>,
    body: Bytes,
) -> Response {
    handle_audio_multipart(engine, headers, query, body, "audio/translations").await
}

pub async fn handle_embeddings(State(engine): State<GatewayEngine>, headers: HeaderMap, Json(payload): Json<Value>) -> Response {
    let key = match resolve_virtual_key(&engine, &headers) {
        Ok(k) => k,
        Err(e) => return e.into_response(),
    };
    let req_model = payload.get("model").and_then(|m| m.as_str()).unwrap_or("openai/text-embedding-3-small").to_string();
    passthrough_response(engine.passthrough_openai_post(key, "embeddings", &req_model, PassthroughPayload::Json(payload)).await)
}

/// Anthropic-native `/v1/messages` endpoint (used by Claude Code CLI and Anthropic SDKs).
pub async fn handle_anthropic_messages(State(engine): State<GatewayEngine>, headers: HeaderMap, Json(payload): Json<Value>) -> Response {
    let key = match resolve_virtual_key(&engine, &headers) {
        Ok(k) => k,
        Err(e) => return e.into_response(),
    };
    let opts = chat_options_from_headers(&headers);

    let requested_model = payload.get("model").and_then(|m| m.as_str()).unwrap_or("auto").to_string();
    let mut oa_payload = anthropic_to_openai(&payload);
    // Resolve bare Anthropic model names (e.g. "claude-sonnet-4-5") to a connected Claude/Anthropic provider or "auto".
    if !requested_model.contains('/') && requested_model != "auto" && !requested_model.starts_with("auto+") {
        let (base, suffix) = requested_model.split_once('+').map(|(b, s)| (b, Some(s))).unwrap_or((&requested_model, None));
        let has_claude_oauth = engine.store.list_accounts().iter().any(|a| a.provider == "claude" && a.enabled);
        let has_anthropic_key = engine.store.get_provider_key("anthropic").is_some();
        let resolved_base = if base.starts_with("claude") && has_claude_oauth {
            format!("claude/{base}")
        } else if base.starts_with("claude") && has_anthropic_key {
            format!("anthropic/{base}")
        } else {
            "auto".to_string()
        };
        oa_payload["model"] = json!(match suffix {
            Some(s) => format!("{resolved_base}+{s}"),
            None => resolved_base,
        });
    }

    match engine.chat_completions_with_options(key, oa_payload, opts).await {
        Ok(ChatReply::Stream(frames)) => {
            let mut conv = OpenAiToAnthropicSse::new(requested_model);
            let mapped = frames.map(move |item| {
                item.map(|bytes| {
                    let s = String::from_utf8_lossy(&bytes);
                    Bytes::from(conv.feed_sse_chunk(&s))
                })
            });
            let mut resp = Response::new(Body::from_stream(mapped));
            let h = resp.headers_mut();
            h.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/event-stream"));
            h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
            resp
        }
        Ok(ChatReply::Json(v)) => Json(openai_completion_to_anthropic(&v, &requested_model)).into_response(),
        Err(e) => {
            let msg = e.body["error"]["message"].as_str().unwrap_or("Gateway error");
            let err_body = json!({ "type": "error", "error": { "type": "api_error", "message": msg } });
            (StatusCode::from_u16(e.status).unwrap_or(StatusCode::BAD_GATEWAY), Json(err_body)).into_response()
        }
    }
}

/// OpenAI Responses API (`/v1/responses`) handler.
pub async fn handle_responses(State(engine): State<GatewayEngine>, headers: HeaderMap, Json(payload): Json<Value>) -> Response {
    let key = match resolve_virtual_key(&engine, &headers) {
        Ok(k) => k,
        Err(e) => return e.into_response(),
    };
    let opts = chat_options_from_headers(&headers);
    let requested_model = payload.get("model").and_then(|m| m.as_str()).unwrap_or("auto").to_string();
    let oa_payload = responses_to_openai(&payload);
    match engine.chat_completions_with_options(key, oa_payload, opts).await {
        Ok(ChatReply::Stream(frames)) => {
            let mut resp = Response::new(Body::from_stream(frames));
            let h = resp.headers_mut();
            h.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/event-stream"));
            h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
            resp
        }
        Ok(ChatReply::Json(v)) => Json(openai_completion_to_responses(&v, &requested_model)).into_response(),
        Err(e) => (StatusCode::from_u16(e.status).unwrap_or(StatusCode::BAD_GATEWAY), Json(e.body)).into_response(),
    }
}

pub async fn handle_health() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}

/// Models of connected providers only (enabled, with an account or key), `auto` modes first.
/// `owned_by` is the provider id so clients can group the list.
pub async fn handle_list_models_v1(State(engine): State<GatewayEngine>) -> Json<Value> {
    let mut data: Vec<Value> =
        engine.auto_model_ids().into_iter().map(|id| json!({ "id": id, "object": "model", "owned_by": "eay-gateway" })).collect();
    let settings = engine.store.get_gateway_settings();
    for id in engine.auto_provider_order(&settings) {
        let (alias, seeds) = match (OAUTH_PROVIDERS.iter().find(|o| o.id == id), PROVIDERS.iter().find(|p| p.id == id)) {
            (Some(o), _) => (o.alias, o.seed_models),
            (None, Some(p)) => (p.alias, p.seed_models),
            _ => continue,
        };
        for m in engine.store.display_models(&id, seeds) {
            data.push(json!({ "id": format!("{alias}/{m}"), "object": "model", "owned_by": id }));
        }
    }
    for agent in engine.store.list_agents().into_iter().filter(|a| a.enabled) {
        data.push(json!({ "id": format!("agent/{}", agent.name), "object": "model", "owned_by": "eay-gateway" }));
    }
    Json(json!({ "object": "list", "data": data }))
}

pub async fn handle_admin_models(State(engine): State<GatewayEngine>) -> Json<Value> {
    let mut models = Vec::new();
    for op in OAUTH_PROVIDERS {
        let enabled = engine.store.is_provider_enabled(op.id);
        let custom = engine.store.get_provider_custom_models(op.id);
        for m in engine.store.display_models(op.id, op.seed_models) {
            let is_custom = custom.iter().any(|c| c == &m);
            models.push(json!({
                "id": format!("{}/{}", op.alias, m),
                "model": m,
                "owned_by": op.id,
                "enabled": enabled,
                "custom": is_custom,
            }));
        }
    }
    for p in PROVIDERS {
        let enabled = engine.store.is_provider_enabled(p.id);
        let custom = engine.store.get_provider_custom_models(p.id);
        for m in engine.store.display_models(p.id, p.seed_models) {
            let is_custom = custom.iter().any(|c| c == &m);
            models.push(json!({
                "id": format!("{}/{}", p.alias, m),
                "model": m,
                "owned_by": p.id,
                "enabled": enabled,
                "custom": is_custom,
            }));
        }
    }
    Json(json!({ "models": models }))
}

// ---- Virtual Keys ----------------------------------------------------

pub async fn handle_list_keys(State(engine): State<GatewayEngine>) -> Json<Value> {
    Json(json!({ "keys": engine.store.list_virtual_keys() }))
}

#[derive(Deserialize)]
pub struct CreateKeyPayload {
    name: String,
    allowed_providers: Option<Vec<String>>,
    rate_limit_per_min: Option<i64>,
    max_tokens_budget: Option<i64>,
    max_cost_usd: Option<f64>,
    default_skills: Option<Vec<String>>,
}

pub async fn handle_create_key(
    State(engine): State<GatewayEngine>,
    Json(payload): Json<CreateKeyPayload>,
) -> Result<Json<Value>, GatewayError> {
    let (mut record, secret) = engine
        .store
        .create_virtual_key(&payload.name, payload.allowed_providers, payload.rate_limit_per_min)
        .map_err(GatewayError::Internal)?;
    if payload.max_tokens_budget.is_some() || payload.max_cost_usd.is_some() {
        engine.store.set_virtual_key_limits(&record.id, payload.max_tokens_budget, payload.max_cost_usd).map_err(GatewayError::Internal)?;
        record.max_tokens_budget = payload.max_tokens_budget;
        record.max_cost_usd = payload.max_cost_usd;
    }
    if let Some(skills) = payload.default_skills.filter(|s| !s.is_empty()) {
        engine.store.set_virtual_key_skills(&record.id, Some(skills.clone())).map_err(GatewayError::Internal)?;
        record.default_skills = Some(skills);
    }
    let mut val = serde_json::to_value(&record).map_err(|e| GatewayError::Internal(e.to_string()))?;
    val["key"] = json!(secret);
    Ok(Json(json!({ "key": val })))
}

// ---- Skills (Context Presets) ----------------------------------------

#[derive(Deserialize)]
pub struct SaveSkillPayload {
    name: Option<String>,
    #[serde(default)]
    description: String,
    prompt: String,
    #[serde(default = "default_true")]
    enabled: bool,
}

fn default_true() -> bool {
    true
}

pub async fn handle_list_skills(State(engine): State<GatewayEngine>) -> Json<Value> {
    Json(json!({ "skills": engine.store.list_skills() }))
}

pub async fn handle_save_skill(
    State(engine): State<GatewayEngine>,
    Json(payload): Json<SaveSkillPayload>,
) -> Result<Json<Value>, GatewayError> {
    let name = payload.name.as_deref().unwrap_or("");
    let skill = engine.store.save_skill(name, &payload.description, &payload.prompt, payload.enabled).map_err(GatewayError::BadRequest)?;
    Ok(Json(json!({ "skill": skill })))
}

pub async fn handle_put_skill(
    State(engine): State<GatewayEngine>,
    Path(name): Path<String>,
    Json(payload): Json<SaveSkillPayload>,
) -> Result<Json<Value>, GatewayError> {
    let skill = engine.store.save_skill(&name, &payload.description, &payload.prompt, payload.enabled).map_err(GatewayError::BadRequest)?;
    Ok(Json(json!({ "skill": skill })))
}

pub async fn handle_toggle_skill(State(engine): State<GatewayEngine>, Path(name): Path<String>) -> Result<Json<Value>, GatewayError> {
    let skill = engine.store.toggle_skill(&name).map_err(GatewayError::NotFound)?;
    Ok(Json(json!({ "skill": skill })))
}

pub async fn handle_delete_skill(State(engine): State<GatewayEngine>, Path(name): Path<String>) -> Json<Value> {
    let deleted = engine.store.delete_skill(&name).unwrap_or(false);
    Json(json!({ "status": if deleted { "deleted" } else { "not_found" } }))
}

// ---- Agents (Personality Profiles) -----------------------------------

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
        .save_agent(name, &p.description, &p.model, &p.skills, &p.personality, p.temperature, p.max_tokens, p.enabled)
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
        .save_agent(&name, &p.description, &p.model, &p.skills, &p.personality, p.temperature, p.max_tokens, p.enabled)
        .map_err(GatewayError::BadRequest)?;
    Ok(Json(json!({ "agent": agent })))
}

pub async fn handle_delete_agent(State(engine): State<GatewayEngine>, Path(name): Path<String>) -> Json<Value> {
    let deleted = engine.store.delete_agent(&name).unwrap_or(false);
    Json(json!({ "status": if deleted { "deleted" } else { "not_found" } }))
}

pub async fn handle_delete_session(State(engine): State<GatewayEngine>, Path(id): Path<String>) -> Json<Value> {
    let deleted = engine.store.delete_session(&id).unwrap_or(false);
    Json(json!({ "status": if deleted { "deleted" } else { "not_found" } }))
}

// ---- Live Activity & Subagents ---------------------------------------

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
}

pub async fn handle_list_subagents(State(engine): State<GatewayEngine>) -> Json<Value> {
    Json(json!({ "subagents": engine.activity.list_runs() }))
}

pub async fn handle_spawn_subagent(
    State(engine): State<GatewayEngine>,
    Json(p): Json<SpawnSubagentPayload>,
) -> Result<Json<Value>, GatewayError> {
    if p.role.trim().is_empty() || p.prompt.trim().is_empty() {
        return Err(GatewayError::BadRequest("Both 'role' and 'prompt' are required".into()));
    }
    let opts = ChatOptions { skills: p.skills, session_id: p.session_id, ..Default::default() };
    let run = engine.spawn_subagent(p.role, p.model, p.prompt, p.system, opts);
    Ok(Json(json!({ "subagent": run })))
}

pub async fn handle_get_subagent(State(engine): State<GatewayEngine>, Path(id): Path<String>) -> Result<Json<Value>, GatewayError> {
    let run = engine.activity.get_run(&id).ok_or_else(|| GatewayError::NotFound("Subagent not found".into()))?;
    Ok(Json(json!({ "subagent": run })))
}

pub async fn handle_delete_subagent(State(engine): State<GatewayEngine>, Path(id): Path<String>) -> Json<Value> {
    let deleted = engine.activity.delete_run(&id);
    Json(json!({ "status": if deleted { "deleted" } else { "not_found" } }))
}

// ---- External MCP Connectors -----------------------------------------

#[derive(Deserialize)]
pub struct SaveMcpPayload {
    name: Option<String>,
    #[serde(default = "default_stdio")]
    transport: String,
    command: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: std::collections::HashMap<String, String>,
    url: Option<String>,
    #[serde(default)]
    headers: std::collections::HashMap<String, String>,
    #[serde(default = "default_true")]
    enabled: bool,
}

fn default_stdio() -> String {
    "stdio".into()
}

pub async fn handle_list_mcp(State(engine): State<GatewayEngine>) -> Json<Value> {
    Json(json!({ "mcp_servers": engine.store.list_mcp_servers() }))
}

pub async fn handle_save_mcp(State(engine): State<GatewayEngine>, Json(p): Json<SaveMcpPayload>) -> Result<Json<Value>, GatewayError> {
    let name = p.name.as_deref().unwrap_or("");
    let rec = engine
        .store
        .save_mcp_server(name, &p.transport, p.command.as_deref(), &p.args, &p.env, p.url.as_deref(), &p.headers, p.enabled)
        .map_err(GatewayError::BadRequest)?;
    Ok(Json(json!({ "mcp_server": rec })))
}

pub async fn handle_toggle_mcp(State(engine): State<GatewayEngine>, Path(name): Path<String>) -> Result<Json<Value>, GatewayError> {
    let rec = engine.store.toggle_mcp_server(&name).map_err(GatewayError::NotFound)?;
    Ok(Json(json!({ "mcp_server": rec })))
}

pub async fn handle_delete_mcp(State(engine): State<GatewayEngine>, Path(name): Path<String>) -> Json<Value> {
    let deleted = engine.store.delete_mcp_server(&name).unwrap_or(false);
    Json(json!({ "status": if deleted { "deleted" } else { "not_found" } }))
}

pub async fn handle_mcp_tools(State(engine): State<GatewayEngine>, Path(name): Path<String>) -> Result<Json<Value>, GatewayError> {
    let srv = engine.store.get_mcp_server(&name).ok_or_else(|| GatewayError::NotFound("MCP server not found".into()))?;
    let tools = eay_gateway_core::mcp::list_tools_openai(&engine.http_client, &srv).await.map_err(GatewayError::BadGateway)?;
    Ok(Json(json!({ "tools": tools })))
}

pub async fn handle_toggle_key(State(engine): State<GatewayEngine>, Path(id): Path<String>) -> Result<Json<Value>, GatewayError> {
    let record = engine.store.toggle_virtual_key(&id).map_err(GatewayError::NotFound)?;
    Ok(Json(json!({ "key": record })))
}

pub async fn handle_delete_key(State(engine): State<GatewayEngine>, Path(id): Path<String>) -> Json<Value> {
    let _ = engine.store.delete_virtual_key(&id);
    Json(json!({ "status": "deleted" }))
}

// ---- Combos ----------------------------------------------------------

pub async fn handle_list_combos(State(engine): State<GatewayEngine>) -> Json<Value> {
    Json(json!({ "combos": engine.store.list_combos() }))
}

pub async fn handle_save_combo(State(engine): State<GatewayEngine>, Json(payload): Json<Value>) -> Result<Json<Value>, GatewayError> {
    let name = payload.get("name").and_then(|n| n.as_str()).unwrap_or("").trim();
    let targets: Vec<String> = payload.get("targets").and_then(|t| serde_json::from_value(t.clone()).ok()).unwrap_or_default();
    // Combos are addressed as `auto/<name>`, so the name must be one path segment.
    if name.is_empty() || name.contains('/') || name.contains(char::is_whitespace) {
        return Err(GatewayError::BadRequest("Combo name must be one word without '/' or spaces".into()));
    }
    if targets.iter().all(|t| t.trim().is_empty()) {
        return Err(GatewayError::BadRequest("Combo needs at least one model".into()));
    }
    engine.store.save_combo(name, &targets).map_err(GatewayError::BadRequest)?;
    Ok(Json(json!({ "combos": engine.store.list_combos() })))
}

pub async fn handle_put_combo(
    State(engine): State<GatewayEngine>,
    Path(name): Path<String>,
    Json(payload): Json<Value>,
) -> Result<Json<Value>, GatewayError> {
    let targets: Vec<String> = payload.get("targets").and_then(|t| serde_json::from_value(t.clone()).ok()).unwrap_or_default();
    engine.store.save_combo(&name, &targets).map_err(GatewayError::BadRequest)?;
    Ok(Json(json!({ "combos": engine.store.list_combos() })))
}

pub async fn handle_delete_combo(State(engine): State<GatewayEngine>, Path(name): Path<String>) -> Json<Value> {
    let _ = engine.store.delete_combo(&name);
    Json(json!({ "combos": engine.store.list_combos() }))
}

// ---- Usage, Stats & Settings ----------------------------------------

#[derive(Deserialize)]
pub struct UsageQuery {
    limit: Option<i64>,
}

pub async fn handle_list_usage(State(engine): State<GatewayEngine>, Query(query): Query<UsageQuery>) -> Json<Value> {
    let limit = query.limit.unwrap_or(100);
    Json(json!({ "usage": engine.store.list_usage(limit) }))
}

pub async fn handle_clear_usage(State(engine): State<GatewayEngine>) -> Json<Value> {
    let _ = engine.store.clear_usage();
    Json(json!({ "status": "cleared" }))
}

pub async fn handle_get_stats(State(engine): State<GatewayEngine>) -> Json<Value> {
    Json(engine.store.get_stats())
}

#[derive(Deserialize)]
pub struct AnalyticsQuery {
    /// Days of history; omit for all.
    days: Option<u32>,
    /// Caller's offset from UTC in minutes (east positive).
    tz_offset_min: Option<i64>,
}

pub async fn handle_analytics(State(engine): State<GatewayEngine>, Query(q): Query<AnalyticsQuery>) -> Json<Value> {
    Json(engine.store.analytics(q.days, q.tz_offset_min.unwrap_or(0) * 60))
}

pub async fn handle_get_settings(State(engine): State<GatewayEngine>) -> Json<Value> {
    Json(settings_response(&engine))
}

/// Settings plus the effective `auto` provider order, so the UI can show and reorder it.
fn settings_response(engine: &GatewayEngine) -> Value {
    let s = engine.store.get_gateway_settings();
    let order = engine.auto_provider_order(&s);
    json!({ "settings": s, "auto_order": order })
}

#[derive(Deserialize)]
pub struct UpdateSettingsPayload {
    routing_strategy: Option<String>,
    fallback_order: Option<String>,
    provider_priority: Option<Vec<String>>,
    auto_models_per_provider: Option<u32>,
    cooldown_secs: Option<u32>,
    gemini_oauth_client_id: Option<String>,
    gemini_oauth_client_secret: Option<String>,
    clear_gemini_secret: Option<bool>,
}

pub async fn handle_update_settings(
    State(engine): State<GatewayEngine>,
    Json(payload): Json<UpdateSettingsPayload>,
) -> Result<Json<Value>, GatewayError> {
    if let Some(strategy) = payload.routing_strategy {
        if !["priority", "round_robin", "fill_first"].contains(&strategy.as_str()) {
            return Err(GatewayError::BadRequest(format!("unknown routing_strategy: {strategy}")));
        }
        let _ = engine.store.set_setting("routing_strategy", &strategy);
    }
    if let Some(order) = payload.fallback_order {
        if !["model_first", "account_first"].contains(&order.as_str()) {
            return Err(GatewayError::BadRequest(format!("unknown fallback_order: {order}")));
        }
        let _ = engine.store.set_setting("fallback_order", &order);
    }
    if let Some(list) = payload.provider_priority {
        let _ = engine.store.set_setting("provider_priority", &json!(list).to_string());
    }
    if let Some(n) = payload.auto_models_per_provider {
        let _ = engine.store.set_setting("auto_models_per_provider", &n.clamp(1, 10).to_string());
    }
    if let Some(n) = payload.cooldown_secs {
        let _ = engine.store.set_setting("cooldown_secs", &n.min(3600).to_string());
    }
    if let Some(client_id) = payload.gemini_oauth_client_id {
        let _ = engine.store.set_setting("gemini_oauth_client_id", &client_id);
    }
    if payload.clear_gemini_secret == Some(true) {
        let _ = engine.store.set_setting("gemini_oauth_client_secret", "");
    } else if let Some(secret) = payload.gemini_oauth_client_secret {
        if !secret.trim().is_empty() {
            let _ = engine.store.set_setting("gemini_oauth_client_secret", &secret);
        }
    }
    Ok(Json(settings_response(&engine)))
}
