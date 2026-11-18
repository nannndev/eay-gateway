use super::media::{extract_file_part, extract_image_part};
use super::{extract_text, now_secs};
use serde_json::{json, Value};
use std::collections::HashMap;
use uuid::Uuid;

fn codex_message_content(content: &Value, role: &str) -> Vec<Value> {
    let text_type = if role == "assistant" { "output_text" } else { "input_text" };
    let mut parts = Vec::new();
    match content {
        Value::String(t) if !t.is_empty() => parts.push(json!({ "type": text_type, "text": t })),
        Value::Array(arr) => {
            for p in arr {
                if let Some(t) = p.as_str() {
                    if !t.is_empty() {
                        parts.push(json!({ "type": text_type, "text": t }));
                    }
                } else if matches!(p["type"].as_str(), Some("text" | "input_text" | "output_text")) {
                    if let Some(t) = p["text"].as_str() {
                        parts.push(json!({ "type": text_type, "text": t }));
                    }
                } else if let Some((url, detail)) = extract_image_part(p) {
                    let mut item = json!({ "type": "input_image", "image_url": url });
                    if let Some(d) = detail {
                        item["detail"] = json!(d);
                    }
                    parts.push(item);
                } else if let Some((data_or_url, filename, _)) = extract_file_part(p) {
                    let mut item = if data_or_url.starts_with("http://") || data_or_url.starts_with("https://") {
                        json!({ "type": "input_file", "file_url": data_or_url })
                    } else {
                        json!({ "type": "input_file", "file_data": data_or_url })
                    };
                    if let Some(name) = filename {
                        item["filename"] = json!(name);
                    }
                    parts.push(item);
                }
            }
        }
        _ => {}
    }
    parts
}

pub fn openai_to_codex(body: &Value, upstream_model: &str) -> Value {
    let mut system_parts = Vec::new();
    let mut items = Vec::new();

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
                items.push(json!({
                    "type": "function_call_output",
                    "call_id": m.get("tool_call_id").and_then(|t| t.as_str()).unwrap_or(""),
                    "output": extract_text(m.get("content").unwrap_or(&Value::Null)),
                }));
                continue;
            }
            let mut content = codex_message_content(m.get("content").unwrap_or(&Value::Null), role);
            if content.is_empty() && role == "user" {
                content.push(json!({ "type": "input_text", "text": "" }));
            }
            if !content.is_empty() {
                items.push(json!({
                    "type": "message",
                    "role": role,
                    "content": content,
                }));
            }
            if role == "assistant" {
                if let Some(tool_calls) = m.get("tool_calls").and_then(|t| t.as_array()) {
                    for c in tool_calls {
                        let fn_obj = c.get("function").cloned().unwrap_or(json!({}));
                        items.push(json!({
                            "type": "function_call",
                            "call_id": c.get("id").and_then(|i| i.as_str()).unwrap_or(""),
                            "name": fn_obj.get("name").and_then(|n| n.as_str()).unwrap_or(""),
                            "arguments": fn_obj.get("arguments").and_then(|a| a.as_str()).unwrap_or("{}"),
                        }));
                    }
                }
            }
        }
    }

    let mut out = json!({
        "model": upstream_model,
        "instructions": system_parts.join("\n\n"),
        "input": items,
        "stream": true,
        "store": false,
        "include": [],
    });

    if let Some(tools) = body.get("tools").and_then(|t| t.as_array()) {
        if !tools.is_empty() {
            let mut codex_tools = Vec::new();
            for t in tools {
                if let Some(fn_obj) = t.get("function") {
                    codex_tools.push(json!({
                        "type": "function",
                        "name": fn_obj.get("name").unwrap_or(&json!("")),
                        "description": fn_obj.get("description").unwrap_or(&json!("")),
                        "parameters": fn_obj.get("parameters").unwrap_or(&json!({"type": "object", "properties": {}})),
                        "strict": false,
                    }));
                }
            }
            out["tools"] = Value::Array(codex_tools);
            out["parallel_tool_calls"] = json!(true);
        }
    }

    out
}

pub struct CodexStreamTranslator {
    pub public_model: String,
    pub id: String,
    pub created: u64,
    pub by_item: HashMap<String, usize>,
    pub tool_calls: Vec<Value>,
}

impl CodexStreamTranslator {
    pub fn new(public_model: String) -> Self {
        Self {
            public_model,
            id: format!("chatcmpl-{}", Uuid::new_v4().simple()),
            created: now_secs(),
            by_item: HashMap::new(),
            tool_calls: Vec::new(),
        }
    }

    pub fn start_chunk(&self) -> String {
        self.chunk_data(
            json!({
                "role": "assistant",
                "content": ""
            }),
            None,
        )
    }

    pub fn chunk_data(&self, delta: Value, finish_reason: Option<&str>) -> String {
        let frame = json!({
            "id": self.id,
            "object": "chat.completion.chunk",
            "created": self.created,
            "model": self.public_model,
            "choices": [{
                "index": 0,
                "delta": delta,
                "finish_reason": finish_reason,
            }]
        });
        format!("data: {}\n\n", serde_json::to_string(&frame).unwrap_or_default())
    }

    pub fn feed(&mut self, ev: &Value) -> Vec<String> {
        let mut chunks = Vec::new();
        let ev_type = ev.get("type").and_then(|t| t.as_str()).unwrap_or("");

        if ev_type == "response.output_text.delta" {
            if let Some(delta_str) = ev.get("delta").and_then(|d| d.as_str()) {
                if !delta_str.is_empty() {
                    chunks.push(self.chunk_data(json!({ "content": delta_str }), None));
                }
            }
        } else if ev_type == "response.output_item.added" {
            if let Some(item) = ev.get("item") {
                if item.get("type").and_then(|t| t.as_str()) == Some("function_call") {
                    let index = self.tool_calls.len();
                    let item_id = item.get("id").and_then(|i| i.as_str()).unwrap_or("");
                    let call_id = item.get("call_id").and_then(|c| c.as_str()).unwrap_or(item_id);
                    let name = item.get("name").and_then(|n| n.as_str()).unwrap_or("");
                    let args = item.get("arguments").and_then(|a| a.as_str()).unwrap_or("");

                    self.by_item.insert(item_id.to_string(), index);
                    self.tool_calls.push(json!({
                        "id": call_id,
                        "type": "function",
                        "function": { "name": name, "arguments": args },
                    }));

                    chunks.push(self.chunk_data(
                        json!({
                            "tool_calls": [{
                                "index": index,
                                "id": call_id,
                                "type": "function",
                                "function": { "name": name, "arguments": args },
                            }]
                        }),
                        None,
                    ));
                }
            }
        } else if ev_type == "response.function_call_arguments.delta" {
            if let Some(item_id) = ev.get("item_id").and_then(|i| i.as_str()) {
                if let Some(&index) = self.by_item.get(item_id) {
                    let delta_args = ev.get("delta").and_then(|d| d.as_str()).unwrap_or("");
                    chunks.push(self.chunk_data(
                        json!({
                            "tool_calls": [{
                                "index": index,
                                "function": { "arguments": delta_args }
                            }]
                        }),
                        None,
                    ));
                }
            }
        } else if ev_type == "response.completed" {
            chunks.push(self.chunk_data(json!({}), Some("stop")));
            chunks.push("data: [DONE]\n\n".to_string());
        }

        chunks
    }
}

/// Converts an OpenAI `/v1/responses` request body into `/v1/chat/completions` format.
pub fn responses_to_openai(body: &Value) -> Value {
    let mut messages = Vec::new();
    if let Some(inst) = body["instructions"].as_str().filter(|s| !s.is_empty()) {
        messages.push(json!({ "role": "system", "content": inst }));
    }
    match &body["input"] {
        Value::String(s) => messages.push(json!({ "role": "user", "content": s })),
        Value::Array(items) => {
            for item in items {
                let ty = item["type"].as_str().unwrap_or("message");
                if ty == "function_call_output" {
                    messages.push(json!({
                        "role": "tool",
                        "tool_call_id": item["call_id"].as_str().unwrap_or(""),
                        "content": extract_text(&item["output"]),
                    }));
                } else {
                    let role = item["role"].as_str().unwrap_or("user");
                    let content = match &item["content"] {
                        Value::String(s) => json!(s),
                        Value::Array(parts) => {
                            let mapped: Vec<Value> = parts
                                .iter()
                                .filter_map(|p| match p["type"].as_str().unwrap_or("") {
                                    "input_text" | "output_text" | "text" => Some(json!({ "type": "text", "text": p["text"].as_str().unwrap_or("") })),
                                    "input_image" => Some(json!({ "type": "image_url", "image_url": { "url": p["image_url"] } })),
                                    "input_file" => Some(json!({ "type": "file", "file": p })),
                                    _ => None,
                                })
                                .collect();
                            Value::Array(mapped)
                        }
                        _ => json!(extract_text(&item["content"])),
                    };
                    messages.push(json!({ "role": role, "content": content }));
                }
            }
        }
        _ => {}
    }

    let mut out = json!({
        "model": body["model"].as_str().unwrap_or("auto"),
        "messages": messages,
        "stream": body["stream"].as_bool().unwrap_or(false),
    });
    if let Some(mt) = body.get("max_output_tokens") {
        out["max_tokens"] = mt.clone();
    }
    if let Some(t) = body.get("temperature") {
        out["temperature"] = t.clone();
    }
    out
}

/// Converts a non-streaming OpenAI `chat.completion` into an OpenAI `/v1/responses` payload.
pub fn openai_completion_to_responses(resp: &Value, requested_model: &str) -> Value {
    let msg = &resp["choices"][0]["message"];
    let mut output = Vec::new();
    if let Some(txt) = msg["content"].as_str().filter(|s| !s.is_empty()) {
        output.push(json!({
            "id": format!("msg_{}", Uuid::new_v4().simple()),
            "type": "message",
            "status": "completed",
            "role": "assistant",
            "content": [{ "type": "output_text", "text": txt }]
        }));
    }
    for tc in msg["tool_calls"].as_array().into_iter().flatten() {
        output.push(json!({
            "type": "function_call",
            "call_id": tc["id"],
            "name": tc["function"]["name"],
            "arguments": tc["function"]["arguments"],
        }));
    }
    json!({
        "id": format!("resp_{}", Uuid::new_v4().simple()),
        "object": "response",
        "status": "completed",
        "model": resp["model"].as_str().unwrap_or(requested_model),
        "output": output,
        "usage": {
            "input_tokens": resp["usage"]["prompt_tokens"].as_i64().unwrap_or(0),
            "output_tokens": resp["usage"]["completion_tokens"].as_i64().unwrap_or(0),
            "total_tokens": resp["usage"]["total_tokens"].as_i64().unwrap_or(0),
        }
    })
}
