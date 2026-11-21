use crate::error::GatewayError;
use axum::{
    body::{Body, Bytes},
    extract::{Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use eay_gateway_core::engine::{ChatError, ChatOptions, ChatReply, GatewayEngine, PassthroughPayload, PassthroughReply};
use eay_gateway_core::providers::{OAUTH_PROVIDERS, PROVIDERS};
use eay_gateway_core::store::{VirtualKeyRecord, KEY_PREFIX};
use eay_gateway_core::translate::{
    anthropic_to_openai, openai_completion_to_anthropic, openai_completion_to_responses, responses_to_openai, OpenAiToAnthropicSse,
    OpenAiToResponsesSse,
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
    // MCP runs local commands, so a rebound browser origin must not be able to trigger it.
    let mcp_servers = headers
        .get("x-eay-mcp")
        .filter(|_| super::is_loopback_host(headers))
        .and_then(|v| v.to_str().ok())
        .map(|s| s.split(',').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect())
        .unwrap_or_default();
    // Lets a client (the console playground) follow its own request on /gateway/events.
    let run_id = headers
        .get("x-eay-run-id")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty() && s.len() <= 64 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'))
        .map(String::from);
    let team = headers.get("x-eay-team").and_then(|v| v.to_str().ok()).map(str::trim).filter(|s| !s.is_empty()).map(String::from);
    let project = headers.get("x-eay-project").and_then(|v| v.to_str().ok()).map(str::trim).filter(|s| !s.is_empty()).map(String::from);
    let no_failover = headers
        .get("x-eay-no-failover")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("true") || v.trim() == "1");
    let source = headers
        .get("x-eay-source")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty() && s.len() <= 64)
        .map(String::from);
    ChatOptions {
        skills,
        session_id,
        team,
        project,
        max_context_chars,
        redact_secrets,
        mcp_servers,
        mcp_allowed: super::is_loopback_host(headers),
        run_id,
        no_failover,
        source,
        ..Default::default()
    }
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
    let echo_run_id = opts.run_id.clone();
    match engine.chat_completions_with_options(key, payload, opts).await {
        Ok(ChatReply::Stream(frames)) => {
            let mut resp = Response::new(Body::from_stream(frames));
            let h = resp.headers_mut();
            h.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/event-stream"));
            h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
            if let Some(rid) = echo_run_id.and_then(|s| HeaderValue::from_str(&s).ok()) {
                h.insert("x-eay-run-id", rid);
            }
            resp
        }
        Ok(ChatReply::Json(v)) => {
            let routed = v.get("model").and_then(|m| m.as_str()).map(String::from);
            let mut resp = Json(v).into_response();
            let h = resp.headers_mut();
            if let Some(rid) = echo_run_id.and_then(|s| HeaderValue::from_str(&s).ok()) {
                h.insert("x-eay-run-id", rid);
            }
            if let Some(m) = routed.and_then(|s| HeaderValue::from_str(&s).ok()) {
                h.insert("x-eay-routed-model", m);
            }
            resp
        }
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
            // Re-encode chat chunks as Responses events; the tail closes the response even if upstream never sent [DONE].
            let conv = std::sync::Arc::new(std::sync::Mutex::new(OpenAiToResponsesSse::new(requested_model)));
            let tail_conv = conv.clone();
            let mapped = frames
                .map(move |item| {
                    item.map(|bytes| {
                        Bytes::from(conv.lock().map(|mut c| c.feed_sse_chunk(&String::from_utf8_lossy(&bytes))).unwrap_or_default())
                    })
                })
                .chain(futures::stream::once(async move { Ok(Bytes::from(tail_conv.lock().map(|mut c| c.finish()).unwrap_or_default())) }));
            let mut resp = Response::new(Body::from_stream(mapped));
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
