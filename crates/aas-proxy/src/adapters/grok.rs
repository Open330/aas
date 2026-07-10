//! Grok CLI agent adapter (OpenAI Chat Completions) + Grok cloud backend.
//! Port of `proxy/adapters/grok.ts`.

use crate::adapters::util::{
    chat_messages_from_common, chat_messages_to_common, chat_tools_from_common,
    chat_tools_to_common, parse_chat_tool_deltas, sse_data as sse, sse_headers, to_text,
};
use crate::models::{resolve_choice, BackendChoice};
use crate::types::{
    AgentAdapter, BackendAdapter, CommonEvent, CommonRequest, CommonResponse, StreamCtx,
    UpstreamRequest,
};
use serde_json::{json, Value};

pub struct GrokAgent;

impl AgentAdapter for GrokAgent {
    fn parse_request(&self, _path: &str, body: &Value) -> CommonRequest {
        let empty: Vec<Value> = Vec::new();
        let msgs = body
            .get("messages")
            .and_then(|v| v.as_array())
            .unwrap_or(&empty);
        let system_parts: Vec<String> = msgs
            .iter()
            .filter(|m| m.get("role").and_then(|v| v.as_str()) == Some("system"))
            .map(|m| to_text(m.get("content").unwrap_or(&Value::Null)))
            .collect();
        let system = if system_parts.is_empty() {
            None
        } else {
            Some(system_parts.join("\n"))
        };
        let non_system: Vec<Value> = msgs
            .iter()
            .filter(|m| m.get("role").and_then(|v| v.as_str()) != Some("system"))
            .cloned()
            .collect();
        CommonRequest {
            model: body
                .get("model")
                .and_then(|v| v.as_str())
                .unwrap_or("asx-proxy")
                .to_string(),
            system,
            messages: chat_messages_to_common(&non_system),
            tools: chat_tools_to_common(body.get("tools")),
            tool_choice: body.get("tool_choice").cloned(),
            parallel_tool_calls: body.get("parallel_tool_calls").and_then(|v| v.as_bool()),
            stream: body
                .get("stream")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            max_tokens: body
                .get("max_tokens")
                .and_then(|v| v.as_i64())
                .or_else(|| body.get("max_completion_tokens").and_then(|v| v.as_i64())),
            temperature: body.get("temperature").and_then(|v| v.as_f64()),
            ..Default::default()
        }
    }

    fn stream_headers(&self) -> Vec<(String, String)> {
        sse_headers()
    }

    fn format_stream_chunk(&self, ev: &CommonEvent, ctx: &mut StreamCtx) -> String {
        let chunk = |delta: Value, finish: Value| -> String {
            sse(&json!({
                "id": ctx.id,
                "object": "chat.completion.chunk",
                "created": ctx.created,
                "model": ctx.model,
                "choices": [{ "index": 0, "delta": delta, "finish_reason": finish }],
            }))
        };
        match ev {
            CommonEvent::Text { text } => {
                let mut delta = json!({ "content": text });
                if ctx.first {
                    delta["role"] = json!("assistant");
                    ctx.first = false;
                }
                chunk(delta, Value::Null)
            }
            CommonEvent::ToolCall {
                id,
                name,
                arguments,
            } => {
                let tc_index = ctx.next_index;
                ctx.next_index += 1;
                ctx.items.push(Value::String(id.clone()));
                let mut delta = json!({
                    "tool_calls": [{ "index": tc_index, "id": id, "type": "function", "function": { "name": name, "arguments": arguments } }],
                });
                if ctx.first {
                    delta["role"] = json!("assistant");
                    ctx.first = false;
                }
                chunk(delta, Value::Null)
            }
            CommonEvent::Done { finish_reason } => {
                let reason = if !ctx.items.is_empty() {
                    "tool_calls".to_string()
                } else {
                    finish_reason
                        .clone()
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| "stop".to_string())
                };
                format!("{}data: [DONE]\n\n", chunk(json!({}), json!(reason)))
            }
            _ => format!("{}data: [DONE]\n\n", chunk(json!({}), json!("stop"))),
        }
    }

    fn format_response(&self, resp: &CommonResponse, req: &CommonRequest) -> Value {
        let mut message = json!({ "role": "assistant", "content": if resp.text.is_empty() { Value::Null } else { Value::String(resp.text.clone()) } });
        if !resp.tool_calls.is_empty() {
            let tcs: Vec<Value> = resp
                .tool_calls
                .iter()
                .map(|tc| json!({ "id": tc.id, "type": "function", "function": { "name": tc.name, "arguments": if tc.arguments.is_empty() { "{}" } else { tc.arguments.as_str() } } }))
                .collect();
            message["tool_calls"] = json!(tcs);
        }
        let finish = if !resp.tool_calls.is_empty() {
            "tool_calls".to_string()
        } else {
            resp.finish_reason
                .clone()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "stop".to_string())
        };
        json!({
            "id": "chatcmpl-asx",
            "object": "chat.completion",
            "created": now_secs(),
            "model": req.model,
            "choices": [{ "index": 0, "message": message, "finish_reason": finish }],
        })
    }

    fn format_models(&self, choices: &[BackendChoice]) -> Value {
        let data: Vec<Value> = choices
            .iter()
            .map(
                |c| json!({ "id": c.id, "object": "model", "created": 0, "owned_by": "asx-proxy" }),
            )
            .collect();
        json!({ "object": "list", "data": data })
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

const GROK_URL: &str = "https://cli-chat-proxy.grok.com/v1/chat/completions";
const GROK_VERSION: &str = "0.2.77";

fn grok_token(cred: &str) -> String {
    if let Ok(d) = serde_json::from_str::<Value>(cred) {
        if let Some(obj) = d.as_object() {
            if let Some(first_val) = obj.values().next() {
                if let Some(k) = first_val.get("key").and_then(|v| v.as_str()) {
                    return k.to_string();
                }
            }
            if let Some(k) = obj.get("key").and_then(|v| v.as_str()) {
                return k.to_string();
            }
        }
    }
    cred.to_string()
}

pub struct GrokBackend;

impl BackendAdapter for GrokBackend {
    fn build_request(&self, req: &CommonRequest, cred: &str) -> UpstreamRequest {
        let choice = resolve_choice("grok", &req.model);
        let messages = chat_messages_from_common(req.system.as_deref(), &req.messages);
        let mut body = json!({ "model": choice.model, "messages": messages, "stream": true });
        if let Some(tools) = chat_tools_from_common(req.tools.as_ref()) {
            body["tools"] = json!(tools);
            if let Some(tc) = &req.tool_choice {
                body["tool_choice"] = tc.clone();
            }
            if let Some(p) = req.parallel_tool_calls {
                body["parallel_tool_calls"] = json!(p);
            }
        }
        UpstreamRequest {
            url: GROK_URL.to_string(),
            headers: vec![
                ("content-type".to_string(), "application/json".to_string()),
                (
                    "Authorization".to_string(),
                    format!("Bearer {}", grok_token(cred)),
                ),
                ("X-XAI-Token-Auth".to_string(), "xai-grok-cli".to_string()),
                (
                    "x-grok-client-version".to_string(),
                    GROK_VERSION.to_string(),
                ),
                (
                    "x-grok-client-identifier".to_string(),
                    "grok-shell".to_string(),
                ),
                (
                    "User-Agent".to_string(),
                    format!("grok-shell/{GROK_VERSION} (macos; aarch64)"),
                ),
                ("x-grok-model-override".to_string(), choice.model.clone()),
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
            let Some(ch) = j
                .get("choices")
                .and_then(|v| v.as_array())
                .and_then(|a| a.first())
            else {
                continue;
            };
            if let Some(text) = ch
                .get("delta")
                .and_then(|d| d.get("content"))
                .and_then(|v| v.as_str())
            {
                if !text.is_empty() {
                    out.push(CommonEvent::Text {
                        text: text.to_string(),
                    });
                }
            }
            if let Some(tcs) = ch
                .get("delta")
                .and_then(|d| d.get("tool_calls"))
                .and_then(|v| v.as_array())
            {
                out.extend(parse_chat_tool_deltas(tcs));
            }
            if let Some(fr) = ch.get("finish_reason").and_then(|v| v.as_str()) {
                out.push(CommonEvent::Done {
                    finish_reason: Some(fr.to_string()),
                });
            }
        }
        out
    }
}
