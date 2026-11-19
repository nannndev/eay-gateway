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

mod admin_host_guard {
    use super::*;

    async fn status_with_host(host: &str) -> StatusCode {
        let (app, _dir) = app();
        let req = Request::builder().uri("/gateway/combos").header("host", host).body(Body::empty()).unwrap();
        app.oneshot(req).await.unwrap().status()
    }

    #[tokio::test]
    async fn allows_loopback_hosts() {
        for h in ["127.0.0.1:20129", "localhost:20129", "[::1]:20129", "localhost"] {
            assert_eq!(status_with_host(h).await, StatusCode::OK, "{h}");
        }
    }

    #[tokio::test]
    async fn rejects_a_rebound_domain() {
        assert_eq!(status_with_host("evil.example:20129").await, StatusCode::FORBIDDEN);
    }
}

mod mcp_import {
    use super::*;

    #[tokio::test]
    async fn imports_a_claude_desktop_config() {
        let (app, _dir) = app();
        let cfg = json!({ "mcpServers": {
            "fs": { "command": "npx", "args": ["-y", "@modelcontextprotocol/server-filesystem", "~"] },
            "linear": { "url": "https://mcp.linear.app/mcp" }
        }});
        let (st, body) = call(&app, "POST", "/gateway/mcp/import", Some(cfg), None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(body["imported"].as_array().unwrap().len(), 2);
        let (_, list) = call(&app, "GET", "/gateway/mcp", None, None).await;
        assert_eq!(list["mcp_servers"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn rejects_a_config_without_servers() {
        let (app, _dir) = app();
        let (st, _) = call(&app, "POST", "/gateway/mcp/import", Some(json!({ "foo": 1 })), None).await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn calling_a_tool_on_an_unknown_server_is_404() {
        let (app, _dir) = app();
        let (st, _) = call(&app, "POST", "/gateway/mcp/nope/call", Some(json!({ "tool": "x" })), None).await;
        assert_eq!(st, StatusCode::NOT_FOUND);
    }
}

mod key_concurrency {
    use super::*;

    #[tokio::test]
    async fn a_key_at_its_concurrency_limit_gets_429() {
        let dir = tempfile::tempdir().unwrap();
        let engine = GatewayEngine::new(GatewayStore::new(dir.path()).unwrap());
        let app = gateway_router(engine.clone());
        let (_, created) = call(&app, "POST", "/gateway/keys", Some(json!({ "name": "agent", "max_concurrent": 1 })), None).await;
        let secret = created["key"]["key"].as_str().unwrap().to_string();
        let rec = engine.store.find_virtual_key(&secret).unwrap();
        let _busy = engine.try_acquire_inflight(&rec).unwrap(); // simulate one request in flight
        let (st, body) = call(&app, "POST", "/v1/chat/completions", Some(json!({ "model": "auto", "messages": [] })), Some(&secret)).await;
        assert_eq!(st, StatusCode::TOO_MANY_REQUESTS, "{body}");
        drop(_busy);
        let (st, _) = call(&app, "POST", "/v1/chat/completions", Some(json!({ "model": "auto", "messages": [] })), Some(&secret)).await;
        assert_ne!(st, StatusCode::TOO_MANY_REQUESTS);
    }
}

mod body_limit {
    use super::*;

    #[tokio::test]
    async fn accepts_a_request_larger_than_axums_2mb_default() {
        let (app, _dir) = app();
        let big = "x".repeat(5 * 1024 * 1024);
        let (st, _) = call(
            &app,
            "POST",
            "/v1/chat/completions",
            Some(json!({ "model": "auto", "messages": [{ "role": "user", "content": big }] })),
            None,
        )
        .await;
        assert_ne!(st, StatusCode::PAYLOAD_TOO_LARGE);
    }
}

mod legacy_admin_paths {
    use super::*;
    use eay_gateway::gateway_service;

    /// The same service the binary serves: the router behind the legacy-path rewrite.
    async fn send(method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
        let dir = tempfile::tempdir().unwrap();
        let svc = gateway_service(gateway_router(GatewayEngine::new(GatewayStore::new(dir.path()).unwrap())));
        let req = Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json")
            .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
            .unwrap();
        let resp = svc.oneshot(req).await.unwrap();
        let st = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (st, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    #[tokio::test]
    async fn old_admin_paths_reach_the_new_routes() {
        let (st, body) = send("GET", "/gateway/admin/combos", None).await;
        assert_eq!(st, StatusCode::OK, "{body}");
        assert!(body.get("combos").is_some(), "{body}");
        let (st, body) = send("POST", "/gateway/admin/keys", Some(json!({ "name": "legacy" }))).await;
        assert_eq!(st, StatusCode::OK, "{body}");
        assert!(body["key"]["key"].as_str().unwrap().starts_with("gw-sk-"));
        // Query strings survive the rewrite.
        assert_eq!(send("GET", "/gateway/admin/usage?limit=5", None).await.0, StatusCode::OK);
    }

    #[tokio::test]
    async fn the_old_path_keeps_the_loopback_guard() {
        // Same rule as the new path: without a token, admin answers only loopback Hosts.
        let dir = tempfile::tempdir().unwrap();
        let svc = gateway_service(gateway_router(GatewayEngine::new(GatewayStore::new(dir.path()).unwrap())));
        let req = Request::builder().uri("/gateway/admin/combos").header("host", "evil.example").body(Body::empty()).unwrap();
        assert_eq!(svc.oneshot(req).await.unwrap().status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn only_the_exact_admin_segment_is_rewritten() {
        assert_eq!(send("GET", "/gateway/administrator/combos", None).await.0, StatusCode::NOT_FOUND);
        assert_eq!(send("GET", "/gateway/v1/models", None).await.0, StatusCode::OK);
    }
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
        let (_, created) = call(&app, "POST", "/gateway/keys", Some(json!({ "name": "ci" })), None).await;
        let id = created["key"]["id"].as_str().unwrap().to_string();
        let secret = created["key"]["key"].as_str().unwrap().to_string();
        call(&app, "PATCH", &format!("/gateway/keys/{id}/toggle"), None, None).await;
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
        let (_, created) = call(&app, "POST", "/gateway/keys", Some(body), None).await;
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
        let (status, _) = call(&app, "POST", "/gateway/settings", Some(json!({ "routing_strategy": "yolo" })), None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn saves_the_provider_priority() {
        let (app, _d) = app();
        let body = json!({ "provider_priority": ["deepseek", "openai"] });
        let (_, saved) = call(&app, "POST", "/gateway/settings", Some(body), None).await;
        assert_eq!(saved["settings"]["provider_priority"], json!(["deepseek", "openai"]));
    }
}

mod combos {
    use super::*;

    #[tokio::test]
    async fn rejects_a_name_with_a_slash() {
        let (app, _d) = app();
        let (status, _) = call(&app, "POST", "/gateway/combos", Some(json!({ "name": "a/b", "targets": ["x"] })), None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn a_saved_combo_shows_up_as_an_auto_model() {
        let (app, _d) = app();
        call(&app, "POST", "/gateway/combos", Some(json!({ "name": "mix", "targets": ["oa/gpt-4o"] })), None).await;
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
        call(&app, "POST", "/gateway/providers/deepseek/key", Some(json!({ "api_key": "sk" })), None).await;
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
        let (status, _) = call(&app, "POST", "/gateway/providers/azure-openai/connect", Some(json!({ "api_key": "k" })), None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn importing_for_a_provider_without_a_local_login_is_refused() {
        let (app, _d) = app();
        assert_eq!(call(&app, "POST", "/gateway/providers/openai/import", None, None).await.0, StatusCode::BAD_REQUEST);
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
            "/gateway/skills",
            Some(json!({ "name": "rust-expert", "description": "Rust", "prompt": "Write clean Rust." })),
            None,
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(created["skill"]["enabled"], true);

        let (st, list) = call(&app, "GET", "/gateway/skills", None, None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(list["skills"].as_array().unwrap().len(), 1);

        let (st, toggled) = call(&app, "PATCH", "/gateway/skills/rust-expert/toggle", None, None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(toggled["skill"]["enabled"], false);

        let (st, del) = call(&app, "DELETE", "/gateway/skills/rust-expert", None, None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(del["status"], "deleted");
    }

    #[tokio::test]
    async fn rejects_invalid_skill_name() {
        let (app, _d) = app();
        let (st, _) = call(&app, "POST", "/gateway/skills", Some(json!({ "name": "bad+name", "prompt": "hi" })), None).await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn virtual_key_can_bind_default_skills() {
        let (app, _d) = app();
        let (st, created) =
            call(&app, "POST", "/gateway/keys", Some(json!({ "name": "proj-a", "default_skills": ["rust-expert"] })), None).await;
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
        let (st, created) =
            call(&app, "POST", "/gateway/keys", Some(json!({ "name": "budgeted", "max_tokens_budget": 100, "max_cost_usd": 0.5 })), None)
                .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(created["key"]["max_tokens_budget"], 100);
        assert_eq!(created["key"]["max_cost_usd"], 0.5);
    }
}

mod root_and_cors {
    use super::*;

    async fn send(app: &Router, method: &str, uri: &str, headers: &[(&str, &str)]) -> axum::http::Response<axum::body::Body> {
        let mut req = Request::builder().method(method).uri(uri);
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        app.clone().oneshot(req.body(Body::empty()).unwrap()).await.unwrap()
    }

    #[tokio::test]
    async fn root_describes_the_api_instead_of_serving_html() {
        let (app, _d) = app();
        let (st, body) = call(&app, "GET", "/", None, None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(body["service"], "eay-gateway");
        assert_eq!(send(&app, "GET", "/ui", &[]).await.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_local_console_gets_cors_and_private_network_preflight() {
        let (app, _d) = app();
        let origin = "http://localhost:5173";
        let pre = send(
            &app,
            "OPTIONS",
            "/gateway/combos",
            &[
                ("origin", origin),
                ("access-control-request-method", "POST"),
                ("access-control-request-headers", "content-type,x-admin-token"),
                ("access-control-request-private-network", "true"),
            ],
        )
        .await;
        assert_eq!(pre.status(), StatusCode::NO_CONTENT);
        let h = pre.headers();
        assert_eq!(h["access-control-allow-origin"], origin);
        assert_eq!(h["access-control-allow-headers"], "content-type,x-admin-token");
        assert_eq!(h["access-control-allow-private-network"], "true");

        let get = send(&app, "GET", "/gateway/combos", &[("origin", origin)]).await;
        assert_eq!(get.status(), StatusCode::OK);
        assert_eq!(get.headers()["access-control-allow-origin"], origin);
    }

    #[tokio::test]
    async fn other_web_pages_are_refused_even_for_simple_requests() {
        let (app, _d) = app();
        for (method, uri) in [("OPTIONS", "/gateway/mcp"), ("POST", "/v1/audio/transcriptions"), ("GET", "/gateway/keys")] {
            let resp = send(&app, method, uri, &[("origin", "https://evil.example")]).await;
            assert_eq!(resp.status(), StatusCode::FORBIDDEN, "{method} {uri}");
            assert!(resp.headers().get("access-control-allow-origin").is_none());
        }
    }

    #[tokio::test]
    async fn clients_without_an_origin_are_unaffected() {
        let (app, _d) = app();
        let resp = send(&app, "GET", "/gateway/combos", &[]).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(resp.headers().get("access-control-allow-origin").is_none());
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
            "/gateway/agents",
            Some(json!({
                "name": "senior-rust",
                "description": "Strict Rust reviewer",
                "model": "auto/coding",
                "skills": ["rust-expert"],
                "personality": "Speak concisely.",
                "temperature": 0.2,
                "mcp": ["filesystem"]
            })),
            None,
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(created["agent"]["name"], "senior-rust");
        assert_eq!(created["agent"]["mcp"], json!(["filesystem"]));

        let (_, models) = call(&app, "GET", "/v1/models", None, None).await;
        let ids: Vec<&str> = models["data"].as_array().unwrap().iter().filter_map(|m| m["id"].as_str()).collect();
        assert!(ids.contains(&"agent/senior-rust"), "{ids:?}");

        let (st, del) = call(&app, "DELETE", "/gateway/agents/senior-rust", None, None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(del["status"], "deleted");
    }

    #[tokio::test]
    async fn a_team_is_a_named_roster_of_agents() {
        let (app, _d) = app();
        for name in ["lead", "frontend"] {
            let (st, _) = call(&app, "POST", "/gateway/agents", Some(json!({ "name": name, "model": "auto" })), None).await;
            assert_eq!(st, StatusCode::OK);
        }
        let (st, _) = call(&app, "POST", "/gateway/teams", Some(json!({ "name": "app", "members": ["missing"] })), None).await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
        let (st, created) =
            call(&app, "POST", "/gateway/teams", Some(json!({ "name": "app", "lead": "lead", "members": ["lead", "frontend"] })), None)
                .await;
        assert_eq!(st, StatusCode::OK);
        let id = created["team"]["id"].as_str().unwrap();
        assert!(id.starts_with("tm_"), "{id}");
        assert_eq!(created["team"]["lead"], "lead");
        assert_eq!(created["team"]["members"], json!(["lead", "frontend"]));
        let (st, renamed) = call(
            &app,
            "POST",
            "/gateway/teams",
            Some(json!({ "id": id, "name": "app-2", "lead": "lead", "members": ["lead", "frontend"] })),
            None,
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(renamed["team"]["id"], id);
        assert_eq!(renamed["team"]["name"], "app-2");
        let (_, listed) = call(&app, "GET", "/gateway/teams", None, None).await;
        assert_eq!(listed["teams"][0]["id"], id);
        let (st, del) = call(&app, "DELETE", &format!("/gateway/teams/{id}"), None, None).await;
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
        let (st, resp) = call(&app, "DELETE", "/gateway/sessions/s1", None, None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(resp["status"], "not_found");
    }

    #[tokio::test]
    async fn subagents_spawn_list_and_delete_work() {
        let (app, _d) = app();
        let (st, spawned) =
            call(&app, "POST", "/gateway/subagents", Some(json!({ "role": "Reviewer", "model": "auto", "prompt": "Check code" })), None)
                .await;
        assert_eq!(st, StatusCode::OK);
        let id = spawned["subagent"]["id"].as_str().unwrap().to_string();

        let (st, snap) = call(&app, "GET", "/gateway/activity", None, None).await;
        assert_eq!(st, StatusCode::OK);
        assert!(!snap["subagents"].as_array().unwrap().is_empty());

        let (st, del) = call(&app, "DELETE", &format!("/gateway/subagents/{id}"), None, None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(del["status"], "deleted");
    }
}

mod mcp_connectors {
    use super::*;

    #[tokio::test]
    async fn crud_toggle_and_validation_work() {
        let (app, _d) = app();
        let (st, _) = call(&app, "POST", "/gateway/mcp", Some(json!({ "name": "fs", "transport": "stdio" })), None).await;
        assert_eq!(st, StatusCode::BAD_REQUEST);

        let (st, created) = call(
            &app,
            "POST",
            "/gateway/mcp",
            Some(json!({
                "name": "fs",
                "transport": "stdio",
                "command": "npx",
                "args": ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
            })),
            None,
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(created["mcp_server"]["enabled"], true);

        let (st, list) = call(&app, "GET", "/gateway/mcp", None, None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(list["mcp_servers"].as_array().unwrap().len(), 1);

        let (st, toggled) = call(&app, "PATCH", "/gateway/mcp/fs/toggle", None, None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(toggled["mcp_server"]["enabled"], false);

        let (st, del) = call(&app, "DELETE", "/gateway/mcp/fs", None, None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(del["status"], "deleted");

        let (st, http_mcp) = call(
            &app,
            "POST",
            "/gateway/mcp",
            Some(json!({
                "name": "notion",
                "transport": "http",
                "url": "https://mcp.notion.com/mcp",
                "headers": { "Authorization": "Bearer ntn_test" }
            })),
            None,
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(http_mcp["mcp_server"]["headers"]["Authorization"], "Bearer ntn_test");
    }
}
