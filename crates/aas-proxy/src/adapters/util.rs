//! Shared helpers for the wire-format adapters (port of `proxy/adapters/util.ts`).

use crate::types::{CommonEvent, CommonMessage, CommonToolDef};
use serde_json::{json, Value};

/// SSE line builders.
pub fn sse_data(obj: &Value) -> String {
    format!("data: {}\n\n", serde_json::to_string(obj).unwrap_or_default())
}

pub fn sse_event(event: &str, data: &Value) -> String {
    format!("event: {}\n{}", event, sse_data(data))
}

/// Headers for a streaming (SSE) response — identical for every agent.
pub fn sse_headers() -> Vec<(String, String)> {
    vec![
        ("Content-Type".to_string(), "text/event-stream".to_string()),
        ("Cache-Control".to_string(), "no-cache".to_string()),
        ("Connection".to_string(), "keep-alive".to_string()),
    ]
}

fn scalar_to_string(v: &Value) -> String {
    match v.as_str() {
        Some(s) => s.to_string(),
        None => v.to_string(),
    }
}

/// One element of a content-block array: `c?.text ?? (typeof c === 'string' ? c : '')`.
fn text_piece(c: &Value) -> String {
    if let Some(t) = c.get("text") {
        if !t.is_null() {
            return scalar_to_string(t);
        }
    }
    if let Some(s) = c.as_str() {
        return s.to_string();
    }
    String::new()
}

/// Flatten message content (string | content-block array) to plain text.
pub fn to_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(arr) => arr.iter().map(text_piece).collect(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

// --- OpenAI Chat Completions helpers (shared by grok + zai backends and the grok agent) ---

/// COMMON messages -> Chat Completions messages, restoring assistant tool_calls and role=tool
/// results so a multi-turn tool session survives the round trip.
pub fn chat_messages_from_common(system: Option<&str>, messages: &[CommonMessage]) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    if let Some(sys) = system {
        out.push(json!({ "role": "system", "content": sys }));
    }
    for m in messages {
        if m.role == "system" {
            continue;
        }
        if m.role == "tool" {
            out.push(json!({
                "role": "tool",
                "tool_call_id": m.tool_call_id.clone().unwrap_or_default(),
                "content": m.content,
            }));
            continue;
        }
        if m.role == "assistant" {
            if let Some(tcs) = m.tool_calls.as_ref().filter(|t| !t.is_empty()) {
                let tool_calls: Vec<Value> = tcs
                    .iter()
                    .map(|tc| {
                        json!({
                            "id": tc.id,
                            "type": "function",
                            "function": {
                                "name": tc.name,
                                "arguments": if tc.arguments.is_empty() { "{}" } else { tc.arguments.as_str() },
                            },
                        })
                    })
                    .collect();
                out.push(json!({
                    "role": "assistant",
                    "content": if m.content.is_empty() { Value::Null } else { Value::String(m.content.clone()) },
                    "tool_calls": tool_calls,
                }));
                continue;
            }
        }
        out.push(json!({ "role": m.role, "content": m.content }));
    }
    out
}

/// COMMON tool defs -> Chat Completions `tools`.
pub fn chat_tools_from_common(tools: Option<&Vec<CommonToolDef>>) -> Option<Vec<Value>> {
    let tools = tools.filter(|t| !t.is_empty())?;
    Some(
        tools
            .iter()
            .map(|t| {
                let mut fnv = json!({
                    "name": t.name,
                    "description": t.description.clone().unwrap_or_default(),
                    "parameters": t.parameters.clone().unwrap_or_else(|| json!({ "type": "object", "properties": {} })),
                });
                if let Some(strict) = t.strict {
                    fnv["strict"] = json!(strict);
                }
                json!({ "type": "function", "function": fnv })
            })
            .collect(),
    )
}

/// Chat Completions request messages -> COMMON (assistant tool_calls, role=tool results).
pub fn chat_messages_to_common(messages: &[Value]) -> Vec<CommonMessage> {
    let mut out: Vec<CommonMessage> = Vec::new();
    for m in messages {
        if m.is_null() {
            continue;
        }
        let role = m.get("role").and_then(|v| v.as_str()).unwrap_or("");
        if role == "tool" {
            out.push(CommonMessage {
                role: "tool".to_string(),
                content: to_text(m.get("content").unwrap_or(&Value::Null)),
                tool_call_id: Some(m.get("tool_call_id").and_then(|v| v.as_str()).unwrap_or("").to_string()),
                tool_name: m.get("name").and_then(|v| v.as_str()).map(|s| s.to_string()),
                ..Default::default()
            });
            continue;
        }
        if role == "assistant" {
            if let Some(tcs) = m.get("tool_calls").and_then(|v| v.as_array()).filter(|a| !a.is_empty()) {
                out.push(CommonMessage {
                    role: "assistant".to_string(),
                    content: to_text(m.get("content").unwrap_or(&Value::Null)),
                    tool_calls: Some(tcs.iter().map(chat_tool_call_to_common).collect()),
                    ..Default::default()
                });
                continue;
            }
        }
        out.push(CommonMessage {
            role: role.to_string(),
            content: to_text(m.get("content").unwrap_or(&Value::Null)),
            ..Default::default()
        });
    }
    out
}

fn chat_tool_call_to_common(tc: &Value) -> crate::types::CommonToolCall {
    let function = tc.get("function");
    let args = function.and_then(|f| f.get("arguments"));
    let arguments = match args {
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => "{}".to_string(),
    };
    crate::types::CommonToolCall {
        id: tc.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        name: function.and_then(|f| f.get("name")).and_then(|v| v.as_str()).unwrap_or("").to_string(),
        arguments,
    }
}

/// Chat Completions request tools -> COMMON tool defs.
pub fn chat_tools_to_common(tools: Option<&Value>) -> Option<Vec<CommonToolDef>> {
    let arr = tools?.as_array()?;
    if arr.is_empty() {
        return None;
    }
    let mut out: Vec<CommonToolDef> = Vec::new();
    for t in arr {
        let fnv = t.get("function").unwrap_or(t);
        let name = fnv.get("name").and_then(|v| v.as_str());
        let name = match name {
            Some(n) if !n.is_empty() => n,
            _ => continue,
        };
        out.push(CommonToolDef {
            name: name.to_string(),
            description: fnv.get("description").and_then(|v| v.as_str()).map(|s| s.to_string()),
            parameters: fnv.get("parameters").cloned(),
            strict: fnv.get("strict").and_then(|v| v.as_bool()),
            builtin_type: None,
        });
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// Chat Completions streaming `delta.tool_calls[]` fragments -> COMMON tool_call_delta events.
pub fn parse_chat_tool_deltas(tool_calls: &[Value]) -> Vec<CommonEvent> {
    let mut out = Vec::new();
    for tc in tool_calls {
        if tc.is_null() {
            continue;
        }
        let function = tc.get("function");
        let args_delta = match function.and_then(|f| f.get("arguments")) {
            Some(Value::String(s)) => Some(s.clone()),
            _ => None,
        };
        out.push(CommonEvent::ToolCallDelta {
            index: tc.get("index").and_then(|v| v.as_i64()).unwrap_or(0),
            id: tc.get("id").and_then(|v| v.as_str()).map(|s| s.to_string()),
            name: function.and_then(|f| f.get("name")).and_then(|v| v.as_str()).map(|s| s.to_string()),
            args_delta,
        });
    }
    out
}
