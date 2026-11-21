use super::activity::ActivityHub;
use crate::store::{GatewayStore, UsageEntry};
use crate::translate::SseToOpenAi;
use bytes::Bytes;
use futures::{stream::BoxStream, StreamExt};
use serde_json::{json, Value};
use std::sync::Arc;

/// Server-sent `chat.completion.chunk` frames, ending with `data: [DONE]`.
pub type ChatStream = BoxStream<'static, Result<Bytes, std::io::Error>>;

/// First in-band error frame of a translated stream, if any.
pub(super) fn sse_error(sse: &str) -> Option<String> {
    sse.lines()
        .filter_map(|l| l.trim().strip_prefix("data:"))
        .filter_map(|d| serde_json::from_str::<Value>(d.trim()).ok())
        .find_map(|v| v["error"]["message"].as_str().map(String::from))
}

/// Records the request's usage once its response stream is finished or dropped (client disconnect).
pub(super) struct UsageGuard {
    pub(super) store: Arc<GatewayStore>,
    pub(super) tr: Arc<parking_lot::Mutex<SseToOpenAi>>,
    pub(super) activity: ActivityHub,
    pub(super) run_id: String,
    pub(super) parent_id: Option<String>,
    pub(super) agent: Option<String>,
    pub(super) target: String,
    pub(super) key_id: Option<String>,
    pub(super) key_name: String,
    pub(super) provider: String,
    pub(super) model: String,
    /// Agent, roster and project written on the usage row. `agent` above is only the activity label.
    pub(super) tags: super::UsageTags,
    pub(super) t0: std::time::Instant,
    pub(super) streaming: bool,
}

impl Drop for UsageGuard {
    fn drop(&mut self) {
        let (inp, out, cached) = {
            let t = self.tr.lock();
            (t.usage.0, t.usage.1, t.cached)
        };
        let duration_ms = self.t0.elapsed().as_millis() as i64;
        self.store.record_usage(&UsageEntry {
            key_id: self.key_id.as_deref(),
            key_name: Some(&self.key_name),
            provider: Some(&self.provider),
            model: Some(&self.model),
            input_tokens: inp,
            output_tokens: out,
            cached_tokens: cached,
            duration_ms,
            status: "success",
            status_code: 200,
            error: None,
            streaming: self.streaming,
            agent: self.tags.agent.as_deref(),
            team: self.tags.team.as_deref(),
            project: self.tags.project.as_deref(),
            source: self.tags.source.as_deref(),
        });
        if let Some(id) = &self.key_id {
            self.store.touch_virtual_key(id, inp, out);
        }
        self.activity.emit_with_meta(
            &self.run_id,
            self.parent_id.as_deref(),
            self.agent.as_deref(),
            "completed",
            format!("Completed ({} tokens, {} ms)", inp + out, duration_ms),
            Some(&self.target),
            None,
            Some(super::activity::ActivityMeta { duration_ms: Some(duration_ms), ..Default::default() }),
        );
    }
}

fn emit_stream_activity(frames: &[String], last_phase: &mut String, guard: &UsageGuard) {
    for frame in frames {
        let Some(data) = frame.trim().strip_prefix("data:") else { continue };
        let Ok(ev) = serde_json::from_str::<Value>(data.trim()) else { continue };
        let delta = &ev["choices"][0]["delta"];
        if let Some(tcs) = delta["tool_calls"].as_array() {
            for tc in tcs {
                if let Some(name) = tc["function"]["name"].as_str().filter(|s| !s.is_empty()) {
                    *last_phase = format!("tool:{name}");
                    guard.activity.emit_phase(
                        &guard.run_id,
                        guard.parent_id.as_deref(),
                        guard.agent.as_deref(),
                        "calling_tool",
                        format!("Calling tool `{name}`..."),
                        Some(&guard.target),
                        Some(name),
                    );
                }
            }
        }
        if delta["reasoning_content"].as_str().is_some_and(|s| !s.is_empty()) && *last_phase != "thinking" {
            *last_phase = "thinking".into();
            guard.activity.emit_phase(
                &guard.run_id,
                guard.parent_id.as_deref(),
                guard.agent.as_deref(),
                "thinking",
                "Thinking...",
                Some(&guard.target),
                None,
            );
        }
        if delta["content"].as_str().is_some_and(|s| !s.is_empty()) && *last_phase != "generating" {
            *last_phase = "generating".into();
            guard.activity.emit_phase(
                &guard.run_id,
                guard.parent_id.as_deref(),
                guard.agent.as_deref(),
                "generating",
                "Generating response...",
                Some(&guard.target),
                None,
            );
        }
    }
}

/// Kiro replies with a binary AWS event stream instead of SSE.
fn kiro_frames(upstream: reqwest::Response, tr: Arc<parking_lot::Mutex<SseToOpenAi>>, guard: UsageGuard, start: String) -> ChatStream {
    let state = Arc::new(parking_lot::Mutex::new(crate::kiro::KiroStream::default()));
    let (tr_end, state_end) = (tr.clone(), state.clone());
    let events = upstream.bytes_stream().map(move |item| {
        let bytes = item.map_err(std::io::Error::other)?;
        let mut t = tr.lock();
        let out = match state.lock().push(&mut t, &bytes) {
            Ok(frames) => frames.concat(),
            Err(e) => format!("data: {}\n\n", json!({ "error": { "message": e } })),
        };
        Ok(Bytes::from(out))
    });
    let tail = futures::stream::once(async move {
        let frames = state_end.lock().finish(&mut tr_end.lock()).concat();
        drop(guard);
        Ok(Bytes::from(frames))
    });
    futures::stream::once(async move { Ok(Bytes::from(start)) }).chain(events).chain(tail).boxed()
}

/// Turns an upstream SSE body into OpenAI chat.completion.chunk frames, ending with [DONE].
pub(super) fn sse_frames(upstream: reqwest::Response, tr: Arc<parking_lot::Mutex<SseToOpenAi>>, guard: UsageGuard) -> ChatStream {
    let start = {
        let t = tr.lock();
        if t.kind_passthrough() {
            String::new()
        } else {
            t.base.start_chunk()
        }
    };
    let tr_end = tr.clone();
    if tr.lock().is_kiro() {
        return kiro_frames(upstream, tr, guard, start);
    }
    let mut buf = String::new();
    let mut last_phase = String::new();
    let guard = Arc::new(parking_lot::Mutex::new(Some(guard)));
    let guard_ev = guard.clone();
    let events = upstream.bytes_stream().map(move |item| {
        let bytes = item.map_err(std::io::Error::other)?;
        buf.push_str(&String::from_utf8_lossy(&bytes));
        let mut out = String::new();
        let mut t = tr.lock();
        // SSE events can span network chunks: only consume complete lines.
        while let Some(i) = buf.find('\n') {
            let line: String = buf.drain(..=i).collect();
            if let Some(data) = line.trim().strip_prefix("data:") {
                if let Ok(ev) = serde_json::from_str::<Value>(data.trim()) {
                    match t.feed(&ev) {
                        Ok(frames) => {
                            if let Some(g) = guard_ev.lock().as_ref() {
                                emit_stream_activity(&frames, &mut last_phase, g);
                            }
                            out.extend(frames);
                        }
                        Err(e) => out.push_str(&format!("data: {}\n\n", json!({ "error": { "message": e } }))),
                    }
                }
            }
        }
        Ok(Bytes::from(out))
    });
    let tail = futures::stream::once(async move {
        let frames = tr_end.lock().end().concat();
        drop(guard.lock().take());
        Ok(Bytes::from(frames))
    });
    futures::stream::once(async move { Ok(Bytes::from(start)) }).chain(events).chain(tail).boxed()
}
