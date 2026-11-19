mod activity;
mod guardrails;
mod routing;
mod state;
mod stream;

pub use activity::{build_tool_activity_meta, ActivityEvent, ActivityHub, ActivityMeta, SubagentRun};
pub use state::{build_gateway_providers, get_oauth_accounts_state};
pub use stream::ChatStream;

#[derive(Default, Clone)]
pub struct ChatOptions {
    pub skills: Vec<String>,
    pub session_id: Option<String>,
    pub max_context_chars: usize,
    pub redact_secrets: bool,
    pub mcp_servers: Vec<String>,
    pub run_id: Option<String>,
    pub agent_label: Option<String>,
}

use self::routing::{cool_key, cooldown_for, filter_multimodal_targets};
use self::stream::{sse_error, sse_frames, UsageGuard};
use crate::providers::{find_provider_by_id, Provider, Target};
use crate::store::{AccountRecord, GatewaySettings, GatewayStore, UsageEntry, VirtualKeyRecord};
use crate::translate::{
    aggregate_sse, detect_media_requirements, normalize_text_documents, openai_to_anthropic, openai_to_commandcode, SseToOpenAi,
};
use bytes::Bytes;
use futures::StreamExt;
use reqwest::Client;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

/// What a chat request produced; the HTTP layer turns this into a response.
pub enum ChatReply {
    /// `stream: true`: OpenAI-style SSE frames.
    Stream(ChatStream),
    /// `stream: false`: one `chat.completion` object.
    Json(Value),
}

/// Raw HTTP reply for OpenAI-compatible passthrough endpoints (images, audio, etc.).
pub struct PassthroughReply {
    pub status: u16,
    pub content_type: String,
    pub body: Bytes,
}

/// Request payload for an OpenAI-compatible passthrough endpoint.
pub enum PassthroughPayload {
    Json(Value),
    Raw { content_type: String, body: Bytes },
}

/// A request the gateway refused or couldn't serve: HTTP status plus an OpenAI-style error body.
#[derive(Debug)]
pub struct ChatError {
    pub status: u16,
    pub body: Value,
}

/// Longest silence allowed from an upstream (reasoning models can think quietly for a while).
const UPSTREAM_IDLE_SECS: u64 = 300;

type McpToolsEntry = (std::time::Instant, Vec<Value>, crate::mcp::McpToolIndex);

#[derive(Clone)]
pub struct GatewayEngine {
    pub store: Arc<GatewayStore>,
    pub http_client: Client,
    pub oauth: crate::oauth::OAuthManager,
    /// Live usage windows per subscription account, cached briefly.
    pub quotas: crate::quota::QuotaCache,
    /// Real-time activity event bus and subagent run tracker.
    pub activity: ActivityHub,
    /// Round-robin cursor per provider.
    rr: Arc<parking_lot::Mutex<HashMap<String, usize>>>,
    /// fill_first: the account that last succeeded, per provider.
    sticky: Arc<parking_lot::Mutex<HashMap<String, String>>>,
    /// MCP `tools/list` per server, keyed by name+updated_at so an edit invalidates it.
    mcp_tools: Arc<parking_lot::Mutex<HashMap<String, McpToolsEntry>>>,
    /// In-flight request count per virtual key, for `max_concurrent`.
    inflight: Arc<parking_lot::Mutex<HashMap<String, i64>>>,
    /// Admission times per virtual key in the last minute, for `rate_limit_per_min`.
    /// Counted once per client request, so failover attempts and MCP rounds do not use up the limit.
    rate_window: Arc<parking_lot::Mutex<HashMap<String, std::collections::VecDeque<std::time::Instant>>>>,
}

/// Held for the life of one request (including its streamed body); frees the key's slot on drop.
pub struct InflightGuard {
    map: Arc<parking_lot::Mutex<HashMap<String, i64>>>,
    key_id: String,
}

impl Drop for InflightGuard {
    fn drop(&mut self) {
        let mut m = self.map.lock();
        if let Some(n) = m.get_mut(&self.key_id) {
            *n -= 1;
            if *n <= 0 {
                m.remove(&self.key_id);
            }
        }
    }
}

impl GatewayEngine {
    pub fn new(store: GatewayStore) -> Self {
        Self {
            store: Arc::new(store),
            // Meta (Muse) answers UA-less requests with a 302 to a login page
            // connect_timeout: a dead host fails over fast. read_timeout resets on every chunk, so long
            // generations keep going but an upstream that goes silent mid-stream is cut off.
            http_client: Client::builder()
                .user_agent(concat!("eay-gateway/", env!("CARGO_PKG_VERSION")))
                .connect_timeout(std::time::Duration::from_secs(10))
                .read_timeout(std::time::Duration::from_secs(UPSTREAM_IDLE_SECS))
                .build()
                .unwrap_or_default(),
            oauth: crate::oauth::OAuthManager::new(),
            quotas: Default::default(),
            activity: ActivityHub::default(),
            rr: Default::default(),
            sticky: Default::default(),
            mcp_tools: Default::default(),
            inflight: Default::default(),
            rate_window: Default::default(),
        }
    }

    /// Admits one client request under the key's `rate_limit_per_min`.
    /// shortcut: in-memory, so the window resets on restart; persist it if keys need hard guarantees across restarts.
    pub fn admit_rate(&self, key: Option<&VirtualKeyRecord>) -> Result<(), ChatError> {
        let Some(k) = key else { return Ok(()) };
        let Some(limit) = k.rate_limit_per_min.filter(|l| *l > 0) else { return Ok(()) };
        let now = std::time::Instant::now();
        let mut w = self.rate_window.lock();
        let q = w.entry(k.id.clone()).or_default();
        while q.front().is_some_and(|t| now.duration_since(*t) >= std::time::Duration::from_secs(60)) {
            q.pop_front();
        }
        if q.len() as i64 >= limit {
            return Err(gateway_err(429, format!("Rate limit of {limit} requests/min reached for key '{}'", k.name)));
        }
        q.push_back(now);
        Ok(())
    }

    /// Takes one of the key's concurrent-request slots, or None when all are busy.
    pub fn try_acquire_inflight(&self, key: &VirtualKeyRecord) -> Option<InflightGuard> {
        let mut m = self.inflight.lock();
        let n = m.entry(key.id.clone()).or_insert(0);
        if key.max_concurrent.is_some_and(|max| max > 0 && *n >= max) {
            return None;
        }
        *n += 1;
        Some(InflightGuard { map: self.inflight.clone(), key_id: key.id.clone() })
    }

    fn check_key_and_resolve(
        &self,
        key: Option<&VirtualKeyRecord>,
        req_model: &str,
    ) -> Result<(Vec<Target>, GatewaySettings, String), ChatError> {
        let key_name = key.map(|k| k.name.clone()).unwrap_or_else(|| "AI Copilot".to_string());
        if let Some(k) = key {
            if let Some(tok_limit) = k.max_tokens_budget.filter(|l| *l > 0) {
                if (k.total_input_tokens + k.total_output_tokens) >= tok_limit {
                    return Err(gateway_err(429, format!("Token budget of {tok_limit} reached for key '{}'", k.name)));
                }
            }
            if let Some(cost_limit) = k.max_cost_usd.filter(|l| *l > 0.0) {
                if self.store.key_total_cost(&k.id) >= cost_limit {
                    return Err(gateway_err(429, format!("Cost budget of ${cost_limit:.4} reached for key '{}'", k.name)));
                }
            }
        }
        let mut targets = self.resolve_targets(req_model);
        if let Some(allowed) = key.and_then(|k| k.allowed_providers.clone()).filter(|a| !a.is_empty()) {
            // Entries may be provider ids ("codex") or aliases ("cx").
            targets.retain(|t| allowed.iter().any(|a| *a == t.provider || t.public().starts_with(&format!("{a}/"))));
            if targets.is_empty() {
                return Err(gateway_err(403, format!("Key '{}' is not allowed to use {req_model}", key_name)));
            }
        }
        Ok((targets, self.store.get_gateway_settings(), key_name))
    }

    fn resolve_apikey_and_base(&self, target: &Target, acc: Option<&AccountRecord>, p: &Provider) -> Option<(Option<String>, String)> {
        let rec = self.store.get_provider_record(&target.provider);
        if rec.as_ref().map(|r| !r.enabled).unwrap_or(false) {
            return None;
        }
        // An extra key (stored as an account) overrides the provider's main key.
        let api_key = match acc {
            Some(a) => a.cred["api_key"].as_str().map(String::from),
            None => self.store.get_provider_key(&target.provider),
        };
        if p.needs_key && api_key.is_none() {
            return None;
        }
        let base = rec.and_then(|r| r.base_url).filter(|s| !s.trim().is_empty()).unwrap_or_else(|| p.base_url.to_string());
        Some((api_key, base.trim_end_matches('/').to_string()))
    }

    #[allow(clippy::too_many_arguments)]
    async fn handle_upstream_error(
        &self,
        upstream: reqwest::Response,
        target: &Target,
        acc: Option<&AccountRecord>,
        cool: &str,
        key: Option<&VirtualKeyRecord>,
        key_name: &str,
        t0: std::time::Instant,
        streaming: bool,
        cooldown_secs: u64,
    ) -> String {
        let status = upstream.status();
        let retry_after = upstream.headers().get("retry-after").and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse().ok());
        let err_text = upstream.text().await.unwrap_or_default();
        self.store.record_usage(&UsageEntry {
            key_id: key.map(|k| k.id.as_str()),
            key_name: Some(key_name),
            provider: Some(&target.provider),
            model: Some(&target.model),
            input_tokens: 0,
            output_tokens: 0,
            cached_tokens: 0,
            duration_ms: t0.elapsed().as_millis() as i64,
            status: "error",
            status_code: i64::from(status.as_u16()),
            error: Some(&err_text),
            streaming,
        });
        let mut msg = format!("{} HTTP {}: {}", target.public(), status, err_text.chars().take(300).collect::<String>());
        if let Some(secs) = cooldown_for(status.as_u16(), retry_after, &err_text, cooldown_secs) {
            self.store.cool_down(cool, secs);
            msg.push_str(&format!(" (cooling {secs}s)"));
        }
        if let Some(a) = acc {
            let _ = self.store.record_account_error(&a.id, Some(&msg));
        }
        msg
    }

    fn build_apikey_chat_request(
        &self,
        p: &Provider,
        base: &str,
        key: Option<String>,
        target: &Target,
        body: &Value,
    ) -> (reqwest::RequestBuilder, &'static str) {
        if p.family == "commandcode" {
            // Same conversation -> same thread, keyed on the first user message.
            let seed = body["messages"]
                .as_array()
                .and_then(|m| m.iter().find(|m| m["role"] == "user"))
                .map(|m| m["content"].to_string())
                .unwrap_or_default();
            let thread = commandcode_thread_id(&seed);
            let r = crate::oauth::commandcode_headers(
                self.http_client.post(format!("{base}/alpha/generate")),
                key.as_deref().unwrap_or(""),
                &thread,
            )
            .json(&openai_to_commandcode(body, &target.model, &thread));
            (r, "commandcode")
        } else if p.family == "openai-responses" {
            // Same wire format as Codex, minus the ChatGPT-specific fields.
            let mut b = crate::translate::openai_to_codex(body, &target.model);
            if let Some(o) = b.as_object_mut() {
                o.remove("store");
                o.remove("include");
            }
            let r = self.http_client.post(format!("{base}/responses")).bearer_auth(key.unwrap_or_default()).json(&b);
            (r, "codex")
        } else if p.family == "anthropic" {
            let mut b = openai_to_anthropic(body, &target.model);
            b["stream"] = json!(true);
            let r = self
                .http_client
                .post(format!("{base}/messages"))
                .header("x-api-key", key.unwrap_or_default())
                .header("anthropic-version", "2023-06-01")
                .json(&b);
            (r, "claude")
        } else {
            let mut b = body.clone();
            b["model"] = json!(target.model);
            b["stream"] = json!(true);
            b["stream_options"] = json!({ "include_usage": true });
            let mut r = self.http_client.post(format!("{base}/chat/completions")).json(&b);
            if let Some(k) = key {
                // Azure's v1 API takes the key in `api-key`, not a bearer token.
                r = if p.id == "azure-openai" { r.header("api-key", k) } else { r.bearer_auth(k) };
            }
            (r, "openai")
        }
    }

    /// Forwards an OpenAI-compatible endpoint (`/images/generations`, `/audio/speech`, `/audio/transcriptions`)
    /// to connected OpenAI-family API-key providers with failover and cooldowns.
    pub async fn passthrough_openai_post(
        &self,
        key: Option<VirtualKeyRecord>,
        path: &str,
        req_model: &str,
        payload: PassthroughPayload,
    ) -> Result<PassthroughReply, ChatError> {
        self.admit_rate(key.as_ref())?;
        let (targets, settings, key_name) = self.check_key_and_resolve(key.as_ref(), req_model)?;
        let mut last_error = format!("no connected OpenAI-compatible provider can serve '{req_model}'");

        for (target, acc) in self.plan_attempts(&targets, &settings) {
            if target.kind != "apikey" || !self.store.is_provider_enabled(&target.provider) {
                continue;
            }
            let Some(p) = find_provider_by_id(&target.provider) else { continue };
            if p.family != crate::providers::OPENAI {
                continue;
            }
            let Some((api_key, base)) = self.resolve_apikey_and_base(&target, acc.as_ref(), p) else {
                continue;
            };

            let cool = cool_key(&target, acc.as_ref());
            let t0 = std::time::Instant::now();
            let mut req = self.http_client.post(format!("{base}/{path}"));
            if let Some(k) = api_key {
                req = if p.id == "azure-openai" { req.header("api-key", k) } else { req.bearer_auth(k) };
            }
            req = match &payload {
                PassthroughPayload::Json(v) => {
                    let mut b = v.clone();
                    b["model"] = json!(target.model);
                    req.json(&b)
                }
                PassthroughPayload::Raw { content_type, body } => req.header("content-type", content_type).body(body.clone()),
            };

            let upstream = match req.send().await {
                Ok(r) => r,
                Err(e) => {
                    last_error = format!("{}: {e}", target.public());
                    continue;
                }
            };
            let status = upstream.status();
            if !status.is_success() {
                last_error = self
                    .handle_upstream_error(
                        upstream,
                        &target,
                        acc.as_ref(),
                        &cool,
                        key.as_ref(),
                        &key_name,
                        t0,
                        false,
                        settings.cooldown_secs,
                    )
                    .await;
                continue;
            }
            self.store.clear_cooldown(&cool);
            if let Some(a) = &acc {
                let _ = self.store.record_account_error(&a.id, None);
                self.sticky.lock().insert(target.provider.clone(), a.id.clone());
            }
            let content_type =
                upstream.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("application/json").to_string();
            let resp_bytes = upstream.bytes().await.map_err(|e| gateway_err(502, e.to_string()))?;
            self.store.record_usage(&UsageEntry {
                key_id: key.as_ref().map(|k| k.id.as_str()),
                key_name: Some(&key_name),
                provider: Some(&target.provider),
                model: Some(&target.model),
                input_tokens: 0,
                output_tokens: 0,
                cached_tokens: 0,
                duration_ms: t0.elapsed().as_millis() as i64,
                status: "ok",
                status_code: i64::from(status.as_u16()),
                error: None,
                streaming: false,
            });
            return Ok(PassthroughReply { status: status.as_u16(), content_type, body: resp_bytes });
        }

        Err(gateway_err(502, format!("No connected AI provider could respond: {last_error}")))
    }

    /// Routes one OpenAI-style chat request across the resolved targets, failing over on errors.
    /// `key` is the caller's virtual key, if it presented one.
    pub async fn chat_completions(&self, key: Option<VirtualKeyRecord>, body: Value) -> Result<ChatReply, ChatError> {
        self.chat_completions_with_options(key, body, ChatOptions::default()).await
    }

    /// Routes one OpenAI-style chat request, injecting any active Context/Skills from the
    /// virtual key, `header_skills` (`x-eay-skills`), or `+skill` model suffix.
    pub async fn chat_completions_with_skills(
        &self,
        key: Option<VirtualKeyRecord>,
        body: Value,
        header_skills: &[String],
    ) -> Result<ChatReply, ChatError> {
        self.chat_completions_with_options(key, body, ChatOptions { skills: header_skills.to_vec(), ..Default::default() }).await
    }

    /// Full chat routing entry point supporting Agent profiles (`agent/<name>`), Skills,
    /// Session memory (`x-eay-session-id`), Context Window Guard, and Secret Redaction.
    pub async fn chat_completions_with_options(
        &self,
        key: Option<VirtualKeyRecord>,
        mut body: Value,
        opts: ChatOptions,
    ) -> Result<ChatReply, ChatError> {
        self.admit_rate(key.as_ref())?;
        normalize_text_documents(&mut body);
        let media_req = detect_media_requirements(&body);
        let raw_model = body.get("model").and_then(|m| m.as_str()).unwrap_or("auto").to_string();
        let (parsed_model, model_skills) = split_model_and_skills(&raw_model);

        let mut base_model = parsed_model.to_string();
        let mut combined_skills: Vec<String> = Vec::new();
        let mut agent_personality: Option<(String, String)> = None;

        if let Some(agent_name) = base_model.strip_prefix("agent/") {
            let agent = self
                .store
                .get_agent(agent_name)
                .filter(|a| a.enabled)
                .ok_or_else(|| gateway_err(404, format!("Agent profile '{agent_name}' not found or disabled")))?;
            base_model = agent.model.clone();
            combined_skills.extend(agent.skills);
            if !agent.personality.is_empty() {
                agent_personality = Some((agent.name, agent.personality));
            }
            if body.get("temperature").is_none() {
                if let Some(t) = agent.temperature {
                    body["temperature"] = json!(t);
                }
            }
            if body.get("max_tokens").is_none() && body.get("max_completion_tokens").is_none() {
                if let Some(mt) = agent.max_tokens {
                    body["max_tokens"] = json!(mt);
                }
            }
        }
        body["model"] = json!(base_model);

        if let Some(ks) = key.as_ref().and_then(|k| k.default_skills.as_ref()) {
            combined_skills.extend(ks.iter().cloned());
        }
        combined_skills.extend(opts.skills.iter().cloned());
        combined_skills.extend(model_skills);

        let incoming_turns: Vec<Value> = body
            .get("messages")
            .and_then(|m| m.as_array())
            .map(|arr| arr.iter().filter(|m| !matches!(m["role"].as_str(), Some("system" | "developer"))).cloned().collect())
            .unwrap_or_default();

        // Sessions are namespaced by caller so one key cannot read or extend another key's memory.
        let session_key = opts.session_id.as_deref().map(|sid| session_storage_id(key.as_ref(), sid));
        if let Some(ref sid) = session_key {
            let history = self.store.get_session_messages(sid);
            if !history.is_empty() {
                if let Some(msgs) = body.get_mut("messages").and_then(|m| m.as_array_mut()) {
                    let sys: Vec<Value> =
                        msgs.iter().filter(|m| matches!(m["role"].as_str(), Some("system" | "developer"))).cloned().collect();
                    let non_sys: Vec<Value> =
                        msgs.iter().filter(|m| !matches!(m["role"].as_str(), Some("system" | "developer"))).cloned().collect();
                    msgs.clear();
                    msgs.extend(sys);
                    msgs.extend(history);
                    msgs.extend(non_sys);
                }
            }
        }

        let agent_label = opts.agent_label.clone().or_else(|| agent_personality.as_ref().map(|(n, _)| n.clone()));
        if let Some((name, persona)) = agent_personality {
            if let Some(msgs) = body.get_mut("messages").and_then(|m| m.as_array_mut()) {
                msgs.insert(0, json!({ "role": "system", "content": format!("[Agent Personality: {name}]\n{persona}") }));
            }
        }
        self.inject_skills(&mut body, &combined_skills);
        if opts.redact_secrets {
            guardrails::redact_secrets_in_body(&mut body);
        }
        if opts.max_context_chars > 0 {
            guardrails::trim_context_messages(&mut body, opts.max_context_chars);
        }

        let run_id = opts.run_id.clone().unwrap_or_else(|| format!("req_{}", uuid::Uuid::new_v4().simple()));
        let parent_id = opts.session_id.clone();
        self.activity.emit_phase(
            &run_id,
            parent_id.as_deref(),
            agent_label.as_deref(),
            "preparing",
            format!("Preparing request ({})", raw_model),
            None,
            None,
        );

        let req_model = base_model;
        let caller_stream = body.get("stream").and_then(|s| s.as_bool()).unwrap_or(false);

        let mcp_records = self.resolve_mcp_servers(&opts.mcp_servers);
        if !mcp_records.is_empty() {
            let mut merged_tools = body.get("tools").and_then(|t| t.as_array()).cloned().unwrap_or_default();
            let mut mcp_index = crate::mcp::McpToolIndex::default();
            for srv in &mcp_records {
                let (tools, index) = self.mcp_tools_cached(srv).await;
                merged_tools.extend(tools);
                mcp_index.extend(index);
            }
            if !merged_tools.is_empty() {
                body["tools"] = Value::Array(merged_tools);
                body["stream"] = json!(false);
                const MAX_MCP_ROUNDS: usize = 8;
                for _ in 0..MAX_MCP_ROUNDS {
                    let ChatReply::Json(reply) = self
                        .execute_chat_turn(
                            key.as_ref(),
                            &body,
                            &req_model,
                            &run_id,
                            parent_id.as_deref(),
                            agent_label.as_deref(),
                            media_req,
                        )
                        .await?
                    else {
                        unreachable!("stream forced false during MCP loop");
                    };
                    let msg = &reply["choices"][0]["message"];
                    let mcp_calls: Vec<Value> = msg["tool_calls"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter(|tc| tc["function"]["name"].as_str().is_some_and(|n| mcp_index.resolve(n).is_some()))
                        .cloned()
                        .collect();
                    if mcp_calls.is_empty() {
                        if let Some(ref sid) = session_key {
                            self.append_session_turn(sid, &incoming_turns, &reply);
                        }
                        return Ok(if caller_stream { json_reply_to_sse(&reply) } else { ChatReply::Json(reply) });
                    }
                    if let Some(msgs) = body.get_mut("messages").and_then(|m| m.as_array_mut()) {
                        msgs.push(msg.clone());
                        // Tool messages are text-only in chat/completions; images follow as one user turn.
                        let mut tool_images: Vec<Value> = Vec::new();
                        for tc in mcp_calls {
                            let call_id = tc["id"].as_str().unwrap_or("");
                            let full_name = tc["function"]["name"].as_str().unwrap_or("");
                            let args: Value =
                                serde_json::from_str(tc["function"]["arguments"].as_str().unwrap_or("{}")).unwrap_or(json!({}));
                            let output = if let Some((srv_name, tool_name)) = mcp_index.resolve(full_name) {
                                let (mut meta, msg_label) = build_tool_activity_meta(full_name, &args);
                                let t_tool = std::time::Instant::now();
                                let res = match mcp_records.iter().find(|s| s.name == srv_name) {
                                    Some(srv) => match crate::mcp::call_tool_rich(&self.http_client, srv, tool_name, args).await {
                                        Ok(out) => {
                                            tool_images.extend(
                                                out.images
                                                    .into_iter()
                                                    .map(|url| json!({ "type": "image_url", "image_url": { "url": url } })),
                                            );
                                            out.text
                                        }
                                        Err(e) => format!("MCP error: {e}"),
                                    },
                                    None => format!("MCP server '{srv_name}' not active"),
                                };
                                meta.duration_ms = Some(t_tool.elapsed().as_millis() as i64);
                                self.activity.emit_with_meta(
                                    &run_id,
                                    parent_id.as_deref(),
                                    agent_label.as_deref(),
                                    "calling_tool",
                                    msg_label,
                                    None,
                                    Some(full_name),
                                    Some(meta),
                                );
                                res
                            } else {
                                "Unknown MCP tool".into()
                            };
                            msgs.push(json!({ "role": "tool", "tool_call_id": call_id, "content": output }));
                        }
                        if !tool_images.is_empty() {
                            let mut parts = vec![json!({ "type": "text", "text": "Images returned by the tool calls above:" })];
                            parts.extend(tool_images);
                            msgs.push(json!({ "role": "user", "content": parts }));
                        }
                    }
                }
            }
        }

        let reply = self
            .execute_chat_turn(key.as_ref(), &body, &req_model, &run_id, parent_id.as_deref(), agent_label.as_deref(), media_req)
            .await?;
        let Some(sid) = session_key else { return Ok(reply) };
        Ok(match reply {
            ChatReply::Json(reply_json) => {
                self.append_session_turn(&sid, &incoming_turns, &reply_json);
                ChatReply::Json(reply_json)
            }
            // Tee the stream; once it ends, fold what the client received into one reply and save it.
            ChatReply::Stream(frames) => {
                let seen = Arc::new(parking_lot::Mutex::new(String::new()));
                let (tee, tail_seen, engine) = (seen.clone(), seen, self.clone());
                let frames = frames
                    .map(move |item| {
                        if let Ok(b) = &item {
                            tee.lock().push_str(&String::from_utf8_lossy(b));
                        }
                        item
                    })
                    .chain(futures::stream::once(async move {
                        let sse = std::mem::take(&mut *tail_seen.lock());
                        let reply = aggregate_sse(&sse, (0, 0));
                        if sse_error(&sse).is_none() && session_reply_has_content(&reply) {
                            engine.append_session_turn(&sid, &incoming_turns, &reply);
                        }
                        Ok(Bytes::new())
                    }))
                    .boxed();
                ChatReply::Stream(frames)
            }
        })
    }

    /// Avoids spawning every stdio MCP server on every chat request just to list its tools.
    async fn mcp_tools_cached(&self, srv: &crate::store::McpServerRecord) -> (Vec<Value>, crate::mcp::McpToolIndex) {
        const TTL: std::time::Duration = std::time::Duration::from_secs(300);
        let key = format!("{}@{}", srv.name, srv.updated_at);
        if let Some((at, tools, index)) = self.mcp_tools.lock().get(&key) {
            if at.elapsed() < TTL {
                return (tools.clone(), index.clone());
            }
        }
        match crate::mcp::list_tools_indexed(&self.http_client, srv).await {
            Ok((tools, index)) => {
                self.mcp_tools.lock().insert(key, (std::time::Instant::now(), tools.clone(), index.clone()));
                (tools, index)
            }
            Err(e) => {
                tracing::warn!("[mcp] tools/list failed for '{}': {e}", srv.name);
                Default::default()
            }
        }
    }

    fn resolve_mcp_servers(&self, requested: &[String]) -> Vec<crate::store::McpServerRecord> {
        if requested.is_empty() {
            return Vec::new();
        }
        let all: Vec<_> = self.store.list_mcp_servers().into_iter().filter(|s| s.enabled).collect();
        if requested.iter().any(|r| r == "*") {
            return all;
        }
        all.into_iter().filter(|s| requested.iter().any(|r| r == &s.name)).collect()
    }

    fn append_session_turn(&self, sid: &str, incoming_turns: &[Value], reply_json: &Value) {
        let mut hist = self.store.get_session_messages(sid);
        hist.extend(incoming_turns.iter().cloned());
        if let Some(assistant_msg) = reply_json.get("choices").and_then(|c| c.get(0)).and_then(|c| c.get("message")) {
            hist.push(assistant_msg.clone());
        }
        let _ = self.store.save_session_messages(sid, &hist);
    }

    #[allow(clippy::too_many_arguments)]
    async fn execute_chat_turn(
        &self,
        key: Option<&VirtualKeyRecord>,
        body: &Value,
        req_model: &str,
        run_id: &str,
        parent_id: Option<&str>,
        agent_label: Option<&str>,
        media_req: crate::translate::MediaRequirements,
    ) -> Result<ChatReply, ChatError> {
        let wants_stream = body.get("stream").and_then(|s| s.as_bool()).unwrap_or(false);
        let (mut targets, settings, key_name) = self.check_key_and_resolve(key, req_model)?;
        if media_req.any() && targets.len() > 1 {
            filter_multimodal_targets(&mut targets, media_req);
        }
        let mut last_error = format!("no connected provider can serve '{req_model}'");

        for (target, acc) in self.plan_attempts(&targets, &settings) {
            if !self.store.is_provider_enabled(&target.provider) {
                continue;
            }
            let target_pub = target.public();
            self.activity.emit_phase(
                run_id,
                parent_id,
                agent_label,
                "routing",
                format!("Routing to {target_pub}..."),
                Some(&target_pub),
                None,
            );
            let cool = cool_key(&target, acc.as_ref());
            let t0 = std::time::Instant::now();
            let mut resp = None;
            if let Some(acc) = acc.as_ref().filter(|_| target.kind == "oauth") {
                // One forced refresh + retry when the upstream rejects the token.
                for force in [false, true] {
                    let cred = match crate::oauth::fresh_credential(&self.http_client, &self.store, acc, force).await {
                        Ok(c) => c,
                        Err(e) => {
                            last_error = e;
                            break;
                        }
                    };
                    let Some((req, kind)) =
                        crate::oauth::subscription_request(&self.http_client, &target.provider, &cred, &target.model, body)
                    else {
                        break;
                    };
                    match req.send().await {
                        Ok(r) if r.status().as_u16() == 401 && !force => {
                            last_error = format!("{}: 401", target.provider);
                            continue;
                        }
                        Ok(r) => {
                            resp = Some((r, kind));
                            break;
                        }
                        Err(e) => {
                            last_error = format!("{}: {e}", target.provider);
                            break;
                        }
                    }
                }
            } else {
                let Some(p) = find_provider_by_id(&target.provider) else { continue };
                let Some((api_key, base)) = self.resolve_apikey_and_base(&target, acc.as_ref(), p) else {
                    continue;
                };
                let (req, kind) = self.build_apikey_chat_request(p, &base, api_key, &target, body);
                match req.header("Accept", "text/event-stream").send().await {
                    Ok(r) => resp = Some((r, kind)),
                    Err(e) => last_error = format!("{}: {e}", target.public()),
                }
            }

            let Some((upstream, kind)) = resp else {
                if let Some(a) = &acc {
                    let _ = self.store.record_account_error(&a.id, Some(&last_error));
                }
                self.activity.emit_phase(
                    run_id,
                    parent_id,
                    agent_label,
                    "failover",
                    format!("Failover from {}: {last_error}", target.public()),
                    Some(&target_pub),
                    None,
                );
                continue;
            };
            if !upstream.status().is_success() {
                last_error = self
                    .handle_upstream_error(upstream, &target, acc.as_ref(), &cool, key, &key_name, t0, wants_stream, settings.cooldown_secs)
                    .await;
                eprintln!("[GATEWAY CHAT] {last_error}");
                self.activity.emit_phase(
                    run_id,
                    parent_id,
                    agent_label,
                    "failover",
                    format!("Failover: {last_error}"),
                    Some(&target_pub),
                    None,
                );
                continue;
            }
            self.store.clear_cooldown(&cool);
            if let Some(a) = &acc {
                let _ = self.store.record_account_error(&a.id, None);
                self.sticky.lock().insert(target.provider.clone(), a.id.clone());
            }

            self.activity.emit_phase(
                run_id,
                parent_id,
                agent_label,
                "waiting_upstream",
                format!("Streaming from {target_pub}..."),
                Some(&target_pub),
                None,
            );
            let tr = Arc::new(parking_lot::Mutex::new(SseToOpenAi::new(kind, target.public())));
            let guard = UsageGuard {
                store: self.store.clone(),
                tr: tr.clone(),
                activity: self.activity.clone(),
                run_id: run_id.to_string(),
                parent_id: parent_id.map(String::from),
                agent: agent_label.map(String::from),
                target: target_pub.clone(),
                key_id: key.map(|k| k.id.clone()),
                key_name: key_name.clone(),
                provider: target.provider.clone(),
                model: target.model.clone(),
                t0,
                streaming: wants_stream,
            };
            let frames = sse_frames(upstream, tr.clone(), guard);

            if wants_stream {
                return Ok(ChatReply::Stream(frames));
            }
            let mut sse = String::new();
            let mut frames = frames;
            while let Some(chunk) = frames.next().await {
                match chunk {
                    Ok(b) => sse.push_str(&String::from_utf8_lossy(&b)),
                    Err(e) => return Err(gateway_err(502, e.to_string())),
                }
            }
            drop(frames); // records usage before we read it below
            let usage = tr.lock().usage;
            if let Some(msg) = sse_error(&sse) {
                return Err(gateway_err(502, msg));
            }
            let reply_json = aggregate_sse(&sse, usage);
            return Ok(ChatReply::Json(reply_json));
        }

        self.activity.emit_phase(run_id, parent_id, agent_label, "failed", format!("Failed: {last_error}"), None, None);
        Err(gateway_err(502, format!("No connected AI provider could respond: {last_error}")))
    }

    /// Starts a subagent in the background. It runs under `key` (budgets and limits apply), may use the
    /// MCP servers in `opts.mcp_servers`, waits for a free slot (at most `MAX_CONCURRENT_SUBAGENTS` run at
    /// once), and is saved to the store when it finishes so history survives restarts.
    #[allow(clippy::too_many_arguments)]
    pub fn spawn_subagent(
        &self,
        key: Option<VirtualKeyRecord>,
        role: String,
        model: String,
        prompt: String,
        system: Option<String>,
        parent_id: Option<String>,
        mut opts: ChatOptions,
    ) -> SubagentRun {
        let id = format!("sub_{}", &uuid::Uuid::new_v4().simple().to_string()[..10]);
        let run = self.activity.create_run_with(
            id.clone(),
            opts.session_id.clone(),
            role.clone(),
            model.clone(),
            parent_id,
            key.as_ref().map(|k| k.id.clone()),
        );
        opts.run_id = Some(id.clone());
        opts.agent_label = Some(role);

        let mut msgs = Vec::new();
        if let Some(sys) = system.filter(|s| !s.trim().is_empty()) {
            msgs.push(json!({ "role": "system", "content": sys }));
        }
        msgs.push(json!({ "role": "user", "content": prompt }));
        let body = json!({ "model": model, "messages": msgs, "stream": false });

        let engine = self.clone();
        let task_id = id.clone();
        let handle = tokio::spawn(async move {
            // Closed only if the hub is dropped; then just run unthrottled.
            let _permit = engine.activity.slots.clone().acquire_owned().await.ok();
            let result = engine.chat_completions_with_options(key, body, opts).await;
            let finished = match result {
                Ok(ChatReply::Json(v)) => {
                    let txt = v["choices"][0]["message"]["content"].as_str().unwrap_or("").to_string();
                    engine.activity.finish_run(&task_id, Some(txt), None)
                }
                Ok(ChatReply::Stream(_)) => engine.activity.finish_run(&task_id, Some(String::new()), None),
                Err(e) => {
                    let msg = e.body["error"]["message"].as_str().unwrap_or("Subagent failed").to_string();
                    engine.activity.finish_run(&task_id, None, Some(msg))
                }
            };
            if let Some(r) = finished {
                engine.store.save_subagent_run(&r);
            }
        });
        self.activity.attach_task(&id, handle.abort_handle());
        run
    }

    /// Loads saved subagent history into the live hub; call once at startup.
    pub fn restore_subagent_history(&self) {
        self.activity.restore_runs(self.store.list_subagent_runs(200));
    }

    fn inject_skills(&self, body: &mut Value, skill_names: &[String]) {
        let mut seen = std::collections::HashSet::new();
        let mut blocks = Vec::new();
        for raw in skill_names {
            let name = raw.trim();
            if name.is_empty() || !seen.insert(name.to_string()) {
                continue;
            }
            if let Some(skill) = self.store.get_skill(name).filter(|s| s.enabled) {
                blocks.push(format!("[Gateway Skill: {}]\n{}", skill.name, skill.prompt));
            }
        }
        if blocks.is_empty() {
            return;
        }
        let combined = blocks.join("\n\n");
        if let Some(msgs) = body.get_mut("messages").and_then(|m| m.as_array_mut()) {
            msgs.insert(0, json!({ "role": "system", "content": combined }));
        }
    }
}

fn split_model_and_skills(raw: &str) -> (&str, Vec<String>) {
    let mut parts = raw.split('+');
    let base = parts.next().unwrap_or(raw).trim();
    let skills = parts.map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect();
    (base, skills)
}

/// Stable UUID-shaped id from a seed (Command Code groups requests by thread).
fn commandcode_thread_id(seed: &str) -> String {
    use sha2::{Digest, Sha256};
    let h = hex::encode(Sha256::digest(seed.as_bytes()));
    format!("{}-{}-4{}-a{}-{}", &h[0..8], &h[8..12], &h[13..16], &h[17..20], &h[20..32])
}

fn json_reply_to_sse(reply: &Value) -> ChatReply {
    let id = reply["id"].clone();
    let created = reply["created"].clone();
    let model = reply["model"].clone();
    let msg = &reply["choices"][0]["message"];
    let finish = reply["choices"][0]["finish_reason"].clone();
    let mut delta = json!({ "role": "assistant" });
    if let Some(c) = msg.get("content").filter(|v| !v.is_null()) {
        delta["content"] = c.clone();
    }
    if let Some(tc) = msg.get("tool_calls").filter(|v| !v.is_null()) {
        delta["tool_calls"] = tc.clone();
    }
    let chunk1 = format!(
        "data: {}\n\n",
        json!({ "id": id, "object": "chat.completion.chunk", "created": created, "model": model, "choices": [{ "index": 0, "delta": delta, "finish_reason": null }] })
    );
    let chunk2 = format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({ "id": id, "object": "chat.completion.chunk", "created": created, "model": model, "choices": [{ "index": 0, "delta": {}, "finish_reason": finish }] })
    );
    ChatReply::Stream(futures::stream::iter(vec![Ok(Bytes::from(chunk1)), Ok(Bytes::from(chunk2))]).boxed())
}

/// A failed or empty stream must not leave a dangling user turn in the session.
fn session_reply_has_content(reply: &Value) -> bool {
    let m = &reply["choices"][0]["message"];
    m["content"].as_str().is_some_and(|c| !c.is_empty()) || m["tool_calls"].as_array().is_some_and(|t| !t.is_empty())
}

/// Storage id for a caller's session: `key:<key-id>:<sid>` for virtual keys, `local:<sid>` otherwise.
pub fn session_storage_id(key: Option<&VirtualKeyRecord>, sid: &str) -> String {
    match key {
        Some(k) => format!("key:{}:{sid}", k.id),
        None => format!("local:{sid}"),
    }
}

fn gateway_err(status: u16, message: String) -> ChatError {
    ChatError { status, body: json!({ "error": { "message": message, "type": "gateway_error" } }) }
}

#[cfg(test)]
mod tests {
    use super::routing::{cooldown_for, filter_multimodal_targets};
    use super::*;
    use crate::translate::MediaRequirements;

    fn engine() -> (GatewayEngine, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        (GatewayEngine::new(GatewayStore::new(dir.path()).unwrap()), dir)
    }

    fn ids(ts: &[Target]) -> Vec<String> {
        ts.iter().map(Target::public).collect()
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn mcp_tools_list_is_cached_until_the_server_changes() {
        let (e, dir) = engine();
        let log = dir.path().join("spawns");
        // Fake stdio MCP server: logs each spawn, answers initialize (id 1) and tools/list (id 2).
        let script = format!(
            "echo x >> {}; read a; echo '{{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{{}}}}'; read b; read c; \
             echo '{{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{{\"tools\":[{{\"name\":\"ping\"}}]}}}}'",
            log.display()
        );
        let mut srv = crate::store::McpServerRecord {
            name: "fake".into(),
            transport: "stdio".into(),
            command: Some("sh".into()),
            args: vec!["-c".into(), script],
            env: Default::default(),
            url: None,
            headers: Default::default(),
            enabled: true,
            updated_at: 1.0,
        };
        let spawns = || std::fs::read_to_string(&log).unwrap_or_default().lines().count();

        let (tools, _) = e.mcp_tools_cached(&srv).await;
        assert_eq!(tools[0]["function"]["name"], "mcp__fake__ping");
        e.mcp_tools_cached(&srv).await;
        assert_eq!(spawns(), 1);

        srv.updated_at = 2.0;
        e.mcp_tools_cached(&srv).await;
        assert_eq!(spawns(), 2);
    }

    #[test]
    fn sessions_are_namespaced_per_key() {
        let (e, _d) = engine();
        let (ka, _) = e.store.create_virtual_key("a", None, None).unwrap();
        let (kb, _) = e.store.create_virtual_key("b", None, None).unwrap();
        let sa = session_storage_id(Some(&ka), "s1");
        assert_ne!(sa, session_storage_id(Some(&kb), "s1"));
        assert_ne!(sa, session_storage_id(None, "s1"));
        e.store.save_session_messages(&sa, &[json!({"role":"user","content":"secret"})]).unwrap();
        assert!(e.store.get_session_messages(&session_storage_id(Some(&kb), "s1")).is_empty());
        assert!(e.store.get_session_messages(&session_storage_id(None, "s1")).is_empty());
    }

    mod subagents {
        use super::*;

        #[tokio::test]
        async fn a_failed_run_is_saved_and_joinable() {
            let (e, _d) = engine();
            let run = e.spawn_subagent(None, "r".into(), "auto".into(), "hi".into(), None, Some("parent_1".into()), ChatOptions::default());
            assert_eq!(run.parent_id.as_deref(), Some("parent_1"));
            let done = e.activity.wait_run(&run.id, std::time::Duration::from_secs(5)).await.unwrap();
            assert_eq!(done.phase, "failed"); // nothing is connected
            assert!(e.store.list_subagent_runs(10).iter().any(|r| r.id == run.id));

            // History comes back after a "restart".
            let e2 = GatewayEngine::new(GatewayStore::new(_d.path()).unwrap());
            e2.restore_subagent_history();
            assert_eq!(e2.activity.get_run(&run.id).unwrap().phase, "failed");
        }

        #[tokio::test]
        async fn cancelling_stops_a_queued_run() {
            let (e, _d) = engine();
            // Hold every slot so the run cannot start.
            let _held = e.activity.slots.clone().acquire_many_owned(activity::MAX_CONCURRENT_SUBAGENTS as u32).await.unwrap();
            let run = e.spawn_subagent(None, "r".into(), "auto".into(), "hi".into(), None, None, ChatOptions::default());
            assert!(e.activity.cancel_run(&run.id));
            assert_eq!(e.activity.get_run(&run.id).unwrap().phase, "cancelled");
            assert!(!e.activity.cancel_run(&run.id), "second cancel is a no-op");
        }

        #[tokio::test]
        async fn waiting_times_out_on_a_running_job() {
            let (e, _d) = engine();
            let _held = e.activity.slots.clone().acquire_many_owned(activity::MAX_CONCURRENT_SUBAGENTS as u32).await.unwrap();
            let run = e.spawn_subagent(None, "r".into(), "auto".into(), "hi".into(), None, None, ChatOptions::default());
            let r = e.activity.wait_run(&run.id, std::time::Duration::from_millis(100)).await.unwrap();
            assert!(!r.is_finished());
        }
    }

    #[test]
    fn concurrent_slots_are_capped_and_freed_on_drop() {
        let (e, _d) = engine();
        let (mut k, _) = e.store.create_virtual_key("k", None, None).unwrap();
        k.max_concurrent = Some(2);
        let a = e.try_acquire_inflight(&k).unwrap();
        let _b = e.try_acquire_inflight(&k).unwrap();
        assert!(e.try_acquire_inflight(&k).is_none());
        drop(a);
        assert!(e.try_acquire_inflight(&k).is_some());
        k.max_concurrent = None;
        assert!((0..10).all(|_| e.try_acquire_inflight(&k).map(std::mem::forget).is_some()));
    }

    #[test]
    fn rate_limit_counts_admitted_requests_not_attempts() {
        let (e, _d) = engine();
        let (k, _) = e.store.create_virtual_key("k", None, Some(2)).unwrap();
        assert!(e.admit_rate(Some(&k)).is_ok());
        // Failed upstream attempts are logged as usage rows but must not use up the limit.
        for _ in 0..5 {
            e.store.record_usage(&UsageEntry {
                key_id: Some(&k.id),
                key_name: Some("k"),
                provider: Some("openai"),
                model: Some("m"),
                input_tokens: 0,
                output_tokens: 0,
                cached_tokens: 0,
                duration_ms: 1,
                status: "error",
                status_code: 429,
                error: None,
                streaming: false,
            });
        }
        assert!(e.admit_rate(Some(&k)).is_ok());
        assert_eq!(e.admit_rate(Some(&k)).unwrap_err().status, 429);
        assert!(e.admit_rate(None).is_ok());
    }

    /// OpenAI (with one custom model) and DeepSeek connected by API key.
    fn two_apikey_providers() -> (GatewayEngine, tempfile::TempDir) {
        let (e, d) = engine();
        e.store.save_provider_key("openai", "sk-1").unwrap();
        e.store.save_provider_key("deepseek", "sk-2").unwrap();
        e.store.add_provider_model("openai", "my-model").unwrap();
        (e, d)
    }

    mod resolve_targets {
        use super::*;

        #[test]
        fn auto_tries_two_models_per_apikey_provider_by_default() {
            let (e, _d) = two_apikey_providers();
            assert_eq!(e.resolve_targets("auto").len(), 4);
        }

        #[test]
        fn auto_puts_a_providers_custom_models_first() {
            let (e, _d) = two_apikey_providers();
            assert_eq!(ids(&e.resolve_targets("auto"))[0], "oa/my-model");
        }

        #[test]
        fn auto_follows_the_saved_provider_priority() {
            let (e, _d) = two_apikey_providers();
            e.store.set_setting("provider_priority", r#"["deepseek","openai"]"#).unwrap();
            let order = ids(&e.resolve_targets("auto"));
            assert!(order[0].starts_with("ds/") && order[2].starts_with("oa/"), "{order:?}");
        }

        #[test]
        fn provider_auto_and_auto_provider_are_the_same() {
            let (e, _d) = two_apikey_providers();
            assert_eq!(ids(&e.resolve_targets("oa/auto")), ids(&e.resolve_targets("auto/oa")));
        }

        #[test]
        fn provider_auto_stays_within_that_provider() {
            let (e, _d) = two_apikey_providers();
            let t = ids(&e.resolve_targets("ds/auto"));
            assert!(t.iter().all(|m| m.starts_with("ds/")), "{t:?}");
        }

        #[test]
        fn an_unknown_combo_resolves_to_nothing() {
            let (e, _d) = engine();
            assert!(e.resolve_targets("auto/missing-combo").is_empty());
        }

        #[test]
        fn a_combo_keeps_its_explicit_models_first() {
            let (e, _d) = engine();
            e.store.save_provider_key("deepseek", "sk").unwrap();
            e.store.save_combo("mix", &["oa/gpt-4o".into(), "ds/auto".into()]).unwrap();
            assert_eq!(ids(&e.resolve_targets("auto/mix"))[0], "oa/gpt-4o");
        }

        #[test]
        fn a_combo_drops_duplicate_targets() {
            let (e, _d) = engine();
            e.store.save_provider_key("deepseek", "sk").unwrap();
            e.store.save_combo("mix", &["oa/gpt-4o".into(), "ds/auto".into(), "oa/gpt-4o".into()]).unwrap();
            let t = ids(&e.resolve_targets("auto/mix"));
            assert!(t[1..].iter().all(|m| m.starts_with("ds/")), "{t:?}");
        }
    }

    mod auto_model_ids {
        use super::*;

        #[test]
        fn lists_saved_combos() {
            let (e, _d) = engine();
            e.store.save_combo("mix", &["oa/gpt-4o".into()]).unwrap();
            assert!(e.auto_model_ids().contains(&"auto/mix".to_string()));
        }
    }

    mod plan_attempts {
        use super::*;

        /// Two Codex accounts, reordered so B comes before A. Returns (engine, dir, a, b).
        fn two_accounts() -> (GatewayEngine, tempfile::TempDir, String, String) {
            let (e, d) = engine();
            let a = e.store.save_account("codex", &json!({"access_token":"a","plan":"plus"}), Some("A"), None).unwrap();
            let b = e.store.save_account("codex", &json!({"access_token":"b","plan":"plus"}), Some("B"), None).unwrap();
            e.store.reorder_accounts("codex", &[b.clone(), a.clone()]).unwrap();
            (e, d, a, b)
        }

        /// (model, account id) for each planned attempt over models m1, m2.
        fn plan(e: &GatewayEngine) -> Vec<(String, String)> {
            let t = |m: &str| Target { provider: "codex".into(), model: m.into(), kind: "oauth".into() };
            let s = e.store.get_gateway_settings();
            e.plan_attempts(&[t("m1"), t("m2")], &s).into_iter().map(|(t, a)| (t.model, a.unwrap().id)).collect()
        }

        fn pair(m: &str, id: &str) -> (String, String) {
            (m.to_string(), id.to_string())
        }

        #[test]
        fn model_first_tries_every_account_in_user_order_before_the_next_model() {
            let (e, _d, a, b) = two_accounts();
            assert_eq!(plan(&e), vec![pair("m1", &b), pair("m1", &a), pair("m2", &b), pair("m2", &a)]);
        }

        #[test]
        fn account_first_tries_every_model_before_the_next_account() {
            let (e, _d, a, b) = two_accounts();
            e.store.set_setting("fallback_order", "account_first").unwrap();
            assert_eq!(plan(&e), vec![pair("m1", &b), pair("m2", &b), pair("m1", &a), pair("m2", &a)]);
        }

        #[test]
        fn a_cooling_account_is_tried_last() {
            let (e, _d, a, b) = two_accounts();
            e.store.cool_down(&b, 60);
            assert_eq!(plan(&e)[0].1, a);
        }

        #[test]
        fn round_robin_starts_consecutive_requests_on_different_accounts() {
            let (e, _d, _a, _b) = two_accounts();
            e.store.set_setting("routing_strategy", "round_robin").unwrap();
            assert_ne!(plan(&e)[0].1, plan(&e)[0].1);
        }

        #[test]
        fn an_apikey_provider_tries_its_main_key_then_extra_keys() {
            let (e, _d) = engine();
            e.store.save_provider_key("deepseek", "sk-main").unwrap();
            let extra = e.store.save_account("deepseek", &json!({"api_key":"sk-2"}), Some("Key …sk-2"), None).unwrap();
            let t = Target { provider: "deepseek".into(), model: "m".into(), kind: "apikey".into() };
            let s = e.store.get_gateway_settings();
            let plan: Vec<Option<String>> = e.plan_attempts(&[t], &s).into_iter().map(|(_, a)| a.map(|a| a.id)).collect();
            assert_eq!(plan, vec![None, Some(extra)]);
        }

        #[test]
        fn fill_first_sticks_to_the_last_account_that_worked() {
            let (e, _d, a, _b) = two_accounts();
            e.store.set_setting("routing_strategy", "fill_first").unwrap();
            e.sticky.lock().insert("codex".into(), a.clone());
            assert_eq!(plan(&e)[0].1, a);
        }
    }

    mod cooldown_for {
        use super::*;

        #[test]
        fn a_429_cools_for_the_default_time() {
            assert_eq!(cooldown_for(429, None, "", 60), Some(60));
        }

        #[test]
        fn retry_after_overrides_the_default() {
            assert_eq!(cooldown_for(429, Some(5), "", 60), Some(5));
        }

        #[test]
        fn a_usage_limit_body_uses_its_reset_time() {
            assert_eq!(cooldown_for(400, None, r#"{"error":{"type":"usage_limit_reached","resets_in_seconds":900}}"#, 60), Some(900));
        }

        #[test]
        fn server_errors_do_not_cool_down() {
            assert_eq!(cooldown_for(500, None, "internal", 60), None);
        }

        #[test]
        fn unsupported_model_errors_do_not_cool_down() {
            assert_eq!(cooldown_for(404, None, "model not supported", 60), None);
        }

        #[test]
        fn a_zero_default_disables_cooldown_without_a_hint() {
            assert_eq!(cooldown_for(429, None, "", 0), None);
        }
    }

    mod multimodal_routing {
        use super::*;

        fn t(provider: &str, model: &str) -> Target {
            Target { provider: provider.into(), model: model.into(), kind: "apikey".into() }
        }

        #[test]
        fn image_requests_skip_text_only_providers_when_vision_is_available() {
            let mut targets = vec![t("deepseek", "deepseek-chat"), t("kiro", "claude-sonnet"), t("claude", "claude-sonnet-4-5")];
            filter_multimodal_targets(&mut targets, MediaRequirements { has_image: true, ..Default::default() });
            assert_eq!(targets, vec![t("claude", "claude-sonnet-4-5")]);
        }

        #[test]
        fn video_requests_route_to_gemini() {
            let mut targets = vec![t("deepseek", "deepseek-chat"), t("claude", "claude-sonnet-4-5"), t("antigravity", "gemini-2.5-pro")];
            filter_multimodal_targets(&mut targets, MediaRequirements { has_video: true, ..Default::default() });
            assert_eq!(targets, vec![t("antigravity", "gemini-2.5-pro")]);
        }

        #[test]
        fn keeps_original_targets_if_none_match() {
            let mut targets = vec![t("deepseek", "deepseek-chat"), t("kiro", "claude-sonnet")];
            let orig = targets.clone();
            filter_multimodal_targets(&mut targets, MediaRequirements { has_video: true, ..Default::default() });
            assert_eq!(targets, orig);
        }
    }

    mod skills {
        use super::*;

        #[test]
        fn splits_model_and_plus_skills() {
            let (model, skills) = split_model_and_skills("auto+rust-expert+concise");
            assert_eq!(model, "auto");
            assert_eq!(skills, vec!["rust-expert", "concise"]);
        }

        #[test]
        fn injects_enabled_skills_and_skips_disabled_or_missing() {
            let dir = tempfile::tempdir().unwrap();
            let store = GatewayStore::new(dir.path()).unwrap();
            store.save_skill("rust", "Rust rules", "Use idiomatic Rust.", true).unwrap();
            store.save_skill("off", "Disabled", "Should not appear.", true).unwrap();
            store.toggle_skill("off").unwrap();
            let engine = GatewayEngine::new(store);

            let mut body = json!({ "messages": [{ "role": "user", "content": "Hi" }] });
            engine.inject_skills(&mut body, &["rust".into(), "off".into(), "missing".into(), "rust".into()]);
            let msgs = body["messages"].as_array().unwrap();
            assert_eq!(msgs.len(), 2);
            assert_eq!(msgs[0]["role"], "system");
            assert!(msgs[0]["content"].as_str().unwrap().contains("Use idiomatic Rust."));
            assert!(!msgs[0]["content"].as_str().unwrap().contains("Should not appear."));
        }
    }

    mod guardrails_tests {
        use super::super::guardrails::{redact_secrets_in_body, trim_context_messages};
        use super::*;

        #[test]
        fn redacts_leaked_tokens_in_user_messages() {
            let mut body = json!({
                "messages": [
                    { "role": "system", "content": "Keep sk-proj-1234567890abcdef" },
                    { "role": "user", "content": "My key is sk-proj-1234567890abcdef and ghp_1234567890abcdef" }
                ]
            });
            redact_secrets_in_body(&mut body);
            assert!(body["messages"][0]["content"].as_str().unwrap().contains("1234567890abcdef"));
            let user_txt = body["messages"][1]["content"].as_str().unwrap();
            assert!(user_txt.contains("sk-proj-[REDACTED]"), "{user_txt}");
            assert!(user_txt.contains("ghp_[REDACTED]"), "{user_txt}");
        }

        #[test]
        fn trims_old_messages_while_keeping_system_and_latest() {
            let mut body = json!({
                "messages": [
                    { "role": "system", "content": "sys" },
                    { "role": "user", "content": "old message 1 that is quite long" },
                    { "role": "assistant", "content": "old reply 1 that is also long" },
                    { "role": "user", "content": "latest" }
                ]
            });
            trim_context_messages(&mut body, 35);
            let msgs = body["messages"].as_array().unwrap();
            assert_eq!(msgs[0]["role"], "system");
            assert_eq!(msgs.last().unwrap()["content"], "latest");
            assert!(msgs.len() < 4);
        }
    }

    mod activity_tests {
        use super::*;

        #[test]
        fn tracks_subagent_lifecycle_and_rich_phases() {
            let hub = ActivityHub::default();
            let mut rx = hub.subscribe();
            let run = hub.create_run("sub_1".into(), Some("sess_1".into()), "Reviewer".into(), "auto".into());
            assert_eq!(run.phase, "queued");

            hub.emit_phase(
                "sub_1",
                Some("sess_1"),
                Some("Reviewer"),
                "calling_tool",
                "Calling tool `read_file`...",
                Some("oa/gpt-4o"),
                Some("read_file"),
            );
            let updated = hub.get_run("sub_1").unwrap();
            assert_eq!(updated.phase, "calling_tool");
            assert_eq!(updated.tool.as_deref(), Some("read_file"));

            hub.finish_run("sub_1", Some("LGTM".into()), None);
            assert_eq!(hub.get_run("sub_1").unwrap().phase, "completed");
            assert!(rx.try_recv().is_ok());
        }

        #[test]
        fn builds_ide_style_metadata_for_file_operations() {
            let (meta_read, msg_read) = build_tool_activity_meta(
                "mcp__fs__read_file",
                &json!({ "path": "crates/eay-gateway-core/Cargo.toml", "start_line": 1, "end_line": 29 }),
            );
            assert_eq!(meta_read.action.as_deref(), Some("Analyzed"));
            assert_eq!(meta_read.file.as_deref(), Some("Cargo.toml"));
            assert_eq!(meta_read.lines.as_deref(), Some("L1-29"));
            assert_eq!(msg_read, "Analyzed Cargo.toml #L1-29");

            let (meta_edit, msg_edit) = build_tool_activity_meta(
                "mcp__fs__edit_file",
                &json!({ "path": "src/models.rs", "old_string": "a", "new_string": "a\nb\nc" }),
            );
            assert_eq!(meta_edit.action.as_deref(), Some("Edited"));
            assert_eq!(meta_edit.added, Some(3));
            assert_eq!(meta_edit.removed, Some(1));
            assert_eq!(msg_edit, "Edited models.rs +3 -1");
        }
    }
}
