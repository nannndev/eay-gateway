use serde_json::{json, Value};

/// Masks common leaked API keys/tokens in user messages before forwarding upstream.
pub fn redact_secrets_in_body(body: &mut Value) {
    let Some(msgs) = body.get_mut("messages").and_then(|m| m.as_array_mut()) else { return };
    for m in msgs {
        if m["role"] == "system" || m["role"] == "developer" {
            continue;
        }
        match m.get_mut("content") {
            Some(Value::String(s)) => *s = redact_text(s),
            Some(Value::Array(parts)) => {
                for p in parts {
                    if let Some(t) = p.get_mut("text").and_then(|v| v.as_str().map(redact_text)) {
                        p["text"] = json!(t);
                    }
                }
            }
            _ => {}
        }
    }
}

fn redact_text(input: &str) -> String {
    const PREFIXES: &[&str] = &["sk-proj-", "sk-ant-", "sk-", "ghp_", "gho_", "AIza", "AKIA", "xoxb-", "gw-sk-"];
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while !rest.is_empty() {
        let earliest = PREFIXES.iter().filter_map(|p| rest.find(p).map(|idx| (idx, *p))).min_by_key(|(idx, _)| *idx);
        let Some((idx, prefix)) = earliest else {
            out.push_str(rest);
            break;
        };
        out.push_str(&rest[..idx]);
        let after = &rest[idx..];
        let tok_len = after.find(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_').unwrap_or(after.len());
        if tok_len >= prefix.len() + 8 {
            out.push_str(prefix);
            out.push_str("[REDACTED]");
            rest = &after[tok_len..];
        } else {
            out.push_str(prefix);
            rest = &after[prefix.len()..];
        }
    }
    out
}

/// Trims older non-system messages when total character count exceeds `max_chars`,
/// always preserving system instructions and the latest message.
pub fn trim_context_messages(body: &mut Value, max_chars: usize) {
    if max_chars == 0 {
        return;
    }
    let Some(msgs) = body.get_mut("messages").and_then(|m| m.as_array_mut()) else { return };
    let msg_len = |m: &Value| m["content"].to_string().len();
    let total: usize = msgs.iter().map(msg_len).sum();
    if total <= max_chars || msgs.len() <= 1 {
        return;
    }

    let mut sys = Vec::new();
    let mut rest = Vec::new();
    for m in msgs.drain(..) {
        if matches!(m["role"].as_str(), Some("system" | "developer")) {
            sys.push(m);
        } else {
            rest.push(m);
        }
    }
    let sys_chars: usize = sys.iter().map(msg_len).sum();
    let budget = max_chars.saturating_sub(sys_chars);

    let mut kept_rev = Vec::new();
    let mut used = 0usize;
    for (i, m) in rest.into_iter().rev().enumerate() {
        let len = msg_len(&m);
        if i == 0 || used + len <= budget {
            used += len;
            kept_rev.push(m);
        } else {
            break;
        }
    }
    kept_rev.reverse();
    msgs.extend(sys);
    msgs.extend(kept_rev);
}
