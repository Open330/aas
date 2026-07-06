//! Adapter registry. Add a provider = add one module here (port of `proxy/adapters/index.ts`).

pub mod claude;
pub mod codex;
pub mod grok;
pub mod util;
pub mod zai;

use crate::types::{AgentAdapter, BackendAdapter};
use std::sync::Arc;

/// `norm(p) = contains 'claude' -> claude, else p.toLowerCase()`.
fn norm(p: &str) -> String {
    if p.contains("claude") {
        "claude".to_string()
    } else {
        p.to_lowercase()
    }
}

pub fn pick_agent(provider: &str) -> Option<Arc<dyn AgentAdapter>> {
    match norm(provider).as_str() {
        "grok" => Some(Arc::new(grok::GrokAgent)),
        "codex" => Some(Arc::new(codex::CodexAgent)),
        "claude" => Some(Arc::new(claude::ClaudeAgent)),
        _ => None,
    }
}

pub fn pick_backend(provider: &str) -> Option<Arc<dyn BackendAdapter>> {
    match norm(provider).as_str() {
        "codex" => Some(Arc::new(codex::CodexBackend)),
        "grok" => Some(Arc::new(grok::GrokBackend)),
        "claude" => Some(Arc::new(claude::ClaudeBackend)),
        "zai" => Some(Arc::new(zai::ZaiBackend)),
        _ => None,
    }
}
