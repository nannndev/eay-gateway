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
    if let Some(effort) = super::reasoning_effort(body) {
        out["reasoning"] = json!({ "effort": effort, "summary": "auto" });
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

        if ev_type == "response.reasoning_summary_text.delta" || ev_type == "response.reasoning_text.delta" {
            if let Some(t) = ev["delta"].as_str().filter(|t| !t.is_empty()) {
                chunks.push(self.chunk_data(json!({ "reasoning_content": t }), None));
            }
        } else if ev_type == "response.output_text.delta" {
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
                if ty == "function_call" {
                    // Consecutive calls belong to one assistant turn, as chat/completions expects.
                    let call = json!({
                        "id": item["call_id"].as_str().unwrap_or(""),
                        "type": "function",
                        "function": { "name": item["name"].as_str().unwrap_or(""), "arguments": item["arguments"].as_str().unwrap_or("{}") },
                    });
                    match messages.last_mut().filter(|m| m["role"] == "assistant" && m.get("tool_calls").is_some()) {
                        Some(prev) => prev["tool_calls"].as_array_mut().into_iter().for_each(|a| a.push(call.clone())),
                        None => messages.push(json!({ "role": "assistant", "content": null, "tool_calls": [call] })),
                    }
                } else if ty == "reasoning" {
                    continue;
                } else if ty == "function_call_output" {
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
                                    "input_text" | "output_text" | "text" => {
                                        Some(json!({ "type": "text", "text": p["text"].as_str().unwrap_or("") }))
                                    }
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
    // Responses tools are flat (`{type, name, parameters}`); chat/completions nests them under `function`.
    // Built-in tools (web_search, file_search, …) have no chat equivalent and are dropped.
    let tools: Vec<Value> = body["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|t| t["type"] == "function")
        .map(|t| {
            let mut f = json!({ "name": t["name"], "parameters": t.get("parameters").cloned().unwrap_or(json!({ "type": "object", "properties": {} })) });
            if let Some(d) = t["description"].as_str() {
                f["description"] = json!(d);
            }
            if let Some(strict) = t.get("strict").filter(|v| v.is_boolean()) {
                f["strict"] = strict.clone();
            }
            json!({ "type": "function", "function": f })
        })
        .collect();
    if !tools.is_empty() {
        out["tools"] = json!(tools);
        match &body["tool_choice"] {
            Value::String(c) => out["tool_choice"] = json!(c),
            Value::Object(o) if o.get("type").and_then(Value::as_str) == Some("function") => {
                out["tool_choice"] = json!({ "type": "function", "function": { "name": o.get("name").cloned().unwrap_or(Value::Null) } })
            }
            _ => {}
        }
        if let Some(p) = body.get("parallel_tool_calls").filter(|v| v.is_boolean()) {
            out["parallel_tool_calls"] = p.clone();
        }
    }
    if let Some(effort) = body["reasoning"]["effort"].as_str().filter(|e| !e.is_empty()) {
        out["reasoning_effort"] = json!(effort);
    }
    if out["stream"] == true {
        out["stream_options"] = json!({ "include_usage": true });
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

/// Stateful converter from OpenAI `chat.completion.chunk` SSE to OpenAI Responses stream events
/// (`response.created` … `response.output_text.delta` … `response.completed`), as Codex CLI expects.
pub struct OpenAiToResponsesSse {
    resp_id: String,
    msg_id: String,
    model: String,
    seq: u64,
    started: bool,
    text: Option<(usize, String)>,
    /// chat tool_call index -> (output_index, call_id, name, arguments)
    tools: HashMap<usize, (usize, String, String, String)>,
    next_output: usize,
    usage: Value,
    finished: bool,
}

impl OpenAiToResponsesSse {
    pub fn new(model: String) -> Self {
        Self {
            resp_id: format!("resp_{}", Uuid::new_v4().simple()),
            msg_id: format!("msg_{}", Uuid::new_v4().simple()),
            model,
            seq: 0,
            started: false,
            text: None,
            tools: HashMap::new(),
            next_output: 0,
            usage: json!({ "input_tokens": 0, "output_tokens": 0, "total_tokens": 0 }),
            finished: false,
        }
    }

    fn event(&mut self, out: &mut String, mut data: Value) {
        data["sequence_number"] = json!(self.seq);
        self.seq += 1;
        let name = data["type"].as_str().unwrap_or("message").to_string();
        out.push_str(&format!("event: {name}\ndata: {}\n\n", serde_json::to_string(&data).unwrap_or_default()));
    }

    fn response(&self, status: &str, output: Vec<Value>) -> Value {
        json!({
            "id": self.resp_id, "object": "response", "created_at": now_secs(), "status": status,
            "model": self.model, "output": output, "usage": if status == "completed" { self.usage.clone() } else { Value::Null },
        })
    }

    fn message_item(&self, text: &str, status: &str) -> Value {
        json!({ "id": self.msg_id, "type": "message", "status": status, "role": "assistant",
                "content": if status == "completed" { json!([{ "type": "output_text", "text": text, "annotations": [] }]) } else { json!([]) } })
    }

    fn tool_item(call_id: &str, name: &str, args: &str, status: &str) -> Value {
        json!({ "id": format!("fc_{call_id}"), "type": "function_call", "status": status, "call_id": call_id, "name": name, "arguments": args })
    }

    fn ensure_started(&mut self, out: &mut String) {
        if !self.started {
            self.started = true;
            let r = self.response("in_progress", vec![]);
            self.event(out, json!({ "type": "response.created", "response": r }));
            let r = self.response("in_progress", vec![]);
            self.event(out, json!({ "type": "response.in_progress", "response": r }));
        }
    }

    pub fn feed_sse_chunk(&mut self, chunk_str: &str) -> String {
        let mut out = String::new();
        for line in chunk_str.lines() {
            let Some(data) = line.trim().strip_prefix("data:") else { continue };
            let data = data.trim();
            if data == "[DONE]" {
                self.finish_into(&mut out);
                continue;
            }
            let Ok(ev) = serde_json::from_str::<Value>(data) else { continue };
            if let Some(err) = ev["error"]["message"].as_str() {
                let msg = err.to_string();
                self.event(&mut out, json!({ "type": "error", "message": msg }));
                continue;
            }
            self.ensure_started(&mut out);
            if let Some(u) = ev.get("usage").filter(|u| u.is_object()) {
                let (i, o) = (u["prompt_tokens"].as_i64().unwrap_or(0), u["completion_tokens"].as_i64().unwrap_or(0));
                self.usage = json!({ "input_tokens": i, "output_tokens": o, "total_tokens": i + o });
            }
            let ch = &ev["choices"][0];
            if let Some(txt) = ch["delta"]["content"].as_str().filter(|s| !s.is_empty()) {
                if self.text.is_none() {
                    let idx = self.next_output;
                    self.next_output += 1;
                    self.text = Some((idx, String::new()));
                    let item = self.message_item("", "in_progress");
                    self.event(&mut out, json!({ "type": "response.output_item.added", "output_index": idx, "item": item }));
                    let id = self.msg_id.clone();
                    self.event(
                        &mut out,
                        json!({ "type": "response.content_part.added", "item_id": id, "output_index": idx, "content_index": 0,
                        "part": { "type": "output_text", "text": "", "annotations": [] } }),
                    );
                }
                let idx = match self.text.as_mut() {
                    Some((i, buf)) => {
                        buf.push_str(txt);
                        *i
                    }
                    None => 0,
                };
                let id = self.msg_id.clone();
                self.event(
                    &mut out,
                    json!({ "type": "response.output_text.delta", "item_id": id, "output_index": idx, "content_index": 0, "delta": txt }),
                );
            }
            for tc in ch["delta"]["tool_calls"].as_array().into_iter().flatten() {
                let k = tc["index"].as_u64().unwrap_or(0) as usize;
                if !self.tools.contains_key(&k) {
                    let idx = self.next_output;
                    self.next_output += 1;
                    let call_id = tc["id"].as_str().map(String::from).unwrap_or_else(|| format!("call_{}", Uuid::new_v4().simple()));
                    let name = tc["function"]["name"].as_str().unwrap_or("").to_string();
                    let item = Self::tool_item(&call_id, &name, "", "in_progress");
                    self.tools.insert(k, (idx, call_id, name, String::new()));
                    self.event(&mut out, json!({ "type": "response.output_item.added", "output_index": idx, "item": item }));
                }
                if let Some(args) = tc["function"]["arguments"].as_str().filter(|s| !s.is_empty()) {
                    let Some(t) = self.tools.get_mut(&k) else { continue };
                    t.3.push_str(args);
                    let (idx, call_id) = (t.0, t.1.clone());
                    self.event(&mut out, json!({ "type": "response.function_call_arguments.delta", "item_id": format!("fc_{call_id}"), "output_index": idx, "delta": args }));
                }
            }
        }
        out
    }

    /// Closes open items and emits `response.completed`; safe to call more than once.
    pub fn finish(&mut self) -> String {
        let mut out = String::new();
        self.finish_into(&mut out);
        out
    }

    fn finish_into(&mut self, out: &mut String) {
        if self.finished {
            return;
        }
        self.finished = true;
        self.ensure_started(out);
        let mut done: Vec<(usize, Value)> = Vec::new();
        if let Some((idx, text)) = self.text.take() {
            let id = self.msg_id.clone();
            self.event(
                out,
                json!({ "type": "response.output_text.done", "item_id": id, "output_index": idx, "content_index": 0, "text": text }),
            );
            self.event(
                out,
                json!({ "type": "response.content_part.done", "item_id": id, "output_index": idx, "content_index": 0,
                "part": { "type": "output_text", "text": text, "annotations": [] } }),
            );
            let item = self.message_item(&text, "completed");
            done.push((idx, item.clone()));
            self.event(out, json!({ "type": "response.output_item.done", "output_index": idx, "item": item }));
        }
        let mut tools: Vec<_> = self.tools.drain().map(|(_, t)| t).collect();
        tools.sort_by_key(|t| t.0);
        for (idx, call_id, name, args) in tools {
            self.event(out, json!({ "type": "response.function_call_arguments.done", "item_id": format!("fc_{call_id}"), "output_index": idx, "arguments": args }));
            let item = Self::tool_item(&call_id, &name, &args, "completed");
            done.push((idx, item.clone()));
            self.event(out, json!({ "type": "response.output_item.done", "output_index": idx, "item": item }));
        }
        done.sort_by_key(|d| d.0);
        let r = self.response("completed", done.into_iter().map(|d| d.1).collect());
        self.event(out, json!({ "type": "response.completed", "response": r }));
    }
}
