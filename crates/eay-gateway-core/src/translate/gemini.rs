use super::extract_text;
use super::media::{extract_file_part, extract_image_part, infer_mime, parse_data_url};
use serde_json::{json, Value};
use uuid::Uuid;

fn gemini_user_parts(content: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    match content {
        Value::String(t) => out.push(json!({ "text": t })),
        Value::Array(parts) => {
            for p in parts {
                if let Some(t) = p.as_str() {
                    out.push(json!({ "text": t }));
                } else if matches!(p["type"].as_str(), Some("text" | "input_text")) {
                    out.push(json!({ "text": p["text"].as_str().unwrap_or("") }));
                } else if let Some((url, _)) = extract_image_part(p) {
                    if let Some((mime, data)) = parse_data_url(url) {
                        out.push(json!({ "inlineData": { "mimeType": mime, "data": data } }));
                    } else if url.starts_with("http://") || url.starts_with("https://") || url.starts_with("gs://") {
                        out.push(json!({ "fileData": { "mimeType": infer_mime(url, "image/png"), "fileUri": url } }));
                    }
                } else if p["type"] == "video_url" {
                    let url = p["video_url"]["url"].as_str().or_else(|| p["video_url"].as_str()).unwrap_or("");
                    if let Some((mime, data)) = parse_data_url(url) {
                        out.push(json!({ "inlineData": { "mimeType": mime, "data": data } }));
                    } else if url.starts_with("http://") || url.starts_with("https://") || url.starts_with("gs://") {
                        out.push(json!({ "fileData": { "mimeType": infer_mime(url, "video/mp4"), "fileUri": url } }));
                    }
                } else if p["type"] == "input_audio" {
                    if let Some(data) = p["input_audio"]["data"].as_str().filter(|s| !s.is_empty()) {
                        let fmt = p["input_audio"]["format"].as_str().unwrap_or("mp3");
                        let mime = match fmt {
                            "wav" => "audio/wav",
                            "ogg" => "audio/ogg",
                            _ => "audio/mpeg",
                        };
                        out.push(json!({ "inlineData": { "mimeType": mime, "data": data } }));
                    }
                } else if let Some((data_or_url, filename, explicit_mime)) = extract_file_part(p) {
                    if let Some((mime, data)) = parse_data_url(data_or_url) {
                        out.push(json!({ "inlineData": { "mimeType": mime, "data": data } }));
                    } else if data_or_url.starts_with("http://") || data_or_url.starts_with("https://") || data_or_url.starts_with("gs://") {
                        let mime = explicit_mime.unwrap_or_else(|| infer_mime(filename.unwrap_or(data_or_url), "application/pdf"));
                        out.push(json!({ "fileData": { "mimeType": mime, "fileUri": data_or_url } }));
                    } else {
                        let mime = explicit_mime.unwrap_or_else(|| infer_mime(filename.unwrap_or(""), "application/pdf"));
                        out.push(json!({ "inlineData": { "mimeType": mime, "data": data_or_url } }));
                    }
                }
            }
        }
        _ => {}
    }
    if out.is_empty() {
        out.push(json!({ "text": "" }));
    }
    out
}

pub fn openai_to_gemini(body: &Value, model: &str, project: Option<&str>) -> Value {
    let msgs = body.get("messages").and_then(|m| m.as_array()).cloned().unwrap_or_default();
    let name_of = |id: &str| -> String {
        for m in &msgs {
            for c in m.get("tool_calls").and_then(|t| t.as_array()).into_iter().flatten() {
                if c["id"].as_str() == Some(id) {
                    return c["function"]["name"].as_str().unwrap_or("tool").to_string();
                }
            }
        }
        "tool".into()
    };
    let mut sys = Vec::new();
    let mut contents: Vec<Value> = Vec::new();
    for m in &msgs {
        let role = m["role"].as_str().unwrap_or("");
        let text = extract_text(m.get("content").unwrap_or(&Value::Null));
        let (grole, parts) = match role {
            "system" | "developer" => {
                if !text.is_empty() {
                    sys.push(text);
                }
                continue;
            }
            "tool" => {
                let resp = serde_json::from_str::<Value>(&text).ok().filter(|v| v.is_object()).unwrap_or(json!({ "result": text }));
                (
                    "user",
                    vec![json!({ "functionResponse": { "name": name_of(m["tool_call_id"].as_str().unwrap_or("")), "response": resp } })],
                )
            }
            "assistant" => {
                let mut p = Vec::new();
                if !text.is_empty() {
                    p.push(json!({ "text": text }));
                }
                for c in m.get("tool_calls").and_then(|t| t.as_array()).into_iter().flatten() {
                    let args: Value = serde_json::from_str(c["function"]["arguments"].as_str().unwrap_or("{}")).unwrap_or(json!({}));
                    p.push(json!({ "functionCall": { "name": c["function"]["name"], "args": args } }));
                }
                ("model", p)
            }
            _ => ("user", gemini_user_parts(m.get("content").unwrap_or(&Value::Null))),
        };
        if parts.is_empty() {
            continue;
        }
        // Gemini needs alternating turns: merge same-role neighbours.
        match contents.last_mut() {
            Some(last) if last["role"] == grole => {
                if let Some(existing) = last["parts"].as_array_mut() {
                    existing.extend(parts);
                }
            }
            _ => contents.push(json!({ "role": grole, "parts": parts })),
        }
    }
    let mut request = json!({ "contents": contents, "generationConfig": {} });
    if !sys.is_empty() {
        request["systemInstruction"] = json!({ "parts": [{ "text": sys.join("\n\n") }] });
    }
    if let Some(mt) = body.get("max_tokens").or_else(|| body.get("max_completion_tokens")) {
        request["generationConfig"]["maxOutputTokens"] = mt.clone();
    }
    if let Some(t) = body.get("temperature") {
        request["generationConfig"]["temperature"] = t.clone();
    }
    let decls: Vec<Value> = body
        .get("tools")
        .and_then(|t| t.as_array())
        .into_iter()
        .flatten()
        .filter_map(|t| t.get("function"))
        .map(|f| {
            json!({
                "name": f["name"],
                "description": f.get("description").cloned().unwrap_or(json!("")),
                "parameters": f.get("parameters").cloned().unwrap_or(json!({ "type": "object", "properties": {} })),
            })
        })
        .collect();
    if !decls.is_empty() {
        request["tools"] = json!([{ "functionDeclarations": decls }]);
    }
    json!({ "model": model, "project": project, "request": request })
}

/// Reshapes a Gemini CLI body into what Antigravity's Cloud Code endpoint expects.
pub fn antigravity_envelope(b: &mut Value, default_signature: &str) {
    if b["project"].is_null() {
        // 9router invents a project id when onboarding gave none.
        b["project"] = json!(format!("useful-fuze-{}", &Uuid::new_v4().simple().to_string()[..5]));
    }
    b["userAgent"] = json!("antigravity");
    b["requestId"] = json!(format!("agent/{}/{}/{}/1", Uuid::new_v4(), chrono::Utc::now().timestamp_millis(), Uuid::new_v4()));
    let r = &mut b["request"];
    r["sessionId"] = json!(Uuid::new_v4().to_string());
    if let Some(n) = r["generationConfig"]["maxOutputTokens"].as_u64() {
        r["generationConfig"]["maxOutputTokens"] = json!(n.min(64_000));
    }
    if r["tools"].is_array() {
        r["toolConfig"] = json!({ "functionCallingConfig": { "mode": "VALIDATED" } });
        for d in r["tools"][0]["functionDeclarations"].as_array_mut().into_iter().flatten() {
            let clean: String = d["name"]
                .as_str()
                .unwrap_or("tool")
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() || "_.:-".contains(c) { c } else { '_' })
                .take(64)
                .collect();
            d["name"] = json!(clean);
        }
    }
    // The first function call of each model turn must carry a thought signature.
    for turn in r["contents"].as_array_mut().into_iter().flatten() {
        let mut first = true;
        for part in turn["parts"].as_array_mut().into_iter().flatten() {
            if part.get("functionCall").is_some() {
                if first && part.get("thoughtSignature").is_none() {
                    part["thoughtSignature"] = json!(default_signature);
                }
                first = false;
            }
        }
    }
}
