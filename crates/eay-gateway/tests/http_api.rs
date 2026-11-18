//! Drives the gateway router in-process (no network, no real providers).

// Test-only file: a failed unwrap is a failed test, which is what we want.
#![allow(clippy::unwrap_used)]

use axum::{
    body::Body,
    http::{Request, StatusCode},
    Router,
};
use eay_gateway::core::{GatewayEngine, GatewayStore};
use eay_gateway::gateway_router;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

/// A router over a fresh store; keep the dir alive for the test's duration.
fn app() -> (Router, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let engine = GatewayEngine::new(GatewayStore::new(dir.path()).unwrap());
    (gateway_router(engine), dir)
}

async fn call(app: &Router, method: &str, uri: &str, body: Option<Value>, auth: Option<&str>) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(uri).header("content-type", "application/json");
    if let Some(a) = auth {
        req = req.header("authorization", format!("Bearer {a}"));
    }
    let req = req.body(body.map_or_else(Body::empty, |b| Body::from(b.to_string()))).unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

mod health {
    use super::*;

    #[tokio::test]
    async fn reports_ok() {
        let (app, _d) = app();
        assert_eq!(call(&app, "GET", "/gateway/health", None, None).await.1["status"], "ok");
    }
}

mod chat_completions {
    use super::*;

    const CHAT: &str = "/v1/chat/completions";

    fn hi() -> Option<Value> {
        Some(json!({ "model": "auto", "messages": [{ "role": "user", "content": "hi" }] }))
    }

    #[tokio::test]
    async fn rejects_an_unknown_gateway_key() {
        let (app, _d) = app();
        assert_eq!(call(&app, "POST", CHAT, hi(), Some("gw-sk-nope")).await.0, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn rejects_a_disabled_gateway_key() {
        let (app, _d) = app();
        let (_, created) = call(&app, "POST", "/gateway/admin/keys", Some(json!({ "name": "ci" })), None).await;
        let id = created["key"]["id"].as_str().unwrap().to_string();
        let secret = created["key"]["key"].as_str().unwrap().to_string();
        call(&app, "PATCH", &format!("/gateway/admin/keys/{id}/toggle"), None, None).await;
        assert_eq!(call(&app, "POST", CHAT, hi(), Some(&secret)).await.0, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn ignores_non_gateway_keys_from_agents() {
        let (app, _d) = app();
        // No provider is connected, so an accepted request fails upstream, not on auth.
        assert_eq!(call(&app, "POST", CHAT, hi(), Some("sk-dummy")).await.0, StatusCode::BAD_GATEWAY);
    }

    #[tokio::test]
    async fn fails_with_bad_gateway_when_nothing_is_connected() {
        let (app, _d) = app();
        assert_eq!(call(&app, "POST", CHAT, hi(), None).await.0, StatusCode::BAD_GATEWAY);
    }

    #[tokio::test]
    async fn enforces_a_keys_allowed_providers() {
        let (app, _d) = app();
        let body = json!({ "name": "ci", "allowed_providers": ["deepseek"] });
        let (_, created) = call(&app, "POST", "/gateway/admin/keys", Some(body), None).await;
        let secret = created["key"]["key"].as_str().unwrap().to_string();
        let req = Some(json!({ "model": "oa/gpt-4o", "messages": [{ "role": "user", "content": "hi" }] }));
        assert_eq!(call(&app, "POST", CHAT, req, Some(&secret)).await.0, StatusCode::FORBIDDEN);
    }
}

mod settings {
    use super::*;

    #[tokio::test]
    async fn rejects_an_unknown_routing_strategy() {
        let (app, _d) = app();
        let (status, _) = call(&app, "POST", "/gateway/admin/settings", Some(json!({ "routing_strategy": "yolo" })), None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn saves_the_provider_priority() {
        let (app, _d) = app();
        let body = json!({ "provider_priority": ["deepseek", "openai"] });
        let (_, saved) = call(&app, "POST", "/gateway/admin/settings", Some(body), None).await;
        assert_eq!(saved["settings"]["provider_priority"], json!(["deepseek", "openai"]));
    }
}

mod combos {
    use super::*;

    #[tokio::test]
    async fn rejects_a_name_with_a_slash() {
        let (app, _d) = app();
        let (status, _) = call(&app, "POST", "/gateway/admin/combos", Some(json!({ "name": "a/b", "targets": ["x"] })), None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn a_saved_combo_shows_up_as_an_auto_model() {
        let (app, _d) = app();
        call(&app, "POST", "/gateway/admin/combos", Some(json!({ "name": "mix", "targets": ["oa/gpt-4o"] })), None).await;
        let (_, models) = call(&app, "GET", "/v1/models", None, None).await;
        let ids: Vec<&str> = models["data"].as_array().unwrap().iter().filter_map(|m| m["id"].as_str()).collect();
        assert!(ids.contains(&"auto/mix"), "{ids:?}");
    }
}

mod models {
    use super::*;

    #[tokio::test]
    async fn lists_only_connected_providers() {
        let (app, _d) = app();
        call(&app, "POST", "/gateway/admin/providers/deepseek/key", Some(json!({ "api_key": "sk" })), None).await;
        let (_, models) = call(&app, "GET", "/v1/models", None, None).await;
        let owners: std::collections::HashSet<&str> =
            models["data"].as_array().unwrap().iter().filter_map(|m| m["owned_by"].as_str()).collect();
        assert_eq!(owners, ["eay-gateway", "deepseek"].into_iter().collect());
    }
}

mod providers {
    use super::*;

    #[tokio::test]
    async fn connecting_azure_without_a_base_url_is_refused() {
        let (app, _d) = app();
        let (status, _) = call(&app, "POST", "/gateway/admin/providers/azure-openai/connect", Some(json!({ "api_key": "k" })), None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn importing_for_a_provider_without_a_local_login_is_refused() {
        let (app, _d) = app();
        assert_eq!(call(&app, "POST", "/gateway/admin/providers/openai/import", None, None).await.0, StatusCode::BAD_REQUEST);
    }
}

mod multimodal_endpoints {
    use super::*;

    #[tokio::test]
    async fn images_generations_rejects_unknown_gateway_key() {
        let (app, _d) = app();
        let body = json!({ "model": "openai/dall-e-3", "prompt": "a cat" });
        let (status, _) = call(&app, "POST", "/v1/images/generations", Some(body), Some("gw-sk-bogus")).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn images_generations_fails_with_bad_gateway_when_nothing_is_connected() {
        let (app, _d) = app();
        let body = json!({ "model": "openai/dall-e-3", "prompt": "a cat" });
        let (status, _) = call(&app, "POST", "/v1/images/generations", Some(body), None).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
    }

    #[tokio::test]
    async fn audio_speech_fails_with_bad_gateway_when_nothing_is_connected() {
        let (app, _d) = app();
        let body = json!({ "model": "openai/tts-1", "input": "hello", "voice": "alloy" });
        let (status, _) = call(&app, "POST", "/v1/audio/speech", Some(body), None).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
    }

    #[tokio::test]
    async fn audio_transcriptions_fails_with_bad_gateway_when_nothing_is_connected() {
        let (app, _d) = app();
        let boundary = "----testboundary";
        let multipart =
            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\nopenai/whisper-1\r\n--{boundary}--\r\n");
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/v1/audio/transcriptions")
            .header("content-type", format!("multipart/form-data; boundary={boundary}"))
            .body(axum::body::Body::from(multipart))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
    }
}

mod skills {
    use super::*;

    #[tokio::test]
    async fn crud_and_toggle_work() {
        let (app, _d) = app();
        let (st, created) = call(
            &app,
            "POST",
            "/gateway/admin/skills",
            Some(json!({ "name": "rust-expert", "description": "Rust", "prompt": "Write clean Rust." })),
            None,
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(created["skill"]["enabled"], true);

        let (st, list) = call(&app, "GET", "/gateway/admin/skills", None, None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(list["skills"].as_array().unwrap().len(), 1);

        let (st, toggled) = call(&app, "PATCH", "/gateway/admin/skills/rust-expert/toggle", None, None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(toggled["skill"]["enabled"], false);

        let (st, del) = call(&app, "DELETE", "/gateway/admin/skills/rust-expert", None, None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(del["status"], "deleted");
    }

    #[tokio::test]
    async fn rejects_invalid_skill_name() {
        let (app, _d) = app();
        let (st, _) = call(
            &app,
            "POST",
            "/gateway/admin/skills",
            Some(json!({ "name": "bad+name", "prompt": "hi" })),
            None,
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn virtual_key_can_bind_default_skills() {
        let (app, _d) = app();
        let (st, created) = call(
            &app,
            "POST",
            "/gateway/admin/keys",
            Some(json!({ "name": "proj-a", "default_skills": ["rust-expert"] })),
            None,
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(created["key"]["default_skills"], json!(["rust-expert"]));
    }
}

mod embeddings_and_anthropic {
    use super::*;

    #[tokio::test]
    async fn embeddings_rejects_unknown_key_and_fails_502_when_nothing_connected() {
        let (app, _d) = app();
        let body = json!({ "model": "openai/text-embedding-3-small", "input": "hello" });
        assert_eq!(call(&app, "POST", "/v1/embeddings", Some(body.clone()), Some("gw-sk-bogus")).await.0, StatusCode::UNAUTHORIZED);
        assert_eq!(call(&app, "POST", "/v1/embeddings", Some(body), None).await.0, StatusCode::BAD_GATEWAY);
    }

    #[tokio::test]
    async fn anthropic_messages_returns_anthropic_error_format() {
        let (app, _d) = app();
        let body = json!({
            "model": "claude-sonnet-4-5",
            "max_tokens": 128,
            "messages": [{ "role": "user", "content": "hi" }]
        });
        let (st, resp) = call(&app, "POST", "/v1/messages", Some(body), None).await;
        assert_eq!(st, StatusCode::BAD_GATEWAY);
        assert_eq!(resp["type"], "error");
        assert_eq!(resp["error"]["type"], "api_error");
    }
}

mod virtual_key_budgets {
    use super::*;

    #[tokio::test]
    async fn enforces_max_tokens_budget_and_max_cost_usd() {
        let (app, _d) = app();
        let (st, created) = call(
            &app,
            "POST",
            "/gateway/admin/keys",
            Some(json!({ "name": "budgeted", "max_tokens_budget": 100, "max_cost_usd": 0.5 })),
            None,
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(created["key"]["max_tokens_budget"], 100);
        assert_eq!(created["key"]["max_cost_usd"], 0.5);
    }
}

mod web_ui {
    use super::*;

    #[tokio::test]
    async fn serves_embedded_console_at_root_and_ui() {
        let (app, _d) = app();
        for path in ["/", "/ui"] {
            let req = axum::http::Request::builder().method("GET").uri(path).body(axum::body::Body::empty()).unwrap();
            let resp = app.clone().oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
            let ct = resp.headers().get("content-type").unwrap().to_str().unwrap();
            assert!(ct.contains("text/html"), "{ct}");
        }
    }
}

mod agents_and_sessions {
    use super::*;

    #[tokio::test]
    async fn agents_crud_and_models_listing_work() {
        let (app, _d) = app();
        let (st, created) = call(
            &app,
            "POST",
            "/gateway/admin/agents",
            Some(json!({
                "name": "senior-rust",
                "description": "Strict Rust reviewer",
                "model": "auto/coding",
                "skills": ["rust-expert"],
                "personality": "Speak concisely.",
                "temperature": 0.2
            })),
            None,
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(created["agent"]["name"], "senior-rust");

        let (_, models) = call(&app, "GET", "/v1/models", None, None).await;
        let ids: Vec<&str> = models["data"].as_array().unwrap().iter().filter_map(|m| m["id"].as_str()).collect();
        assert!(ids.contains(&"agent/senior-rust"), "{ids:?}");

        let (st, del) = call(&app, "DELETE", "/gateway/admin/agents/senior-rust", None, None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(del["status"], "deleted");
    }

    #[tokio::test]
    async fn responses_api_fails_502_when_nothing_connected() {
        let (app, _d) = app();
        let body = json!({ "model": "auto", "input": "hi" });
        assert_eq!(call(&app, "POST", "/v1/responses", Some(body), None).await.0, StatusCode::BAD_GATEWAY);
    }

    #[tokio::test]
    async fn delete_session_returns_not_found_when_missing() {
        let (app, _d) = app();
        let (st, resp) = call(&app, "DELETE", "/gateway/admin/sessions/s1", None, None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(resp["status"], "not_found");
    }

    #[tokio::test]
    async fn subagents_spawn_list_and_delete_work() {
        let (app, _d) = app();
        let (st, spawned) = call(
            &app,
            "POST",
            "/gateway/admin/subagents",
            Some(json!({ "role": "Reviewer", "model": "auto", "prompt": "Check code" })),
            None,
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let id = spawned["subagent"]["id"].as_str().unwrap().to_string();

        let (st, snap) = call(&app, "GET", "/gateway/admin/activity", None, None).await;
        assert_eq!(st, StatusCode::OK);
        assert!(!snap["subagents"].as_array().unwrap().is_empty());

        let (st, del) = call(&app, "DELETE", &format!("/gateway/admin/subagents/{id}"), None, None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(del["status"], "deleted");
    }
}
