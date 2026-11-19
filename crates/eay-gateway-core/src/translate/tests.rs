use super::*;
use serde_json::json;

/// Feeds events in order and returns every frame produced, joined.
fn feed_all(t: &mut SseToOpenAi, events: &[Value]) -> String {
    events.iter().map(|e| t.feed(e).unwrap().concat()).collect()
}

mod openai_to_codex {
    use super::*;
    use serde_json::json;

    fn sample() -> Value {
        openai_to_codex(
            &json!({"messages":[{"role":"system","content":"You are Beacon AI."},{"role":"user","content":"Hello world"}]}),
            "gpt-5.6-terra",
        )
    }

    #[test]
    fn uses_the_upstream_model_name() {
        assert_eq!(sample()["model"], "gpt-5.6-terra");
    }

    #[test]
    fn moves_system_messages_into_instructions() {
        assert_eq!(sample()["instructions"], "You are Beacon AI.");
    }

    #[test]
    fn keeps_only_non_system_turns_as_input() {
        assert_eq!(sample()["input"], json!([{"type":"message","role":"user","content":[{"type":"input_text","text":"Hello world"}]}]));
    }
}

mod openai_to_anthropic {
    use super::*;
    use serde_json::json;

    fn sample() -> Value {
        openai_to_anthropic(
            &json!({"messages":[{"role":"system","content":"System prompt"},{"role":"user","content":"Ping"}]}),
            "claude-sonnet-5-5",
        )
    }

    #[test]
    fn moves_system_messages_into_the_system_field() {
        assert_eq!(sample()["system"], "System prompt");
    }

    #[test]
    fn keeps_only_non_system_turns_as_messages() {
        assert_eq!(sample()["messages"].as_array().map(Vec::len), Some(1));
    }
}

mod openai_to_claude_oauth {
    use super::*;
    use serde_json::json;

    fn sample() -> Value {
        openai_to_claude_oauth(
            &json!({"messages":[{"role":"system","content":"Be terse"},{"role":"user","content":"Ping"}]}),
            "claude-sonnet-5-5",
        )
    }

    #[test]
    fn leads_the_system_prompt_with_the_claude_code_identity() {
        assert_eq!(sample()["system"][0]["text"], CLAUDE_CODE_SYSTEM);
    }

    #[test]
    fn keeps_the_callers_system_prompt_after_the_identity() {
        assert_eq!(sample()["system"][1]["text"], "Be terse");
    }

    #[test]
    fn always_streams() {
        assert_eq!(sample()["stream"], true);
    }
}

mod openai_to_gemini {
    use super::*;
    use serde_json::json;

    #[test]
    fn merges_consecutive_same_role_turns() {
        let g = openai_to_gemini(
            &json!({"messages":[{"role":"user","content":"a"},{"role":"user","content":"b"}]}),
            "gemini-2.5-flash",
            Some("proj"),
        );
        assert_eq!(g["request"]["contents"].as_array().map(Vec::len), Some(1));
    }

    #[test]
    fn wraps_the_request_with_the_project() {
        let g = openai_to_gemini(&json!({"messages":[]}), "gemini-2.5-flash", Some("proj"));
        assert_eq!(g["project"], "proj");
    }
}

mod antigravity_envelope {
    use super::*;
    use serde_json::json;

    fn sample() -> Value {
        let mut b = openai_to_gemini(
            &json!({"max_tokens":200000,"messages":[{"role":"user","content":"a"},
            {"role":"assistant","tool_calls":[{"id":"1","function":{"name":"x","arguments":"{}"}},{"id":"2","function":{"name":"y","arguments":"{}"}}]}],
            "tools":[{"function":{"name":"my tool!","parameters":{}}}]}),
            "m",
            None,
        );
        antigravity_envelope(&mut b, "SIG");
        b
    }

    #[test]
    fn invents_a_project_when_none_is_known() {
        let b = sample();
        assert!(b["project"].as_str().is_some_and(|p| p.starts_with("useful-fuze-")), "project: {}", b["project"]);
    }

    #[test]
    fn caps_max_output_tokens_at_64k() {
        assert_eq!(sample()["request"]["generationConfig"]["maxOutputTokens"], 64000);
    }

    #[test]
    fn replaces_disallowed_characters_in_tool_names() {
        assert_eq!(sample()["request"]["tools"][0]["functionDeclarations"][0]["name"], "my_tool_");
    }

    #[test]
    fn signs_the_first_function_call_of_a_turn() {
        assert_eq!(sample()["request"]["contents"][1]["parts"][0]["thoughtSignature"], "SIG");
    }

    #[test]
    fn leaves_later_function_calls_unsigned() {
        assert!(sample()["request"]["contents"][1]["parts"][1].get("thoughtSignature").is_none());
    }
}

mod openai_to_commandcode {
    use super::*;
    use serde_json::json;

    fn sample() -> Value {
        openai_to_commandcode(
            &json!({"messages":[
                {"role":"system","content":"Be brief"},
                {"role":"user","content":"hi"},
                {"role":"assistant","content":"","tool_calls":[{"id":"c1","type":"function","function":{"name":"ls","arguments":"{\"p\":1}"}}]},
                {"role":"tool","tool_call_id":"c1","content":"ok"}
            ],"tools":[{"type":"function","function":{"name":"ls","parameters":{"type":"object"}}}]}),
            "deepseek/deepseek-v4-pro",
            "t-1",
        )
    }

    #[test]
    fn carries_the_thread_id() {
        assert_eq!(sample()["threadId"], "t-1");
    }

    #[test]
    fn moves_system_messages_into_params_system() {
        assert_eq!(sample()["params"]["system"], "Be brief");
    }

    #[test]
    fn parses_tool_call_arguments_into_objects() {
        assert_eq!(sample()["params"]["messages"][1]["content"][0]["input"]["p"], 1);
    }

    #[test]
    fn recovers_the_tool_name_for_tool_results_from_the_call_id() {
        assert_eq!(sample()["params"]["messages"][2]["content"][0]["toolName"], "ls");
    }

    #[test]
    fn converts_tools_to_input_schema() {
        assert_eq!(sample()["params"]["tools"][0]["input_schema"]["type"], "object");
    }
}

mod sse_claude {
    use super::*;
    use serde_json::json;

    #[test]
    fn text_deltas_become_content_chunks() {
        let mut t = SseToOpenAi::new("claude", "cc/x".into());
        let out = feed_all(&mut t, &[json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hi"}})]);
        assert!(out.contains("\"content\":\"Hi\""), "{out}");
    }

    #[test]
    fn tool_use_blocks_become_numbered_tool_calls() {
        let mut t = SseToOpenAi::new("claude", "cc/x".into());
        let out = feed_all(
            &mut t,
            &[json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"probe"}})],
        );
        assert!(out.contains("\"name\":\"probe\"") && out.contains("\"index\":0"), "{out}");
    }

    #[test]
    fn tool_use_stop_finishes_with_tool_calls_and_done() {
        let mut t = SseToOpenAi::new("claude", "cc/x".into());
        let out = feed_all(
            &mut t,
            &[
                json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"probe"}}),
                json!({"type":"message_delta","delta":{"stop_reason":"tool_use"}}),
            ],
        );
        assert!(out.contains("\"finish_reason\":\"tool_calls\"") && out.ends_with("data: [DONE]\n\n"), "{out}");
    }

    #[test]
    fn end_adds_nothing_after_the_stream_finished() {
        let mut t = SseToOpenAi::new("claude", "cc/x".into());
        feed_all(&mut t, &[json!({"type":"message_stop"})]);
        assert!(t.end().is_empty());
    }

    #[test]
    fn error_events_become_errors() {
        let mut t = SseToOpenAi::new("claude", "cc/x".into());
        assert!(t.feed(&json!({"type":"error","error":{"message":"overloaded"}})).is_err());
    }
}

mod sse_codex {
    use super::*;
    use serde_json::json;

    fn finished() -> (SseToOpenAi, String) {
        super::sse_codex_finished()
    }

    #[test]
    fn records_usage_from_response_completed() {
        assert_eq!(finished().0.usage, (7, 3));
    }

    #[test]
    fn end_adds_nothing_after_response_completed() {
        assert!(finished().0.end().is_empty());
    }

    #[test]
    fn response_failed_becomes_an_error() {
        let mut t = SseToOpenAi::new("codex", "cx/gpt-5".into());
        assert!(t.feed(&json!({"type":"response.failed","response":{"error":{"message":"boom"}}})).is_err());
    }
}

mod sse_commandcode {
    use super::*;
    use serde_json::json;

    fn new() -> SseToOpenAi {
        SseToOpenAi::new("commandcode", "cmc/x".into())
    }

    #[test]
    fn text_deltas_become_content_chunks() {
        let out = feed_all(&mut new(), &[json!({"type":"text-delta","text":"Hi"})]);
        assert!(out.contains("\"content\":\"Hi\""), "{out}");
    }

    #[test]
    fn reasoning_deltas_become_reasoning_content() {
        let out = feed_all(&mut new(), &[json!({"type":"reasoning-delta","text":"hmm"})]);
        assert!(out.contains("\"reasoning_content\":\"hmm\""), "{out}");
    }

    #[test]
    fn a_tool_call_after_streamed_input_is_not_emitted_twice() {
        let mut t = new();
        feed_all(
            &mut t,
            &[json!({"type":"tool-input-start","id":"k1","toolName":"ls"}), json!({"type":"tool-input-delta","id":"k1","delta":"{}"})],
        );
        assert!(t.feed(&json!({"type":"tool-call","toolCallId":"k1","toolName":"ls","input":{}})).unwrap().is_empty());
    }

    #[test]
    fn finish_after_tool_calls_reports_tool_calls() {
        let mut t = new();
        let out = feed_all(
            &mut t,
            &[json!({"type":"tool-input-start","id":"k1","toolName":"ls"}), json!({"type":"finish","finishReason":"tool-calls"})],
        );
        assert!(out.contains("\"finish_reason\":\"tool_calls\""), "{out}");
    }

    #[test]
    fn finish_records_total_usage() {
        let mut t = new();
        feed_all(&mut t, &[json!({"type":"finish","finishReason":"stop","totalUsage":{"inputTokens":9,"outputTokens":4}})]);
        assert_eq!(t.usage, (9, 4));
    }

    #[test]
    fn error_events_carry_the_upstream_message() {
        let err = new().feed(&json!({"type":"error","error":"quota"})).unwrap_err();
        assert!(err.contains("quota"), "{err}");
    }
}

mod cached_tokens {
    use super::*;
    use serde_json::json;

    #[test]
    fn claude_cache_reads_are_counted() {
        let mut t = SseToOpenAi::new("claude", "cc/x".into());
        feed_all(&mut t, &[json!({"type":"message_start","message":{"usage":{"input_tokens":4,"cache_read_input_tokens":6}}})]);
        assert_eq!((t.usage.0, t.cached), (10, 6));
    }

    #[test]
    fn openai_cached_prompt_tokens_are_counted() {
        let mut t = SseToOpenAi::new("openai", "oa/x".into());
        feed_all(
            &mut t,
            &[json!({"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":1,"prompt_tokens_details":{"cached_tokens":7}}})],
        );
        assert_eq!(t.cached, 7);
    }

    #[test]
    fn codex_cached_input_tokens_are_counted() {
        let mut t = SseToOpenAi::new("codex", "cx/x".into());
        feed_all(
            &mut t,
            &[
                json!({"type":"response.completed","response":{"usage":{"input_tokens":9,"output_tokens":1,"input_tokens_details":{"cached_tokens":5}}}}),
            ],
        );
        assert_eq!(t.cached, 5);
    }
}

mod sse_gemini {
    use super::*;
    use serde_json::json;

    fn out() -> String {
        let mut t = SseToOpenAi::new("gemini", "gc/x".into());
        feed_all(
            &mut t,
            &[
                json!({"response":{"candidates":[{"content":{"parts":[{"text":"yo"},{"functionCall":{"name":"f","args":{}}}]},"finishReason":"STOP"}]}}),
            ],
        )
    }

    #[test]
    fn text_parts_become_content() {
        assert!(out().contains("\"content\":\"yo\""));
    }

    #[test]
    fn function_calls_become_tool_calls() {
        assert!(out().contains("\"name\":\"f\""));
    }

    #[test]
    fn stop_after_a_function_call_reports_tool_calls() {
        assert!(out().contains("\"finish_reason\":\"tool_calls\""));
    }
}

mod sse_openai_passthrough {
    use super::*;
    use serde_json::json;

    #[test]
    fn records_usage_from_the_usage_chunk() {
        let mut t = SseToOpenAi::new("openai", "oa/gpt".into());
        feed_all(&mut t, &[json!({"choices":[],"usage":{"prompt_tokens":4,"completion_tokens":2}})]);
        assert_eq!(t.usage, (4, 2));
    }

    #[test]
    fn end_appends_done_exactly_once() {
        let mut t = SseToOpenAi::new("openai", "oa/gpt".into());
        let first = t.end();
        assert_eq!((first, t.end()), (vec!["data: [DONE]\n\n".to_string()], vec![]));
    }
}

mod aggregate_sse {
    use super::*;

    fn agg() -> Value {
        let (t, sse) = super::sse_codex_finished();
        aggregate_sse(&sse, t.usage)
    }

    #[test]
    fn joins_content_deltas() {
        assert_eq!(agg()["choices"][0]["message"]["content"], "Hello");
    }

    #[test]
    fn joins_tool_call_argument_deltas() {
        assert_eq!(agg()["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"], "{}");
    }

    #[test]
    fn keeps_the_finish_reason() {
        assert_eq!(agg()["choices"][0]["finish_reason"], "tool_calls");
    }

    #[test]
    fn totals_usage() {
        assert_eq!(agg()["usage"]["total_tokens"], 10);
    }
}

/// Shared fixture: a finished Codex stream with text, one tool call and usage.
fn sse_codex_finished() -> (SseToOpenAi, String) {
    let mut t = SseToOpenAi::new("codex", "cx/gpt-5".into());
    let mut sse = t.base.start_chunk();
    sse += &feed_all(
        &mut t,
        &[
            json!({"type":"response.output_text.delta","delta":"He"}),
            json!({"type":"response.output_text.delta","delta":"llo"}),
            json!({"type":"response.output_item.added","item":{"type":"function_call","id":"i1","call_id":"c1","name":"f"}}),
            json!({"type":"response.function_call_arguments.delta","item_id":"i1","delta":"{}"}),
            json!({"type":"response.completed","response":{"usage":{"input_tokens":7,"output_tokens":3}}}),
        ],
    );
    (t, sse)
}

mod multimodal {
    use super::*;
    use serde_json::json;

    #[test]
    fn anthropic_translates_images_and_pdf_documents() {
        let body = json!({
            "messages": [{
                "role": "user",
                "content": [
                    { "type": "text", "text": "Inspect these" },
                    { "type": "image_url", "image_url": { "url": "data:image/png;base64,AAA=" } },
                    { "type": "image_url", "image_url": { "url": "https://example.com/a.jpg" } },
                    { "type": "file", "file": { "file_data": "data:application/pdf;base64,JVBER", "filename": "spec.pdf" } }
                ]
            }]
        });
        let out = openai_to_anthropic(&body, "claude-sonnet-4-5");
        let blocks = out["messages"][0]["content"].as_array().unwrap();
        assert_eq!(blocks[0], json!({ "type": "text", "text": "Inspect these" }));
        assert_eq!(blocks[1], json!({ "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": "AAA=" } }));
        assert_eq!(blocks[2], json!({ "type": "image", "source": { "type": "url", "url": "https://example.com/a.jpg" } }));
        assert_eq!(
            blocks[3],
            json!({ "type": "document", "source": { "type": "base64", "media_type": "application/pdf", "data": "JVBER" } })
        );
    }

    #[test]
    fn gemini_translates_image_video_audio_and_document() {
        let body = json!({
            "messages": [{
                "role": "user",
                "content": [
                    { "type": "text", "text": "Analyze" },
                    { "type": "image_url", "image_url": { "url": "data:image/jpeg;base64,IMG" } },
                    { "type": "video_url", "video_url": { "url": "https://example.com/clip.mp4" } },
                    { "type": "input_audio", "input_audio": { "data": "AUD", "format": "wav" } },
                    { "type": "file", "file": { "file_data": "RAWPDF", "filename": "report.pdf" } }
                ]
            }]
        });
        let out = openai_to_gemini(&body, "gemini-2.5-pro", Some("p1"));
        let parts = out["request"]["contents"][0]["parts"].as_array().unwrap();
        assert_eq!(parts[0], json!({ "text": "Analyze" }));
        assert_eq!(parts[1], json!({ "inlineData": { "mimeType": "image/jpeg", "data": "IMG" } }));
        assert_eq!(parts[2], json!({ "fileData": { "mimeType": "video/mp4", "fileUri": "https://example.com/clip.mp4" } }));
        assert_eq!(parts[3], json!({ "inlineData": { "mimeType": "audio/wav", "data": "AUD" } }));
        assert_eq!(parts[4], json!({ "inlineData": { "mimeType": "application/pdf", "data": "RAWPDF" } }));
    }

    #[test]
    fn codex_translates_images_and_files() {
        let body = json!({
            "messages": [{
                "role": "user",
                "content": [
                    { "type": "text", "text": "Check" },
                    { "type": "image_url", "image_url": { "url": "data:image/png;base64,XYZ", "detail": "high" } },
                    { "type": "file", "file": { "file_data": "data:application/pdf;base64,PDF", "filename": "a.pdf" } }
                ]
            }]
        });
        let out = openai_to_codex(&body, "gpt-5");
        let content = out["input"][0]["content"].as_array().unwrap();
        assert_eq!(content[0], json!({ "type": "input_text", "text": "Check" }));
        assert_eq!(content[1], json!({ "type": "input_image", "image_url": "data:image/png;base64,XYZ", "detail": "high" }));
        assert_eq!(content[2], json!({ "type": "input_file", "file_data": "data:application/pdf;base64,PDF", "filename": "a.pdf" }));
    }

    #[test]
    fn decodes_text_documents_inline_while_keeping_pdfs_binary() {
        use base64::Engine as _;
        let code_b64 = base64::engine::general_purpose::STANDARD.encode("fn main() {}\n");
        let mut body = json!({
            "messages": [{
                "role": "user",
                "content": [
                    { "type": "file", "file": { "file_data": format!("data:text/plain;base64,{code_b64}"), "filename": "main.rs" } },
                    { "type": "file", "file": { "file_data": "data:application/pdf;base64,JVBER", "filename": "doc.pdf" } }
                ]
            }]
        });
        normalize_text_documents(&mut body);
        let parts = body["messages"][0]["content"].as_array().unwrap();
        assert_eq!(parts[0]["type"], "text");
        assert!(parts[0]["text"].as_str().unwrap().contains("fn main() {}"));
        assert_eq!(parts[1]["type"], "file");

        let req = detect_media_requirements(&body);
        assert!(req.has_document);
        assert!(!req.has_image);
    }

    #[test]
    fn text_only_file_leaves_no_binary_media_requirement() {
        use base64::Engine as _;
        let txt_b64 = base64::engine::general_purpose::STANDARD.encode("hello world");
        let mut body = json!({
            "messages": [{
                "role": "user",
                "content": [
                    { "type": "file", "file": { "file_data": txt_b64, "filename": "notes.md" } }
                ]
            }]
        });
        normalize_text_documents(&mut body);
        assert!(!detect_media_requirements(&body).any());
    }
}

mod anthropic_native {
    use super::*;

    #[test]
    fn converts_request_with_system_image_and_tool_result() {
        let req = json!({
            "model": "claude-sonnet-4-5",
            "system": "Be concise.",
            "max_tokens": 1024,
            "messages": [
                { "role": "user", "content": [
                    { "type": "text", "text": "See image" },
                    { "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": "AAA=" } }
                ]},
                { "role": "assistant", "content": [
                    { "type": "tool_use", "id": "tu_1", "name": "ls", "input": { "path": "." } }
                ]},
                { "role": "user", "content": [
                    { "type": "tool_result", "tool_use_id": "tu_1", "content": "main.rs" }
                ]}
            ]
        });
        let oa = anthropic_to_openai(&req);
        let msgs = oa["messages"].as_array().unwrap();
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[1]["content"][1]["image_url"]["url"], "data:image/png;base64,AAA=");
        assert_eq!(msgs[2]["tool_calls"][0]["function"]["name"], "ls");
        assert_eq!(msgs[3], json!({ "role": "tool", "tool_call_id": "tu_1", "content": "main.rs" }));
    }

    #[test]
    fn converts_openai_completion_to_anthropic_message() {
        let oa = json!({
            "id": "chatcmpl-1",
            "model": "oa/gpt-4o",
            "choices": [{ "message": { "role": "assistant", "content": "Hi!" }, "finish_reason": "stop" }],
            "usage": { "prompt_tokens": 12, "completion_tokens": 3 }
        });
        let ant = openai_completion_to_anthropic(&oa, "claude-sonnet-4-5");
        assert_eq!(ant["type"], "message");
        assert_eq!(ant["stop_reason"], "end_turn");
        assert_eq!(ant["content"][0], json!({ "type": "text", "text": "Hi!" }));
        assert_eq!(ant["usage"]["input_tokens"], 12);
        assert_eq!(ant["usage"]["output_tokens"], 3);
    }

    #[test]
    fn converts_openai_sse_to_anthropic_sse() {
        let mut conv = OpenAiToAnthropicSse::new("claude-sonnet-4-5".into());
        let out = conv.feed_sse_chunk(
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
        );
        assert!(out.contains("event: message_start"));
        assert!(out.contains("event: content_block_delta"));
        assert!(out.contains("\"text\":\"Hel\""));
        assert!(out.contains("event: message_stop"));
    }
}

mod responses_native {
    use super::*;

    #[test]
    fn converts_responses_request_and_completion_roundtrip() {
        let req = json!({
            "model": "auto",
            "instructions": "Be brief.",
            "input": [{ "role": "user", "content": [{ "type": "input_text", "text": "Hello" }] }],
            "max_output_tokens": 500
        });
        let oa = responses_to_openai(&req);
        assert_eq!(oa["messages"][0], json!({ "role": "system", "content": "Be brief." }));
        assert_eq!(oa["messages"][1]["content"][0]["text"], "Hello");
        assert_eq!(oa["max_tokens"], 500);

        let oa_resp = json!({
            "model": "oa/gpt-4o",
            "choices": [{ "message": { "role": "assistant", "content": "World" } }],
            "usage": { "prompt_tokens": 5, "completion_tokens": 1, "total_tokens": 6 }
        });
        let resp = openai_completion_to_responses(&oa_resp, "auto");
        assert_eq!(resp["object"], "response");
        assert_eq!(resp["output"][0]["content"][0]["text"], "World");
        assert_eq!(resp["usage"]["total_tokens"], 6);
    }
}

mod openai_to_responses_sse {
    use super::super::OpenAiToResponsesSse;
    use serde_json::{json, Value};

    fn events(out: &str) -> Vec<Value> {
        out.lines().filter_map(|l| l.strip_prefix("data: ")).map(|d| serde_json::from_str(d).unwrap()).collect()
    }
    fn chunk(v: Value) -> String {
        format!("data: {v}\n\n")
    }

    #[test]
    fn streams_text_as_output_text_deltas_and_completes() {
        let mut c = OpenAiToResponsesSse::new("auto".into());
        let mut out = c.feed_sse_chunk(&chunk(json!({"choices":[{"delta":{"content":"Hel"}}]})));
        out += &c.feed_sse_chunk(&chunk(json!({"choices":[{"delta":{"content":"lo"}}]})));
        out += &c.feed_sse_chunk(&chunk(json!({"choices":[],"usage":{"prompt_tokens":3,"completion_tokens":2}})));
        out += &c.feed_sse_chunk("data: [DONE]\n\n");
        let ev = events(&out);
        let types: Vec<&str> = ev.iter().map(|e| e["type"].as_str().unwrap()).collect();
        assert_eq!(types[0], "response.created");
        assert_eq!(
            ev.iter().filter(|e| e["type"] == "response.output_text.delta").map(|e| e["delta"].as_str().unwrap()).collect::<String>(),
            "Hello"
        );
        let done = ev.last().unwrap();
        assert_eq!(done["type"], "response.completed");
        assert_eq!(done["response"]["output"][0]["content"][0]["text"], "Hello");
        assert_eq!(done["response"]["usage"]["total_tokens"], 5);
        // sequence numbers strictly increase
        assert!(ev.windows(2).all(|w| w[0]["sequence_number"].as_u64() < w[1]["sequence_number"].as_u64()));
    }

    #[test]
    fn streams_tool_calls_as_function_call_items() {
        let mut c = OpenAiToResponsesSse::new("auto".into());
        let mut out = c.feed_sse_chunk(&chunk(
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"shell","arguments":"{\"cmd\":"}}]}}]}),
        ));
        out += &c.feed_sse_chunk(&chunk(
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"ls\"}"}}]},"finish_reason":"tool_calls"}]}),
        ));
        out += &c.finish();
        let ev = events(&out);
        let added = ev.iter().find(|e| e["type"] == "response.output_item.added").unwrap();
        assert_eq!(added["item"]["type"], "function_call");
        assert_eq!(added["item"]["name"], "shell");
        let item = &ev.last().unwrap()["response"]["output"][0];
        assert_eq!(item["call_id"], "call_1");
        assert_eq!(item["arguments"], "{\"cmd\":\"ls\"}");
    }

    #[test]
    fn finishing_twice_emits_completed_once() {
        let mut c = OpenAiToResponsesSse::new("m".into());
        let out = c.feed_sse_chunk("data: [DONE]\n\n") + &c.finish();
        assert_eq!(events(&out).iter().filter(|e| e["type"] == "response.completed").count(), 1);
    }
}

mod responses_request_to_openai {
    use super::super::responses_to_openai;
    use serde_json::json;

    #[test]
    fn carries_function_tools_and_drops_builtins() {
        let out = responses_to_openai(&json!({
            "model": "auto", "input": "hi", "tool_choice": { "type": "function", "name": "shell" },
            "tools": [
                { "type": "function", "name": "shell", "description": "run", "parameters": { "type": "object", "properties": { "cmd": { "type": "string" } } } },
                { "type": "web_search" }
            ]
        }));
        assert_eq!(out["tools"].as_array().unwrap().len(), 1);
        assert_eq!(out["tools"][0]["function"]["name"], "shell");
        assert_eq!(out["tools"][0]["function"]["parameters"]["properties"]["cmd"]["type"], "string");
        assert_eq!(out["tool_choice"]["function"]["name"], "shell");
    }

    #[test]
    fn replays_a_tool_round_trip_from_the_input_history() {
        let out = responses_to_openai(&json!({ "model": "auto", "input": [
            { "role": "user", "content": "list files" },
            { "type": "reasoning", "summary": [] },
            { "type": "function_call", "call_id": "c1", "name": "shell", "arguments": "{\"cmd\":\"ls\"}" },
            { "type": "function_call", "call_id": "c2", "name": "shell", "arguments": "{\"cmd\":\"pwd\"}" },
            { "type": "function_call_output", "call_id": "c1", "output": "a.txt" },
            { "type": "function_call_output", "call_id": "c2", "output": "/tmp" }
        ]}));
        let m = out["messages"].as_array().unwrap();
        assert_eq!(m.len(), 4, "{out}");
        assert_eq!(m[1]["role"], "assistant");
        assert_eq!(m[1]["tool_calls"].as_array().unwrap().len(), 2);
        assert_eq!(m[1]["tool_calls"][1]["id"], "c2");
        assert_eq!(m[2]["role"], "tool");
        assert_eq!(m[2]["tool_call_id"], "c1");
    }
}
