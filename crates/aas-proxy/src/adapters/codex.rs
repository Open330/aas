//! Codex (ChatGPT subscription) adapter — Responses API backend + codex CLI agent.
//! Port of `proxy/adapters/codex.ts`.

use crate::adapters::util::{sse_event as resp, sse_headers, to_text};
use crate::models::{resolve_choice, BackendChoice};
use crate::types::{
    AgentAdapter, BackendAdapter, CommonEvent, CommonMessage, CommonRequest, CommonResponse,
    CommonToolCall, CommonToolDef, StreamCtx, UpstreamRequest,
};
use serde_json::{json, Map, Value};

const CODEX_URL: &str = "https://chatgpt.com/backend-api/codex/responses";

fn extract_auth(cred: &str) -> (String, String) {
    if let Ok(d) = serde_json::from_str::<Value>(cred) {
        let t = d.get("tokens").unwrap_or(&d);
        let token = t
            .get("access_token")
            .and_then(|v| v.as_str())
            .or_else(|| t.get("id_token").and_then(|v| v.as_str()))
            .unwrap_or("")
            .to_string();
        let account = t
            .get("account_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        return (token, account);
    }
    (cred.to_string(), String::new())
}

/// COMMON tool defs -> Responses flat function tools.
fn to_responses_tools(tools: Option<&Vec<CommonToolDef>>) -> Vec<Value> {
    tools
        .map(|ts| {
            ts.iter()
                .map(|t| {
                    json!({
                        "type": "function",
                        "name": t.name,
                        "description": t.description.clone().unwrap_or_default(),
                        "parameters": t.parameters.clone().unwrap_or_else(|| json!({ "type": "object", "properties": {} })),
                        "strict": t.strict.unwrap_or(false),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// COMMON messages -> Responses `input` items, preserving tool calls/results.
fn messages_to_input(messages: &[CommonMessage]) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for m in messages {
        if m.role == "system" {
            continue;
        }
        if m.role == "tool" {
            out.push(json!({ "type": "function_call_output", "call_id": m.tool_call_id.clone().unwrap_or_default(), "output": m.content }));
            continue;
        }
        if m.role == "assistant" {
            if let Some(tcs) = m.tool_calls.as_ref().filter(|t| !t.is_empty()) {
                if !m.content.is_empty() {
                    out.push(json!({ "type": "message", "role": "assistant", "content": [{ "type": "output_text", "text": m.content }] }));
                }
                for tc in tcs {
                    out.push(json!({ "type": "function_call", "call_id": tc.id, "name": tc.name, "arguments": if tc.arguments.is_empty() { "{}" } else { tc.arguments.as_str() } }));
                }
                continue;
            }
        }
        let is_assistant = m.role == "assistant";
        out.push(json!({
            "type": "message",
            "role": if is_assistant { "assistant" } else { "user" },
            "content": [{ "type": if is_assistant { "output_text" } else { "input_text" }, "text": m.content }],
        }));
    }
    out
}

pub struct CodexBackend;

impl BackendAdapter for CodexBackend {
    fn build_request(&self, req: &CommonRequest, cred: &str) -> UpstreamRequest {
        let (token, account) = extract_auth(cred);
        let mut sys_parts: Vec<String> = Vec::new();
        if let Some(s) = &req.system {
            if !s.is_empty() {
                sys_parts.push(s.clone());
            }
        }
        for m in &req.messages {
            if m.role == "system" && !m.content.is_empty() {
                sys_parts.push(m.content.clone());
            }
        }
        let sys = sys_parts.join("\n");
        let choice = resolve_choice("codex", &req.model);
        let body = json!({
            "model": choice.model,
            "instructions": if sys.is_empty() { "You are a helpful assistant.".to_string() } else { sys },
            "input": messages_to_input(&req.messages),
            "stream": true,
            "store": false,
            "tools": to_responses_tools(req.tools.as_ref()),
            "tool_choice": req.tool_choice.clone().unwrap_or_else(|| json!("auto")),
            "parallel_tool_calls": req.parallel_tool_calls.unwrap_or(false),
            "reasoning": { "effort": choice.effort.clone().or_else(|| req.reasoning_effort.clone()).unwrap_or_else(|| "low".to_string()) },
            "include": [],
        });
        UpstreamRequest {
            url: CODEX_URL.to_string(),
            headers: vec![
                ("Authorization".to_string(), format!("Bearer {token}")),
                ("chatgpt-account-id".to_string(), account),
                (
                    "OpenAI-Beta".to_string(),
                    "responses=experimental".to_string(),
                ),
                ("originator".to_string(), "codex_cli_rs".to_string()),
                ("content-type".to_string(), "application/json".to_string()),
                ("accept".to_string(), "text/event-stream".to_string()),
                ("session_id".to_string(), uuid::Uuid::new_v4().to_string()),
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
            if payload.is_empty() || payload == "[DONE]" {
                continue;
            }
            let Ok(j) = serde_json::from_str::<Value>(payload) else {
                continue;
            };
            let jtype = j.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let output_index = j.get("output_index").and_then(|v| v.as_i64()).unwrap_or(0);
            match jtype {
                "response.output_text.delta" => {
                    if let Some(d) = j.get("delta").and_then(|v| v.as_str()) {
                        out.push(CommonEvent::Text {
                            text: d.to_string(),
                        });
                    }
                }
                "response.output_item.added"
                    if j.get("item")
                        .and_then(|i| i.get("type"))
                        .and_then(|v| v.as_str())
                        == Some("function_call") =>
                {
                    let item = j.get("item").unwrap();
                    let id = item
                        .get("call_id")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .or_else(|| item.get("id").and_then(|v| v.as_str()))
                        .map(|s| s.to_string());
                    let args_delta = match item.get("arguments") {
                        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
                        _ => None,
                    };
                    out.push(CommonEvent::ToolCallDelta {
                        index: output_index,
                        id,
                        name: item
                            .get("name")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string()),
                        args_delta,
                    });
                }
                "response.function_call_arguments.delta" => {
                    if let Some(d) = j.get("delta").and_then(|v| v.as_str()) {
                        out.push(CommonEvent::ToolCallDelta {
                            index: output_index,
                            id: None,
                            name: None,
                            args_delta: Some(d.to_string()),
                        });
                    }
                }
                "response.completed" => out.push(CommonEvent::Done {
                    finish_reason: Some("stop".to_string()),
                }),
                "response.failed" | "error" => {
                    let msg = j
                        .get("response")
                        .and_then(|r| r.get("error"))
                        .and_then(|e| e.get("message"))
                        .and_then(|v| v.as_str())
                        .or_else(|| j.get("message").and_then(|v| v.as_str()))
                        .unwrap_or("codex error")
                        .to_string();
                    out.push(CommonEvent::Error { message: msg });
                }
                _ => {}
            }
        }
        out
    }
}

// --- Codex CLI agent side ---

/// `it.content ?? it.text ?? ''` for a non-array content value.
fn item_content_string(it: &Value) -> String {
    match it.get("content") {
        Some(Value::Array(_)) => to_text(it.get("content").unwrap()),
        Some(c) if !c.is_null() => match c.as_str() {
            Some(s) => s.to_string(),
            None => c.to_string(),
        },
        _ => match it.get("text") {
            Some(t) if !t.is_null() => t
                .as_str()
                .map(|s| s.to_string())
                .unwrap_or_else(|| t.to_string()),
            _ => String::new(),
        },
    }
}

fn responses_input_to_messages(input: &Value) -> Vec<CommonMessage> {
    let items: Vec<Value> = match input {
        Value::Array(a) => a.clone(),
        Value::Null => Vec::new(),
        other => vec![other.clone()],
    };
    let mut out: Vec<CommonMessage> = Vec::new();
    for it in &items {
        if let Some(s) = it.as_str() {
            out.push(CommonMessage {
                role: "user".to_string(),
                content: s.to_string(),
                ..Default::default()
            });
            continue;
        }
        if it.is_null() {
            continue;
        }
        match it.get("type").and_then(|v| v.as_str()) {
            Some("function_call") => {
                let ns = it.get("namespace").and_then(|v| v.as_str());
                let name = format!(
                    "{}{}",
                    ns.map(|n| format!("{n}__")).unwrap_or_default(),
                    it.get("name").and_then(|v| v.as_str()).unwrap_or("")
                );
                let arguments = match it.get("arguments") {
                    Some(Value::String(s)) => s.clone(),
                    Some(other) => other.to_string(),
                    None => "{}".to_string(),
                };
                let id = it
                    .get("call_id")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .or_else(|| it.get("id").and_then(|v| v.as_str()))
                    .unwrap_or("")
                    .to_string();
                out.push(CommonMessage {
                    role: "assistant".to_string(),
                    content: String::new(),
                    tool_calls: Some(vec![CommonToolCall {
                        id,
                        name,
                        arguments,
                    }]),
                    ..Default::default()
                });
            }
            Some("function_call_output") => {
                // `typeof it.output === 'string' ? it.output : JSON.stringify(it.output ?? '')`.
                let content = match it.get("output") {
                    Some(Value::String(s)) => s.clone(),
                    Some(other) if !other.is_null() => other.to_string(),
                    _ => "\"\"".to_string(), // JSON.stringify('')
                };
                let call_id = it
                    .get("call_id")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .or_else(|| it.get("id").and_then(|v| v.as_str()))
                    .unwrap_or("")
                    .to_string();
                out.push(CommonMessage {
                    role: "tool".to_string(),
                    content,
                    tool_call_id: Some(call_id),
                    ..Default::default()
                });
            }
            _ => {
                let raw_role = it.get("role").and_then(|v| v.as_str()).unwrap_or("user");
                let role = if raw_role == "developer" {
                    "system"
                } else {
                    raw_role
                };
                let content = item_content_string(it);
                if !content.is_empty() {
                    out.push(CommonMessage {
                        role: role.to_string(),
                        content,
                        ..Default::default()
                    });
                }
            }
        }
    }
    out
}

fn parse_tools(tools: Option<&Value>) -> (Option<Vec<CommonToolDef>>, Option<Vec<String>>) {
    let Some(arr) = tools.and_then(|v| v.as_array()).filter(|a| !a.is_empty()) else {
        return (None, None);
    };
    let mut defs: Vec<CommonToolDef> = Vec::new();
    let mut namespaces: Vec<String> = Vec::new();
    for t in arr {
        if t.is_null() {
            continue;
        }
        if t.get("type").and_then(|v| v.as_str()) == Some("namespace") {
            let ns_name = t.get("name").and_then(|v| v.as_str());
            let ns_tools = t.get("tools").and_then(|v| v.as_array());
            if let (Some(ns_name), Some(ns_tools)) = (ns_name, ns_tools) {
                namespaces.push(ns_name.to_string());
                for nt in ns_tools {
                    let Some(nt_name) = nt
                        .get("name")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                    else {
                        continue;
                    };
                    defs.push(CommonToolDef {
                        name: format!("{ns_name}__{nt_name}"),
                        description: nt
                            .get("description")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string()),
                        parameters: nt.get("parameters").cloned(),
                        strict: nt.get("strict").and_then(|v| v.as_bool()),
                        builtin_type: None,
                    });
                }
                continue;
            }
        }
        let fnv = t.get("function").unwrap_or(t);
        let Some(name) = fnv
            .get("name")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let strict = t
            .get("strict")
            .and_then(|v| v.as_bool())
            .or_else(|| fnv.get("strict").and_then(|v| v.as_bool()));
        defs.push(CommonToolDef {
            name: name.to_string(),
            description: fnv
                .get("description")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            parameters: fnv.get("parameters").cloned(),
            strict,
            builtin_type: None,
        });
    }
    (
        if defs.is_empty() { None } else { Some(defs) },
        if namespaces.is_empty() {
            None
        } else {
            Some(namespaces)
        },
    )
}

/// `multi_agent_v1__spawn_agent` -> (name, Some(namespace)); untouched if no namespace matches.
fn split_namespaced(flat: &str, namespaces: Option<&Vec<String>>) -> (String, Option<String>) {
    if let Some(nss) = namespaces {
        for ns in nss {
            let prefix = format!("{ns}__");
            if let Some(rest) = flat.strip_prefix(&prefix) {
                return (rest.to_string(), Some(ns.clone()));
            }
        }
    }
    (flat.to_string(), None)
}

pub struct CodexAgent;

impl AgentAdapter for CodexAgent {
    fn parse_request(&self, _path: &str, body: &Value) -> CommonRequest {
        let all = responses_input_to_messages(body.get("input").unwrap_or(&Value::Null));
        let messages: Vec<CommonMessage> =
            all.iter().filter(|m| m.role != "system").cloned().collect();
        let sys_from_input: Vec<String> = all
            .iter()
            .filter(|m| m.role == "system")
            .map(|m| m.content.clone())
            .collect();
        let sys_from_input = sys_from_input.join("\n");
        let (tools, namespaces) = parse_tools(body.get("tools"));
        let mut sys_parts: Vec<String> = Vec::new();
        if let Some(instr) = body
            .get("instructions")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            sys_parts.push(instr.to_string());
        }
        if !sys_from_input.is_empty() {
            sys_parts.push(sys_from_input);
        }
        let system = if sys_parts.is_empty() {
            None
        } else {
            Some(sys_parts.join("\n"))
        };
        CommonRequest {
            model: body
                .get("model")
                .and_then(|v| v.as_str())
                .unwrap_or("codex")
                .to_string(),
            system,
            messages,
            tools,
            tool_namespaces: namespaces,
            tool_choice: body.get("tool_choice").cloned(),
            parallel_tool_calls: body.get("parallel_tool_calls").and_then(|v| v.as_bool()),
            stream: body.get("stream").and_then(|v| v.as_bool()) != Some(false),
            max_tokens: body.get("max_output_tokens").and_then(|v| v.as_i64()),
            temperature: body.get("temperature").and_then(|v| v.as_f64()),
            reasoning_effort: body
                .get("reasoning")
                .and_then(|r| r.get("effort"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
        }
    }

    fn stream_headers(&self) -> Vec<(String, String)> {
        sse_headers()
    }

    fn format_stream_chunk(&self, ev: &CommonEvent, ctx: &mut StreamCtx) -> String {
        let mut out = String::new();
        if ctx.first {
            ctx.first = false;
            out += &resp(
                "response.created",
                &json!({ "type": "response.created", "response": { "id": ctx.id, "object": "response", "status": "in_progress", "model": ctx.model, "output": [] } }),
            );
        }
        macro_rules! open_text {
            () => {{
                if ctx.text_open {
                    String::new()
                } else {
                    ctx.text_open = true;
                    ctx.text_index = ctx.next_index;
                    ctx.next_index += 1;
                    ctx.item_id = Some(format!("msg_{}", ctx.id));
                    ctx.acc = String::new();
                    let item_id = ctx.item_id.clone().unwrap();
                    resp("response.output_item.added", &json!({ "type": "response.output_item.added", "output_index": ctx.text_index, "item": { "id": item_id, "type": "message", "role": "assistant", "content": [] } }))
                        + &resp("response.content_part.added", &json!({ "type": "response.content_part.added", "item_id": item_id, "output_index": ctx.text_index, "content_index": 0, "part": { "type": "output_text", "text": "" } }))
                }
            }};
        }
        macro_rules! close_text {
            () => {{
                if !ctx.text_open {
                    String::new()
                } else {
                    ctx.text_open = false;
                    let text = ctx.acc.clone();
                    let item_id = ctx.item_id.clone().unwrap_or_default();
                    let item = json!({ "id": item_id, "type": "message", "role": "assistant", "content": [{ "type": "output_text", "text": text }] });
                    ctx.items.push(item.clone());
                    resp("response.output_text.done", &json!({ "type": "response.output_text.done", "item_id": item_id, "output_index": ctx.text_index, "content_index": 0, "text": text }))
                        + &resp("response.content_part.done", &json!({ "type": "response.content_part.done", "item_id": item_id, "output_index": ctx.text_index, "content_index": 0, "part": { "type": "output_text", "text": text } }))
                        + &resp("response.output_item.done", &json!({ "type": "response.output_item.done", "output_index": ctx.text_index, "item": item }))
                }
            }};
        }

        match ev {
            CommonEvent::Text { text } => {
                out += &open_text!();
                ctx.acc.push_str(text);
                let item_id = ctx.item_id.clone().unwrap_or_default();
                out += &resp(
                    "response.output_text.delta",
                    &json!({ "type": "response.output_text.delta", "item_id": item_id, "output_index": ctx.text_index, "content_index": 0, "delta": text }),
                );
                out
            }
            CommonEvent::ToolCall {
                id,
                name,
                arguments,
            } => {
                out += &close_text!();
                let idx = ctx.next_index;
                ctx.next_index += 1;
                let item_id = if id.is_empty() {
                    format!("fc_{idx}")
                } else {
                    format!("fc_{id}")
                };
                let call_id = if id.is_empty() {
                    item_id.clone()
                } else {
                    id.clone()
                };
                let args = if arguments.is_empty() {
                    "{}".to_string()
                } else {
                    arguments.clone()
                };
                let (name, namespace) = split_namespaced(name, ctx.tool_namespaces.as_ref());
                let mut item = Map::new();
                item.insert("id".to_string(), json!(item_id));
                item.insert("type".to_string(), json!("function_call"));
                item.insert("call_id".to_string(), json!(call_id));
                item.insert("name".to_string(), json!(name));
                if let Some(ns) = &namespace {
                    item.insert("namespace".to_string(), json!(ns));
                }
                item.insert("arguments".to_string(), json!(args));
                let item_full = Value::Object(item.clone());
                ctx.items.push(item_full.clone());
                let mut added_item = item.clone();
                added_item.insert("arguments".to_string(), json!(""));
                out += &resp(
                    "response.output_item.added",
                    &json!({ "type": "response.output_item.added", "output_index": idx, "item": Value::Object(added_item) }),
                );
                out += &resp(
                    "response.function_call_arguments.delta",
                    &json!({ "type": "response.function_call_arguments.delta", "item_id": item_id, "output_index": idx, "delta": args }),
                );
                out += &resp(
                    "response.function_call_arguments.done",
                    &json!({ "type": "response.function_call_arguments.done", "item_id": item_id, "output_index": idx, "arguments": args }),
                );
                out += &resp(
                    "response.output_item.done",
                    &json!({ "type": "response.output_item.done", "output_index": idx, "item": item_full }),
                );
                out
            }
            other => {
                if let CommonEvent::Error { message } = other {
                    out += &open_text!();
                    ctx.acc.push_str(&format!("[asx-proxy] {message}"));
                }
                if !ctx.text_open && ctx.items.is_empty() {
                    out += &open_text!();
                }
                out += &close_text!();
                out += &resp(
                    "response.completed",
                    &json!({ "type": "response.completed", "response": { "id": ctx.id, "object": "response", "status": "completed", "model": ctx.model, "output": ctx.items } }),
                );
                out
            }
        }
    }

    fn format_response(&self, resp_in: &CommonResponse, req: &CommonRequest) -> Value {
        let mut output: Vec<Value> = Vec::new();
        if !resp_in.text.is_empty() {
            output.push(json!({ "type": "message", "role": "assistant", "content": [{ "type": "output_text", "text": resp_in.text }] }));
        }
        for tc in &resp_in.tool_calls {
            let (name, namespace) = split_namespaced(&tc.name, req.tool_namespaces.as_ref());
            let mut item = Map::new();
            item.insert("id".to_string(), json!(format!("fc_{}", tc.id)));
            item.insert("type".to_string(), json!("function_call"));
            item.insert("call_id".to_string(), json!(tc.id));
            item.insert("name".to_string(), json!(name));
            if let Some(ns) = namespace {
                item.insert("namespace".to_string(), json!(ns));
            }
            item.insert(
                "arguments".to_string(),
                json!(if tc.arguments.is_empty() {
                    "{}".to_string()
                } else {
                    tc.arguments.clone()
                }),
            );
            output.push(Value::Object(item));
        }
        if output.is_empty() {
            output.push(json!({ "type": "message", "role": "assistant", "content": [{ "type": "output_text", "text": "" }] }));
        }
        json!({ "id": "resp_asx", "object": "response", "status": "completed", "output": output })
    }

    fn format_models(&self, choices: &[BackendChoice]) -> Value {
        let models: Vec<Value> = choices
            .iter()
            .enumerate()
            .map(|(i, c)| codex_model_info(&c.id, i as i64, c.effort.as_deref(), None, None))
            .collect();
        json!({ "models": models })
    }
}

fn reasoning_levels() -> Value {
    json!([
        { "effort": "low", "description": "Fast responses with lighter reasoning" },
        { "effort": "medium", "description": "Balances speed and reasoning depth" },
        { "effort": "high", "description": "Greater reasoning depth for complex problems" },
        { "effort": "xhigh", "description": "Extra-high reasoning for hard multi-step work" },
        { "effort": "max", "description": "Maximum reasoning budget" },
        { "effort": "ultra", "description": "Ultra reasoning (Sol/Terra)" },
    ])
}

/// Full codex ModelInfo (codex 0.142.x deserializes GET /models into `{ models: ModelInfo[] }`).
pub fn codex_model_info(
    slug: &str,
    priority: i64,
    effort: Option<&str>,
    provider: Option<&str>,
    hidden: Option<bool>,
) -> Value {
    let mut m = json!({
        "slug": slug,
        "display_name": slug,
        "description": null,
        "default_reasoning_level": effort.unwrap_or("medium"),
        "supported_reasoning_levels": reasoning_levels(),
        "shell_type": "shell_command",
        "visibility": "list",
        "supported_in_api": true,
        "priority": priority,
        "availability_nux": null,
        "upgrade": null,
        "base_instructions": "",
        "supports_reasoning_summaries": true,
        "default_reasoning_summary": "none",
        "support_verbosity": false,
        "default_verbosity": null,
        "apply_patch_tool_type": "freeform",
        "web_search_tool_type": "text_and_image",
        "truncation_policy": { "mode": "tokens", "limit": 10000 },
        "supports_parallel_tool_calls": true,
        "supports_image_detail_original": true,
        "context_window": 200000,
        "max_context_window": 200000,
        "auto_compact_token_limit": null,
        "experimental_supported_tools": [],
        "input_modalities": ["text", "image"],
        "supports_search_tool": false,
        "service_tiers": [],
        "additional_speed_tiers": [],
    });
    if let Some(p) = provider {
        m["provider"] = json!(p);
    }
    if let Some(h) = hidden {
        m["hidden"] = json!(h);
    }
    m
}
