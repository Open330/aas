//! Claude (Anthropic Messages) adapter — both agent and backend sides.
//! Port of `proxy/adapters/claude.ts`.

use crate::adapters::util::{sse_event as anth_event, sse_headers, to_text};
use crate::models::{resolve_choice, BackendChoice};
use crate::types::{
    AgentAdapter, BackendAdapter, CommonEvent, CommonMessage, CommonRequest, CommonResponse,
    CommonToolCall, CommonToolDef, StreamCtx, UpstreamRequest,
};
use serde_json::{json, Value};

fn zero_usage() -> Value {
    json!({
        "input_tokens": 0,
        "cache_creation_input_tokens": 0,
        "cache_read_input_tokens": 0,
        "output_tokens": 0,
    })
}

/// `b.input ?? {}` -> JSON string.
fn input_json(b: &Value) -> String {
    match b.get("input") {
        Some(v) if !v.is_null() => serde_json::to_string(v).unwrap_or_else(|_| "{}".to_string()),
        _ => "{}".to_string(),
    }
}

/// Parse a stored tool-call arguments string; empty or invalid -> `{}`.
fn args_to_input(arguments: &str) -> Value {
    if arguments.is_empty() {
        return json!({});
    }
    serde_json::from_str(arguments).unwrap_or_else(|_| json!({}))
}

/// Anthropic messages (blocks incl. tool_use/tool_result) -> COMMON, keeping tool sessions intact.
fn anth_messages_to_common(messages: &Value) -> Vec<CommonMessage> {
    let mut out: Vec<CommonMessage> = Vec::new();
    let empty: Vec<Value> = Vec::new();
    let arr = messages.as_array().unwrap_or(&empty);
    for m in arr {
        let role = m
            .get("role")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let content = m.get("content");
        match content {
            Some(Value::String(s)) => {
                if !s.is_empty() {
                    out.push(CommonMessage {
                        role,
                        content: s.clone(),
                        ..Default::default()
                    });
                }
                continue;
            }
            Some(Value::Array(blocks)) => {
                let mut text = String::new();
                let mut tool_calls: Vec<CommonToolCall> = Vec::new();
                let mut tool_results: Vec<CommonMessage> = Vec::new();
                for b in blocks {
                    if b.is_null() {
                        continue;
                    }
                    match b.get("type").and_then(|v| v.as_str()) {
                        Some("text") => {
                            text.push_str(b.get("text").and_then(|v| v.as_str()).unwrap_or(""))
                        }
                        Some("tool_use") => tool_calls.push(CommonToolCall {
                            id: b
                                .get("id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                            name: b
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                            arguments: input_json(b),
                        }),
                        Some("tool_result") => tool_results.push(CommonMessage {
                            role: "tool".to_string(),
                            content: to_text(b.get("content").unwrap_or(&Value::Null)),
                            tool_call_id: Some(
                                b.get("tool_use_id")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string(),
                            ),
                            is_error: if b.get("is_error").and_then(|v| v.as_bool()) == Some(true) {
                                Some(true)
                            } else {
                                None
                            },
                            ..Default::default()
                        }),
                        _ => {}
                    }
                }
                if role == "assistant" {
                    if !text.is_empty() || !tool_calls.is_empty() {
                        out.push(CommonMessage {
                            role: "assistant".to_string(),
                            content: text,
                            tool_calls: if tool_calls.is_empty() {
                                None
                            } else {
                                Some(tool_calls)
                            },
                            ..Default::default()
                        });
                    }
                } else {
                    for tr in tool_results {
                        out.push(tr);
                    }
                    if !text.is_empty() {
                        out.push(CommonMessage {
                            role: "user".to_string(),
                            content: text,
                            ..Default::default()
                        });
                    }
                }
                continue;
            }
            other => {
                let t = to_text(other.unwrap_or(&Value::Null));
                if !t.is_empty() {
                    out.push(CommonMessage {
                        role,
                        content: t,
                        ..Default::default()
                    });
                }
                continue;
            }
        }
    }
    out
}

const CLAUDE_ID_PREFIX: &str = "claude-asx-";

pub fn wrap_model_id(id: &str) -> String {
    let lower = id.to_lowercase();
    if lower.starts_with("claude") || lower.starts_with("anthropic") {
        id.to_string()
    } else {
        format!("{CLAUDE_ID_PREFIX}{id}")
    }
}

fn unwrap_model_id(id: &str) -> String {
    id.strip_prefix(CLAUDE_ID_PREFIX).unwrap_or(id).to_string()
}

pub struct ClaudeAgent;

impl AgentAdapter for ClaudeAgent {
    fn parse_request(&self, _path: &str, body: &Value) -> CommonRequest {
        let tools = body.get("tools").and_then(|v| v.as_array()).map(|arr| {
            arr.iter()
                .filter(|t| {
                    t.get("name")
                        .and_then(|v| v.as_str())
                        .map(|s| !s.is_empty())
                        .unwrap_or(false)
                })
                .map(|t| CommonToolDef {
                    name: t
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    description: t
                        .get("description")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    parameters: t.get("input_schema").cloned(),
                    builtin_type: t
                        .get("type")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    strict: None,
                })
                .collect::<Vec<_>>()
        });
        let system = match body.get("system") {
            Some(Value::String(s)) => Some(s.clone()),
            Some(v @ Value::Array(_)) => Some(to_text(v)),
            _ => None,
        };
        let parallel = if body
            .get("tool_choice")
            .and_then(|tc| tc.get("disable_parallel_tool_use"))
            .and_then(|v| v.as_bool())
            == Some(true)
        {
            Some(false)
        } else {
            None
        };
        CommonRequest {
            model: unwrap_model_id(
                body.get("model")
                    .and_then(|v| v.as_str())
                    .unwrap_or("claude"),
            ),
            system,
            messages: anth_messages_to_common(body.get("messages").unwrap_or(&Value::Null)),
            tools: tools.filter(|t| !t.is_empty()),
            tool_choice: body.get("tool_choice").cloned(),
            parallel_tool_calls: parallel,
            stream: body
                .get("stream")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            max_tokens: body.get("max_tokens").and_then(|v| v.as_i64()),
            temperature: body.get("temperature").and_then(|v| v.as_f64()),
            ..Default::default()
        }
    }

    fn stream_headers(&self) -> Vec<(String, String)> {
        sse_headers()
    }

    fn format_stream_chunk(&self, ev: &CommonEvent, ctx: &mut StreamCtx) -> String {
        let mut out = String::new();
        if ctx.first {
            ctx.first = false;
            out += &anth_event(
                "message_start",
                &json!({
                    "type": "message_start",
                    "message": { "id": ctx.id, "type": "message", "role": "assistant", "model": ctx.model, "content": [], "stop_reason": null, "usage": zero_usage() },
                }),
            );
        }
        // openText / closeText as inline helpers operating on ctx.
        macro_rules! open_text {
            () => {{
                if ctx.text_open {
                    String::new()
                } else {
                    ctx.text_open = true;
                    ctx.text_index = ctx.next_index;
                    ctx.next_index += 1;
                    anth_event(
                        "content_block_start",
                        &json!({ "type": "content_block_start", "index": ctx.text_index, "content_block": { "type": "text", "text": "" } }),
                    )
                }
            }};
        }
        macro_rules! close_text {
            () => {{
                if !ctx.text_open {
                    String::new()
                } else {
                    ctx.text_open = false;
                    anth_event("content_block_stop", &json!({ "type": "content_block_stop", "index": ctx.text_index }))
                }
            }};
        }

        match ev {
            CommonEvent::Text { text } => {
                out += &open_text!();
                out += &anth_event(
                    "content_block_delta",
                    &json!({ "type": "content_block_delta", "index": ctx.text_index, "delta": { "type": "text_delta", "text": text } }),
                );
                out
            }
            CommonEvent::ToolCall {
                id,
                name,
                arguments,
            } => {
                out += &close_text!();
                let index = ctx.next_index;
                ctx.next_index += 1;
                ctx.items.push(Value::String(id.clone()));
                let args = if arguments.is_empty() {
                    "{}"
                } else {
                    arguments.as_str()
                };
                out += &anth_event(
                    "content_block_start",
                    &json!({ "type": "content_block_start", "index": index, "content_block": { "type": "tool_use", "id": id, "name": name, "input": {} } }),
                );
                out += &anth_event(
                    "content_block_delta",
                    &json!({ "type": "content_block_delta", "index": index, "delta": { "type": "input_json_delta", "partial_json": args } }),
                );
                out += &anth_event(
                    "content_block_stop",
                    &json!({ "type": "content_block_stop", "index": index }),
                );
                out
            }
            other => {
                // done or error: surface error text, ensure at least one block, then close the message.
                if let CommonEvent::Error { message } = other {
                    out += &open_text!();
                    out += &anth_event(
                        "content_block_delta",
                        &json!({ "type": "content_block_delta", "index": ctx.text_index, "delta": { "type": "text_delta", "text": format!("[asx-proxy] {message}") } }),
                    );
                }
                if !ctx.text_open && ctx.items.is_empty() {
                    out += &open_text!();
                }
                out += &close_text!();
                let stop_reason = if ctx.items.is_empty() {
                    "end_turn"
                } else {
                    "tool_use"
                };
                out += &anth_event(
                    "message_delta",
                    &json!({ "type": "message_delta", "delta": { "stop_reason": stop_reason }, "usage": { "output_tokens": 0 } }),
                );
                out += &anth_event("message_stop", &json!({ "type": "message_stop" }));
                out
            }
        }
    }

    fn format_response(&self, resp: &CommonResponse, _req: &CommonRequest) -> Value {
        let mut content: Vec<Value> = Vec::new();
        if !resp.text.is_empty() {
            content.push(json!({ "type": "text", "text": resp.text }));
        }
        for tc in &resp.tool_calls {
            content.push(json!({ "type": "tool_use", "id": tc.id, "name": tc.name, "input": args_to_input(&tc.arguments) }));
        }
        if content.is_empty() {
            content.push(json!({ "type": "text", "text": "" }));
        }
        json!({
            "id": "msg_asx",
            "type": "message",
            "role": "assistant",
            "content": content,
            "stop_reason": if resp.tool_calls.is_empty() { "end_turn" } else { "tool_use" },
            "usage": zero_usage(),
        })
    }

    fn format_models(&self, choices: &[BackendChoice]) -> Value {
        let data: Vec<Value> = choices
            .iter()
            .map(|c| json!({ "id": wrap_model_id(&c.id), "type": "model", "display_name": c.id, "created_at": "2025-01-01T00:00:00Z" }))
            .collect();
        let first = data
            .first()
            .and_then(|d| d.get("id"))
            .cloned()
            .unwrap_or(Value::Null);
        let last = data
            .last()
            .and_then(|d| d.get("id"))
            .cloned()
            .unwrap_or(Value::Null);
        json!({ "data": data, "has_more": false, "first_id": first, "last_id": last })
    }
}

/// Claude Code OAuth requires the first system block to be exactly this identity line.
const CLAUDE_CODE_ID: &str = "You are Claude Code, Anthropic's official CLI for Claude.";

/// COMMON messages -> Anthropic messages, restoring tool_use/tool_result blocks. Consecutive
/// tool results merge into one user turn.
fn common_to_anth_messages(messages: &[CommonMessage]) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for m in messages {
        if m.role == "system" {
            continue;
        }
        if m.role == "tool" {
            let mut block = json!({ "type": "tool_result", "tool_use_id": m.tool_call_id.clone().unwrap_or_default(), "content": m.content });
            if m.is_error == Some(true) {
                block["is_error"] = json!(true);
            }
            let mut merged = false;
            if let Some(last) = out.last_mut() {
                let is_tool_result_user = last.get("role").and_then(|v| v.as_str()) == Some("user")
                    && last
                        .get("content")
                        .and_then(|v| v.as_array())
                        .and_then(|a| a.first())
                        .and_then(|b| b.get("type"))
                        .and_then(|v| v.as_str())
                        == Some("tool_result");
                if is_tool_result_user {
                    if let Some(arr) = last.get_mut("content").and_then(|v| v.as_array_mut()) {
                        arr.push(block.clone());
                        merged = true;
                    }
                }
            }
            if !merged {
                out.push(json!({ "role": "user", "content": [block] }));
            }
            continue;
        }
        if m.role == "assistant" {
            if let Some(tcs) = m.tool_calls.as_ref().filter(|t| !t.is_empty()) {
                let mut content: Vec<Value> = Vec::new();
                if !m.content.is_empty() {
                    content.push(json!({ "type": "text", "text": m.content }));
                }
                for tc in tcs {
                    content.push(json!({ "type": "tool_use", "id": tc.id, "name": tc.name, "input": args_to_input(&tc.arguments) }));
                }
                out.push(json!({ "role": "assistant", "content": content }));
                continue;
            }
        }
        let role = if m.role == "assistant" {
            "assistant"
        } else {
            "user"
        };
        out.push(json!({ "role": role, "content": m.content }));
    }
    out
}

fn anth_tool_choice(tc: Option<&Value>) -> Option<Value> {
    let tc = tc?;
    if tc.is_null() {
        return None;
    }
    if let Some(s) = tc.as_str() {
        return match s {
            "auto" => Some(json!({ "type": "auto" })),
            "required" | "any" => Some(json!({ "type": "any" })),
            "none" => None,
            _ => None,
        };
    }
    if tc.is_object() {
        if tc
            .get("name")
            .and_then(|v| v.as_str())
            .map(|s| !s.is_empty())
            .unwrap_or(false)
        {
            return Some(json!({ "type": "tool", "name": tc.get("name").unwrap() }));
        }
        if tc.get("type").is_some() {
            return Some(tc.clone());
        }
    }
    None
}

fn claude_token(cred: &str) -> String {
    if let Ok(d) = serde_json::from_str::<Value>(cred) {
        if d.get("type").and_then(|v| v.as_str()) == Some("claude-code-oauth-token") {
            return d
                .get("token")
                .and_then(|v| v.as_str())
                .unwrap_or(cred)
                .to_string();
        }
        if let Some(t) = d
            .get("claudeAiOauth")
            .and_then(|o| o.get("accessToken"))
            .and_then(|v| v.as_str())
        {
            return t.to_string();
        }
        if let Some(t) = d.get("accessToken").and_then(|v| v.as_str()) {
            return t.to_string();
        }
        if let Some(t) = d.get("apiKey").and_then(|v| v.as_str()) {
            return t.to_string();
        }
    }
    cred.to_string()
}

pub struct ClaudeBackend;

impl BackendAdapter for ClaudeBackend {
    fn build_request(&self, req: &CommonRequest, cred: &str) -> UpstreamRequest {
        let token = claude_token(cred);
        let choice = resolve_choice("claude", &req.model);
        let mut system: Vec<Value> = vec![json!({ "type": "text", "text": CLAUDE_CODE_ID })];
        if let Some(sys) = &req.system {
            if sys != CLAUDE_CODE_ID {
                system.push(json!({ "type": "text", "text": sys }));
            }
        }
        let mut body = json!({
            "model": choice.model,
            "system": system,
            "messages": common_to_anth_messages(&req.messages),
            "stream": true,
            "max_tokens": req.max_tokens.unwrap_or(8192),
        });
        if !choice.model.to_lowercase().contains("fable") {
            body["thinking"] = json!({ "type": "disabled" });
        }
        let tools: Vec<Value> = req
            .tools
            .as_ref()
            .map(|ts| {
                ts.iter()
                    .map(|t| {
                        if let Some(bt) = &t.builtin_type {
                            json!({ "type": bt, "name": t.name })
                        } else {
                            json!({
                                "name": t.name,
                                "description": t.description.clone().unwrap_or_default(),
                                "input_schema": t.parameters.clone().unwrap_or_else(|| json!({ "type": "object", "properties": {} })),
                            })
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        if !tools.is_empty() {
            body["tools"] = json!(tools);
            let base = anth_tool_choice(req.tool_choice.as_ref()).or(
                if req.parallel_tool_calls == Some(false) {
                    Some(json!({ "type": "auto" }))
                } else {
                    None
                },
            );
            if let Some(mut tc) = base {
                if req.parallel_tool_calls == Some(false) {
                    if let Some(obj) = tc.as_object_mut() {
                        obj.insert("disable_parallel_tool_use".to_string(), json!(true));
                    }
                }
                body["tool_choice"] = tc;
            }
        }
        UpstreamRequest {
            url: "https://api.anthropic.com/v1/messages?beta=true".to_string(),
            headers: vec![
                ("content-type".to_string(), "application/json".to_string()),
                ("authorization".to_string(), format!("Bearer {token}")),
                ("anthropic-version".to_string(), "2023-06-01".to_string()),
                (
                    "anthropic-beta".to_string(),
                    "claude-code-20250219,oauth-2025-04-20".to_string(),
                ),
                (
                    "anthropic-dangerous-direct-browser-access".to_string(),
                    "true".to_string(),
                ),
            ],
            body: body.to_string(),
        }
    }

    fn parse_stream_chunk(&self, block: &str) -> Vec<CommonEvent> {
        let mut out = Vec::new();
        for line in block.split('\n') {
            let Some(rest) = line.strip_prefix("data:") else {
                continue;
            };
            let payload = rest.trim();
            if payload.is_empty() {
                continue;
            }
            let Ok(j) = serde_json::from_str::<Value>(payload) else {
                continue;
            };
            let jtype = j.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let delta_type = j
                .get("delta")
                .and_then(|d| d.get("type"))
                .and_then(|v| v.as_str());
            if jtype == "content_block_start"
                && j.get("content_block")
                    .and_then(|c| c.get("type"))
                    .and_then(|v| v.as_str())
                    == Some("tool_use")
            {
                let cb = j.get("content_block").unwrap();
                out.push(CommonEvent::ToolCallDelta {
                    index: j.get("index").and_then(|v| v.as_i64()).unwrap_or(0),
                    id: cb.get("id").and_then(|v| v.as_str()).map(|s| s.to_string()),
                    name: cb
                        .get("name")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    args_delta: None,
                });
            } else if jtype == "content_block_delta" && delta_type == Some("text_delta") {
                out.push(CommonEvent::Text {
                    text: j
                        .get("delta")
                        .and_then(|d| d.get("text"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                });
            } else if jtype == "content_block_delta" && delta_type == Some("input_json_delta") {
                if let Some(pj) = j
                    .get("delta")
                    .and_then(|d| d.get("partial_json"))
                    .and_then(|v| v.as_str())
                {
                    out.push(CommonEvent::ToolCallDelta {
                        index: j.get("index").and_then(|v| v.as_i64()).unwrap_or(0),
                        id: None,
                        name: None,
                        args_delta: Some(pj.to_string()),
                    });
                }
            } else if jtype == "message_stop" {
                out.push(CommonEvent::Done {
                    finish_reason: Some("stop".to_string()),
                });
            } else if jtype == "error" {
                out.push(CommonEvent::Error {
                    message: j
                        .get("error")
                        .and_then(|e| e.get("message"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("anthropic error")
                        .to_string(),
                });
            }
        }
        out
    }
}
