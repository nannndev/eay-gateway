mod anthropic;
mod codex;
mod commandcode;
mod gemini;
mod media;
mod sse;

#[cfg(test)]
mod tests;

pub use anthropic::{
    anthropic_to_openai, openai_completion_to_anthropic, openai_to_anthropic, openai_to_claude_oauth, OpenAiToAnthropicSse,
    CLAUDE_CODE_SYSTEM,
};
pub use codex::{openai_completion_to_responses, openai_to_codex, responses_to_openai, CodexStreamTranslator, OpenAiToResponsesSse};
pub use commandcode::openai_to_commandcode;
pub use gemini::{antigravity_envelope, openai_to_gemini};
pub use media::{detect_media_requirements, normalize_text_documents, MediaRequirements};
pub use sse::{aggregate_sse, SseToOpenAi};

use serde_json::Value;
use std::time::{SystemTime, UNIX_EPOCH};

/// Drops fields only the gateway adds (Claude thinking replay) before a body is sent as-is to an
/// OpenAI-compatible upstream; strict providers reject unknown fields. Client fields are kept.
pub fn strip_gateway_fields(body: &mut Value) {
    if let Some(o) = body.as_object_mut() {
        o.remove("thinking_budget_tokens");
    }
    for m in body["messages"].as_array_mut().into_iter().flatten() {
        if let Some(o) = m.as_object_mut() {
            o.remove("thinking_blocks");
        }
    }
}

/// Reasoning effort a request asks for, in OpenAI terms (`minimal`/`low`/`medium`/`high`): from
/// `reasoning_effort` or `reasoning.effort`. Anthropic `thinking` is mapped to this when a request
/// enters through `/v1/messages`, so every upstream reads one field.
pub fn reasoning_effort(body: &Value) -> Option<&str> {
    body["reasoning_effort"].as_str().or(body["reasoning"]["effort"].as_str()).filter(|e| !e.is_empty() && *e != "none")
}

/// Anthropic `thinking.budget_tokens` → effort level (and back), using Claude Code's own steps
/// (think ≈ 4k, think hard ≈ 10k, ultrathink ≈ 32k).
pub fn budget_to_effort(budget: u64) -> &'static str {
    match budget {
        0..=2048 => "minimal",
        2049..=6000 => "low",
        6001..=16000 => "medium",
        _ => "high",
    }
}

pub fn effort_to_budget(effort: &str) -> u64 {
    match effort {
        "minimal" => 1024,
        "low" => 4096,
        "medium" => 10_000,
        _ => 32_000,
    }
}

pub(super) fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

pub(super) fn extract_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(arr) => {
            let mut out = String::new();
            for part in arr {
                if let Some(txt) = part.get("text").and_then(|t| t.as_str()) {
                    out.push_str(txt);
                }
            }
            out
        }
        _ => String::new(),
    }
}
