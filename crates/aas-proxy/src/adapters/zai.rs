//! Z.AI (GLM coding) backend adapter — backend only. Port of `proxy/adapters/zai.ts`.

use crate::adapters::util::{
    chat_messages_from_common, chat_tools_from_common, parse_chat_tool_deltas,
};
use crate::models::resolve_choice;
use crate::types::{BackendAdapter, CommonEvent, CommonRequest, UpstreamRequest};
use serde_json::{json, Map, Value};

const ZAI_URL: &str = "https://api.z.ai/api/coding/paas/v4/chat/completions";

/// z.ai overload codes, sometimes carried on a 200 body ({"error":{"code":"1305",...}}).
const ZAI_RETRY_CODES: [&str; 4] = ["1305", "1304", "1302", "1301"];

pub fn is_zai_overload(body: &str) -> bool {
    if body.is_empty() {
        return false;
    }
    if ZAI_RETRY_CODES
        .iter()
        .any(|c| body.contains(&format!("\"{c}\"")))
    {
        return true;
    }
    let lower = body.to_lowercase();
    lower.contains("overload")
        || lower.contains("try again later")
        || lower.contains("rate limit")
        || lower.contains("too many requests")
}

pub struct ZaiBackend;

impl BackendAdapter for ZaiBackend {
    fn has_is_retryable(&self) -> bool {
        true
    }

    fn is_retryable(&self, _status: u16, body: &str) -> bool {
        is_zai_overload(body)
    }

    fn build_request(&self, req: &CommonRequest, cred: &str) -> UpstreamRequest {
        let choice = resolve_choice("zai", &req.model);
        let messages = chat_messages_from_common(req.system.as_deref(), &req.messages);
        let mut body = Map::new();
        body.insert("model".to_string(), json!(choice.model));
        body.insert("messages".to_string(), json!(messages));
        body.insert("stream".to_string(), json!(true));
        // JS `JSON.stringify` drops undefined keys — omit rather than emit null.
        if let Some(mt) = req.max_tokens {
            body.insert("max_tokens".to_string(), json!(mt));
        }
        if let Some(t) = req.temperature {
            body.insert("temperature".to_string(), json!(t));
        }
        // GLM reasoning control: z.ai's coding endpoint takes `thinking: {type}` (not reasoning_effort).
        let effort = choice
            .effort
            .clone()
            .or_else(|| req.reasoning_effort.clone());
        if let Some(e) = effort {
            let ty = if e == "none" || e == "off" {
                "disabled"
            } else {
                "enabled"
            };
            body.insert("thinking".to_string(), json!({ "type": ty }));
        }
        if let Some(tools) = chat_tools_from_common(req.tools.as_ref()) {
            body.insert("tools".to_string(), json!(tools));
            if let Some(tc) = &req.tool_choice {
                body.insert("tool_choice".to_string(), tc.clone());
            }
        }
        UpstreamRequest {
            url: ZAI_URL.to_string(),
            headers: vec![
                ("Authorization".to_string(), format!("Bearer {cred}")),
                ("Content-Type".to_string(), "application/json".to_string()),
                ("Accept-Language".to_string(), "en-US,en".to_string()),
            ],
            body: Value::Object(body).to_string(),
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
            let ch = j
                .get("choices")
                .and_then(|v| v.as_array())
                .and_then(|a| a.first());
            let text = ch
                .and_then(|c| c.get("delta").and_then(|d| d.get("content")))
                .or_else(|| ch.and_then(|c| c.get("message").and_then(|m| m.get("content"))))
                .and_then(|v| v.as_str());
            if let Some(t) = text {
                if !t.is_empty() {
                    out.push(CommonEvent::Text {
                        text: t.to_string(),
                    });
                }
            }
            let tool_calls = ch
                .and_then(|c| c.get("delta").and_then(|d| d.get("tool_calls")))
                .or_else(|| ch.and_then(|c| c.get("message").and_then(|m| m.get("tool_calls"))))
                .and_then(|v| v.as_array());
            if let Some(tcs) = tool_calls {
                out.extend(parse_chat_tool_deltas(tcs));
            }
            if let Some(fr) = ch
                .and_then(|c| c.get("finish_reason"))
                .and_then(|v| v.as_str())
            {
                out.push(CommonEvent::Done {
                    finish_reason: Some(fr.to_string()),
                });
            }
        }
        out
    }
}
