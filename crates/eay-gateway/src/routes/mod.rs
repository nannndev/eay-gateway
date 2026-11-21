pub mod accounts;
pub mod agents;
pub mod battles;
pub mod connectors;
pub mod inference;
pub mod keys;
pub mod projects;
pub mod providers;
pub mod runs;
pub mod settings;
pub mod skills;

use axum::{
    extract::Request,
    http::{header, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, patch, post, put},
    Json, Router,
};
use eay_gateway_core::engine::GatewayEngine;
use serde_json::json;

/// `/`: the gateway has no UI of its own; point people at the console and the API.
async fn handle_root() -> Json<serde_json::Value> {
    Json(json!({
        "service": "eay-gateway",
        "version": env!("CARGO_PKG_VERSION"),
        "console": "https://github.com/nannndev/eay-console",
        "openai_base_url": "/v1",
        "health": "/gateway/health",
    }))
}

/// Browser origins allowed to call this gateway (`--console-origin`, comma-separated), e.g. the
/// hosted eay-console. Anything else gets no CORS headers, so other web pages still cannot read
/// responses or send JSON to admin routes.
fn allowed_origins() -> Vec<String> {
    std::env::var("EAY_GATEWAY_CONSOLE_ORIGINS")
        .unwrap_or_default()
        .split(',')
        .map(|o| o.trim().trim_end_matches('/').to_string())
        .filter(|o| !o.is_empty())
        .collect()
}

/// Whether `origin` may call the gateway from a browser. `http://localhost:<port>` and
/// `http://127.0.0.1:<port>` are always allowed so the console can be run locally.
fn origin_allowed(origin: &str, allowed: &[String]) -> bool {
    let local = ["http://localhost:", "http://127.0.0.1:", "http://[::1]:"]
        .iter()
        .any(|p| origin.strip_prefix(p).is_some_and(|port| !port.is_empty() && port.chars().all(|c| c.is_ascii_digit())));
    local || allowed.iter().any(|a| a == origin)
}

/// CORS for the console. Requests without an `Origin` (CLIs, SDKs, desktop apps calling from
/// native code) pass through untouched. A browser request from an origin that is not allowed is
/// refused outright, not just left without CORS headers: "simple" cross-site requests (form posts,
/// multipart audio uploads) would otherwise still run and spend the user's quota. Preflights from
/// allowed origins are answered here, including Chrome's Private Network Access check, which a
/// public page needs to call 127.0.0.1.
async fn cors_middleware(req: Request, next: Next) -> Response {
    let Some(origin) = req.headers().get(header::ORIGIN).and_then(|v| v.to_str().ok()).map(str::to_string) else {
        return next.run(req).await;
    };
    if !origin_allowed(&origin, &allowed_origins()) {
        let msg = format!("Origin {origin} is not allowed; start the gateway with --console-origin {origin}");
        return (StatusCode::FORBIDDEN, Json(json!({ "detail": msg, "error": { "message": msg } }))).into_response();
    }
    if req.method() == axum::http::Method::OPTIONS {
        let mut resp = StatusCode::NO_CONTENT.into_response();
        let h = resp.headers_mut();
        h.insert(header::ACCESS_CONTROL_ALLOW_METHODS, axum::http::HeaderValue::from_static("GET, POST, PUT, PATCH, DELETE, OPTIONS"));
        if let Some(asked) = req.headers().get(header::ACCESS_CONTROL_REQUEST_HEADERS) {
            h.insert(header::ACCESS_CONTROL_ALLOW_HEADERS, asked.clone());
        }
        if req.headers().get("access-control-request-private-network").is_some() {
            h.insert("access-control-allow-private-network", axum::http::HeaderValue::from_static("true"));
        }
        h.insert(header::ACCESS_CONTROL_MAX_AGE, axum::http::HeaderValue::from_static("600"));
        add_cors_origin(h, &origin);
        return resp;
    }
    let mut resp = next.run(req).await;
    add_cors_origin(resp.headers_mut(), &origin);
    resp
}

fn add_cors_origin(h: &mut axum::http::HeaderMap, origin: &str) {
    if let Ok(v) = axum::http::HeaderValue::from_str(origin) {
        h.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, v);
        h.insert(header::VARY, axum::http::HeaderValue::from_static("Origin"));
        h.insert(
            header::ACCESS_CONTROL_EXPOSE_HEADERS,
            axum::http::HeaderValue::from_static("x-eay-run-id, x-eay-routed-model, x-eay-provider, x-eay-cost-usd"),
        );
    }
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

/// Builds the Axum router for OpenAI-compatible inference and gateway management APIs.
pub fn gateway_router(engine: GatewayEngine) -> Router {
    let public = Router::new()
        .route("/", get(handle_root))
        // Core chat completions, Anthropic/Responses native, multimodal passthrough & health
        .route("/v1/chat/completions", post(inference::handle_chat_completions))
        .route("/gateway/v1/chat/completions", post(inference::handle_chat_completions))
        .route("/v1/messages", post(inference::handle_anthropic_messages))
        .route("/gateway/v1/messages", post(inference::handle_anthropic_messages))
        .route("/v1/responses", post(inference::handle_responses))
        .route("/gateway/v1/responses", post(inference::handle_responses))
        .route("/v1/embeddings", post(inference::handle_embeddings))
        .route("/gateway/v1/embeddings", post(inference::handle_embeddings))
        .route("/v1/images/generations", post(inference::handle_images_generations))
        .route("/gateway/v1/images/generations", post(inference::handle_images_generations))
        .route("/v1/audio/speech", post(inference::handle_audio_speech))
        .route("/gateway/v1/audio/speech", post(inference::handle_audio_speech))
        .route("/v1/audio/transcriptions", post(inference::handle_audio_transcriptions))
        .route("/gateway/v1/audio/transcriptions", post(inference::handle_audio_transcriptions))
        .route("/v1/audio/translations", post(inference::handle_audio_translations))
        .route("/gateway/v1/audio/translations", post(inference::handle_audio_translations))
        .route("/v1/models", get(inference::handle_list_models_v1))
        .route("/gateway/v1/models", get(inference::handle_list_models_v1))
        .route("/gateway/health", get(inference::handle_health))
        .layer(middleware::from_fn_with_state(engine.clone(), concurrency_middleware))
        // axum's 2 MB default rejects long agent contexts, inline images/PDFs and audio uploads.
        .layer(axum::extract::DefaultBodyLimit::max(MAX_BODY_BYTES));

    let admin_routes = Router::new()
        // Accounts (OAuth)
        .route("/gateway/accounts", get(accounts::handle_list_accounts).post(accounts::handle_save_account))
        .route("/gateway/accounts/usage", get(accounts::handle_accounts_usage))
        .route("/gateway/accounts/:provider", delete(accounts::handle_delete_account_by_query))
        .route("/gateway/accounts/:provider/:id", delete(accounts::handle_remove_account))
        .route("/gateway/accounts/:provider/:id/toggle", patch(accounts::handle_toggle_account))
        .route("/gateway/accounts/:provider/reorder", post(accounts::handle_reorder_accounts))
        .route("/gateway/accounts/:provider/start", post(accounts::handle_oauth_start))
        .route("/gateway/accounts/:provider/complete", post(accounts::handle_oauth_complete))
        .route("/gateway/accounts/:provider/poll", post(accounts::handle_oauth_poll))
        .route("/gateway/accounts/:provider/test", post(accounts::handle_oauth_test))
        // Providers & Custom Models
        .route("/gateway/providers", get(providers::handle_list_providers))
        .route("/gateway/providers/:id/toggle", patch(providers::handle_toggle_provider))
        .route("/gateway/providers/:id/import", post(providers::handle_import_local_key))
        .route("/gateway/providers/:id/connect", post(providers::handle_connect_provider))
        .route("/gateway/providers/:id/disconnect", post(providers::handle_disconnect_provider))
        .route("/gateway/providers/:id/key", post(providers::handle_save_provider_key))
        .route("/gateway/providers/:id/keys", post(providers::handle_add_provider_key))
        .route(
            "/gateway/providers/:id/models",
            get(providers::handle_get_provider_models)
                .post(providers::handle_add_provider_model)
                .put(providers::handle_set_provider_models)
                .delete(providers::handle_delete_provider_model_body),
        )
        .route("/gateway/providers/:id/models/sync", post(providers::handle_sync_provider_models))
        .route("/gateway/providers/:id/models/:model", delete(providers::handle_delete_provider_model))
        // Virtual Keys
        .route("/gateway/keys", get(keys::handle_list_keys).post(keys::handle_create_key))
        .route("/gateway/keys/:id/toggle", patch(keys::handle_toggle_key))
        .route("/gateway/keys/:id", delete(keys::handle_delete_key))
        .route("/gateway/keys/:id/project", put(projects::handle_link_key))
        // Projects
        .route("/gateway/projects", get(projects::handle_list_projects).post(projects::handle_create_project))
        .route(
            "/gateway/projects/:id",
            get(projects::handle_get_project).put(projects::handle_put_project).delete(projects::handle_delete_project),
        )
        .route("/gateway/projects/:id/usage", get(projects::handle_project_usage))
        .route("/gateway/fs/dirs", get(projects::handle_list_dirs))
        .route(
            "/gateway/projects/:id/files",
            get(projects::handle_project_files)
                .put(projects::handle_write_project_file)
                .patch(projects::handle_rename_project_file)
                .delete(projects::handle_delete_project_file),
        )
        .route("/gateway/projects/:id/mkdir", post(projects::handle_mkdir_project))
        .route("/gateway/projects/:id/search", get(projects::handle_search_project_files))
        .route("/gateway/projects/:id/files/raw", get(projects::handle_project_file_raw))
        .route("/gateway/projects/:id/changes", get(projects::handle_project_changes))
        .route("/gateway/projects/:id/diff", get(projects::handle_project_diff))
        .route("/gateway/projects/:id/git", post(projects::handle_project_git))
        .route("/gateway/projects/:id/terminal", post(projects::handle_project_terminal))
        // Combos
        .route("/gateway/combos", get(settings::handle_list_combos).post(settings::handle_save_combo))
        .route("/gateway/combos/:name", put(settings::handle_put_combo).delete(settings::handle_delete_combo))
        // Skills (Context Presets) & Agents (Personalities) & Sessions
        .route("/gateway/skills", get(skills::handle_list_skills).post(skills::handle_save_skill))
        .route("/gateway/skills/marketplace", get(skills::handle_marketplace_search))
        .route("/gateway/skills/marketplace/fetch", post(skills::handle_marketplace_fetch))
        .route("/gateway/skills/:name", put(skills::handle_put_skill).delete(skills::handle_delete_skill))
        .route("/gateway/skills/:name/toggle", patch(skills::handle_toggle_skill))
        .route("/gateway/agents", get(agents::handle_list_agents).post(agents::handle_save_agent))
        .route("/gateway/agents/:name", put(agents::handle_put_agent).delete(agents::handle_delete_agent))
        .route("/gateway/agents/:name/toggle", patch(agents::handle_toggle_agent))
        .route("/gateway/teams", get(agents::handle_list_teams).post(agents::handle_save_team))
        .route("/gateway/teams/:id", delete(agents::handle_delete_team))
        .route("/gateway/teams/:id/project", put(projects::handle_link_team))
        .route("/gateway/sessions", get(agents::handle_list_sessions))
        .route(
            "/gateway/sessions/:id",
            get(agents::handle_get_session).patch(agents::handle_rename_session).delete(agents::handle_delete_session),
        )
        // Live Activity & Subagents
        .route("/gateway/events", get(runs::handle_activity_events))
        .route("/gateway/activity", get(runs::handle_activity_snapshot))
        .route("/gateway/subagents", get(runs::handle_list_subagents).post(runs::handle_spawn_subagent))
        .route("/gateway/subagents/tree", get(runs::handle_subagent_tree))
        .route("/gateway/subagents/:id", get(runs::handle_get_subagent).delete(runs::handle_delete_subagent))
        .route("/gateway/subagents/:id/cancel", post(runs::handle_cancel_subagent))
        .route("/gateway/approvals", get(runs::handle_list_approvals))
        .route("/gateway/approvals/:id", post(runs::handle_resolve_approval))
        // External MCP Connectors
        .route("/gateway/mcp", get(connectors::handle_list_mcp).post(connectors::handle_save_mcp))
        .route("/gateway/mcp/import", post(connectors::handle_import_mcp))
        .route("/gateway/mcp/:name", delete(connectors::handle_delete_mcp))
        .route("/gateway/mcp/:name/call", post(connectors::handle_mcp_call))
        .route("/gateway/mcp/:name/toggle", patch(connectors::handle_toggle_mcp))
        .route("/gateway/mcp/:name/tools", post(connectors::handle_mcp_tools))
        // Models, Usage, Stats, Settings
        .route("/gateway/models", get(settings::handle_admin_models))
        .route("/gateway/usage", get(settings::handle_list_usage).delete(settings::handle_clear_usage))
        .route("/gateway/stats", get(settings::handle_get_stats))
        .route("/gateway/analytics", get(settings::handle_analytics))
        .route("/gateway/settings", get(settings::handle_get_settings).post(settings::handle_update_settings))
        // Battles, ELO Ratings & Benchmarks (eay-battle)
        .route("/gateway/battles", get(battles::handle_list_battles).post(battles::handle_save_battle))
        .route("/gateway/battles/:id", delete(battles::handle_delete_battle))
        .route("/gateway/elo", get(battles::handle_list_elo).delete(battles::handle_reset_elo))
        .route("/gateway/elo/match", post(battles::handle_record_elo_match))
        .route("/gateway/benchmarks", get(battles::handle_list_benchmarks).post(battles::handle_save_benchmark))
        .route("/gateway/benchmarks/:id", delete(battles::handle_delete_benchmark))
        .layer(middleware::from_fn(admin_auth_middleware));

    // CORS sits outside every other layer so preflights never reach auth or concurrency checks.
    public.merge(admin_routes).with_state(engine).layer(middleware::from_fn(cors_middleware))
}

/// The gateway as a service: the router, with legacy `/gateway/admin/*` paths rewritten first.
/// Use this to serve it; `gateway_router` alone has no `/gateway/admin` paths.
pub fn gateway_service(router: Router) -> LegacyAdminService {
    tower::ServiceBuilder::new().map_request(rewrite_legacy_admin_uri as fn(Request) -> Request).service(router)
}

pub type LegacyAdminService = tower::util::MapRequest<Router, fn(Request) -> Request>;

/// Management routes lived under `/gateway/admin/*` until 0.4. Clients built against that
/// (OpenNotchi, eay-console before 0.4) keep working: `/gateway/admin/keys` is answered by
/// `/gateway/keys` with the same auth and CORS. It runs before routing, because a router layer runs
/// only after a route has matched.
fn rewrite_legacy_admin_uri(mut req: Request) -> Request {
    if let Some(new) = legacy_admin_path(req.uri()).and_then(|p| p.parse().ok()) {
        *req.uri_mut() = new;
    }
    req
}

/// `/gateway/admin/keys?x=1` → `/gateway/keys?x=1`; None for any other path.
fn legacy_admin_path(uri: &axum::http::Uri) -> Option<String> {
    let rest = uri.path().strip_prefix("/gateway/admin")?;
    if !(rest.is_empty() || rest.starts_with('/')) {
        return None;
    }
    let path = if rest.is_empty() { "/gateway".to_string() } else { format!("/gateway{rest}") };
    Some(match uri.query() {
        Some(q) => format!("{path}?{q}"),
        None => path,
    })
}

#[cfg(test)]
mod tests {
    use super::{admin_access, AdminAccess::*};

    #[test]
    fn only_the_console_and_local_dev_origins_are_allowed() {
        use super::origin_allowed;
        let allowed = vec!["https://nannndev.github.io".to_string()];
        assert!(origin_allowed("https://nannndev.github.io", &allowed));
        assert!(origin_allowed("http://localhost:5173", &allowed));
        assert!(origin_allowed("http://127.0.0.1:4173", &allowed));
        assert!(!origin_allowed("https://evil.example", &allowed));
        assert!(!origin_allowed("https://nannndev.github.io.evil.example", &allowed));
        assert!(!origin_allowed("http://localhost.evil.example:80", &allowed));
        assert!(!origin_allowed("http://localhost:", &allowed));
        assert!(!origin_allowed("null", &allowed));
    }

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
