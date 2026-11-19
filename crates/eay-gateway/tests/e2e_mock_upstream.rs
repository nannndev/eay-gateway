//! End-to-end: the real gateway router in front of a fake OpenAI-compatible upstream on a local port.
//! Covers the success paths the other tests cannot reach without a provider: streaming, the three
//! client protocols, tool calls, failover and session memory.

#![allow(clippy::unwrap_used)]

use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use eay_gateway::core::{GatewayEngine, GatewayStore};
use eay_gateway::gateway_router;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

/// Every request body the fake upstream received, newest last.
type Seen = Arc<Mutex<Vec<Value>>>;

fn sse(chunks: &[Value]) -> String {
    let mut s: String = chunks.iter().map(|c| format!("data: {c}\n\n")).collect();
    s.push_str("data: [DONE]\n\n");
    s
}

fn chunk(delta: Value, finish: Option<&str>) -> Value {
    json!({ "id": "c1", "object": "chat.completion.chunk", "created": 1, "model": "mock-model",
            "choices": [{ "index": 0, "delta": delta, "finish_reason": finish }] })
}

/// Replies "pong" (or a tool call when the request offers a `get_weather` tool), streamed or not.
async fn chat(State(seen): State<Seen>, Json(body): Json<Value>) -> Response {
    seen.lock().unwrap().push(body.clone());
    let tool_result = body["messages"].as_array().and_then(|m| m.iter().rev().find(|m| m["role"] == "tool")).map(|m| m["content"].clone());
    let wants_tool =
        body["tools"].as_array().is_some_and(|t| t.iter().any(|t| t["function"]["name"] == "get_weather")) && tool_result.is_none();
    // Gateway-side tool loop: call the first offered MCP tool (or `eay_delegate`), then echo what
    // it returned. A subagent's task text starts with "SUB:", and it just answers "sub done".
    let user_text =
        body["messages"].as_array().and_then(|m| m.iter().find(|m| m["role"] == "user")).and_then(|m| m["content"].as_str()).unwrap_or("");
    // A subagent answers directly (it may be offered `delegate` too, but does not use it).
    if user_text.starts_with("SLOW:") && tool_result.is_none() {
        // A subagent that takes a while: parallel delegation must overlap these.
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        let name = user_text.trim_start_matches("SLOW:").trim().to_string();
        return (
            [("content-type", "text/event-stream")],
            sse(&[chunk(json!({ "role": "assistant", "content": format!("done {name}") }), Some("stop"))]),
        )
            .into_response();
    }
    if user_text.starts_with("SUB:") && tool_result.is_none() {
        return (
            [("content-type", "text/event-stream")],
            sse(&[chunk(json!({ "role": "assistant", "content": "sub done" }), Some("stop"))]),
        )
            .into_response();
    }
    let mcp_tool = body["tools"]
        .as_array()
        .and_then(|t| t.iter().find_map(|t| t["function"]["name"].as_str().filter(|n| n.starts_with("mcp__") || *n == "eay_delegate")));
    // "FANOUT" hands three slow subtasks out in one reply, the third to the saved agent "tester".
    if mcp_tool == Some("eay_delegate") && tool_result.is_none() && user_text.starts_with("FANOUT") {
        let calls: Vec<Value> = ["a", "b", "c"]
            .iter()
            .enumerate()
            .map(|(i, n)| {
                let mut args = json!({ "role": format!("Worker {n}"), "task": format!("SLOW: {n}") });
                if *n == "c" {
                    args["agent"] = json!("tester");
                }
                json!({ "index": i, "id": format!("call_{n}"), "type": "function", "function": { "name": "eay_delegate", "arguments": args.to_string() } })
            })
            .collect();
        return (
            [("content-type", "text/event-stream")],
            sse(&[chunk(json!({ "role": "assistant", "tool_calls": calls }), Some("tool_calls"))]),
        )
            .into_response();
    }
    if mcp_tool == Some("eay_delegate")
        && body["messages"].as_array().is_some_and(|m| m.iter().filter(|m| m["role"] == "tool").count() == 3)
    {
        // The lead reports what its three workers said, in call order.
        let outs: Vec<String> = body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| m["role"] == "tool")
            .map(|m| m["content"].as_str().unwrap_or("").to_string())
            .collect();
        return (
            [("content-type", "text/event-stream")],
            sse(&[chunk(json!({ "role": "assistant", "content": outs.join(" | ") }), Some("stop"))]),
        )
            .into_response();
    }
    if mcp_tool == Some("eay_delegate") && tool_result.is_none() && !user_text.starts_with("SUB:") {
        // "LOOP" keeps delegating at every level; anything else delegates once.
        let task = if user_text.starts_with("LOOP") { "LOOP" } else { "SUB: do part one" };
        let args = json!({ "role": "Helper", "task": task }).to_string();
        let delta = json!({ "role": "assistant", "tool_calls": [{ "index": 0, "id": "call_d", "type": "function",
            "function": { "name": "eay_delegate", "arguments": args } }] });
        return ([("content-type", "text/event-stream")], sse(&[chunk(delta, Some("tool_calls"))])).into_response();
    }
    if let Some(name) = mcp_tool {
        let delta = match &tool_result {
            Some(out) => json!({ "role": "assistant", "content": format!("tool said: {}", out.as_str().unwrap_or("")) }),
            None => json!({ "role": "assistant", "tool_calls": [{ "index": 0, "id": "call_m", "type": "function",
                "function": { "name": name, "arguments": "{}" } }] }),
        };
        let finish = if tool_result.is_some() { "stop" } else { "tool_calls" };
        return ([("content-type", "text/event-stream")], sse(&[chunk(delta, Some(finish))])).into_response();
    }
    let usage = json!({ "prompt_tokens": 7, "completion_tokens": 3, "total_tokens": 10 });
    if body["stream"] == true {
        let mut chunks = if wants_tool {
            vec![
                chunk(
                    json!({ "role": "assistant", "tool_calls": [{ "index": 0, "id": "call_1", "type": "function",
                    "function": { "name": "get_weather", "arguments": "{\"city\":" } }] }),
                    None,
                ),
                chunk(json!({ "tool_calls": [{ "index": 0, "function": { "arguments": "\"Oslo\"}" } }] }), Some("tool_calls")),
            ]
        } else {
            vec![chunk(json!({ "role": "assistant", "content": "po" }), None), chunk(json!({ "content": "ng" }), Some("stop"))]
        };
        chunks.push(json!({ "id": "c1", "object": "chat.completion.chunk", "model": "mock-model", "choices": [], "usage": usage }));
        return ([("content-type", "text/event-stream")], sse(&chunks)).into_response();
    }
    let message = if wants_tool {
        json!({ "role": "assistant", "content": null, "tool_calls": [{ "id": "call_1", "type": "function",
            "function": { "name": "get_weather", "arguments": "{\"city\":\"Oslo\"}" } }] })
    } else {
        json!({ "role": "assistant", "content": "pong" })
    };
    Json(json!({ "id": "c1", "object": "chat.completion", "created": 1, "model": "mock-model",
        "choices": [{ "index": 0, "message": message, "finish_reason": if wants_tool { "tool_calls" } else { "stop" } }],
        "usage": usage }))
    .into_response()
}

async fn rate_limited() -> Response {
    (StatusCode::TOO_MANY_REQUESTS, Json(json!({ "error": { "message": "rate limit exceeded" } }))).into_response()
}

async fn models() -> Json<Value> {
    Json(json!({ "object": "list", "data": [{ "id": "mock-model", "object": "model" }] }))
}

/// Starts the fake upstream; `/ok/v1` answers, `/limited/v1` always returns 429.
async fn start_upstream() -> (String, Seen) {
    let seen: Seen = Default::default();
    let app = Router::new()
        .route("/ok/v1/chat/completions", post(chat))
        .route("/ok/v1/models", get(models))
        .route("/limited/v1/chat/completions", post(rate_limited))
        .route("/limited/v1/models", get(models))
        .with_state(seen.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), seen)
}

struct Gw {
    app: Router,
    seen: Seen,
    _dir: tempfile::TempDir,
}

/// Gateway with `deepseek` pointed at the healthy upstream. With `failover`, `openai` (always 429)
/// is connected too and tried first.
async fn gateway(failover: bool) -> Gw {
    let (base, seen) = start_upstream().await;
    let dir = tempfile::tempdir().unwrap();
    let engine = GatewayEngine::new(GatewayStore::new(dir.path()).unwrap());
    engine.store.connect_provider("deepseek", "sk-test", Some(&format!("{base}/ok/v1"))).unwrap();
    engine.store.set_provider_models("deepseek", &["mock-model".to_string()]).unwrap();
    let mut priority = vec!["deepseek"];
    if failover {
        engine.store.connect_provider("openai", "sk-test", Some(&format!("{base}/limited/v1"))).unwrap();
        engine.store.set_provider_models("openai", &["mock-model".to_string()]).unwrap();
        priority.insert(0, "openai");
    }
    engine.store.set_setting("provider_priority", &json!(priority).to_string()).unwrap();
    engine.store.set_setting("auto_models_per_provider", "1").unwrap();
    Gw { app: gateway_router(engine), seen, _dir: dir }
}

async fn post_raw(app: &Router, uri: &str, body: Value, headers: &[(&str, &str)]) -> (StatusCode, String) {
    let mut req = Request::builder().method("POST").uri(uri).header("content-type", "application/json");
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let resp = app.clone().oneshot(req.body(Body::from(body.to_string())).unwrap()).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

/// `data:` payloads of an SSE body, parsed (skips `[DONE]`).
fn events(body: &str) -> Vec<Value> {
    body.lines().filter_map(|l| l.strip_prefix("data:")).filter_map(|d| serde_json::from_str(d.trim()).ok()).collect()
}

fn hello() -> Value {
    json!([{ "role": "user", "content": "ping" }])
}

#[tokio::test]
async fn chat_completions_non_streaming() {
    let gw = gateway(false).await;
    let (st, body) = post_raw(&gw.app, "/v1/chat/completions", json!({ "model": "auto", "messages": hello() }), &[]).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["choices"][0]["message"]["content"], "pong");
}

#[tokio::test]
async fn chat_completions_streaming() {
    let gw = gateway(false).await;
    let (st, body) = post_raw(&gw.app, "/v1/chat/completions", json!({ "model": "auto", "stream": true, "messages": hello() }), &[]).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let text: String = events(&body).iter().filter_map(|e| e["choices"][0]["delta"]["content"].as_str().map(String::from)).collect();
    assert_eq!(text, "pong");
    assert!(body.trim_end().ends_with("data: [DONE]"), "{body}");
}

#[tokio::test]
async fn fails_over_from_a_rate_limited_provider() {
    let gw = gateway(true).await;
    let (st, body) = post_raw(&gw.app, "/v1/chat/completions", json!({ "model": "auto", "messages": hello() }), &[]).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(serde_json::from_str::<Value>(&body).unwrap()["choices"][0]["message"]["content"], "pong");
}

#[tokio::test]
async fn anthropic_messages_streaming_with_a_tool_call() {
    let gw = gateway(false).await;
    let req = json!({
        "model": "claude-sonnet-4-5", "max_tokens": 100, "stream": true,
        "messages": [{ "role": "user", "content": "weather?" }],
        "tools": [{ "name": "get_weather", "description": "w", "input_schema": { "type": "object", "properties": { "city": { "type": "string" } } } }]
    });
    let (st, body) = post_raw(&gw.app, "/v1/messages", req, &[]).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let ev = events(&body);
    let start = ev.iter().find(|e| e["type"] == "content_block_start" && e["content_block"]["type"] == "tool_use").expect(&body);
    assert_eq!(start["content_block"]["name"], "get_weather");
    let args: String =
        ev.iter().filter(|e| e["delta"]["type"] == "input_json_delta").map(|e| e["delta"]["partial_json"].as_str().unwrap()).collect();
    assert_eq!(serde_json::from_str::<Value>(&args).unwrap(), json!({ "city": "Oslo" }));
    let stop = ev.iter().find(|e| e["type"] == "message_delta").unwrap();
    assert_eq!(stop["delta"]["stop_reason"], "tool_use");
    // The upstream saw the tool in OpenAI form.
    assert_eq!(gw.seen.lock().unwrap().last().unwrap()["tools"][0]["function"]["name"], "get_weather");
}

#[tokio::test]
async fn responses_streaming_text_and_tool_call() {
    let gw = gateway(false).await;
    let (st, body) = post_raw(&gw.app, "/v1/responses", json!({ "model": "auto", "stream": true, "input": "ping" }), &[]).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let ev = events(&body);
    let text: String = ev.iter().filter(|e| e["type"] == "response.output_text.delta").map(|e| e["delta"].as_str().unwrap()).collect();
    assert_eq!(text, "pong");
    let done = ev.iter().find(|e| e["type"] == "response.completed").expect(&body);
    assert_eq!(done["response"]["usage"]["total_tokens"], 10);

    let req = json!({ "model": "auto", "stream": true, "input": "weather?",
        "tools": [{ "type": "function", "name": "get_weather", "parameters": { "type": "object", "properties": {} } }] });
    let (st, body) = post_raw(&gw.app, "/v1/responses", req, &[]).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let done = events(&body).into_iter().find(|e| e["type"] == "response.completed").expect(&body);
    let item = &done["response"]["output"][0];
    assert_eq!(item["type"], "function_call");
    assert_eq!(item["name"], "get_weather");
    assert_eq!(item["call_id"], "call_1");
    assert_eq!(item["arguments"], "{\"city\":\"Oslo\"}");
}

#[tokio::test]
async fn streamed_turns_are_saved_to_session_memory() {
    let gw = gateway(false).await;
    let sid = [("x-eay-session-id", "s1")];
    let first = json!({ "model": "auto", "stream": true, "messages": [{ "role": "user", "content": "remember me" }] });
    let (st, body) = post_raw(&gw.app, "/v1/chat/completions", first, &sid).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let second = json!({ "model": "auto", "messages": [{ "role": "user", "content": "and now?" }] });
    post_raw(&gw.app, "/v1/chat/completions", second, &sid).await;
    let sent = gw.seen.lock().unwrap().last().unwrap()["messages"].clone();
    let contents: Vec<&str> = sent.as_array().unwrap().iter().filter_map(|m| m["content"].as_str()).collect();
    assert_eq!(contents, ["remember me", "pong", "and now?"], "{sent}");
}

/// A stdio MCP server whose only tool has a name too long for OpenAI/Anthropic (> 64 chars with prefix).
#[cfg(unix)]
#[tokio::test]
async fn gateway_runs_an_mcp_tool_with_a_shortened_name() {
    let gw = gateway(false).await;
    let long = "x".repeat(70);
    let script = format!(
        r#"while read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p'); [ -z "$id" ] && continue
  case "$line" in
    *tools/list*) printf '{{"jsonrpc":"2.0","id":%s,"result":{{"tools":[{{"name":"{long}","inputSchema":{{"type":"object"}}}}]}}}}\n' "$id" ;;
    *tools/call*) printf '{{"jsonrpc":"2.0","id":%s,"result":{{"content":[{{"type":"text","text":"42"}}]}}}}\n' "$id" ;;
    *) printf '{{"jsonrpc":"2.0","id":%s,"result":{{}}}}\n' "$id" ;;
  esac
done"#
    );
    let (st, body) = post_raw(
        &gw.app,
        "/gateway/admin/mcp",
        json!({ "name": "longtools", "transport": "stdio", "command": "sh", "args": ["-c", script] }),
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");

    let (st, body) =
        post_raw(&gw.app, "/v1/chat/completions", json!({ "model": "auto", "messages": hello() }), &[("x-eay-mcp", "longtools")]).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(serde_json::from_str::<Value>(&body).unwrap()["choices"][0]["message"]["content"], "tool said: 42");
    let offered = gw.seen.lock().unwrap()[0]["tools"][0]["function"]["name"].as_str().unwrap().to_string();
    assert!(offered.len() <= 64, "{offered}");
}

/// Fake Streamable-HTTP MCP server: counts `initialize`, requires the session id it issued,
/// and its one tool returns a screenshot.
async fn start_http_mcp() -> (String, Arc<Mutex<u32>>) {
    let inits: Arc<Mutex<u32>> = Default::default();
    let handler = {
        let inits = inits.clone();
        move |headers: axum::http::HeaderMap, Json(req): Json<Value>| {
            let inits = inits.clone();
            async move {
                let id = req["id"].clone();
                match req["method"].as_str().unwrap_or("") {
                    "initialize" => {
                        *inits.lock().unwrap() += 1;
                        let body = json!({ "jsonrpc": "2.0", "id": id, "result": { "protocolVersion": "2024-11-05" } });
                        ([("mcp-session-id", "sess-1")], Json(body)).into_response()
                    }
                    "notifications/initialized" => StatusCode::ACCEPTED.into_response(),
                    _ if headers.get("mcp-session-id").and_then(|v| v.to_str().ok()) != Some("sess-1") => {
                        StatusCode::NOT_FOUND.into_response()
                    }
                    "tools/list" => Json(json!({ "jsonrpc": "2.0", "id": id, "result": { "tools": [{ "name": "screenshot", "inputSchema": { "type": "object" } }] } })).into_response(),
                    _ => Json(json!({ "jsonrpc": "2.0", "id": id, "result": { "content": [
                        { "type": "text", "text": "took it" },
                        { "type": "image", "data": "iVBORw0KGgo=", "mimeType": "image/png" }
                    ] } }))
                    .into_response(),
                }
            }
        }
    };
    let app = Router::new().route("/mcp", post(handler));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}/mcp"), inits)
}

#[tokio::test]
async fn http_mcp_reuses_its_session_and_passes_images_to_the_model() {
    let gw = gateway(false).await;
    let (url, inits) = start_http_mcp().await;
    let (st, body) = post_raw(&gw.app, "/gateway/admin/mcp", json!({ "name": "browser", "transport": "http", "url": url }), &[]).await;
    assert_eq!(st, StatusCode::OK, "{body}");

    let (st, body) =
        post_raw(&gw.app, "/v1/chat/completions", json!({ "model": "auto", "messages": hello() }), &[("x-eay-mcp", "browser")]).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    // tools/list + tools/call, one handshake.
    assert_eq!(*inits.lock().unwrap(), 1);

    // The model got the image as an image part, not base64 inside the tool text.
    let last = gw.seen.lock().unwrap().last().unwrap()["messages"].clone();
    let tool_msg = last.as_array().unwrap().iter().find(|m| m["role"] == "tool").unwrap();
    assert!(!tool_msg["content"].as_str().unwrap().contains("iVBORw0KGgo"), "{tool_msg}");
    let img = last.as_array().unwrap().iter().find_map(|m| m["content"].as_array()?.iter().find(|p| p["type"] == "image_url").cloned());
    assert_eq!(img.unwrap()["image_url"]["url"], "data:image/png;base64,iVBORw0KGgo=");

    // A direct tool call reuses the same session too.
    let (st, _) = post_raw(&gw.app, "/gateway/admin/mcp/browser/call", json!({ "tool": "screenshot" }), &[]).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(*inits.lock().unwrap(), 1);
}

#[tokio::test]
async fn claude_code_thinking_reaches_an_openai_upstream_as_reasoning_effort() {
    let gw = gateway(false).await;
    let req = json!({
        "model": "claude-sonnet-5-5", "max_tokens": 2000, "stream": true,
        "thinking": { "type": "enabled", "budget_tokens": 31999 },
        "messages": [{ "role": "user", "content": "ping" }]
    });
    let (st, body) = post_raw(&gw.app, "/v1/messages", req, &[]).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let sent = gw.seen.lock().unwrap().last().unwrap().clone();
    assert_eq!(sent["reasoning_effort"], "high", "{sent}");
    assert!(sent.get("thinking_budget_tokens").is_none(), "gateway-only field leaked upstream: {sent}");
    assert!(sent.get("thinking").is_none());
}

/// stdio MCP server with one read-only tool and one write tool.
#[cfg(unix)]
fn two_tool_mcp_script() -> String {
    r#"while read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p'); [ -z "$id" ] && continue
  case "$line" in
    *tools/list*) printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"read_file","annotations":{"readOnlyHint":true},"inputSchema":{"type":"object"}},{"name":"write_file","inputSchema":{"type":"object"}}]}}\n' "$id" ;;
    *tools/call*) printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"done"}]}}\n' "$id" ;;
    *) printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$id" ;;
  esac
done"#
        .to_string()
}

#[cfg(unix)]
#[tokio::test]
async fn tool_policies_limit_gateway_run_mcp_tools() {
    let gw = gateway(false).await;
    let (st, body) = post_raw(
        &gw.app,
        "/gateway/admin/mcp",
        json!({ "name": "files", "transport": "stdio", "command": "sh", "args": ["-c", two_tool_mcp_script()] }),
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let offered = |seen: &Seen| -> Vec<String> {
        seen.lock().unwrap().last().unwrap()["tools"]
            .as_array()
            .map(|t| t.iter().filter_map(|t| t["function"]["name"].as_str().map(String::from)).collect())
            .unwrap_or_default()
    };

    // An agent in read-only mode only sees tools the server marks read-only.
    let (st, body) = post_raw(
        &gw.app,
        "/gateway/admin/agents",
        json!({ "name": "reviewer", "model": "auto", "tool_policy": { "mode": "read_only" } }),
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    post_raw(&gw.app, "/v1/chat/completions", json!({ "model": "agent/reviewer", "messages": hello() }), &[("x-eay-mcp", "files")]).await;
    assert_eq!(offered(&gw.seen), ["mcp__files__read_file"]);

    // A key that denies read_file, used with no agent, sees only write_file.
    let (st, created) =
        post_raw(&gw.app, "/gateway/admin/keys", json!({ "name": "ci", "tool_policy": { "deny": ["read_file"] } }), &[]).await;
    assert_eq!(st, StatusCode::OK, "{created}");
    let secret = serde_json::from_str::<Value>(&created).unwrap()["key"]["key"].as_str().unwrap().to_string();
    let bearer = format!("Bearer {secret}");
    post_raw(
        &gw.app,
        "/v1/chat/completions",
        json!({ "model": "auto", "messages": hello() }),
        &[("x-eay-mcp", "files"), ("authorization", &bearer)],
    )
    .await;
    assert_eq!(offered(&gw.seen), ["mcp__files__write_file"]);

    // Agent read-only + key denying read_file: the two together leave nothing, so no tools are sent.
    post_raw(
        &gw.app,
        "/v1/chat/completions",
        json!({ "model": "agent/reviewer", "messages": hello() }),
        &[("x-eay-mcp", "files"), ("authorization", &bearer)],
    )
    .await;
    assert!(offered(&gw.seen).is_empty());

    // Unknown modes are rejected, and no key is left behind.
    let (st, _) = post_raw(&gw.app, "/gateway/admin/keys", json!({ "name": "bad", "tool_policy": { "mode": "yolo" } }), &[]).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let req = Request::builder().uri("/gateway/admin/keys").body(Body::empty()).unwrap();
    let resp = gw.app.clone().oneshot(req).await.unwrap();
    let keys: Value = serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let names: Vec<&str> = keys["keys"].as_array().unwrap().iter().filter_map(|k| k["name"].as_str()).collect();
    assert_eq!(names, ["ci"], "a rejected key must not be created");
}

async fn get_json(app: &Router, uri: &str) -> (StatusCode, Value) {
    let resp = app.clone().oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap()).await.unwrap();
    let st = resp.status();
    (st, serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap_or(Value::Null))
}

#[tokio::test]
async fn sessions_can_be_listed_and_read() {
    let gw = gateway(false).await;
    post_raw(&gw.app, "/v1/chat/completions", json!({ "model": "auto", "messages": hello() }), &[("x-eay-session-id", "chat-1")]).await;
    let (st, list) = get_json(&gw.app, "/gateway/admin/sessions").await;
    assert_eq!(st, StatusCode::OK);
    let s = &list["sessions"][0];
    assert_eq!(s["session"], "chat-1");
    assert_eq!(s["owner"], "local");
    assert_eq!(s["messages"], 2);

    let (st, one) = get_json(&gw.app, &format!("/gateway/admin/sessions/{}", s["id"].as_str().unwrap())).await;
    assert_eq!(st, StatusCode::OK, "{one}");
    assert_eq!(one["messages"][1]["content"], "pong");
    // A bare session name finds the local session too.
    assert_eq!(get_json(&gw.app, "/gateway/admin/sessions/chat-1").await.0, StatusCode::OK);
    assert_eq!(get_json(&gw.app, "/gateway/admin/sessions/nope").await.0, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_model_can_delegate_to_a_subagent_when_its_agent_allows_it() {
    let gw = gateway(false).await;
    // Without max_subagents the model is not offered the delegate tool.
    post_raw(&gw.app, "/gateway/admin/agents", json!({ "name": "plain", "model": "auto" }), &[]).await;
    post_raw(&gw.app, "/v1/chat/completions", json!({ "model": "agent/plain", "messages": hello() }), &[]).await;
    assert!(gw.seen.lock().unwrap().last().unwrap().get("tools").is_none());

    let (st, body) =
        post_raw(&gw.app, "/gateway/admin/agents", json!({ "name": "lead", "model": "auto", "tool_policy": { "max_subagents": 2 } }), &[])
            .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let (st, body) = post_raw(&gw.app, "/v1/chat/completions", json!({ "model": "agent/lead", "messages": hello() }), &[]).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    // The lead's final answer echoes the subagent's result, which came back as the tool output.
    assert_eq!(serde_json::from_str::<Value>(&body).unwrap()["choices"][0]["message"]["content"], "tool said: sub done");

    let (_, subs) = get_json(&gw.app, "/gateway/admin/subagents").await;
    let sub = &subs["subagents"][0];
    assert_eq!(sub["role"], "Helper");
    assert_eq!(sub["phase"], "completed");
    assert!(sub["parent_id"].as_str().unwrap().starts_with("req_"), "{sub}");
    assert_eq!(subs["subagents"].as_array().unwrap().len(), 1, "the subagent did not start another one");
}

#[tokio::test]
async fn delegation_stops_at_the_depth_limit() {
    let gw = gateway(false).await;
    // A model that always delegates: each level starts another, until the gateway stops offering it.
    post_raw(&gw.app, "/gateway/admin/agents", json!({ "name": "loop", "model": "auto", "tool_policy": { "max_subagents": 5 } }), &[])
        .await;
    let (st, _) = post_raw(
        &gw.app,
        "/v1/chat/completions",
        json!({ "model": "agent/loop", "messages": [{ "role": "user", "content": "LOOP" }] }),
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (_, subs) = get_json(&gw.app, "/gateway/admin/subagents").await;
    assert_eq!(subs["subagents"].as_array().unwrap().len(), 3, "MAX_DELEGATE_DEPTH levels, no more");
}

#[cfg(unix)]
#[tokio::test]
async fn manual_approval_holds_a_tool_until_someone_answers() {
    let gw = gateway(false).await;
    post_raw(
        &gw.app,
        "/gateway/admin/mcp",
        json!({ "name": "files", "transport": "stdio", "command": "sh", "args": ["-c", two_tool_mcp_script()] }),
        &[],
    )
    .await;
    post_raw(
        &gw.app,
        "/gateway/admin/agents",
        json!({ "name": "careful", "model": "auto", "tool_policy": { "approval": "manual", "allow": ["files/write_file"] } }),
        &[],
    )
    .await;

    for (approve, expected) in [(true, "tool said: done"), (false, "The user did not approve")] {
        let app = gw.app.clone();
        let req = tokio::spawn(async move {
            post_raw(&app, "/v1/chat/completions", json!({ "model": "agent/careful", "messages": hello() }), &[("x-eay-mcp", "files")])
                .await
        });
        // Wait for the tool call to show up as pending, then answer it.
        let mut id = None;
        for _ in 0..50 {
            let (_, list) = get_json(&gw.app, "/gateway/admin/approvals").await;
            if let Some(a) = list["approvals"].as_array().and_then(|a| a.first()) {
                assert_eq!(a["tool"], "mcp__files__write_file");
                id = a["id"].as_str().map(String::from);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        let id = id.expect("tool call never became pending");
        let (st, _) = post_raw(&gw.app, &format!("/gateway/admin/approvals/{id}"), json!({ "approve": approve }), &[]).await;
        assert_eq!(st, StatusCode::OK);
        let (st, body) = req.await.unwrap();
        assert_eq!(st, StatusCode::OK, "{body}");
        let answer = serde_json::from_str::<Value>(&body).unwrap()["choices"][0]["message"]["content"].as_str().unwrap().to_string();
        assert!(answer.contains(expected), "approve={approve}: {answer}");
        // Answering twice is refused.
        assert_eq!(
            post_raw(&gw.app, &format!("/gateway/admin/approvals/{id}"), json!({ "approve": true }), &[]).await.0,
            StatusCode::NOT_FOUND
        );
    }
}

#[tokio::test]
async fn a_lead_runs_its_subagents_in_parallel_and_can_use_saved_agents() {
    let gw = gateway(false).await;
    post_raw(&gw.app, "/gateway/admin/agents", json!({ "name": "lead", "model": "auto", "tool_policy": { "max_subagents": 5 } }), &[])
        .await;
    post_raw(&gw.app, "/gateway/admin/agents", json!({ "name": "tester", "model": "auto", "description": "Writes tests" }), &[]).await;

    let t0 = std::time::Instant::now();
    let (st, body) = post_raw(
        &gw.app,
        "/v1/chat/completions",
        json!({ "model": "agent/lead", "messages": [{ "role": "user", "content": "FANOUT" }] }),
        &[],
    )
    .await;
    let elapsed = t0.elapsed();
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(serde_json::from_str::<Value>(&body).unwrap()["choices"][0]["message"]["content"], "done a | done b | done c");
    // Three 400 ms subagents: about 0.4 s in parallel, 1.2 s or more if they ran one after another.
    assert!(elapsed < std::time::Duration::from_millis(1000), "subagents did not overlap: {elapsed:?}");

    // The lead was told which saved agents exist, and the third worker ran as agent/tester.
    let lead_req = gw
        .seen
        .lock()
        .unwrap()
        .iter()
        .find(|b| {
            b["messages"][0]["content"].as_str().is_some_and(|c| c.contains("FANOUT"))
                || b["messages"].as_array().is_some_and(|m| m.iter().any(|m| m["content"] == "FANOUT"))
        })
        .cloned()
        .unwrap();
    let delegate = lead_req["tools"].as_array().unwrap().iter().find(|t| t["function"]["name"] == "eay_delegate").unwrap().clone();
    assert!(delegate["function"]["description"].as_str().unwrap().contains("tester: Writes tests"), "{delegate}");
    assert_eq!(delegate["function"]["parameters"]["properties"]["agent"]["enum"], json!(["tester"]));

    // The tree endpoint nests the three workers under the lead's request, in start order.
    let (_, tree) = get_json(&gw.app, "/gateway/admin/subagents/tree").await;
    let roots = tree["tree"].as_array().unwrap();
    assert_eq!(roots.len(), 3, "workers are roots: their parent is the lead's request, not a subagent run");
    let models: Vec<&str> = roots.iter().filter_map(|r| r["model"].as_str()).collect();
    assert!(models.contains(&"agent/tester"), "{models:?}");
    assert!(roots.iter().all(|r| r["children"].as_array().unwrap().is_empty()));
}

#[tokio::test]
async fn the_tree_nests_children_under_their_parent_run() {
    let gw = gateway(false).await;
    post_raw(&gw.app, "/gateway/admin/agents", json!({ "name": "loop", "model": "auto", "tool_policy": { "max_subagents": 5 } }), &[])
        .await;
    post_raw(&gw.app, "/v1/chat/completions", json!({ "model": "agent/loop", "messages": [{ "role": "user", "content": "LOOP" }] }), &[])
        .await;
    let (_, tree) = get_json(&gw.app, "/gateway/admin/subagents/tree").await;
    let roots = tree["tree"].as_array().unwrap();
    assert_eq!(roots.len(), 1, "{tree}");
    // sub → sub → sub: three levels, one child each.
    let mut depth = 0;
    let mut node = &roots[0];
    loop {
        depth += 1;
        match node["children"].as_array().unwrap().first() {
            Some(c) => node = c,
            None => break,
        }
    }
    assert_eq!(depth, 3);
}
