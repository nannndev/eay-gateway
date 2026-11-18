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

async fn admin_auth_middleware(req: Request, next: Next) -> Response {
    let token = std::env::var("EAY_GATEWAY_ADMIN_TOKEN").ok().filter(|t| !t.trim().is_empty());
    let Some(expected) = token else {
        return next.run(req).await;
    };
    let provided = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer ").or(v.strip_prefix("bearer ")))
        .or_else(|| req.headers().get("x-admin-token").and_then(|v| v.to_str().ok()))
        .map(str::trim);
    if provided == Some(expected.trim()) {
        next.run(req).await
    } else {
        (StatusCode::UNAUTHORIZED, Json(json!({ "detail": "Admin token required" }))).into_response()
    }
}

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
        .route("/gateway/health", get(admin::handle_health));

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
        // Models, Usage, Stats, Settings
        .route("/gateway/admin/models", get(admin::handle_admin_models))
        .route("/gateway/admin/usage", get(admin::handle_list_usage).delete(admin::handle_clear_usage))
        .route("/gateway/admin/stats", get(admin::handle_get_stats))
        .route("/gateway/admin/analytics", get(admin::handle_analytics))
        .route("/gateway/admin/settings", get(admin::handle_get_settings).post(admin::handle_update_settings))
        .layer(middleware::from_fn(admin_auth_middleware));

    public.merge(admin_routes).with_state(engine)
}
