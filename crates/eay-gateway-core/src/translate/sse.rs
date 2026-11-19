use super::codex::CodexStreamTranslator;
use super::now_secs;
use serde_json::{json, Value};
use std::collections::HashMap;
use uuid::Uuid;

/// Translates a provider's SSE events into OpenAI chat.completion.chunk frames.
pub struct SseToOpenAi {
    pub base: CodexStreamTranslator,
    kind: &'static str,
    claude_tools: HashMap<u64, usize>,
    tool_count: usize,
    finished: bool,
    /// (input, output) tokens reported by the upstream, if any. Input includes cached tokens.
    pub usage: (i64, i64),
    /// Input tokens served from the provider's prompt cache (part of `usage.0`).
    pub cached: i64,
    cc_tools: HashMap<String, usize>,
    cc_finish: Option<String>,
}

impl SseToOpenAi {
    pub fn new(kind: &'static str, public_model: String) -> Self {
        Self {
            base: CodexStreamTranslator::new(public_model),
            kind,
            claude_tools: HashMap::new(),
            tool_count: 0,
            finished: false,
            usage: (0, 0),
            cached: 0,
            cc_tools: HashMap::new(),
            cc_finish: None,
        }
    }

    pub fn is_kiro(&self) -> bool {
        self.kind == "kiro"
    }

    pub fn kind_passthrough(&self) -> bool {
        self.kind == "openai"
    }

    fn tool_frame(&self, index: usize, id: Option<&str>, name: Option<&str>, args: &str) -> String {
        let mut call = json!({ "index": index, "function": { "arguments": args } });
        if let (Some(id), Some(name)) = (id, name) {
            call["id"] = json!(id);
            call["type"] = json!("function");
            call["function"]["name"] = json!(name);
        }
        self.base.chunk_data(json!({ "tool_calls": [call] }), None)
    }

    fn finish(&mut self, reason: &str) -> Vec<String> {
        if self.finished {
            return vec![];
        }
        self.finished = true;
        let reason = if self.tool_count > 0 { "tool_calls" } else { reason };
        vec![self.base.chunk_data(json!({}), Some(reason)), "data: [DONE]\n\n".to_string()]
    }

    /// Err carries an upstream error event.
    pub fn feed(&mut self, ev: &Value) -> Result<Vec<String>, String> {
        let mut out = Vec::new();
        if self.kind == "openai" {
            // Pass-through; just note usage (sent when stream_options.include_usage is set).
            if let Some(u) = ev.get("usage").filter(|u| u.is_object()) {
                self.usage = (u["prompt_tokens"].as_i64().unwrap_or(0), u["completion_tokens"].as_i64().unwrap_or(0));
                self.cached = u["prompt_tokens_details"]["cached_tokens"].as_i64().unwrap_or(0);
            }
            if let Some(e) = ev.get("error") {
                return Err(e["message"].as_str().unwrap_or("Upstream error").to_string());
            }
            out.push(format!("data: {}\n\n", ev));
        } else if self.kind == "codex" {
            match ev["type"].as_str().unwrap_or("") {
                "response.completed" => {
                    let u = &ev["response"]["usage"];
                    self.usage = (u["input_tokens"].as_i64().unwrap_or(0), u["output_tokens"].as_i64().unwrap_or(0));
                    self.cached = u["input_tokens_details"]["cached_tokens"].as_i64().unwrap_or(0);
                    self.tool_count = self.base.tool_calls.len();
                    let reason = if ev["response"]["incomplete_details"]["reason"] == "max_output_tokens" { "length" } else { "stop" };
                    out.extend(self.finish(reason));
                }
                "response.failed" | "error" => {
                    let e = ev["response"]["error"]["message"].as_str().or(ev["message"].as_str()).or(ev["error"]["message"].as_str());
                    return Err(e.unwrap_or("Codex error").to_string());
                }
                _ => out.extend(self.base.feed(ev)),
            }
        } else if self.kind == "commandcode" {
            match ev["type"].as_str().unwrap_or("") {
                "text-delta" => {
                    if let Some(t) = ev["text"].as_str().or(ev["delta"].as_str()).filter(|t| !t.is_empty()) {
                        out.push(self.base.chunk_data(json!({ "content": t }), None));
                    }
                }
                "reasoning-delta" => {
                    if let Some(t) = ev["text"].as_str().filter(|t| !t.is_empty()) {
                        out.push(self.base.chunk_data(json!({ "reasoning_content": t }), None));
                    }
                }
                "tool-input-start" | "tool-call" => {
                    let id = ev["id"].as_str().or(ev["toolCallId"].as_str()).unwrap_or("").to_string();
                    if !self.cc_tools.contains_key(&id) {
                        let idx = self.tool_count;
                        self.tool_count += 1;
                        self.cc_tools.insert(id.clone(), idx);
                        // A whole tool-call carries its input; a streamed one sends it as deltas.
                        let args = if ev["type"] == "tool-call" {
                            ev["input"].as_str().map(String::from).unwrap_or_else(|| ev["input"].to_string())
                        } else {
                            String::new()
                        };
                        out.push(self.tool_frame(idx, Some(&id), ev["toolName"].as_str(), &args));
                    }
                }
                "tool-input-delta" => {
                    let id = ev["id"].as_str().or(ev["toolCallId"].as_str()).unwrap_or("");
                    if let Some(&idx) = self.cc_tools.get(id) {
                        out.push(self.tool_frame(idx, None, None, ev["delta"].as_str().or(ev["inputTextDelta"].as_str()).unwrap_or("")));
                    }
                }
                "finish-step" | "finish" => {
                    let u = if ev["type"] == "finish" && ev["totalUsage"].is_object() { &ev["totalUsage"] } else { &ev["usage"] };
                    if u.is_object() {
                        self.usage = (u["inputTokens"].as_i64().unwrap_or(0), u["outputTokens"].as_i64().unwrap_or(0));
                        self.cached = u["inputTokenDetails"]["cacheReadTokens"].as_i64().unwrap_or(0);
                    }
                    if let Some(r) = ev["finishReason"].as_str() {
                        self.cc_finish = Some(r.to_string());
                    }
                    if ev["type"] == "finish" {
                        let reason = match self.cc_finish.as_deref().unwrap_or("stop") {
                            "length" => "length",
                            "content-filter" => "content_filter",
                            _ => "stop", // tool calls are detected by finish() itself
                        };
                        out.extend(self.finish(reason));
                    }
                }
                "error" => {
                    let e = &ev["error"];
                    let msg = e
                        .as_str()
                        .map(String::from)
                        .or_else(|| e["message"].as_str().map(String::from))
                        .or_else(|| ev["message"].as_str().map(String::from))
                        .unwrap_or_else(|| e.to_string());
                    return Err(format!("Command Code: {msg}"));
                }
                _ => {}
            }
        } else if self.kind == "claude" {
            match ev["type"].as_str().unwrap_or("") {
                "message_start" => {
                    let u = &ev["message"]["usage"];
                    self.cached = u["cache_read_input_tokens"].as_i64().unwrap_or(0);
                    self.usage.0 =
                        u["input_tokens"].as_i64().unwrap_or(0) + self.cached + u["cache_creation_input_tokens"].as_i64().unwrap_or(0);
                }
                "content_block_start" if ev["content_block"]["type"] == "tool_use" => {
                    let idx = self.tool_count;
                    self.claude_tools.insert(ev["index"].as_u64().unwrap_or(0), idx);
                    self.tool_count += 1;
                    out.push(self.tool_frame(idx, ev["content_block"]["id"].as_str(), ev["content_block"]["name"].as_str(), ""));
                }
                // Thinking travels as `reasoning_content` plus `thinking_signature`, so a Claude
                // client can send the signed block back with its tool results.
                "content_block_start" if ev["content_block"]["type"] == "redacted_thinking" => {
                    if let Some(data) = ev["content_block"]["data"].as_str() {
                        out.push(self.base.chunk_data(json!({ "redacted_thinking": data }), None));
                    }
                }
                "content_block_delta" => {
                    let d = &ev["delta"];
                    if d["type"] == "thinking_delta" {
                        if let Some(t) = d["thinking"].as_str().filter(|t| !t.is_empty()) {
                            out.push(self.base.chunk_data(json!({ "reasoning_content": t }), None));
                        }
                    } else if d["type"] == "signature_delta" {
                        if let Some(sig) = d["signature"].as_str().filter(|t| !t.is_empty()) {
                            out.push(self.base.chunk_data(json!({ "thinking_signature": sig }), None));
                        }
                    } else if d["type"] == "text_delta" {
                        if let Some(t) = d["text"].as_str().filter(|t| !t.is_empty()) {
                            out.push(self.base.chunk_data(json!({ "content": t }), None));
                        }
                    } else if d["type"] == "input_json_delta" {
                        if let Some(&idx) = self.claude_tools.get(&ev["index"].as_u64().unwrap_or(0)) {
                            out.push(self.tool_frame(idx, None, None, d["partial_json"].as_str().unwrap_or("")));
                        }
                    }
                }
                "message_delta" => {
                    if let Some(o) = ev["usage"]["output_tokens"].as_i64() {
                        self.usage.1 = o;
                    }
                    if let Some(r) = ev["delta"]["stop_reason"].as_str() {
                        out.extend(self.finish(if r == "max_tokens" { "length" } else { "stop" }));
                    }
                }
                "message_stop" => out.extend(self.finish("stop")),
                "error" => return Err(ev["error"]["message"].as_str().unwrap_or("Claude error").to_string()),
                _ => {}
            }
        } else {
            // gemini: events are {response:{candidates:[...]}}; function calls arrive whole.
            let r = ev.get("response").unwrap_or(ev);
            if let Some(u) = r.get("usageMetadata") {
                self.usage = (
                    u["promptTokenCount"].as_i64().unwrap_or(0),
                    u["candidatesTokenCount"].as_i64().unwrap_or(0) + u["thoughtsTokenCount"].as_i64().unwrap_or(0),
                );
                self.cached = u["cachedContentTokenCount"].as_i64().unwrap_or(0);
            }
            let cand = &r["candidates"][0];
            for part in cand["content"]["parts"].as_array().into_iter().flatten() {
                if part["thought"] == true {
                    if let Some(t) = part["text"].as_str().filter(|t| !t.is_empty()) {
                        out.push(self.base.chunk_data(json!({ "reasoning_content": t }), None));
                    }
                    continue;
                }
                if let Some(t) = part["text"].as_str().filter(|t| !t.is_empty()) {
                    out.push(self.base.chunk_data(json!({ "content": t }), None));
                }
                if let Some(fc) = part.get("functionCall") {
                    let idx = self.tool_count;
                    self.tool_count += 1;
                    let id = format!("call_{}_{}", idx, Uuid::new_v4().simple());
                    let args = serde_json::to_string(fc.get("args").unwrap_or(&json!({}))).unwrap_or_default();
                    out.push(self.tool_frame(idx, Some(&id), fc["name"].as_str(), &args));
                }
            }
            if let Some(fr) = cand["finishReason"].as_str() {
                out.extend(self.finish(if fr == "MAX_TOKENS" { "length" } else { "stop" }));
            }
            if let Some(e) = ev.get("error") {
                return Err(e["message"].as_str().unwrap_or("Gemini error").to_string());
            }
        }
        Ok(out)
    }

    /// Called when the upstream stream ends, in case it never sent a stop event.
    pub fn end(&mut self) -> Vec<String> {
        if self.kind == "openai" {
            // Upstream already sent its own finish chunk; only the [DONE] line (not JSON) was dropped.
            if self.finished {
                return vec![];
            }
            self.finished = true;
            return vec!["data: [DONE]\n\n".to_string()];
        }
        self.finish("stop")
    }
}

/// Folds OpenAI chat.completion.chunk SSE frames into one non-streaming chat.completion.
pub fn aggregate_sse(sse: &str, usage: (i64, i64)) -> Value {
    let (mut id, mut model, mut created) = (Value::Null, Value::Null, json!(now_secs()));
    let mut content = String::new();
    let mut reasoning = String::new();
    let mut signature = String::new();
    let mut redacted: Vec<Value> = Vec::new();
    let mut finish = Value::Null;
    let mut tools: Vec<Value> = Vec::new();
    for line in sse.lines() {
        let Some(data) = line.trim().strip_prefix("data:") else { continue };
        let Ok(ev) = serde_json::from_str::<Value>(data.trim()) else { continue };
        if id.is_null() {
            id = ev["id"].clone();
            model = ev["model"].clone();
            created = ev["created"].clone();
        }
        let ch = &ev["choices"][0];
        if let Some(t) = ch["delta"]["content"].as_str() {
            content.push_str(t);
        }
        if let Some(t) = ch["delta"]["reasoning_content"].as_str() {
            reasoning.push_str(t);
        }
        if let Some(t) = ch["delta"]["thinking_signature"].as_str() {
            signature.push_str(t);
        }
        if let Some(d) = ch["delta"]["redacted_thinking"].as_str() {
            redacted.push(json!({ "type": "redacted_thinking", "data": d }));
        }
        if !ch["finish_reason"].is_null() {
            finish = ch["finish_reason"].clone();
        }
        for tc in ch["delta"]["tool_calls"].as_array().into_iter().flatten() {
            let i = tc["index"].as_u64().unwrap_or(0) as usize;
            while tools.len() <= i {
                tools.push(json!({ "id": "", "type": "function", "function": { "name": "", "arguments": "" } }));
            }
            if let Some(v) = tc["id"].as_str() {
                tools[i]["id"] = json!(v);
            }
            if let Some(v) = tc["function"]["name"].as_str() {
                tools[i]["function"]["name"] = json!(v);
            }
            if let Some(v) = tc["function"]["arguments"].as_str() {
                let cur = tools[i]["function"]["arguments"].as_str().unwrap_or("").to_string();
                tools[i]["function"]["arguments"] = json!(cur + v);
            }
        }
    }
    let mut message = json!({ "role": "assistant", "content": content });
    if !tools.is_empty() {
        message["tool_calls"] = json!(tools);
    }
    if !reasoning.is_empty() {
        message["reasoning_content"] = json!(reasoning);
    }
    let mut blocks = Vec::new();
    if !signature.is_empty() {
        blocks.push(json!({ "type": "thinking", "thinking": reasoning, "signature": signature }));
    }
    blocks.extend(redacted);
    if !blocks.is_empty() {
        message["thinking_blocks"] = json!(blocks);
    }
    json!({
        "id": id, "object": "chat.completion", "created": created, "model": model,
        "choices": [{ "index": 0, "message": message, "finish_reason": if finish.is_null() { json!("stop") } else { finish } }],
        "usage": { "prompt_tokens": usage.0, "completion_tokens": usage.1, "total_tokens": usage.0 + usage.1 },
    })
}
