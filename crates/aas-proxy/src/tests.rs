//! Unit + integration tests for the proxy port. Covers the SSE framer, tool accumulator, retry
//! decision logic, per-adapter wire translation, model resolution, and the §H streaming invariants.

use crate::adapters::claude::{ClaudeAgent, ClaudeBackend};
use crate::adapters::codex::{CodexAgent, CodexBackend};
use crate::adapters::grok::{GrokAgent, GrokBackend};
use crate::adapters::zai::{is_zai_overload, ZaiBackend};
use crate::models::{backend_choices, resolve_choice};
use crate::retry::{backoff_ms, classify_body, is_retryable_fetch_error, should_return_stream, BodyDecision};
use crate::sse::{SseFramer, ToolAccumulator};
use crate::types::{AgentAdapter, BackendAdapter, CommonEvent, CommonMessage, CommonRequest, StreamCtx};
use serde_json::{json, Value};
use std::sync::Mutex;

// Serializes tests that read/write the process-global model env (`ASX_*_MODELS`, `ASX_MODELS_CONFIG`).
static MODEL_LOCK: Mutex<()> = Mutex::new(());

fn lock_models() -> std::sync::MutexGuard<'static, ()> {
    MODEL_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Clear model env so `backend_choices` falls back to built-in defaults (point the config file at a
/// path that does not exist so a real `~/.../asx/models.json` on the dev machine can't leak in).
fn reset_model_env() {
    for p in ["CODEX", "ZAI", "CLAUDE", "GROK"] {
        std::env::remove_var(format!("ASX_{p}_MODELS"));
    }
    std::env::set_var("ASX_MODELS_CONFIG", "/nonexistent/aas-proxy-test-models.json");
}

fn header<'a>(headers: &'a [(String, String)], key: &str) -> Option<&'a str> {
    headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v.as_str())
}

fn user(text: &str) -> CommonMessage {
    CommonMessage { role: "user".into(), content: text.into(), ..Default::default() }
}

// ---------------------------------------------------------------------------
// SSE framer
// ---------------------------------------------------------------------------

#[test]
fn framer_splits_blocks_across_reads() {
    let mut f = SseFramer::new();
    assert_eq!(f.feed(b"data: a\n\ndata: "), vec!["data: a".to_string()]);
    assert!(f.feed(b"b").is_empty());
    assert_eq!(f.feed(b"\n\n"), vec!["data: b".to_string()]);
}

#[test]
fn framer_normalizes_crlf() {
    let mut f = SseFramer::new();
    assert_eq!(f.feed(b"data: x\r\n\r\n"), vec!["data: x".to_string()]);
}

#[test]
fn framer_crlf_split_across_reads() {
    let mut f = SseFramer::new();
    // A CRLF blank-line separator split across two reads must still normalize + frame.
    assert!(f.feed(b"data: y\r").is_empty());
    assert_eq!(f.feed(b"\n\r\ndata: z\r\n\r\n"), vec!["data: y".to_string(), "data: z".to_string()]);
}

#[test]
fn framer_trailing_block_without_final_blank() {
    let mut f = SseFramer::new();
    assert_eq!(f.feed(b"data: a\n\ndata: b"), vec!["data: a".to_string()]);
    // No terminating blank line — finish() emits the trailing block.
    assert_eq!(f.finish(), vec!["data: b".to_string()]);
}

#[test]
fn framer_multibyte_char_split_across_reads() {
    let s = "data: 😀\n\n"; // emoji is 4 UTF-8 bytes at offset 6..10
    let bytes = s.as_bytes();
    let mut f = SseFramer::new();
    assert!(f.feed(&bytes[..8]).is_empty()); // splits the emoji mid-sequence
    assert_eq!(f.feed(&bytes[8..]), vec!["data: 😀".to_string()]);
}

#[test]
fn framer_finish_ignores_blank_trailing() {
    let mut f = SseFramer::new();
    assert_eq!(f.feed(b"data: a\n\n"), vec!["data: a".to_string()]);
    assert!(f.finish().is_empty()); // only whitespace remains
}

// ---------------------------------------------------------------------------
// Tool accumulator
// ---------------------------------------------------------------------------

fn delta(index: i64, id: Option<&str>, name: Option<&str>, args: Option<&str>) -> CommonEvent {
    CommonEvent::ToolCallDelta {
        index,
        id: id.map(|s| s.to_string()),
        name: name.map(|s| s.to_string()),
        args_delta: args.map(|s| s.to_string()),
    }
}

#[test]
fn accumulator_merges_by_index_preserving_order() {
    let mut acc = ToolAccumulator::new();
    acc.push(&delta(0, Some("a"), Some("f"), None));
    acc.push(&delta(1, Some("b"), Some("g"), None));
    acc.push(&delta(0, None, None, Some("{\"x\"")));
    acc.push(&delta(0, None, None, Some(":1}")));
    acc.push(&delta(1, None, None, Some("{}")));
    let list = acc.list();
    assert_eq!(
        list,
        vec![
            CommonEvent::ToolCall { id: "a".into(), name: "f".into(), arguments: "{\"x\":1}".into() },
            CommonEvent::ToolCall { id: "b".into(), name: "g".into(), arguments: "{}".into() },
        ]
    );
}

#[test]
fn accumulator_first_seen_order_even_when_later_index_first() {
    let mut acc = ToolAccumulator::new();
    acc.push(&delta(2, Some("second"), Some("y"), Some("{}")));
    acc.push(&delta(1, Some("first"), Some("x"), Some("{}")));
    let ids: Vec<String> = acc.list().into_iter().map(|e| match e { CommonEvent::ToolCall { id, .. } => id, _ => unreachable!() }).collect();
    assert_eq!(ids, vec!["second".to_string(), "first".to_string()]);
    acc.clear();
    assert!(acc.is_empty());
}

// ---------------------------------------------------------------------------
// Retry decision logic (pure) — reproduces the §H named scenarios
// ---------------------------------------------------------------------------

#[test]
fn retry_zai_1305_is_retried_even_at_200() {
    // z.ai returns HTTP 200 with an overload code in the body.
    let body = r#"{"error":{"code":"1305","message":"temporarily overloaded"}}"#;
    assert!(is_zai_overload(body));
    assert_eq!(classify_body(200, is_zai_overload(body), 0, 4), BodyDecision::Retry);
}

#[test]
fn retry_503_is_retried() {
    assert_eq!(classify_body(503, false, 0, 4), BodyDecision::Retry);
}

#[test]
fn retry_400_and_401_are_fatal() {
    assert_eq!(classify_body(400, false, 0, 4), BodyDecision::Fatal);
    assert_eq!(classify_body(401, false, 0, 4), BodyDecision::Fatal);
    // Even if a backend hook says retryable, a fatal status wins.
    assert_eq!(classify_body(400, true, 0, 4), BodyDecision::Fatal);
}

#[test]
fn retry_gives_up_when_out_of_attempts() {
    assert_eq!(classify_body(503, false, 4, 4), BodyDecision::GiveUp);
    // Non-retryable, non-fatal status (e.g. 418) also gives up.
    assert_eq!(classify_body(418, false, 0, 4), BodyDecision::GiveUp);
}

#[test]
fn retry_all_zai_overload_codes_and_regex() {
    for code in ["1301", "1302", "1304", "1305"] {
        assert!(is_zai_overload(&format!("{{\"code\":\"{code}\"}}")), "code {code}");
    }
    assert!(is_zai_overload("server is overloaded"));
    assert!(is_zai_overload("Too Many Requests"));
    assert!(is_zai_overload("please try again later"));
    assert!(!is_zai_overload(""));
    assert!(!is_zai_overload("all good"));
    // The zai backend delegates its body hook to is_zai_overload.
    assert!(ZaiBackend.is_retryable(200, "overloaded"));
    assert!(ZaiBackend.has_is_retryable());
}

#[test]
fn should_return_stream_matrix() {
    // event-stream 200 -> stream immediately (no body read), regardless of backend hook.
    assert!(should_return_stream(true, "text/event-stream", true));
    assert!(should_return_stream(true, "text/event-stream; charset=utf-8", false));
    // 200 non-stream with a body hook (zai) -> must inspect the body first.
    assert!(!should_return_stream(true, "application/json", true));
    // 200 non-stream with no body hook (claude/codex/grok) -> stream anyway.
    assert!(should_return_stream(true, "application/json", false));
    // Not ok -> never a happy stream.
    assert!(!should_return_stream(false, "text/event-stream", false));
}

#[test]
fn network_error_retry_classification() {
    assert!(is_retryable_fetch_error("connection reset by peer"));
    assert!(is_retryable_fetch_error("operation timed out"));
    assert!(!is_retryable_fetch_error("invalid api key"));
    assert!(!is_retryable_fetch_error("invalid url"));
    assert!(!is_retryable_fetch_error("403 forbidden"));
    assert!(!is_retryable_fetch_error("auth error"));
    assert!(!is_retryable_fetch_error("certificate has expired")); // "cert"
    assert!(!is_retryable_fetch_error("hostname mismatch"));
}

#[test]
fn backoff_grows_and_caps() {
    assert_eq!(backoff_ms(1, 0), 500);
    assert_eq!(backoff_ms(2, 0), 1000);
    assert_eq!(backoff_ms(3, 0), 2000);
    assert_eq!(backoff_ms(7, 0), 30_000); // 500*64=32000 -> capped at 30000
    assert_eq!(backoff_ms(2, 123), 1123); // jitter added
}

// ---------------------------------------------------------------------------
// Path classification
// ---------------------------------------------------------------------------

#[test]
fn path_classification() {
    use axum::http::Method;
    use crate::server::{is_inference, is_models};
    assert!(is_inference(&Method::POST, "/v1/messages"));
    assert!(is_inference(&Method::POST, "/backend-api/codex/responses"));
    assert!(is_inference(&Method::POST, "/v1/chat/completions"));
    assert!(is_inference(&Method::POST, "/v1/completions"));
    assert!(!is_inference(&Method::GET, "/v1/messages"));
    assert!(!is_inference(&Method::POST, "/health"));
    assert!(is_models(&Method::GET, "/v1/models"));
    assert!(is_models(&Method::GET, "/models/"));
    assert!(!is_models(&Method::GET, "/models/foo"));
    assert!(!is_models(&Method::POST, "/v1/models"));
}

// ---------------------------------------------------------------------------
// Claude adapter
// ---------------------------------------------------------------------------

#[test]
fn claude_backend_build_request_wire() {
    let _g = lock_models();
    reset_model_env();
    let req = CommonRequest {
        model: "claude-opus-4-8".into(),
        system: Some("Follow the rules.".into()),
        messages: vec![user("hello")],
        stream: true,
        max_tokens: Some(1234),
        ..Default::default()
    };
    let up = ClaudeBackend.build_request(&req, "tok-abc");
    assert_eq!(up.url, "https://api.anthropic.com/v1/messages?beta=true");
    assert_eq!(header(&up.headers, "authorization"), Some("Bearer tok-abc"));
    assert_eq!(header(&up.headers, "anthropic-version"), Some("2023-06-01"));
    assert_eq!(header(&up.headers, "anthropic-beta"), Some("claude-code-20250219,oauth-2025-04-20"));
    assert_eq!(header(&up.headers, "anthropic-dangerous-direct-browser-access"), Some("true"));

    let body: Value = serde_json::from_str(&up.body).unwrap();
    assert_eq!(body["model"], "claude-opus-4-8");
    assert_eq!(body["system"][0]["text"], "You are Claude Code, Anthropic's official CLI for Claude.");
    assert_eq!(body["system"][1]["text"], "Follow the rules.");
    assert_eq!(body["thinking"]["type"], "disabled");
    assert_eq!(body["max_tokens"], 1234);
    // Never send sampling params to Claude Code OAuth inference.
    assert!(body.get("temperature").is_none());
    assert!(body.get("top_p").is_none());
    assert!(body.get("top_k").is_none());
}

#[test]
fn claude_backend_token_extraction() {
    let up = ClaudeBackend.build_request(&CommonRequest { model: "x".into(), stream: true, ..Default::default() }, r#"{"claudeAiOauth":{"accessToken":"oauth-tok"}}"#);
    assert_eq!(header(&up.headers, "authorization"), Some("Bearer oauth-tok"));
    let up2 = ClaudeBackend.build_request(&CommonRequest { model: "x".into(), stream: true, ..Default::default() }, r#"{"type":"claude-code-oauth-token","token":"ll-tok"}"#);
    assert_eq!(header(&up2.headers, "authorization"), Some("Bearer ll-tok"));
}

#[test]
fn claude_backend_fable_keeps_thinking() {
    let _g = lock_models();
    reset_model_env();
    std::env::set_var("ASX_CLAUDE_MODELS", "claude-fable-5");
    let req = CommonRequest { model: "claude-fable-5".into(), stream: true, ..Default::default() };
    let up = ClaudeBackend.build_request(&req, "t");
    let body: Value = serde_json::from_str(&up.body).unwrap();
    assert_eq!(body["model"], "claude-fable-5");
    assert!(body.get("thinking").is_none(), "Fable cannot disable thinking, so the key is omitted");
    std::env::remove_var("ASX_CLAUDE_MODELS");
}

#[test]
fn claude_backend_parse_stream_chunks() {
    let b = ClaudeBackend;
    // tool_use open carries id + name
    let evs = b.parse_stream_chunk("event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"tu_1\",\"name\":\"read\"}}");
    assert_eq!(evs, vec![CommonEvent::ToolCallDelta { index: 1, id: Some("tu_1".into()), name: Some("read".into()), args_delta: None }]);
    // text delta
    let evs = b.parse_stream_chunk("data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}");
    assert_eq!(evs, vec![CommonEvent::Text { text: "hi".into() }]);
    // input_json_delta -> args
    let evs = b.parse_stream_chunk("data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"p\\\":1}\"}}");
    assert_eq!(evs, vec![CommonEvent::ToolCallDelta { index: 1, id: None, name: None, args_delta: Some("{\"p\":1}".into()) }]);
    // message_stop -> done
    assert_eq!(b.parse_stream_chunk("data: {\"type\":\"message_stop\"}"), vec![CommonEvent::Done { finish_reason: Some("stop".into()) }]);
    // error
    assert_eq!(b.parse_stream_chunk("data: {\"type\":\"error\",\"error\":{\"message\":\"boom\"}}"), vec![CommonEvent::Error { message: "boom".into() }]);
}

#[test]
fn claude_agent_parse_request_tools_and_messages() {
    let body = json!({
        "model": "claude-asx-glm-5.2",
        "system": "sys prompt",
        "stream": true,
        "max_tokens": 500,
        "tools": [{ "name": "search", "description": "d", "input_schema": { "type": "object" } }],
        "tool_choice": { "disable_parallel_tool_use": true },
        "messages": [
            { "role": "user", "content": "hi" },
            { "role": "assistant", "content": [ { "type": "text", "text": "ok" }, { "type": "tool_use", "id": "t1", "name": "search", "input": { "q": 1 } } ] },
            { "role": "user", "content": [ { "type": "tool_result", "tool_use_id": "t1", "content": "res", "is_error": true } ] }
        ]
    });
    let req = ClaudeAgent.parse_request("/v1/messages", &body);
    assert_eq!(req.model, "glm-5.2"); // claude-asx- prefix stripped
    assert_eq!(req.system.as_deref(), Some("sys prompt"));
    assert_eq!(req.parallel_tool_calls, Some(false));
    assert_eq!(req.tools.as_ref().unwrap()[0].name, "search");
    // messages: user, assistant(+toolcall), tool result
    assert_eq!(req.messages.len(), 3);
    assert_eq!(req.messages[1].role, "assistant");
    assert_eq!(req.messages[1].tool_calls.as_ref().unwrap()[0].id, "t1");
    assert_eq!(req.messages[2].role, "tool");
    assert_eq!(req.messages[2].tool_call_id.as_deref(), Some("t1"));
    assert_eq!(req.messages[2].is_error, Some(true));
}

#[test]
fn claude_agent_format_models_wraps_ids() {
    let choices = vec![
        crate::models::BackendChoice { id: "glm-5.2".into(), model: "glm-5.2".into(), effort: None },
        crate::models::BackendChoice { id: "claude-opus-4-8".into(), model: "claude-opus-4-8".into(), effort: None },
    ];
    let out = ClaudeAgent.format_models(&choices);
    assert_eq!(out["data"][0]["id"], "claude-asx-glm-5.2"); // wrapped
    assert_eq!(out["data"][0]["display_name"], "glm-5.2");
    assert_eq!(out["data"][1]["id"], "claude-opus-4-8"); // already claude* -> unchanged
    assert_eq!(out["has_more"], false);
}

// ---------------------------------------------------------------------------
// Codex adapter
// ---------------------------------------------------------------------------

#[test]
fn codex_backend_build_request_wire() {
    let _g = lock_models();
    reset_model_env();
    let req = CommonRequest {
        model: "gpt-5.5-high".into(),
        system: Some("instr".into()),
        messages: vec![user("go")],
        stream: true,
        ..Default::default()
    };
    let up = CodexBackend.build_request(&req, r#"{"tokens":{"access_token":"at","account_id":"acc"}}"#);
    assert_eq!(up.url, "https://chatgpt.com/backend-api/codex/responses");
    assert_eq!(header(&up.headers, "authorization"), Some("Bearer at"));
    assert_eq!(header(&up.headers, "chatgpt-account-id"), Some("acc"));
    assert_eq!(header(&up.headers, "OpenAI-Beta"), Some("responses=experimental"));
    assert_eq!(header(&up.headers, "originator"), Some("codex_cli_rs"));
    let body: Value = serde_json::from_str(&up.body).unwrap();
    assert_eq!(body["model"], "gpt-5.5"); // effort split off
    assert_eq!(body["instructions"], "instr");
    assert_eq!(body["reasoning"]["effort"], "high");
    assert_eq!(body["stream"], true);
    assert_eq!(body["store"], false);
    assert_eq!(body["input"][0]["type"], "message");
    assert_eq!(body["input"][0]["content"][0]["type"], "input_text");
}

#[test]
fn codex_backend_parse_stream_chunks() {
    let b = CodexBackend;
    assert_eq!(b.parse_stream_chunk("data: {\"type\":\"response.output_text.delta\",\"delta\":\"hey\"}"), vec![CommonEvent::Text { text: "hey".into() }]);
    let evs = b.parse_stream_chunk("data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"function_call\",\"call_id\":\"c1\",\"name\":\"do\"}}");
    assert_eq!(evs, vec![CommonEvent::ToolCallDelta { index: 0, id: Some("c1".into()), name: Some("do".into()), args_delta: None }]);
    assert_eq!(b.parse_stream_chunk("data: {\"type\":\"response.function_call_arguments.delta\",\"output_index\":0,\"delta\":\"{}\"}"), vec![CommonEvent::ToolCallDelta { index: 0, id: None, name: None, args_delta: Some("{}".into()) }]);
    assert_eq!(b.parse_stream_chunk("data: {\"type\":\"response.completed\"}"), vec![CommonEvent::Done { finish_reason: Some("stop".into()) }]);
}

#[test]
fn codex_agent_namespace_flatten_and_split() {
    let body = json!({
        "model": "gpt-5.5",
        "instructions": "top",
        "stream": true,
        "tools": [
            { "type": "namespace", "name": "multi_agent_v1", "tools": [ { "name": "spawn_agent", "description": "s", "parameters": {} } ] },
            { "type": "function", "name": "plain_tool" }
        ],
        "input": [ { "type": "message", "role": "user", "content": [ { "type": "input_text", "text": "hi" } ] } ]
    });
    let req = CodexAgent.parse_request("/responses", &body);
    let names: Vec<&str> = req.tools.as_ref().unwrap().iter().map(|t| t.name.as_str()).collect();
    assert!(names.contains(&"multi_agent_v1__spawn_agent"));
    assert!(names.contains(&"plain_tool"));
    assert_eq!(req.tool_namespaces.as_deref(), Some(&["multi_agent_v1".to_string()][..]));

    // On the way out, a flattened tool call is split back into {name, namespace}.
    let mut ctx = StreamCtx::new("id".into(), 0, "m".into(), req.tool_namespaces.clone());
    ctx.first = false; // skip response.created for a focused assertion
    let out = CodexAgent.format_stream_chunk(&CommonEvent::ToolCall { id: "c1".into(), name: "multi_agent_v1__spawn_agent".into(), arguments: "{}".into() }, &mut ctx);
    assert!(out.contains("\"namespace\":\"multi_agent_v1\""));
    assert!(out.contains("\"name\":\"spawn_agent\""));
}

#[test]
fn codex_agent_format_models_shape() {
    let choices = vec![crate::models::BackendChoice { id: "gpt-5.5-high".into(), model: "gpt-5.5".into(), effort: Some("high".into()) }];
    let out = CodexAgent.format_models(&choices);
    assert_eq!(out["models"][0]["slug"], "gpt-5.5-high");
    assert_eq!(out["models"][0]["default_reasoning_level"], "high");
    assert_eq!(out["models"][0]["context_window"], 200000);
}

// ---------------------------------------------------------------------------
// Grok adapter
// ---------------------------------------------------------------------------

#[test]
fn grok_backend_build_request_wire() {
    let _g = lock_models();
    reset_model_env();
    let req = CommonRequest { model: "grok-build".into(), messages: vec![user("hi")], stream: true, ..Default::default() };
    let up = GrokBackend.build_request(&req, "jwt-token");
    assert_eq!(up.url, "https://cli-chat-proxy.grok.com/v1/chat/completions");
    assert_eq!(header(&up.headers, "authorization"), Some("Bearer jwt-token"));
    assert_eq!(header(&up.headers, "X-XAI-Token-Auth"), Some("xai-grok-cli"));
    assert_eq!(header(&up.headers, "x-grok-client-identifier"), Some("grok-shell"));
    assert_eq!(header(&up.headers, "x-grok-model-override"), Some("grok-build"));
    let body: Value = serde_json::from_str(&up.body).unwrap();
    assert_eq!(body["model"], "grok-build");
    assert_eq!(body["stream"], true);
}

#[test]
fn grok_backend_drops_reasoning_content() {
    let evs = GrokBackend.parse_stream_chunk("data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"think\",\"content\":\"answer\"}}]}");
    // Only the real content becomes a Text event; reasoning_content is ignored.
    assert_eq!(evs, vec![CommonEvent::Text { text: "answer".into() }]);
    let evs = GrokBackend.parse_stream_chunk("data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}");
    assert_eq!(evs, vec![CommonEvent::Done { finish_reason: Some("stop".into()) }]);
}

#[test]
fn grok_agent_stream_and_done_framing() {
    let mut ctx = StreamCtx::new("cid".into(), 100, "grok-build".into(), None);
    let first = GrokAgent.format_stream_chunk(&CommonEvent::Text { text: "hi".into() }, &mut ctx);
    assert!(first.contains("chat.completion.chunk"));
    assert!(first.contains("\"role\":\"assistant\"")); // role only on first
    let second = GrokAgent.format_stream_chunk(&CommonEvent::Text { text: "!".into() }, &mut ctx);
    assert!(!second.contains("\"role\":\"assistant\""));
    let done = GrokAgent.format_stream_chunk(&CommonEvent::Done { finish_reason: None }, &mut ctx);
    assert!(done.contains("\"finish_reason\":\"stop\""));
    assert!(done.ends_with("data: [DONE]\n\n"));
}

#[test]
fn grok_agent_round_trips_chat_tool_calls() {
    let body = json!({
        "model": "grok-build",
        "stream": true,
        "messages": [
            { "role": "system", "content": "sys" },
            { "role": "user", "content": "hi" },
            { "role": "assistant", "content": null, "tool_calls": [ { "id": "tc1", "type": "function", "function": { "name": "f", "arguments": "{\"a\":1}" } } ] },
            { "role": "tool", "tool_call_id": "tc1", "content": "result" }
        ],
        "tools": [ { "type": "function", "function": { "name": "f", "parameters": {} } } ]
    });
    let req = GrokAgent.parse_request("/v1/chat/completions", &body);
    assert_eq!(req.system.as_deref(), Some("sys"));
    assert_eq!(req.messages.len(), 3); // system filtered out
    assert_eq!(req.messages[1].tool_calls.as_ref().unwrap()[0].id, "tc1");
    assert_eq!(req.messages[2].role, "tool");
    assert_eq!(req.tools.as_ref().unwrap()[0].name, "f");
}

// ---------------------------------------------------------------------------
// Z.AI backend
// ---------------------------------------------------------------------------

#[test]
fn zai_backend_thinking_and_headers() {
    let _g = lock_models();
    reset_model_env();
    // glm-5.2 default choice carries effort "high" -> thinking enabled.
    let up = ZaiBackend.build_request(&CommonRequest { model: "glm-5.2".into(), messages: vec![user("hi")], stream: true, ..Default::default() }, "zkey");
    assert_eq!(up.url, "https://api.z.ai/api/coding/paas/v4/chat/completions");
    assert_eq!(header(&up.headers, "authorization"), Some("Bearer zkey"));
    assert_eq!(header(&up.headers, "Accept-Language"), Some("en-US,en"));
    let body: Value = serde_json::from_str(&up.body).unwrap();
    assert_eq!(body["thinking"]["type"], "enabled");
    assert!(body.get("reasoning_effort").is_none()); // GLM uses thinking, not reasoning_effort
    // no max_tokens/temperature keys when unset (JSON.stringify drops undefined)
    assert!(body.get("max_tokens").is_none());
    assert!(body.get("temperature").is_none());

    // glm-4.5-air has no effort -> no thinking key.
    let up2 = ZaiBackend.build_request(&CommonRequest { model: "glm-4.5-air".into(), messages: vec![user("hi")], stream: true, ..Default::default() }, "zkey");
    let body2: Value = serde_json::from_str(&up2.body).unwrap();
    assert!(body2.get("thinking").is_none());
}

#[test]
fn zai_backend_parse_stream_chunks() {
    let evs = ZaiBackend.parse_stream_chunk("data: {\"choices\":[{\"delta\":{\"content\":\"tok\"}}]}");
    assert_eq!(evs, vec![CommonEvent::Text { text: "tok".into() }]);
    let evs = ZaiBackend.parse_stream_chunk("data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\ndata: [DONE]");
    assert_eq!(evs, vec![CommonEvent::Done { finish_reason: Some("stop".into()) }]);
}

// ---------------------------------------------------------------------------
// Models resolution / precedence
// ---------------------------------------------------------------------------

#[test]
fn models_defaults_per_provider() {
    let _g = lock_models();
    reset_model_env();
    let codex = backend_choices("codex");
    assert_eq!(codex.iter().map(|c| c.id.clone()).collect::<Vec<_>>(), vec!["gpt-5.5-high", "gpt-5.5-medium", "gpt-5.5-low", "gpt-5.5-xhigh"]);
    assert!(codex.iter().all(|c| c.model == "gpt-5.5"));
    let claude = backend_choices("claude");
    assert_eq!(claude[0].id, "claude-opus-4-8");
    let zai = backend_choices("zai");
    assert_eq!(zai.len(), 4);
    assert_eq!(zai[1].effort.as_deref(), Some("max"));
    assert_eq!(backend_choices("grok")[0].id, "grok-build");
}

#[test]
fn models_precedence_env_over_file_over_defaults() {
    let _g = lock_models();
    reset_model_env();

    // File override.
    let tmp = std::env::temp_dir().join(format!("aas-proxy-models-{}.json", uuid::Uuid::new_v4()));
    std::fs::write(&tmp, r#"{"zai":["glm-file-model"]}"#).unwrap();
    std::env::set_var("ASX_MODELS_CONFIG", tmp.to_string_lossy().to_string());
    std::env::remove_var("ASX_ZAI_MODELS");
    assert_eq!(backend_choices("zai")[0].id, "glm-file-model");

    // Env override wins over file.
    std::env::set_var("ASX_ZAI_MODELS", "glm-env:high,glm-two");
    let choices = backend_choices("zai");
    assert_eq!(choices[0].id, "glm-env-high");
    assert_eq!(choices[0].effort.as_deref(), Some("high"));
    assert_eq!(choices[1].id, "glm-two");

    // resolve_choice falls back to the first entry for an unknown id.
    assert_eq!(resolve_choice("zai", "does-not-exist").id, "glm-env-high");
    assert_eq!(resolve_choice("zai", "glm-two").id, "glm-two");

    std::env::remove_var("ASX_ZAI_MODELS");
    let _ = std::fs::remove_file(&tmp);
    reset_model_env();
}

// ---------------------------------------------------------------------------
// §H streaming invariants via the server's transcode helpers
// ---------------------------------------------------------------------------

#[test]
fn stream_buffers_tool_calls_and_flushes_before_done() {
    use crate::server::process_event;
    let agent = GrokAgent;
    let mut ctx = StreamCtx::new("cid".into(), 0, "grok-build".into(), None);
    let mut acc = ToolAccumulator::new();
    let mut saw_done = false;

    // tool_call_delta fragments are held (no output yet).
    assert!(process_event(&agent, &mut ctx, &mut acc, &mut saw_done, delta(0, Some("t1"), Some("f"), Some("{}"))).is_empty());
    assert!(!saw_done);
    // done -> flush the buffered tool call, THEN emit done.
    let out = process_event(&agent, &mut ctx, &mut acc, &mut saw_done, CommonEvent::Done { finish_reason: None });
    assert!(saw_done);
    assert_eq!(out.len(), 2);
    assert!(out[0].contains("\"tool_calls\"")); // flushed tool call first
    assert!(out[0].contains("\"id\":\"t1\""));
    assert!(out[1].contains("\"finish_reason\":\"tool_calls\"")); // then the terminator
    assert!(out[1].ends_with("data: [DONE]\n\n"));
}

#[test]
fn truncated_stream_gets_synthetic_terminator() {
    use crate::server::{finalize_stream, process_event};
    let agent = GrokAgent;
    let mut ctx = StreamCtx::new("cid".into(), 0, "grok-build".into(), None);
    let mut acc = ToolAccumulator::new();
    let mut saw_done = false;

    // Some text arrives, but the upstream never sends a done event.
    let _ = process_event(&agent, &mut ctx, &mut acc, &mut saw_done, CommonEvent::Text { text: "partial".into() });
    assert!(!saw_done);
    let tail = finalize_stream(&agent, &mut ctx, &mut acc, saw_done, None);
    let joined = tail.join("");
    assert!(joined.contains("ended unexpectedly"));
    assert!(joined.ends_with("data: [DONE]\n\n"));
}

#[test]
fn midstream_error_flushes_and_terminates() {
    use crate::server::finalize_stream;
    let agent = ClaudeAgent;
    let mut ctx = StreamCtx::new("cid".into(), 0, "claude-opus-4-8".into(), None);
    let mut acc = ToolAccumulator::new();
    // A pending tool call plus a mid-stream connection error.
    acc.push(&delta(0, Some("t1"), Some("f"), Some("{}")));
    let tail = finalize_stream(&agent, &mut ctx, &mut acc, false, Some("connection reset".into()));
    let joined = tail.join("");
    assert!(joined.contains("tool_use")); // flushed the held tool call
    assert!(joined.contains("stream interrupted: connection reset"));
    assert!(joined.contains("message_stop")); // clean terminator, not raw JSON
}

// ---------------------------------------------------------------------------
// End-to-end server (no upstream needed for models / fake-auth routes)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn server_models_and_fake_auth_routes() {
    use crate::server::{start_proxy, Credential, ProxyStartOptions};
    let handle = start_proxy(ProxyStartOptions {
        source_provider: "grok".into(),
        target_provider: "zai".into(),
        target_credential: Credential { raw: Some("zkey".into()), api_key: None },
        tmp_dir: None,
        port: None,
    })
    .await
    .unwrap();

    let client = reqwest::Client::new();

    // GET /v1/models -> grok agent frames an OpenAI list.
    let models: Value = client.get(format!("{}/v1/models", handle.url)).send().await.unwrap().json().await.unwrap();
    assert_eq!(models["object"], "list");
    assert!(models["data"].as_array().map(|a| !a.is_empty()).unwrap_or(false));

    // Any non-inference request -> fake-auth checkpoint.
    let auth: Value = client.get(format!("{}/health", handle.url)).send().await.unwrap().json().await.unwrap();
    assert_eq!(auth["ok"], true);
    assert_eq!(auth["authenticated"], true);
    assert_eq!(auth["via"], "asx-proxy");

    handle.stop().await;
}
