use super::extract_text;
use super::media::{extract_image_part, parse_data_url};
use serde_json::{json, Value};
use std::collections::HashMap;

fn commandcode_user_parts(content: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    match content {
        Value::String(t) => out.push(json!({ "type": "text", "text": t })),
        Value::Array(parts) => {
            for p in parts {
                if let Some(t) = p.as_str() {
                    out.push(json!({ "type": "text", "text": t }));
                } else if matches!(p["type"].as_str(), Some("text" | "input_text")) {
                    out.push(json!({ "type": "text", "text": p["text"].as_str().unwrap_or("") }));
                } else if let Some((url, _)) = extract_image_part(p) {
                    // Only inline data URLs are forwarded, as 9router does.
                    if let Some((mime, _)) = parse_data_url(url) {
                        out.push(json!({ "type": "image", "image": url, "mimeType": mime, "mediaType": mime }));
                    }
                }
            }
        }
        _ => {}
    }
    if out.is_empty() {
        out.push(json!({ "type": "text", "text": "" }));
    }
    out
}

/// Builds a Command Code generate request. `thread_id` should be stable per conversation.
pub fn openai_to_commandcode(body: &Value, model: &str, thread_id: &str) -> Value {
    let mut system = Vec::new();
    let mut messages = Vec::new();
    let mut tool_names: HashMap<String, String> = HashMap::new();
    let msgs = body["messages"].as_array().cloned().unwrap_or_default();
    for m in &msgs {
        for c in m["tool_calls"].as_array().into_iter().flatten() {
            if let (Some(id), Some(name)) = (c["id"].as_str(), c["function"]["name"].as_str()) {
                tool_names.insert(id.to_string(), name.to_string());
            }
        }
    }
    for m in &msgs {
        match m["role"].as_str().unwrap_or("user") {
            "system" | "developer" => {
                let t = extract_text(&m["content"]);
                if !t.is_empty() {
                    system.push(t);
                }
            }
            "tool" => {
                let id = m["tool_call_id"].as_str().unwrap_or("");
                let name = m["name"].as_str().map(String::from).or_else(|| tool_names.get(id).cloned()).unwrap_or_else(|| "unknown".into());
                let text = m["content"].as_str().map(String::from).unwrap_or_else(|| extract_text(&m["content"]));
                messages.push(json!({ "role": "tool", "content": [{
                    "type": "tool-result", "toolCallId": id, "toolName": name,
                    "output": { "type": "text", "value": text },
                }]}));
            }
            "assistant" => {
                let mut parts = Vec::new();
                if let Some(r) = m["reasoning_content"].as_str().filter(|r| !r.trim().is_empty()) {
                    parts.push(json!({ "type": "reasoning", "text": r }));
                }
                let t = extract_text(&m["content"]);
                if !t.is_empty() {
                    parts.push(json!({ "type": "text", "text": t }));
                }
                for c in m["tool_calls"].as_array().into_iter().flatten() {
                    let args = &c["function"]["arguments"];
                    let input = match args {
                        Value::String(raw) if raw.trim().is_empty() => json!({}),
                        Value::String(raw) => {
                            serde_json::from_str::<Value>(raw).ok().filter(|v| v.is_object()).unwrap_or_else(|| json!({ "raw": raw }))
                        }
                        Value::Null => json!({}),
                        other => other.clone(),
                    };
                    parts.push(json!({
                        "type": "tool-call",
                        "toolCallId": c["id"].as_str().unwrap_or(""),
                        "toolName": c["function"]["name"].as_str().unwrap_or(""),
                        "input": input,
                    }));
                }
                if parts.is_empty() {
                    parts.push(json!({ "type": "text", "text": "" }));
                }
                messages.push(json!({ "role": "assistant", "content": parts }));
            }
            _ => messages.push(json!({ "role": "user", "content": commandcode_user_parts(&m["content"]) })),
        }
    }
    let mut params = json!({
        "model": model,
        "messages": messages,
        "stream": true,
        "max_tokens": body["max_tokens"].as_u64().or(body["max_completion_tokens"].as_u64()).unwrap_or(64_000),
        "temperature": body["temperature"].as_f64().unwrap_or(0.3),
    });
    if !system.is_empty() {
        params["system"] = json!(system.join("\n\n"));
    }
    if let Some(p) = body.get("top_p").filter(|v| !v.is_null()) {
        params["top_p"] = p.clone();
    }
    if let Some(e) = body["reasoning_effort"].as_str().or(body["reasoning"]["effort"].as_str()) {
        params["reasoning_effort"] = json!(e);
    }
    let tools: Vec<Value> = body["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| {
            let f = &t["function"];
            f["name"].as_str().map(|n| {
                json!({
                    "name": n,
                    "description": f["description"],
                    "input_schema": if f["parameters"].is_null() { json!({"type":"object"}) } else { f["parameters"].clone() },
                })
            })
        })
        .collect();
    if !tools.is_empty() {
        params["tools"] = json!(tools);
    }
    json!({
        "threadId": thread_id,
        "memory": null, "taste": null, "skills": null,
        "permissionMode": "standard",
        "config": {
            "workingDir": "/", "date": chrono::Utc::now().format("%Y-%m-%d").to_string(),
            "environment": std::env::consts::OS, "structure": [], "isGitRepo": false,
            "currentBranch": "", "mainBranch": "", "gitStatus": "", "recentCommits": [],
        },
        "params": params,
    })
}
