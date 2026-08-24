//! Kimi (Moonshot AI) backend adapter.
//!
//! Kimi serves an Anthropic-compatible `/v1/messages`. When the agent is Claude Code the proxy
//! therefore relays the request body verbatim (see [`BackendAdapter::passthrough`]) instead of
//! routing it through COMMON — COMMON does not model `cache_control`, thinking blocks or image
//! parts, and dropping prompt caching on a per-token-billed provider costs real money. Agents on a
//! different wire (codex, grok) still take the translating path below.
//!
//! Kimi runs several platforms whose API keys are not interchangeable, so the host is supplied per
//! account rather than hardcoded.

use crate::adapters::claude::{anth_tool_choice, common_to_anth_messages, ClaudeBackend};
use crate::models::resolve_choice_for;
use crate::types::{BackendAdapter, CommonEvent, CommonRequest, UpstreamRequest};
use serde_json::{json, Value};

/// Used when an account has no recorded endpoint (Kimi's per-token billing platform).
pub const DEFAULT_KIMI_BASE: &str = "https://api.moonshot.ai";

pub struct KimiBackend {
    base: String,
}

impl KimiBackend {
    pub fn new(base: Option<&str>) -> Self {
        let base = base
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(DEFAULT_KIMI_BASE);
        KimiBackend {
            base: base.trim_end_matches('/').to_string(),
        }
    }

    fn messages_url(&self) -> String {
        format!("{}/v1/messages", self.base)
    }

    /// Kimi authenticates with a plain bearer key. None of Claude Code's OAuth beta headers apply.
    fn headers(&self, cred: &str) -> Vec<(String, String)> {
        vec![
            ("content-type".to_string(), "application/json".to_string()),
            (
                "authorization".to_string(),
                format!("Bearer {}", cred.trim()),
            ),
            ("anthropic-version".to_string(), "2023-06-01".to_string()),
        ]
    }
}

/// Whether an agent talks the Anthropic wire, and can therefore be relayed rather than translated.
fn speaks_anthropic(agent_provider: &str) -> bool {
    agent_provider.to_lowercase().contains("claude")
}

impl BackendAdapter for KimiBackend {
    fn passthrough(
        &self,
        agent_provider: &str,
        body: &Value,
        cred: &str,
    ) -> Option<UpstreamRequest> {
        if !speaks_anthropic(agent_provider) {
            return None;
        }
        let mut body = body.clone();
        let requested = body
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or_default();
        // The agent picked one of our catalog ids; map it back to the real upstream model.
        let choice = resolve_choice_for("kimi", Some(&self.base), requested);
        body["model"] = json!(choice.model);
        Some(UpstreamRequest {
            url: self.messages_url(),
            headers: self.headers(cred),
            body: body.to_string(),
        })
    }

    fn build_request(&self, req: &CommonRequest, cred: &str) -> UpstreamRequest {
        let choice = resolve_choice_for("kimi", Some(&self.base), &req.model);
        let mut body = json!({
            "model": choice.model,
            "messages": common_to_anth_messages(&req.messages),
            "stream": true,
            "max_tokens": req.max_tokens.unwrap_or(8192),
        });
        if let Some(system) = &req.system {
            body["system"] = json!([{ "type": "text", "text": system }]);
        }
        if let Some(temperature) = req.temperature {
            body["temperature"] = json!(temperature);
        }
        let tools: Vec<Value> = req
            .tools
            .as_ref()
            .map(|ts| {
                ts.iter()
                    .map(|t| {
                        json!({
                            "name": t.name,
                            "description": t.description.clone().unwrap_or_default(),
                            "input_schema": t.parameters.clone().unwrap_or_else(|| json!({ "type": "object", "properties": {} })),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        if !tools.is_empty() {
            body["tools"] = json!(tools);
            if let Some(tc) = anth_tool_choice(req.tool_choice.as_ref()) {
                body["tool_choice"] = tc;
            }
        }
        UpstreamRequest {
            url: self.messages_url(),
            headers: self.headers(cred),
            body: body.to_string(),
        }
    }

    /// Kimi streams the Anthropic event shape, so Claude's parser applies unchanged.
    fn parse_stream_chunk(&self, event_block: &str) -> Vec<CommonEvent> {
        ClaudeBackend.parse_stream_chunk(event_block)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passthrough_only_applies_to_anthropic_agents() {
        let backend = KimiBackend::new(None);
        let body = json!({"model": "kimi-k2", "messages": []});
        assert!(backend.passthrough("claude", &body, "sk-x").is_some());
        assert!(backend.passthrough("claude-code", &body, "sk-x").is_some());
        assert!(backend.passthrough("codex", &body, "sk-x").is_none());
        assert!(backend.passthrough("grok", &body, "sk-x").is_none());
    }

    #[test]
    fn passthrough_preserves_unmodelled_fields_and_swaps_auth() {
        let backend = KimiBackend::new(None);
        // `cache_control` and `thinking` are exactly what the COMMON round trip would drop.
        let body = json!({
            "model": "kimi-k2",
            "system": [{"type": "text", "text": "sys", "cache_control": {"type": "ephemeral"}}],
            "thinking": {"type": "enabled", "budget_tokens": 1024},
            "messages": [{"role": "user", "content": [{"type": "image", "source": {"type": "base64"}}]}],
            "stream": false
        });
        let up = backend
            .passthrough("claude", &body, "  sk-secret  ")
            .unwrap();
        let sent: Value = serde_json::from_str(&up.body).unwrap();
        assert_eq!(sent["system"][0]["cache_control"]["type"], "ephemeral");
        assert_eq!(sent["thinking"]["budget_tokens"], 1024);
        assert_eq!(sent["messages"][0]["content"][0]["type"], "image");
        assert_eq!(sent["stream"], false, "client's stream choice is preserved");
        assert_eq!(up.url, "https://api.moonshot.ai/v1/messages");

        let auth = up
            .headers
            .iter()
            .find(|(k, _)| k == "authorization")
            .map(|(_, v)| v.as_str());
        assert_eq!(auth, Some("Bearer sk-secret"));
        // Claude Code's OAuth beta headers must not be forwarded to a third party.
        assert!(!up.headers.iter().any(|(k, _)| k == "anthropic-beta"));
    }

    #[test]
    fn account_endpoint_selects_the_platform_and_is_normalized() {
        let cn = KimiBackend::new(Some("https://api.moonshot.cn/"));
        assert_eq!(cn.messages_url(), "https://api.moonshot.cn/v1/messages");
        let code = KimiBackend::new(Some("https://api.kimi.com/coding"));
        assert_eq!(
            code.messages_url(),
            "https://api.kimi.com/coding/v1/messages"
        );
        // Blank falls back rather than producing "/v1/messages".
        assert_eq!(
            KimiBackend::new(Some("   ")).messages_url(),
            "https://api.moonshot.ai/v1/messages"
        );
    }
}
