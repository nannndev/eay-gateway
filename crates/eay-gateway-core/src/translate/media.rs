use serde_json::{json, Value};

/// Splits a `data:<mime>;base64,<data>` URL into `(mime, base64_data)`.
pub(super) fn parse_data_url(url: &str) -> Option<(&str, &str)> {
    let rest = url.strip_prefix("data:")?;
    let (meta, data) = rest.split_once(',')?;
    let base64_meta = meta.strip_suffix(";base64")?;
    let mime = base64_meta.split(';').next().filter(|m| !m.is_empty()).unwrap_or("application/octet-stream");
    Some((mime, data))
}

pub(super) fn infer_mime(filename: &str, default_mime: &'static str) -> &'static str {
    let lower = filename.to_ascii_lowercase();
    if lower.ends_with(".pdf") {
        "application/pdf"
    } else if lower.ends_with(".txt") {
        "text/plain"
    } else if lower.ends_with(".md") {
        "text/markdown"
    } else if lower.ends_with(".csv") {
        "text/csv"
    } else if lower.ends_with(".json") {
        "application/json"
    } else if lower.ends_with(".png") {
        "image/png"
    } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        "image/jpeg"
    } else if lower.ends_with(".webp") {
        "image/webp"
    } else if lower.ends_with(".gif") {
        "image/gif"
    } else if lower.ends_with(".mp4") {
        "video/mp4"
    } else if lower.ends_with(".webm") {
        "video/webm"
    } else if lower.ends_with(".mov") {
        "video/quicktime"
    } else if lower.ends_with(".mp3") {
        "audio/mpeg"
    } else if lower.ends_with(".wav") {
        "audio/wav"
    } else if lower.ends_with(".ogg") {
        "audio/ogg"
    } else {
        default_mime
    }
}

pub(super) fn extract_image_part(part: &Value) -> Option<(&str, Option<&str>)> {
    let ty = part["type"].as_str().unwrap_or("");
    if ty != "image_url" && ty != "input_image" {
        return None;
    }
    let url = part["image_url"]["url"].as_str().or_else(|| part["image_url"].as_str()).filter(|s| !s.is_empty())?;
    let detail = part["image_url"]["detail"].as_str().or_else(|| part["detail"].as_str());
    Some((url, detail))
}

pub(super) fn extract_file_part(part: &Value) -> Option<(&str, Option<&str>, Option<&str>)> {
    let ty = part["type"].as_str().unwrap_or("");
    if ty != "file" && ty != "input_file" {
        return None;
    }
    let obj = if ty == "file" { &part["file"] } else { part };
    let data_or_url =
        obj["file_data"].as_str().or_else(|| obj["file_url"].as_str()).or_else(|| obj["url"].as_str()).filter(|s| !s.is_empty())?;
    let filename = obj["filename"].as_str();
    let mime = obj["mime_type"].as_str().or_else(|| obj["media_type"].as_str());
    Some((data_or_url, filename, mime))
}

fn is_text_document(mime: &str, filename: Option<&str>) -> bool {
    if mime.starts_with("text/")
        || matches!(
            mime,
            "application/json"
                | "application/xml"
                | "application/yaml"
                | "application/x-yaml"
                | "application/toml"
                | "application/javascript"
                | "application/typescript"
        )
    {
        return true;
    }
    let Some(name) = filename else { return false };
    let lower = name.to_ascii_lowercase();
    [
        ".txt", ".md", ".csv", ".json", ".yaml", ".yml", ".toml", ".xml", ".html", ".css", ".js", ".ts", ".jsx", ".tsx", ".rs", ".py",
        ".go", ".java", ".c", ".cpp", ".h", ".sh", ".sql", ".env", ".log",
    ]
    .iter()
    .any(|ext| lower.ends_with(ext))
}

/// Decodes base64-encoded plain-text / source-code file parts into normal text blocks
/// so every provider (including text-only models like DeepSeek/Groq/Kiro) can read them.
pub fn normalize_text_documents(body: &mut Value) {
    use base64::Engine as _;
    let Some(messages) = body.get_mut("messages").and_then(|m| m.as_array_mut()) else { return };
    for msg in messages {
        let Some(parts) = msg.get_mut("content").and_then(|c| c.as_array_mut()) else { continue };
        for part in parts.iter_mut() {
            let decoded = {
                let Some((data_or_url, filename, explicit_mime)) = extract_file_part(part) else { continue };
                if data_or_url.starts_with("http://") || data_or_url.starts_with("https://") || data_or_url.starts_with("gs://") {
                    continue;
                }
                let (mime, b64) = if let Some((m, d)) = parse_data_url(data_or_url) {
                    (m, d)
                } else {
                    (explicit_mime.unwrap_or_else(|| infer_mime(filename.unwrap_or(""), "application/octet-stream")), data_or_url)
                };
                if !is_text_document(mime, filename) {
                    continue;
                }
                let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(b64.trim()) else { continue };
                let Ok(text) = String::from_utf8(bytes) else { continue };
                match filename {
                    Some(name) if !name.is_empty() => format!("\n--- File: {name} ---\n{text}\n--- End of {name} ---\n"),
                    _ => text,
                }
            };
            *part = json!({ "type": "text", "text": decoded });
        }
    }
}

/// Non-text binary media requirements detected in an OpenAI chat request.
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaRequirements {
    pub has_image: bool,
    pub has_document: bool,
    pub has_video: bool,
    pub has_audio: bool,
}

impl MediaRequirements {
    pub fn any(self) -> bool {
        self.has_image || self.has_document || self.has_video || self.has_audio
    }
}

/// Scans `messages[].content` for binary media (images, PDFs/documents, video, audio).
pub fn detect_media_requirements(body: &Value) -> MediaRequirements {
    let mut req = MediaRequirements::default();
    let Some(messages) = body.get("messages").and_then(|m| m.as_array()) else { return req };
    for msg in messages {
        let Some(parts) = msg.get("content").and_then(|c| c.as_array()) else { continue };
        for p in parts {
            if extract_image_part(p).is_some() {
                req.has_image = true;
            } else if p["type"] == "video_url" {
                req.has_video = true;
            } else if p["type"] == "input_audio" {
                req.has_audio = true;
            } else if p["type"] == "document" || extract_file_part(p).is_some() {
                req.has_document = true;
            }
        }
    }
    req
}
