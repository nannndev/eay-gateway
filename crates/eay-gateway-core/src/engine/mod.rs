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
    /// The caller may start local MCP servers (a loopback request, or a parent run that could).
    /// An agent's own MCP list is ignored when this is false, so a remote key cannot start a process.
    pub mcp_allowed: bool,
    pub run_id: Option<String>,
    pub agent_label: Option<String>,
    /// Policy inherited from the agent that started this run (a subagent may never get more
    /// than its parent). Applied on top of the agent's and key's own policies.
    pub inherited_policy: Option<crate::mcp::ToolPolicy>,
    /// How many levels of `delegate` led to this run (0 = a request from a client).
    pub depth: u32,
    /// Saved roster from `x-eay-team`. Loaded once on the client request.
    pub team: Option<String>,
    /// Member names once the roster is loaded. `None` means no roster, so every saved agent is offered.
    /// A child run carries this so it cannot delegate outside the roster.
    pub team_members: Option<Vec<String>>,
    /// Project from `x-eay-project` (id `prj_…`). A key bound to a project overrides it; a child run
    /// inherits its parent's.
    pub project: Option<String>,
}

/// Who a usage row is billed to. Cloned into stream guards, so it owns its strings.
#[derive(Debug, Clone, Default)]
pub(crate) struct UsageTags {
    /// Saved agent that spoke, or a worker's role. Empty for a raw model call.
    pub agent: Option<String>,
    /// Roster id (`tm_…`).
    pub team: Option<String>,
    /// Project id (`prj_…`).
    pub project: Option<String>,
}

/// Roster for one request. `id` is empty until the header or the session has been resolved.
struct ResolvedTeam {
    id: Option<String>,
    members: Vec<String>,
}

/// How long a `delegate` call waits for its subagent.
const DELEGATE_WAIT_SECS: u64 = 600;

/// One policy at least as strict as every one of `policies`, handed to subagents so they never get
/// more than their parent: the strictest mode and approval, allow lists intersected (a pattern
/// survives only if every list has it), all denies, and the smallest subagent budget.
fn combine_policies(policies: &[&crate::mcp::ToolPolicy]) -> crate::mcp::ToolPolicy {
    let rank_mode = |m: Option<&str>| match m {
        Some("none") => 2,
        Some("read_only") => 1,
        _ => 0,
    };
    let rank_approval = |a: Option<&str>| match a {
        Some("manual") => 2,
        Some("manual_for_writes") => 1,
        _ => 0,
    };
    let mode = policies.iter().map(|p| p.mode.as_deref()).max_by_key(|m| rank_mode(*m)).flatten().map(String::from);
    let approval = policies.iter().map(|p| p.approval.as_deref()).max_by_key(|a| rank_approval(*a)).flatten().map(String::from);
    let allows: Vec<&Vec<String>> = policies.iter().filter_map(|p| p.allow.as_ref()).collect();
    // Intersect allow lists by pattern text; patterns that differ in form are kept conservatively.
    let allow = allows.split_first().map(|(first, rest)| first.iter().filter(|a| rest.iter().all(|r| r.contains(a))).cloned().collect());
    let mut deny: Vec<String> = policies.iter().flat_map(|p| p.deny.iter().cloned()).collect();
    deny.sort();
    deny.dedup();
    let max_subagents = policies.iter().filter_map(|p| p.max_subagents).min();
    crate::mcp::ToolPolicy { mode, allow, deny, approval, max_subagents }
}

/// Request servers first, then the agent's own list when local MCP is allowed. Duplicates are dropped.
fn merge_mcp(request: &[String], agent: &[String], allowed: bool) -> Vec<String> {
    let mut out = request.to_vec();
    if !allowed {
        return out;
    }
    for name in agent {
        let name = name.trim();
        if !name.is_empty() && !out.iter().any(|s| s == name) {
            out.push(name.to_string());
        }
    }
    out
}

fn clip_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max).collect::<String>())
    }
}

/// How many subagents this run may start. A saved agent delegates only when its own policy (or the
/// key) turns it on, and never above the parent. A generic helper with no policy of its own inherits
/// the parent's budget, so a lead can ask one worker to split the work further. Depth still caps the chain.
fn delegation_limit(
    depth: u32,
    agent: &crate::mcp::ToolPolicy,
    key: &crate::mcp::ToolPolicy,
    inherited: Option<&crate::mcp::ToolPolicy>,
    saved_agent: bool,
) -> u32 {
    if depth >= MAX_DELEGATE_DEPTH {
        return 0;
    }
    let own = crate::mcp::subagent_budget(&[agent, key]);
    let explicit = saved_agent || agent.max_subagents.is_some() || key.max_subagents.is_some();
    match inherited.and_then(|p| p.max_subagents) {
        Some(parent) if explicit => own.min(parent),
        Some(parent) => parent,
        None => own,
    }
}

/// Text of an OpenAI chat completion. Content may be a string or a list of parts.
fn message_text(reply: &Value) -> String {
    let content = &reply["choices"][0]["message"]["content"];
    if let Some(s) = content.as_str() {
        return s.to_string();
    }
    content
        .as_array()
        .map(|parts| parts.iter().filter_map(|p| p["text"].as_str().or_else(|| p.as_str())).collect::<Vec<_>>().join(""))
        .unwrap_or_default()
}

/// Saved agents the speaker may hand work to. With a roster, only its other members.
fn roster_agents(agents: &[(String, String)], current: &str, members: Option<&[String]>) -> Vec<(String, String)> {
    agents.iter().filter(|(name, _)| name != current && members.is_none_or(|ms| ms.iter().any(|m| m == name))).cloned().collect()
}

fn delegate_report(role: &str, agent: Option<&str>, body: &str) -> String {
    match agent.filter(|a| !a.is_empty()) {
        Some(a) => format!("[{role} · {a}]\n{body}"),
        None => format!("[{role}]\n{body}"),
    }
}

/// The client request, once it has delegated, so the run tree has a root to hang the team on.
/// Dropped without `settle` (the client left, or the request failed) cancels the team.
struct TeamRoot {
    engine: GatewayEngine,
    id: String,
    active: bool,
    settled: bool,
}

impl TeamRoot {
    fn settle_json(&mut self, reply: &Value) {
        if !self.active || self.settled {
            return;
        }
        self.settled = true;
        if let Some(r) = self.engine.activity.finish_run(&self.id, Some(message_text(reply)), None) {
            self.engine.store.save_subagent_run(&r);
        }
    }

    fn settle_chat(&mut self, reply: &ChatReply) {
        match reply {
            ChatReply::Json(v) => self.settle_json(v),
            ChatReply::Stream(_) if self.active && !self.settled => {
                self.settled = true;
                if let Some(r) = self.engine.activity.finish_run(&self.id, None, None) {
                    self.engine.store.save_subagent_run(&r);
                }
            }
            ChatReply::Stream(_) => {}
        }
    }
}

impl Drop for TeamRoot {
    fn drop(&mut self) {
        if !self.active || self.settled {
            return;
        }
        self.engine.cancel_subagent(&self.id);
    }
}

/// Cancels the subagent (and everything it started) if this is dropped while the run is still going.
/// That covers a client disconnect and a delegate wait that gives up.
struct StopOnDrop {
    engine: GatewayEngine,
    id: String,
}

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.engine.cancel_subagent(&self.id);
    }
}

/// Name of the gateway-provided tool that lets a model start subagents.
pub const DELEGATE_TOOL: &str = "eay_delegate";
/// Team thread for the lead and the workers it started. Other runs cannot see it.
pub const SAY_TOOL: &str = "eay_say";
/// Deepest chain of subagents a model may build with `delegate`.
pub const MAX_DELEGATE_DEPTH: u32 = 3;

/// The `eay_delegate` tool. `agents` are the saved agent profiles the model may hand work to
/// (name, description); they are listed so the model knows which specialists exist.
fn delegate_tool_schema(agents: &[(String, String)]) -> Value {
    let mut description = String::from(
        "Hand an independent subtask to a subagent and get its final answer back. Use it for work that can be done separately (research one topic, review one file). Call it several times in one reply to run subtasks at the same time. Give the subagent everything it needs in `task`; it cannot see this conversation, but it shares this run's team thread (eay_say) with you and the other workers.",
    );
    let mut props = json!({
        "role": { "type": "string", "description": "Short role, e.g. 'Test writer'" },
        "task": { "type": "string", "description": "Complete instructions for the subagent" },
        "model": { "type": "string", "description": "Optional model id; defaults to this request's model" }
    });
    if !agents.is_empty() {
        description.push_str(
            "\n\nSaved agents you can hand work to with `agent` (each brings its own model, skills, personality and tool limits):",
        );
        for (name, desc) in agents {
            description.push_str(&format!("\n- {name}{}", if desc.is_empty() { String::new() } else { format!(": {desc}") }));
        }
        let names: Vec<&str> = agents.iter().map(|(n, _)| n.as_str()).collect();
        props["agent"] = json!({ "type": "string", "enum": names, "description": "Optional saved agent to run the subtask as" });
    }
    json!({
        "type": "function",
        "function": {
            "name": DELEGATE_TOOL,
            "description": description,
            "parameters": { "type": "object", "properties": props, "required": ["role", "task"] }
        }
    })
}

fn say_tool_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": SAY_TOOL,
            "description": "Read this run's team thread, and optionally post a note the lead and the other workers in this run can see. Agents that are not part of this run cannot see it. Omit message to only read.",
            "parameters": {
                "type": "object",
                "properties": {
                    "message": { "type": "string", "description": "Note to post. Leave out to only read the thread." }
                }
            }
        }
    })
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
        tags: &UsageTags,
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
            agent: tags.agent.as_deref(),
            team: tags.team.as_deref(),
            project: tags.project.as_deref(),
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
            crate::translate::strip_gateway_fields(&mut b);
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
        // Embeddings, images and audio count toward the key's project too.
        let passthrough_tags = UsageTags { project: key.as_ref().and_then(|k| k.project.clone()), ..Default::default() };

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
                        &passthrough_tags,
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
                agent: None,
                team: None,
                project: passthrough_tags.project.as_deref(),
            });
            return Ok(PassthroughReply { status: status.as_u16(), content_type, body: resp_bytes });
        }

        Err(gateway_err(502, format!("No connected AI provider could respond: {last_error}")))
    }

    /// The project a request runs in. A key bound to a project always wins, so a key cannot reach
    /// another project by sending a header; otherwise `x-eay-project`, then the session's binding.
    /// The first request that names a project binds its session to it. An unknown id is an error.
    fn resolve_project(
        &self,
        key: Option<&VirtualKeyRecord>,
        opts: &ChatOptions,
        session_storage: Option<&str>,
    ) -> Result<Option<crate::store::ProjectRecord>, ChatError> {
        let from_key = key.and_then(|k| k.project.clone());
        let asked = opts.project.clone().filter(|p| !p.is_empty());
        if let (Some(bound), Some(asked)) = (&from_key, &asked) {
            if bound != asked {
                return Err(gateway_err(403, format!("This key belongs to project {bound}, not {asked}")));
            }
        }
        let id = from_key.or(asked).or_else(|| session_storage.and_then(|s| self.store.session_project(s)));
        let Some(id) = id else { return Ok(None) };
        let project = self.store.get_project(&id).ok_or_else(|| gateway_err(404, format!("Project '{id}' not found")))?;
        if let Some(sid) = session_storage {
            match self.store.session_project(sid) {
                Some(bound) if bound != project.id => {
                    return Err(gateway_err(409, format!("Session belongs to project {bound}; start a new session for {}", project.id)));
                }
                Some(_) => {}
                None => self.store.set_session_project(sid, &project.id).map_err(|e| gateway_err(400, e))?,
            }
        }
        Ok(Some(project))
    }

    /// Members of the roster for this request. `None` means every saved agent may be delegated to.
    /// A header binds the session; later requests on that session reuse it. Two sessions may name
    /// the same roster and still keep separate transcripts.
    fn resolve_team(&self, opts: &ChatOptions, session_storage: Option<&str>) -> Result<Option<ResolvedTeam>, ChatError> {
        if let Some(members) = &opts.team_members {
            return Ok(Some(ResolvedTeam { id: opts.team.clone().filter(|id| !id.is_empty()), members: members.clone() }));
        }
        let named = match &opts.team {
            Some(raw) => {
                let name = raw.trim();
                if name.is_empty() || name.len() > 64 || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
                    return Err(gateway_err(400, "Team id or name must use only letters, digits, '-' or '_'".into()));
                }
                Some(name.to_string())
            }
            None => session_storage.and_then(|id| self.store.session_team(id)),
        };
        let Some(token) = named else { return Ok(None) };
        let team = self.lookup_team(&token)?;
        // Store the id, even when the request named the team by its label.
        if opts.team.is_some() {
            if let Some(id) = session_storage {
                self.store.set_session_team(id, &team.id).map_err(|e| gateway_err(400, e))?;
            }
        }
        Ok(Some(ResolvedTeam { id: Some(team.id), members: team.members }))
    }

    /// `token` is a team id, or a name when exactly one team uses it.
    fn lookup_team(&self, token: &str) -> Result<crate::store::TeamRecord, ChatError> {
        if let Some(team) = self.store.get_team(token) {
            return Ok(team);
        }
        let same: Vec<_> = self.store.list_teams().into_iter().filter(|t| t.name == token).collect();
        match same.as_slice() {
            [team] => Ok(team.clone()),
            [] => Err(gateway_err(404, format!("Team '{token}' not found"))),
            _ => Err(gateway_err(400, format!("More than one team is named '{token}'. Send the team id."))),
        }
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
        // The agent profile this request runs as, with or without a personality.
        let mut running_agent: Option<String> = None;
        let mut agent_tool_policy = crate::mcp::ToolPolicy::default();
        let mut agent_mcp: Vec<String> = Vec::new();

        if let Some(agent_name) = base_model.strip_prefix("agent/") {
            let agent = self
                .store
                .get_agent(agent_name)
                .filter(|a| a.enabled)
                .ok_or_else(|| gateway_err(404, format!("Agent profile '{agent_name}' not found or disabled")))?;
            base_model = agent.model.clone();
            running_agent = Some(agent.name.clone());
            combined_skills.extend(agent.skills);
            agent_mcp = agent.mcp;
            agent_tool_policy = agent.tool_policy.clone();
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
        let project = self.resolve_project(key.as_ref(), &opts, session_key.as_deref())?;
        let roster = self.resolve_team(&opts, session_key.as_deref())?;
        let team_id = roster.as_ref().and_then(|t| t.id.clone());
        let team_members = roster.map(|t| t.members);
        if let (Some(name), Some(members)) = (running_agent.as_deref(), team_members.as_ref()) {
            if !members.iter().any(|m| m == name) {
                return Err(gateway_err(400, format!("Agent '{name}' is not on this team")));
            }
        }
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
        // Usage names the saved agent. A worker with no profile keeps its role. A raw model stays blank.
        let usage_agent = running_agent.clone().or(agent_label.clone()).filter(|name| !name.is_empty());
        let usage_tags = UsageTags { agent: usage_agent.clone(), team: team_id.clone(), project: project.as_ref().map(|p| p.id.clone()) };
        // The project's instructions come first, so an agent's personality and skills refine them.
        let project_root = project.as_ref().and_then(|p| p.root.clone());
        let project_text = project.as_ref().map(|p| {
            let mut text = p.instructions.trim().to_string();
            if let Some(root) = &project_root {
                let note = format!(
                    "You work in the project folder {root}. Use the {} tools (paths relative to that folder) to look at and change its files.",
                    crate::workspace::SERVER
                );
                text = if text.is_empty() { note } else { format!("{note}\n\n{text}") };
            }
            text
        });
        if let Some(text) = project_text.filter(|t| !t.is_empty()) {
            if let Some(msgs) = body.get_mut("messages").and_then(|m| m.as_array_mut()) {
                let name = project.as_ref().map(|p| p.name.as_str()).unwrap_or("");
                msgs.insert(0, json!({ "role": "system", "content": format!("[Project: {name}]\n{text}") }));
            }
        }
        if let Some((name, persona)) = agent_personality {
            if let Some(msgs) = body.get_mut("messages").and_then(|m| m.as_array_mut()) {
                let at = usize::from(msgs.first().and_then(|m| m["content"].as_str()).is_some_and(|c| c.starts_with("[Project: ")));
                msgs.insert(at, json!({ "role": "system", "content": format!("[Agent Personality: {name}]\n{persona}") }));
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
        // A subagent's events point at the run that started it. A client request points at its session.
        let parent_id = self.activity.get_run(&run_id).and_then(|r| r.parent_id).or_else(|| opts.session_id.clone());
        let mut team = TeamRoot { engine: self.clone(), id: run_id.clone(), active: false, settled: false };
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

        let mcp_servers = merge_mcp(&opts.mcp_servers, &agent_mcp, opts.mcp_allowed);
        let mcp_records = self.resolve_mcp_servers(&mcp_servers);
        // The agent's, the key's and the parent run's tool policies all apply; the strictest wins.
        let key_policy = key.as_ref().map(|k| k.tool_policy.clone()).unwrap_or_default();
        let inherited = opts.inherited_policy.clone().unwrap_or_default();
        let policies = [&agent_tool_policy, &key_policy, &inherited];
        // `delegate` is offered only when some policy turns it on, never past the depth limit, and a
        // subagent never gets a bigger budget than its parent had.
        let delegate_budget =
            delegation_limit(opts.depth, &agent_tool_policy, &key_policy, opts.inherited_policy.as_ref(), running_agent.is_some());
        // A worker can talk to its lead and siblings even when it is not allowed to delegate further.
        let in_team = opts.depth > 0 || delegate_budget > 0;
        if !mcp_records.is_empty() || in_team || project_root.is_some() {
            let mut merged_tools = body.get("tools").and_then(|t| t.as_array()).cloned().unwrap_or_default();
            let mut mcp_index = crate::mcp::McpToolIndex::default();
            if in_team {
                merged_tools.push(say_tool_schema());
            }
            if delegate_budget > 0 {
                // Offer saved agents other than the one running now (an agent can still delegate to itself by model).
                let current = running_agent.clone().unwrap_or_default();
                let all: Vec<(String, String)> =
                    self.store.list_agents().into_iter().filter(|a| a.enabled).map(|a| (a.name, a.description)).collect();
                let agents: Vec<(String, String)> = roster_agents(&all, &current, team_members.as_deref()).into_iter().take(30).collect();
                merged_tools.push(delegate_tool_schema(&agents));
            }
            let mut delegated: u32 = 0;
            // What a subagent started here may do: the strictest of everything above, one level deeper.
            let child_policy = combine_policies(&policies);
            for srv in &mcp_records {
                let (tools, index) = self.mcp_tools_cached(srv).await;
                for t in tools {
                    let shown = t["function"]["name"].as_str().unwrap_or("");
                    if index.get(shown).is_some_and(|r| crate::mcp::all_permit(&policies, r)) {
                        merged_tools.push(t);
                    }
                }
                mcp_index.extend(index);
            }
            if project_root.is_some() {
                let (tools, index) = crate::workspace::tools();
                for t in tools {
                    let shown = t["function"]["name"].as_str().unwrap_or("");
                    if index.get(shown).is_some_and(|r| crate::mcp::all_permit(&policies, r)) {
                        merged_tools.push(t);
                    }
                }
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
                            &usage_tags,
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
                        .filter(|tc| {
                            tc["function"]["name"]
                                .as_str()
                                .is_some_and(|n| n == DELEGATE_TOOL || n == SAY_TOOL || mcp_index.resolve(n).is_some())
                        })
                        .cloned()
                        .collect();
                    if mcp_calls.is_empty() {
                        team.settle_json(&reply);
                        if let Some(ref sid) = session_key {
                            self.append_session_turn(sid, &incoming_turns, &reply);
                        }
                        return Ok(if caller_stream { json_reply_to_sse(&reply) } else { ChatReply::Json(reply) });
                    }
                    if let Some(msgs) = body.get_mut("messages").and_then(|m| m.as_array_mut()) {
                        msgs.push(msg.clone());
                        // Tool messages are text-only in chat/completions; images follow as one user turn.
                        let mut tool_images: Vec<Value> = Vec::new();
                        // All `eay_delegate` calls in this turn run at the same time (the subagent slot
                        // pool still caps how many are active); results keep the model's call order.
                        let delegate_calls: Vec<(String, Value)> = mcp_calls
                            .iter()
                            .filter(|tc| tc["function"]["name"] == DELEGATE_TOOL)
                            .map(|tc| {
                                let args = serde_json::from_str(tc["function"]["arguments"].as_str().unwrap_or("{}")).unwrap_or(json!({}));
                                (tc["id"].as_str().unwrap_or("").to_string(), args)
                            })
                            .collect();
                        let mut delegate_outputs: HashMap<String, String> = HashMap::new();
                        let speaker = agent_label.clone().or_else(|| running_agent.clone()).unwrap_or_else(|| "agent".into());
                        let team_root = self.activity.team_root(&run_id);
                        // Notes go up before workers start, so a note in this same reply is already on the thread.
                        for tc in &mcp_calls {
                            if tc["function"]["name"] != SAY_TOOL {
                                continue;
                            }
                            let call_id = tc["id"].as_str().unwrap_or("").to_string();
                            let args: Value =
                                serde_json::from_str(tc["function"]["arguments"].as_str().unwrap_or("{}")).unwrap_or(json!({}));
                            let thread = match args["message"].as_str().map(str::trim).filter(|s| !s.is_empty()) {
                                Some(message) => self.activity.team_post(&team_root, &speaker, message),
                                None => self.activity.team_post(&team_root, &speaker, ""),
                            };
                            delegate_outputs.insert(call_id, thread);
                        }
                        let mut runs = Vec::new();
                        for (call_id, args) in &delegate_calls {
                            if delegated >= delegate_budget {
                                delegate_outputs.insert(
                                    call_id.clone(),
                                    format!("Subagent limit reached ({delegate_budget} per request). Do the rest yourself."),
                                );
                                continue;
                            }
                            delegated += 1;
                            let fut = self.run_delegate(
                                key.clone(),
                                args,
                                &req_model,
                                &run_id,
                                running_agent.as_deref().or(agent_label.as_deref()),
                                &child_policy,
                                opts.depth,
                                &mcp_servers,
                                opts.mcp_allowed,
                                team_members.as_deref(),
                                team_id.as_deref(),
                                usage_tags.project.as_deref(),
                            );
                            runs.push(async move { (call_id.clone(), fut.await) });
                        }
                        if !runs.is_empty() && self.activity.get_run(&run_id).is_none() {
                            let role = running_agent.clone().or_else(|| agent_label.clone()).unwrap_or_else(|| "request".into());
                            let model = running_agent.as_ref().map(|n| format!("agent/{n}")).unwrap_or_else(|| req_model.clone());
                            self.activity.create_run_with(
                                run_id.clone(),
                                opts.session_id.clone(),
                                role.clone(),
                                model,
                                None,
                                key.as_ref().map(|k| k.id.clone()),
                            );
                            self.activity.emit_phase(&run_id, None, Some(&role), "generating", format!("Running {role}"), None, None);
                            team.active = true;
                        }
                        delegate_outputs.extend(futures::future::join_all(runs).await);
                        // Cancelled while the workers ran: do not ask the model for a wrap-up turn.
                        if self.activity.get_run(&run_id).is_some_and(|r| r.phase == "cancelled") {
                            return Err(gateway_err(409, "This run was cancelled.".into()));
                        }
                        for tc in mcp_calls {
                            let call_id = tc["id"].as_str().unwrap_or("");
                            let full_name = tc["function"]["name"].as_str().unwrap_or("");
                            let args: Value =
                                serde_json::from_str(tc["function"]["arguments"].as_str().unwrap_or("{}")).unwrap_or(json!({}));
                            if full_name == DELEGATE_TOOL || full_name == SAY_TOOL {
                                let output = delegate_outputs.remove(call_id).unwrap_or_default();
                                msgs.push(json!({ "role": "tool", "tool_call_id": call_id, "content": output }));
                                continue;
                            }
                            // Re-check at run time: a model can name a tool it was never offered.
                            let tool_ref = mcp_index.get(full_name).filter(|r| crate::mcp::all_permit(&policies, r)).cloned();
                            let approved = match &tool_ref {
                                Some(r) if crate::mcp::any_needs_approval(&policies, r.read_only) => {
                                    self.activity
                                        .request_approval(&run_id, parent_id.as_deref(), agent_label.as_deref(), full_name, args.clone())
                                        .await
                                }
                                _ => true,
                            };
                            let output = if tool_ref.is_none() {
                                format!("Tool `{full_name}` is not allowed for this agent or key.")
                            } else if !approved {
                                format!(
                                    "The user did not approve running `{full_name}`. Do not retry it; continue without it or ask the user."
                                )
                            } else if let Some((srv_name, tool_name)) = mcp_index.resolve(full_name) {
                                let (mut meta, msg_label) = build_tool_activity_meta(full_name, &args);
                                let t_tool = std::time::Instant::now();
                                let res = match mcp_records.iter().find(|s| s.name == srv_name) {
                                    _ if srv_name == crate::workspace::SERVER => match &project_root {
                                        Some(root) => crate::workspace::call(root.clone(), tool_name.to_string(), args).await,
                                        None => "This project has no folder.".into(),
                                    },
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
            .execute_chat_turn(
                key.as_ref(),
                &body,
                &req_model,
                &run_id,
                parent_id.as_deref(),
                agent_label.as_deref(),
                media_req,
                &usage_tags,
            )
            .await?;
        team.settle_chat(&reply);
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
            // Memory keeps what was said, not the model's private reasoning (it is large and
            // some providers reject it in later turns).
            let mut m = assistant_msg.clone();
            if let Some(o) = m.as_object_mut() {
                o.remove("reasoning_content");
                o.remove("thinking_blocks");
            }
            hist.push(m);
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
        tags: &UsageTags,
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
                    .handle_upstream_error(
                        upstream,
                        &target,
                        acc.as_ref(),
                        &cool,
                        key,
                        &key_name,
                        t0,
                        wants_stream,
                        settings.cooldown_secs,
                        tags,
                    )
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
                tags: tags.clone(),
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
                Ok(ChatReply::Json(v)) => engine.activity.finish_run(&task_id, Some(message_text(&v)), None),
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

    /// Runs one `delegate` tool call: starts a subagent one level deeper, under the same key, with
    /// at most the parent's permissions, waits for it, and returns its answer as the tool result.
    #[allow(clippy::too_many_arguments)]
    async fn run_delegate(
        &self,
        key: Option<VirtualKeyRecord>,
        args: &Value,
        default_model: &str,
        parent_run: &str,
        parent_agent: Option<&str>,
        child_policy: &crate::mcp::ToolPolicy,
        depth: u32,
        mcp_servers: &[String],
        mcp_allowed: bool,
        team_members: Option<&[String]>,
        team_id: Option<&str>,
        project: Option<&str>,
    ) -> String {
        let task = args["task"].as_str().unwrap_or("").trim();
        if task.is_empty() {
            return "delegate needs a non-empty `task`.".into();
        }
        let role = args["role"].as_str().filter(|r| !r.trim().is_empty()).unwrap_or("Subagent").to_string();
        let agent_name = args["agent"].as_str().map(str::trim).filter(|a| !a.is_empty()).map(str::to_string);
        if let Some(agent) = agent_name.as_deref() {
            if let Some(members) = team_members {
                if !members.iter().any(|m| m == agent) {
                    return delegate_report(&role, Some(agent), &format!("Agent '{agent}' is not on this team."));
                }
            }
        }
        // A saved agent brings its own model, skills, personality and tool policy; its policy still
        // applies on top of the parent's, so it can never get more than the parent had.
        let model = match agent_name.as_deref() {
            Some(agent) => match self.store.get_agent(agent).filter(|a| a.enabled) {
                Some(_) => format!("agent/{agent}"),
                None => return delegate_report(&role, agent_name.as_deref(), &format!("No enabled agent named '{agent}'.")),
            },
            None => args["model"].as_str().filter(|m| !m.trim().is_empty()).unwrap_or(default_model).to_string(),
        };
        let opts = ChatOptions {
            mcp_servers: mcp_servers.to_vec(),
            mcp_allowed,
            inherited_policy: Some(child_policy.clone()),
            depth: depth + 1,
            team: team_id.filter(|id| !id.is_empty()).map(str::to_string),
            team_members: team_members.map(|m| m.to_vec()),
            project: project.map(str::to_string),
            ..Default::default()
        };
        let root = self.activity.team_root(parent_run);
        let thread = self.activity.team_post(&root, &role, &format!("started: {}", clip_chars(task, 300)));
        let parent = parent_agent.unwrap_or("the lead");
        let system = Some(format!(
            "You are a subagent started by {parent}. You share a team thread with the lead and the other workers in this run. Nobody outside this run can read it.\n\nTeam thread:\n{thread}\n\nPost a finding with eay_say, then reply with your result."
        ));
        let run = self.spawn_subagent(key, role.clone(), model, task.to_string(), system, Some(parent_run.to_string()), opts);
        // Dropped while still waiting (client disconnect) stops this worker and anyone it started.
        let stop = StopOnDrop { engine: self.clone(), id: run.id.clone() };
        let waited = self.activity.wait_run(&run.id, std::time::Duration::from_secs(DELEGATE_WAIT_SECS)).await;
        let body = match &waited {
            Some(r) if r.phase == "completed" => r.output.clone().unwrap_or_default(),
            Some(r) if r.is_finished() => format!("Subagent '{role}' {}: {}", r.phase, r.error.clone().unwrap_or_default()),
            _ => format!("Subagent '{role}' did not finish within {DELEGATE_WAIT_SECS}s and was stopped."),
        };
        drop(stop);
        let _ = self.activity.team_post(&root, &role, &format!("finished: {}", clip_chars(&body, 400)));
        delegate_report(&role, agent_name.as_deref(), &body)
    }

    /// Stops a run and every run under it, and saves the ones that were still going.
    pub fn cancel_subagent(&self, id: &str) -> bool {
        let stopped = self.activity.cancel_tree(id);
        let mut any = !stopped.is_empty();
        for id in &stopped {
            if let Some(r) = self.activity.get_run(id) {
                self.store.save_subagent_run(&r);
            }
        }
        // A client request that delegated has a tree root but no task. Mark that root too.
        if let Some(r) = self.activity.mark_cancelled(id) {
            self.store.save_subagent_run(&r);
            any = true;
        }
        any
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
        async fn cancelling_a_parent_stops_its_children() {
            let (e, _d) = engine();
            let _held = e.activity.slots.clone().acquire_many_owned(activity::MAX_CONCURRENT_SUBAGENTS as u32).await.unwrap();
            let parent = e.spawn_subagent(None, "lead".into(), "auto".into(), "hi".into(), None, None, ChatOptions::default());
            let child =
                e.spawn_subagent(None, "worker".into(), "auto".into(), "hi".into(), None, Some(parent.id.clone()), ChatOptions::default());
            assert!(e.cancel_subagent(&parent.id));
            assert_eq!(e.activity.get_run(&parent.id).unwrap().phase, "cancelled");
            assert_eq!(e.activity.get_run(&child.id).unwrap().phase, "cancelled");
            let saved = e.store.list_subagent_runs(10);
            assert!(saved.iter().any(|r| r.id == child.id && r.phase == "cancelled"));
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
                agent: None,
                team: None,
                project: None,
            });
        }
        assert!(e.admit_rate(Some(&k)).is_ok());
        assert_eq!(e.admit_rate(Some(&k)).unwrap_err().status, 429);
        assert!(e.admit_rate(None).is_ok());
    }

    #[test]
    fn a_child_policy_is_at_least_as_strict_as_every_parent_policy() {
        use crate::mcp::{McpToolRef, ToolPolicy};
        let agent = ToolPolicy { allow: Some(vec!["fs/*".into(), "github/*".into()]), max_subagents: Some(5), ..Default::default() };
        let key = ToolPolicy {
            mode: Some("read_only".into()),
            deny: vec!["secret".into()],
            approval: Some("manual_for_writes".into()),
            allow: Some(vec!["fs/*".into()]),
            max_subagents: Some(2),
        };
        let child = combine_policies(&[&agent, &key]);
        assert_eq!(child.mode.as_deref(), Some("read_only"));
        assert_eq!(child.approval.as_deref(), Some("manual_for_writes"));
        assert_eq!(child.allow, Some(vec!["fs/*".to_string()]), "allow lists intersect");
        assert_eq!(child.deny, ["secret"]);
        assert_eq!(child.max_subagents, Some(2));
        // Whatever the parents forbid, the child forbids.
        let gh = McpToolRef { server: "github".into(), tool: "x".into(), read_only: true };
        assert!(!key.permits(&gh) && !child.permits(&gh));
        assert_eq!(combine_policies(&[&ToolPolicy::default()]), ToolPolicy::default());
    }

    #[test]
    fn a_saved_agent_does_not_inherit_a_budget_it_did_not_set() {
        use crate::mcp::ToolPolicy;
        let parent = ToolPolicy { max_subagents: Some(5), ..Default::default() };
        let quiet = ToolPolicy::default();
        let off = ToolPolicy { max_subagents: Some(0), ..Default::default() };
        // A specialist with no budget stays quiet, even when the lead may delegate.
        assert_eq!(delegation_limit(0, &quiet, &quiet, Some(&parent), true), 0);
        assert_eq!(delegation_limit(0, &off, &quiet, Some(&parent), true), 0);
        // It may lower the parent's budget, never raise it.
        let capped = ToolPolicy { max_subagents: Some(2), ..Default::default() };
        let greedy = ToolPolicy { max_subagents: Some(9), ..Default::default() };
        assert_eq!(delegation_limit(0, &capped, &quiet, Some(&parent), true), 2);
        assert_eq!(delegation_limit(0, &greedy, &quiet, Some(&parent), true), 5);
        // A generic helper inherits, so one worker can split work further.
        assert_eq!(delegation_limit(1, &quiet, &quiet, Some(&parent), false), 5);
        // An explicit 0 on the key stops that too.
        assert_eq!(delegation_limit(1, &quiet, &off, Some(&parent), false), 0);
        assert_eq!(delegation_limit(MAX_DELEGATE_DEPTH, &capped, &quiet, Some(&parent), false), 0);
        assert_eq!(delegation_limit(0, &parent, &quiet, None, true), 5);
    }

    #[test]
    fn an_agents_mcp_list_is_kept_and_only_merged_when_local_mcp_is_allowed() {
        use crate::mcp::ToolPolicy;
        let (e, _d) = engine();
        let saved = e
            .store
            .save_agent(
                "tester",
                "Writes tests",
                "auto",
                &[],
                "",
                None,
                None,
                &ToolPolicy::default(),
                true,
                &["filesystem".into(), "filesystem".into()],
            )
            .unwrap();
        assert_eq!(saved.mcp, ["filesystem"]);
        assert_eq!(e.store.get_agent("tester").unwrap().mcp, ["filesystem"]);
        assert!(e.store.save_agent("bad", "", "auto", &[], "", None, None, &ToolPolicy::default(), true, &["no spaces".into()]).is_err());
        assert_eq!(merge_mcp(&[], &["filesystem".into()], true), ["filesystem"]);
        assert_eq!(merge_mcp(&["github".into()], &["filesystem".into()], true), ["github", "filesystem"]);
        assert!(merge_mcp(&[], &["filesystem".into()], false).is_empty());
    }

    #[test]
    fn a_team_thread_stays_inside_one_run() {
        let (e, _d) = engine();
        let hub = &e.activity;
        hub.create_run_with("req_1".into(), None, "lead".into(), "auto".into(), None, None);
        hub.create_run_with("sub_a".into(), None, "a".into(), "auto".into(), Some("req_1".into()), None);
        hub.create_run_with("sub_b".into(), None, "b".into(), "auto".into(), Some("sub_a".into()), None);
        assert_eq!(hub.team_root("sub_b"), "req_1");
        assert_eq!(hub.team_post("req_1", "lead", ""), "(no notes yet)");
        let thread = hub.team_post("req_1", "Worker a", "the bug is in auth.rs");
        assert!(thread.contains("[Worker a] the bug is in auth.rs"), "{thread}");
        assert_eq!(hub.team_post("req_other", "", ""), "(no notes yet)");
    }

    #[test]
    fn message_text_reads_a_string_or_text_parts() {
        assert_eq!(message_text(&json!({"choices":[{"message":{"content":"hi"}}]})), "hi");
        let parts = json!({"choices":[{"message":{"content":[{"type":"text","text":"a"},{"type":"text","text":"b"}]}}]});
        assert_eq!(message_text(&parts), "ab");
        assert_eq!(delegate_report("Worker a", None, "done"), "[Worker a]\ndone");
        assert_eq!(delegate_report("Worker c", Some("tester"), "done"), "[Worker c · tester]\ndone");
    }

    #[test]
    fn a_saved_team_is_a_roster_and_each_session_keeps_its_own_chat() {
        use crate::mcp::ToolPolicy;
        let (e, _d) = engine();
        let quiet = ToolPolicy::default();
        for name in ["lead", "frontend", "outsider"] {
            e.store.save_agent(name, "", "auto", &[], "", None, None, &quiet, true, &[]).unwrap();
        }
        assert!(e.store.save_team(None, "app", "", &["nope".into()]).is_err());
        let team = e.store.save_team(None, "app", "frontend", &["lead".into(), "frontend".into(), "lead".into()]).unwrap();
        assert!(team.id.starts_with("tm_"), "{}", team.id);
        assert_eq!(team.lead, "frontend");
        assert_eq!(team.members, ["lead", "frontend"]);
        let renamed = e.store.save_team(Some(&team.id), "renamed", "frontend", &team.members).unwrap();
        assert_eq!(renamed.id, team.id);
        let other = e.store.save_team(None, "renamed", "lead", &["lead".into()]).unwrap();
        assert_ne!(other.id, team.id, "two teams may share a label");

        e.store.set_session_team("local:s1", &team.id).unwrap();
        e.store.set_session_team("local:s2", &team.id).unwrap();
        e.store.save_session_messages("local:s1", &[json!({"role":"user","content":"one"})]).unwrap();
        e.store.save_session_messages("local:s2", &[json!({"role":"user","content":"two"})]).unwrap();
        assert_eq!(e.store.session_team("local:s1").as_deref(), Some(team.id.as_str()));
        assert_eq!(e.store.session_team("local:s2").as_deref(), Some(team.id.as_str()));
        assert_eq!(e.store.get_session_messages("local:s1")[0]["content"], "one");
        assert_eq!(e.store.get_session_messages("local:s2")[0]["content"], "two");
        assert!(e.store.delete_session("local:s1").unwrap());
        assert_eq!(e.lookup_team(&team.id).unwrap().name, "renamed", "forgetting a session does not delete the roster");
        assert!(e.lookup_team("renamed").is_err(), "a shared label is not an address");
        assert!(e.store.session_team("local:s1").is_none());

        let all = [("lead".into(), String::new()), ("frontend".into(), String::new()), ("outsider".into(), String::new())];
        assert_eq!(roster_agents(&all, "lead", Some(&team.members)), vec![("frontend".into(), String::new())]);
        assert_eq!(roster_agents(&all, "lead", None).len(), 2);
    }

    #[tokio::test]
    async fn an_unanswered_approval_counts_as_denied() {
        let (e, _d) = engine();
        let approved = e.activity.request_approval_for("r", None, None, "t", json!({}), std::time::Duration::from_millis(50)).await;
        assert!(!approved);
        assert!(e.activity.list_approvals().is_empty(), "expired approvals are removed");
    }

    #[test]
    fn the_run_tree_nests_by_parent_and_treats_unknown_parents_as_roots() {
        let (e, _d) = engine();
        let h = &e.activity;
        h.create_run_with("a".into(), None, "lead".into(), "auto".into(), Some("req_1".into()), None);
        h.create_run_with("b".into(), None, "child 1".into(), "auto".into(), Some("a".into()), None);
        h.create_run_with("c".into(), None, "child 2".into(), "auto".into(), Some("a".into()), None);
        h.create_run_with("d".into(), None, "grandchild".into(), "auto".into(), Some("b".into()), None);
        h.create_run_with("z".into(), None, "other".into(), "auto".into(), None, None);
        let tree = h.run_tree();
        assert_eq!(tree.len(), 2, "a (parent is a request) and z are roots");
        let a = tree.iter().find(|n| n["id"] == "a").unwrap();
        let kids: Vec<&str> = a["children"].as_array().unwrap().iter().map(|c| c["id"].as_str().unwrap()).collect();
        assert_eq!(kids, ["b", "c"], "children in start order");
        assert_eq!(a["children"][0]["children"][0]["id"], "d");
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
