pub mod accounts;
pub mod admin;
pub mod providers;

use axum::{
    extract::Request,
    http::{header, StatusCode},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{delete, get, patch, post, put},
    Json, Router,
};
use eay_gateway_core::engine::GatewayEngine;
use serde_json::json;

async fn handle_ui() -> Html<&'static str> {
    Html(include_str!("../ui.html"))
}

/// Without an admin token, only loopback Host headers may reach admin routes: blocks DNS rebinding
/// from a browser tab (which could otherwise register a stdio MCP server = arbitrary command).
pub(crate) fn is_loopback_host(headers: &axum::http::HeaderMap) -> bool {
    let Some(host) = headers.get(header::HOST).and_then(|v| v.to_str().ok()) else {
        return true; // HTTP/1.0 or in-process calls; browsers always send Host
    };
    let name = if host.starts_with('[') { host.split(']').next().map(|h| &h[1..]) } else { host.split(':').next() };
    matches!(name, Some("localhost" | "127.0.0.1" | "::1"))
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[derive(Debug, PartialEq)]
enum AdminAccess {
    Allow,
    ReadOnly,
    Unauthorized,
    LoopbackOnly,
}

/// `full` can do everything; `read` can only GET (dashboards, monitors). With neither set,
/// admin is loopback-only.
fn admin_access(full: Option<&str>, read: Option<&str>, provided: Option<&str>, is_get: bool, loopback: bool) -> AdminAccess {
    if full.is_none() && read.is_none() {
        return if loopback { AdminAccess::Allow } else { AdminAccess::LoopbackOnly };
    }
    let is = |t: Option<&str>| matches!((provided, t), (Some(p), Some(t)) if constant_time_eq(p.as_bytes(), t.as_bytes()));
    match (is(full), is(read)) {
        (true, _) => AdminAccess::Allow,
        (_, true) if is_get => AdminAccess::Allow,
        (_, true) => AdminAccess::ReadOnly,
        _ => AdminAccess::Unauthorized,
    }
}

async fn admin_auth_middleware(req: Request, next: Next) -> Response {
    let env_token = |name: &str| std::env::var(name).ok().map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
    let (full, read) = (env_token("EAY_GATEWAY_ADMIN_TOKEN"), env_token("EAY_GATEWAY_ADMIN_READ_TOKEN"));
    let provided = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer ").or(v.strip_prefix("bearer ")))
        .or_else(|| req.headers().get("x-admin-token").and_then(|v| v.to_str().ok()))
        .map(str::trim);
    let access =
        admin_access(full.as_deref(), read.as_deref(), provided, req.method() == axum::http::Method::GET, is_loopback_host(req.headers()));
    let (status, detail) = match access {
        AdminAccess::Allow => return next.run(req).await,
        AdminAccess::ReadOnly => (StatusCode::FORBIDDEN, "This admin token is read-only"),
        AdminAccess::Unauthorized => (StatusCode::UNAUTHORIZED, "Admin token required"),
        AdminAccess::LoopbackOnly => (StatusCode::FORBIDDEN, "Admin API is loopback-only; set --admin-token to allow other hosts"),
    };
    (status, Json(json!({ "detail": detail }))).into_response()
}

/// Enforces a virtual key's `max_concurrent`; the slot is released when the response body
/// (including a long SSE stream) has been fully sent or dropped.
async fn concurrency_middleware(axum::extract::State(engine): axum::extract::State<GatewayEngine>, req: Request, next: Next) -> Response {
    let secret = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer ").or(v.strip_prefix("bearer ")))
        .or_else(|| req.headers().get("x-api-key").and_then(|v| v.to_str().ok()))
        .map(str::trim)
        .filter(|s| s.starts_with(eay_gateway_core::store::KEY_PREFIX));
    let Some(key) = secret.and_then(|s| engine.store.find_virtual_key(s)).filter(|k| k.max_concurrent.is_some()) else {
        return next.run(req).await;
    };
    let Some(guard) = engine.try_acquire_inflight(&key) else {
        let msg = format!("Key '{}' already has {} requests in flight", key.name, key.max_concurrent.unwrap_or(0));
        return (StatusCode::TOO_MANY_REQUESTS, Json(json!({ "detail": msg, "error": { "message": msg, "type": "rate_limit" } })))
            .into_response();
    };
    let resp = next.run(req).await;
    let (parts, body) = resp.into_parts();
    let stream = futures::StreamExt::map(body.into_data_stream(), move |chunk| {
        let _ = &guard; // keep the slot until the stream ends
        chunk
    });
    Response::from_parts(parts, axum::body::Body::from_stream(stream))
}

/// Largest accepted request body: base64 media inflates ~33%, so this fits a 25 MB audio file
/// or several large images in one chat request.
pub const MAX_BODY_BYTES: usize = 64 * 1024 * 1024;

/// Builds the Axum router for OpenAI-compatible inference and gateway admin APIs.
pub fn gateway_router(engine: GatewayEngine) -> Router {
    let public = Router::new()
        // Embedded Web Console
        .route("/", get(handle_ui))
        .route("/ui", get(handle_ui))
        // Core chat completions, Anthropic/Responses native, multimodal passthrough & health
        .route("/v1/chat/completions", post(admin::handle_chat_completions))
        .route("/gateway/v1/chat/completions", post(admin::handle_chat_completions))
        .route("/v1/messages", post(admin::handle_anthropic_messages))
        .route("/gateway/v1/messages", post(admin::handle_anthropic_messages))
        .route("/v1/responses", post(admin::handle_responses))
        .route("/gateway/v1/responses", post(admin::handle_responses))
        .route("/v1/embeddings", post(admin::handle_embeddings))
        .route("/gateway/v1/embeddings", post(admin::handle_embeddings))
        .route("/v1/images/generations", post(admin::handle_images_generations))
        .route("/gateway/v1/images/generations", post(admin::handle_images_generations))
        .route("/v1/audio/speech", post(admin::handle_audio_speech))
        .route("/gateway/v1/audio/speech", post(admin::handle_audio_speech))
        .route("/v1/audio/transcriptions", post(admin::handle_audio_transcriptions))
        .route("/gateway/v1/audio/transcriptions", post(admin::handle_audio_transcriptions))
        .route("/v1/audio/translations", post(admin::handle_audio_translations))
        .route("/gateway/v1/audio/translations", post(admin::handle_audio_translations))
        .route("/v1/models", get(admin::handle_list_models_v1))
        .route("/gateway/v1/models", get(admin::handle_list_models_v1))
        .route("/gateway/health", get(admin::handle_health))
        .layer(middleware::from_fn_with_state(engine.clone(), concurrency_middleware))
        // axum's 2 MB default rejects long agent contexts, inline images/PDFs and audio uploads.
        .layer(axum::extract::DefaultBodyLimit::max(MAX_BODY_BYTES));

    let admin_routes = Router::new()
        // Accounts (OAuth)
        .route("/gateway/admin/accounts", get(accounts::handle_list_accounts).post(accounts::handle_save_account))
        .route("/gateway/admin/accounts/usage", get(accounts::handle_accounts_usage))
        .route("/gateway/admin/accounts/:provider", delete(accounts::handle_delete_account_by_query))
        .route("/gateway/admin/accounts/:provider/:id", delete(accounts::handle_remove_account))
        .route("/gateway/admin/accounts/:provider/:id/toggle", patch(accounts::handle_toggle_account))
        .route("/gateway/admin/accounts/:provider/reorder", post(accounts::handle_reorder_accounts))
        .route("/gateway/admin/accounts/:provider/start", post(accounts::handle_oauth_start))
        .route("/gateway/admin/accounts/:provider/complete", post(accounts::handle_oauth_complete))
        .route("/gateway/admin/accounts/:provider/poll", post(accounts::handle_oauth_poll))
        .route("/gateway/admin/accounts/:provider/test", post(accounts::handle_oauth_test))
        // Providers & Custom Models
        .route("/gateway/admin/providers", get(providers::handle_list_providers))
        .route("/gateway/admin/providers/:id/toggle", patch(providers::handle_toggle_provider))
        .route("/gateway/admin/providers/:id/import", post(providers::handle_import_local_key))
        .route("/gateway/admin/providers/:id/connect", post(providers::handle_connect_provider))
        .route("/gateway/admin/providers/:id/disconnect", post(providers::handle_disconnect_provider))
        .route("/gateway/admin/providers/:id/key", post(providers::handle_save_provider_key))
        .route("/gateway/admin/providers/:id/keys", post(providers::handle_add_provider_key))
        .route(
            "/gateway/admin/providers/:id/models",
            get(providers::handle_get_provider_models)
                .post(providers::handle_add_provider_model)
                .put(providers::handle_set_provider_models)
                .delete(providers::handle_delete_provider_model_body),
        )
        .route("/gateway/admin/providers/:id/models/sync", post(providers::handle_sync_provider_models))
        .route("/gateway/admin/providers/:id/models/:model", delete(providers::handle_delete_provider_model))
        // Virtual Keys
        .route("/gateway/admin/keys", get(admin::handle_list_keys).post(admin::handle_create_key))
        .route("/gateway/admin/keys/:id/toggle", patch(admin::handle_toggle_key))
        .route("/gateway/admin/keys/:id", delete(admin::handle_delete_key))
        // Combos
        .route("/gateway/admin/combos", get(admin::handle_list_combos).post(admin::handle_save_combo))
        .route("/gateway/admin/combos/:name", put(admin::handle_put_combo).delete(admin::handle_delete_combo))
        // Skills (Context Presets) & Agents (Personalities) & Sessions
        .route("/gateway/admin/skills", get(admin::handle_list_skills).post(admin::handle_save_skill))
        .route("/gateway/admin/skills/:name", put(admin::handle_put_skill).delete(admin::handle_delete_skill))
        .route("/gateway/admin/skills/:name/toggle", patch(admin::handle_toggle_skill))
        .route("/gateway/admin/agents", get(admin::handle_list_agents).post(admin::handle_save_agent))
        .route("/gateway/admin/agents/:name", put(admin::handle_put_agent).delete(admin::handle_delete_agent))
        .route("/gateway/admin/sessions/:id", delete(admin::handle_delete_session))
        // Live Activity & Subagents
        .route("/gateway/admin/events", get(admin::handle_activity_events))
        .route("/gateway/admin/activity", get(admin::handle_activity_snapshot))
        .route("/gateway/admin/subagents", get(admin::handle_list_subagents).post(admin::handle_spawn_subagent))
        .route("/gateway/admin/subagents/:id", get(admin::handle_get_subagent).delete(admin::handle_delete_subagent))
        .route("/gateway/admin/subagents/:id/cancel", post(admin::handle_cancel_subagent))
        // External MCP Connectors
        .route("/gateway/admin/mcp", get(admin::handle_list_mcp).post(admin::handle_save_mcp))
        .route("/gateway/admin/mcp/import", post(admin::handle_import_mcp))
        .route("/gateway/admin/mcp/:name", delete(admin::handle_delete_mcp))
        .route("/gateway/admin/mcp/:name/call", post(admin::handle_mcp_call))
        .route("/gateway/admin/mcp/:name/toggle", patch(admin::handle_toggle_mcp))
        .route("/gateway/admin/mcp/:name/tools", post(admin::handle_mcp_tools))
        // Models, Usage, Stats, Settings
        .route("/gateway/admin/models", get(admin::handle_admin_models))
        .route("/gateway/admin/usage", get(admin::handle_list_usage).delete(admin::handle_clear_usage))
        .route("/gateway/admin/stats", get(admin::handle_get_stats))
        .route("/gateway/admin/analytics", get(admin::handle_analytics))
        .route("/gateway/admin/settings", get(admin::handle_get_settings).post(admin::handle_update_settings))
        .layer(middleware::from_fn(admin_auth_middleware));

    public.merge(admin_routes).with_state(engine)
}

#[cfg(test)]
mod tests {
    use super::{admin_access, AdminAccess::*};

    #[test]
    fn admin_tokens_grant_full_or_read_only_access() {
        assert_eq!(admin_access(None, None, None, false, true), Allow);
        assert_eq!(admin_access(None, None, None, true, false), LoopbackOnly);
        assert_eq!(admin_access(Some("full"), Some("ro"), Some("full"), false, false), Allow);
        assert_eq!(admin_access(Some("full"), Some("ro"), Some("ro"), true, false), Allow);
        assert_eq!(admin_access(Some("full"), Some("ro"), Some("ro"), false, true), ReadOnly);
        assert_eq!(admin_access(Some("full"), Some("ro"), Some("nope"), true, true), Unauthorized);
        assert_eq!(admin_access(None, Some("ro"), None, true, true), Unauthorized);
    }
}
