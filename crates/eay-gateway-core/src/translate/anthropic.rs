use super::extract_text;
use super::media::{extract_file_part, extract_image_part, infer_mime, parse_data_url};
use serde_json::{json, Value};
use uuid::Uuid;

pub fn openai_to_anthropic(body: &Value, model: &str) -> Value {
    let mut system_parts = Vec::new();
    let mut turns = Vec::new();

    if let Some(messages) = body.get("messages").and_then(|m| m.as_array()) {
        for m in messages {
            let role = m.get("role").and_then(|r| r.as_str()).unwrap_or("");
            if role == "system" || role == "developer" {
                let txt = extract_text(m.get("content").unwrap_or(&Value::Null));
                if !txt.is_empty() {
                    system_parts.push(txt);
                }
                continue;
            }
            if role == "tool" {
                turns.push(json!({
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": m.get("tool_call_id").and_then(|t| t.as_str()).unwrap_or(""),
                        "content": extract_text(m.get("content").unwrap_or(&Value::Null)),
                    }]
                }));
            } else if role == "assistant" {
                let mut blocks = Vec::new();
                let txt = extract_text(m.get("content").unwrap_or(&Value::Null));
                if !txt.is_empty() {
                    blocks.push(json!({ "type": "text", "text": txt }));
                }
                if let Some(calls) = m.get("tool_calls").and_then(|t| t.as_array()) {
                    for c in calls {
                        let fn_obj = c.get("function").cloned().unwrap_or(json!({}));
                        let args_str = fn_obj.get("arguments").and_then(|a| a.as_str()).unwrap_or("{}");
                        let args_val: Value = serde_json::from_str(args_str).unwrap_or(json!({}));
                        blocks.push(json!({
                            "type": "tool_use",
                            "id": c.get("id").and_then(|i| i.as_str()).unwrap_or(&format!("toolu_{}", Uuid::new_v4().simple())),
                            "name": fn_obj.get("name").and_then(|n| n.as_str()).unwrap_or(""),
                            "input": args_val,
                        }));
                    }
                }
                turns.push(json!({ "role": "assistant", "content": blocks }));
            } else {
                turns.push(json!({
                    "role": "user",
                    "content": anthropic_user_blocks(m.get("content").unwrap_or(&Value::Null)),
                }));
            }
        }
    }

    let max_tokens = body.get("max_tokens").or_else(|| body.get("max_completion_tokens")).and_then(|v| v.as_u64()).unwrap_or(8192);

    let mut out = json!({
        "model": model,
        "messages": turns,
        "max_tokens": max_tokens,
    });

    if !system_parts.is_empty() {
        out["system"] = json!(system_parts.join("\n\n"));
    }
    if let Some(temp) = body.get("temperature") {
        out["temperature"] = temp.clone();
    }
    if let Some(tools) = body.get("tools").and_then(|t| t.as_array()) {
        let mut anthropic_tools = Vec::new();
        for t in tools {
            if let Some(fn_obj) = t.get("function") {
                anthropic_tools.push(json!({
                    "name": fn_obj.get("name").unwrap_or(&json!("")),
                    "description": fn_obj.get("description").unwrap_or(&json!("")),
                    "input_schema": fn_obj.get("parameters").unwrap_or(&json!({"type": "object", "properties": {}})),
                }));
            }
        }
        if !anthropic_tools.is_empty() {
            out["tools"] = Value::Array(anthropic_tools);
        }
    }

    out
}

/// Claude OAuth tokens are only accepted for requests that identify as Claude Code.
pub const CLAUDE_CODE_SYSTEM: &str = "You are Claude Code, Anthropic's official CLI for Claude.";

pub fn openai_to_claude_oauth(body: &Value, model: &str) -> Value {
    let mut out = openai_to_anthropic(body, model);
    let mut system = vec![json!({ "type": "text", "text": CLAUDE_CODE_SYSTEM })];
    if let Some(sys) = out.get("system").and_then(|v| v.as_str()) {
        system.push(json!({ "type": "text", "text": sys }));
    }
    out["system"] = json!(system);
    out["stream"] = json!(true);
    out
}

fn anthropic_user_blocks(content: &Value) -> Vec<Value> {
    let mut blocks = Vec::new();
    match content {
        Value::String(t) => blocks.push(json!({ "type": "text", "text": t })),
        Value::Array(parts) => {
            for p in parts {
                if let Some(t) = p.as_str() {
                    blocks.push(json!({ "type": "text", "text": t }));
                } else if matches!(p["type"].as_str(), Some("text" | "input_text")) {
                    blocks.push(json!({ "type": "text", "text": p["text"].as_str().unwrap_or("") }));
                } else if let Some((url, _)) = extract_image_part(p) {
                    if let Some((mime, data)) = parse_data_url(url) {
                        blocks.push(json!({
                            "type": "image",
                            "source": { "type": "base64", "media_type": mime, "data": data },
                        }));
                    } else if url.starts_with("http://") || url.starts_with("https://") {
                        blocks.push(json!({
                            "type": "image",
                            "source": { "type": "url", "url": url },
                        }));
                    }
                } else if p["type"] == "document" && p["source"].is_object() {
                    blocks.push(json!({ "type": "document", "source": p["source"] }));
                } else if let Some((data_or_url, filename, explicit_mime)) = extract_file_part(p) {
                    if let Some((mime, data)) = parse_data_url(data_or_url) {
                        blocks.push(json!({
                            "type": "document",
                            "source": { "type": "base64", "media_type": mime, "data": data },
                        }));
                    } else if data_or_url.starts_with("http://") || data_or_url.starts_with("https://") {
                        blocks.push(json!({
                            "type": "document",
                            "source": { "type": "url", "url": data_or_url },
                        }));
                    } else {
                        let mime = explicit_mime.unwrap_or_else(|| infer_mime(filename.unwrap_or(""), "application/pdf"));
                        blocks.push(json!({
                            "type": "document",
                            "source": { "type": "base64", "media_type": mime, "data": data_or_url },
                        }));
                    }
                }
            }
        }
        _ => {}
    }
    if blocks.is_empty() {
        blocks.push(json!({ "type": "text", "text": "" }));
    }
    blocks
}

/// Converts an Anthropic `/v1/messages` request body into OpenAI `/v1/chat/completions` format.
pub fn anthropic_to_openai(body: &Value) -> Value {
    let mut messages = Vec::new();
    match &body["system"] {
        Value::String(s) if !s.is_empty() => messages.push(json!({ "role": "system", "content": s })),
        Value::Array(arr) => {
            let txt: Vec<&str> = arr.iter().filter_map(|b| b["text"].as_str()).collect();
            if !txt.is_empty() {
                messages.push(json!({ "role": "system", "content": txt.join("\n\n") }));
            }
        }
        _ => {}
    }

    for m in body["messages"].as_array().into_iter().flatten() {
        let role = m["role"].as_str().unwrap_or("user");
        match &m["content"] {
            Value::String(s) => messages.push(json!({ "role": role, "content": s })),
            Value::Array(blocks) if role == "assistant" => {
                let mut text = String::new();
                let mut tool_calls = Vec::new();
                for b in blocks {
                    match b["type"].as_str().unwrap_or("") {
                        "text" => text.push_str(b["text"].as_str().unwrap_or("")),
                        "tool_use" => {
                            let args = serde_json::to_string(&b["input"]).unwrap_or_else(|_| "{}".into());
                            tool_calls.push(json!({
                                "id": b["id"].as_str().unwrap_or(""),
                                "type": "function",
                                "function": { "name": b["name"].as_str().unwrap_or(""), "arguments": args },
                            }));
                        }
                        _ => {}
                    }
                }
                let mut msg = json!({ "role": "assistant", "content": text });
                if !tool_calls.is_empty() {
                    msg["tool_calls"] = Value::Array(tool_calls);
                }
                messages.push(msg);
            }
            Value::Array(blocks) => {
                let mut user_parts = Vec::new();
                for b in blocks {
                    match b["type"].as_str().unwrap_or("") {
                        "text" => user_parts.push(json!({ "type": "text", "text": b["text"].as_str().unwrap_or("") })),
                        "image" => {
                            let src = &b["source"];
                            let url = if src["type"] == "base64" {
                                format!(
                                    "data:{};base64,{}",
                                    src["media_type"].as_str().unwrap_or("image/png"),
                                    src["data"].as_str().unwrap_or("")
                                )
                            } else {
                                src["url"].as_str().unwrap_or("").to_string()
                            };
                            if !url.is_empty() {
                                user_parts.push(json!({ "type": "image_url", "image_url": { "url": url } }));
                            }
                        }
                        "document" => {
                            let src = &b["source"];
                            if src["type"] == "base64" {
                                let data_url = format!(
                                    "data:{};base64,{}",
                                    src["media_type"].as_str().unwrap_or("application/pdf"),
                                    src["data"].as_str().unwrap_or("")
                                );
                                user_parts.push(json!({ "type": "file", "file": { "file_data": data_url } }));
                            } else if let Some(u) = src["url"].as_str() {
                                user_parts.push(json!({ "type": "file", "file": { "file_url": u } }));
                            }
                        }
                        "tool_result" => {
                            if !user_parts.is_empty() {
                                messages.push(json!({ "role": role, "content": std::mem::take(&mut user_parts) }));
                            }
                            let content = match &b["content"] {
                                Value::String(s) => s.clone(),
                                other => extract_text(other),
                            };
                            messages.push(json!({
                                "role": "tool",
                                "tool_call_id": b["tool_use_id"].as_str().unwrap_or(""),
                                "content": content,
                            }));
                        }
                        _ => {}
                    }
                }
                if !user_parts.is_empty() {
                    messages.push(json!({ "role": role, "content": user_parts }));
                }
            }
            _ => {}
        }
    }

    let mut out = json!({
        "model": body["model"].as_str().unwrap_or("auto"),
        "messages": messages,
        "stream": body["stream"].as_bool().unwrap_or(false),
    });
    if let Some(mt) = body.get("max_tokens") {
        out["max_tokens"] = mt.clone();
    }
    if let Some(t) = body.get("temperature") {
        out["temperature"] = t.clone();
    }
    if let Some(tools) = body.get("tools").and_then(|t| t.as_array()) {
        let oa_tools: Vec<Value> = tools
            .iter()
            .filter_map(|t| {
                t["name"].as_str().map(|name| {
                    json!({
                        "type": "function",
                        "function": {
                            "name": name,
                            "description": t.get("description").cloned().unwrap_or(json!("")),
                            "parameters": t.get("input_schema").cloned().unwrap_or(json!({ "type": "object", "properties": {} })),
                        }
                    })
                })
            })
            .collect();
        if !oa_tools.is_empty() {
            out["tools"] = Value::Array(oa_tools);
        }
    }
    out
}

/// Converts a non-streaming OpenAI `chat.completion` response into an Anthropic `message` object.
pub fn openai_completion_to_anthropic(resp: &Value, requested_model: &str) -> Value {
    let choice = &resp["choices"][0];
    let msg = &choice["message"];
    let mut content = Vec::new();
    if let Some(txt) = msg["content"].as_str().filter(|s| !s.is_empty()) {
        content.push(json!({ "type": "text", "text": txt }));
    }
    for tc in msg["tool_calls"].as_array().into_iter().flatten() {
        let input: Value = serde_json::from_str(tc["function"]["arguments"].as_str().unwrap_or("{}")).unwrap_or(json!({}));
        content.push(json!({
            "type": "tool_use",
            "id": tc["id"].as_str().unwrap_or(""),
            "name": tc["function"]["name"].as_str().unwrap_or(""),
            "input": input,
        }));
    }
    let stop_reason = match choice["finish_reason"].as_str().unwrap_or("stop") {
        "tool_calls" => "tool_use",
        "length" => "max_tokens",
        _ => "end_turn",
    };
    json!({
        "id": resp["id"].as_str().map(String::from).unwrap_or_else(|| format!("msg_{}", Uuid::new_v4().simple())),
        "type": "message",
        "role": "assistant",
        "model": resp["model"].as_str().unwrap_or(requested_model),
        "content": content,
        "stop_reason": stop_reason,
        "stop_sequence": null,
        "usage": {
            "input_tokens": resp["usage"]["prompt_tokens"].as_i64().unwrap_or(0),
            "output_tokens": resp["usage"]["completion_tokens"].as_i64().unwrap_or(0),
        }
    })
}

/// Stateful converter from OpenAI `chat.completion.chunk` SSE lines to Anthropic SSE events.
pub struct OpenAiToAnthropicSse {
    msg_id: String,
    model: String,
    started: bool,
    text_block_open: bool,
    next_index: usize,
    tool_blocks: std::collections::HashMap<usize, usize>,
    finished: bool,
}

impl OpenAiToAnthropicSse {
    pub fn new(model: String) -> Self {
        Self {
            msg_id: format!("msg_{}", Uuid::new_v4().simple()),
            model,
            started: false,
            text_block_open: false,
            next_index: 0,
            tool_blocks: std::collections::HashMap::new(),
            finished: false,
        }
    }

    fn event(name: &str, data: Value) -> String {
        format!("event: {name}\ndata: {}\n\n", serde_json::to_string(&data).unwrap_or_default())
    }

    fn ensure_started(&mut self, out: &mut String) {
        if !self.started {
            self.started = true;
            out.push_str(&Self::event(
                "message_start",
                json!({
                    "type": "message_start",
                    "message": {
                        "id": self.msg_id, "type": "message", "role": "assistant",
                        "content": [], "model": self.model, "stop_reason": null, "stop_sequence": null,
                        "usage": { "input_tokens": 0, "output_tokens": 0 }
                    }
                }),
            ));
        }
    }

    pub fn feed_sse_chunk(&mut self, chunk_str: &str) -> String {
        let mut out = String::new();
        for line in chunk_str.lines() {
            let Some(data) = line.trim().strip_prefix("data:") else { continue };
            let data = data.trim();
            if data == "[DONE]" {
                self.finish_into(&mut out, "end_turn");
                continue;
            }
            let Ok(ev) = serde_json::from_str::<Value>(data) else { continue };
            if let Some(err) = ev["error"]["message"].as_str() {
                out.push_str(&Self::event("error", json!({ "type": "error", "error": { "type": "api_error", "message": err } })));
                continue;
            }
            self.ensure_started(&mut out);
            let ch = &ev["choices"][0];
            if let Some(txt) = ch["delta"]["content"].as_str().filter(|s| !s.is_empty()) {
                if !self.text_block_open {
                    self.text_block_open = true;
                    let idx = self.next_index;
                    self.next_index += 1;
                    out.push_str(&Self::event(
                        "content_block_start",
                        json!({ "type": "content_block_start", "index": idx, "content_block": { "type": "text", "text": "" } }),
                    ));
                }
                out.push_str(&Self::event(
                    "content_block_delta",
                    json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": txt } }),
                ));
            }
            for tc in ch["delta"]["tool_calls"].as_array().into_iter().flatten() {
                let tc_idx = tc["index"].as_u64().unwrap_or(0) as usize;
                let block_idx = match self.tool_blocks.get(&tc_idx) {
                    Some(&b) => b,
                    None => {
                        if self.text_block_open {
                            out.push_str(&Self::event("content_block_stop", json!({ "type": "content_block_stop", "index": 0 })));
                            self.text_block_open = false;
                        }
                        let b = self.next_index;
                        self.next_index += 1;
                        self.tool_blocks.insert(tc_idx, b);
                        out.push_str(&Self::event(
                            "content_block_start",
                            json!({
                                "type": "content_block_start",
                                "index": b,
                                "content_block": {
                                    "type": "tool_use",
                                    "id": tc["id"].as_str().unwrap_or(""),
                                    "name": tc["function"]["name"].as_str().unwrap_or(""),
                                    "input": {}
                                }
                            }),
                        ));
                        b
                    }
                };
                if let Some(args) = tc["function"]["arguments"].as_str().filter(|s| !s.is_empty()) {
                    out.push_str(&Self::event(
                        "content_block_delta",
                        json!({
                            "type": "content_block_delta",
                            "index": block_idx,
                            "delta": { "type": "input_json_delta", "partial_json": args }
                        }),
                    ));
                }
            }
            if let Some(fr) = ch["finish_reason"].as_str() {
                let reason = match fr {
                    "tool_calls" => "tool_use",
                    "length" => "max_tokens",
                    _ => "end_turn",
                };
                self.finish_into(&mut out, reason);
            }
        }
        out
    }

    fn finish_into(&mut self, out: &mut String, reason: &str) {
        if self.finished {
            return;
        }
        self.finished = true;
        self.ensure_started(out);
        if self.text_block_open {
            out.push_str(&Self::event("content_block_stop", json!({ "type": "content_block_stop", "index": 0 })));
            self.text_block_open = false;
        }
        for &b in self.tool_blocks.values() {
            out.push_str(&Self::event("content_block_stop", json!({ "type": "content_block_stop", "index": b })));
        }
        out.push_str(&Self::event(
            "message_delta",
            json!({ "type": "message_delta", "delta": { "stop_reason": reason, "stop_sequence": null }, "usage": { "output_tokens": 0 } }),
        ));
        out.push_str(&Self::event("message_stop", json!({ "type": "message_stop" })));
    }
}
