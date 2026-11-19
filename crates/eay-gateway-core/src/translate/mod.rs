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
