//! Selectable backend models (port of `proxy/models.ts`).
//!
//! Each choice is one entry in the agent's model picker; the agent sends the chosen `id` back and
//! the backend adapter maps it to the real upstream (model + reasoning effort).
//!
//! Precedence: `ASX_<PROV>_MODELS` env > `<asx config dir>/models.json` > built-in defaults.

use serde_json::Value;
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq)]
pub struct BackendChoice {
    /// shown to the agent / picker.
    pub id: String,
    /// real upstream model id.
    pub model: String,
    /// reasoning effort, if the backend supports it.
    pub effort: Option<String>,
}

impl BackendChoice {
    fn new(id: impl Into<String>, model: impl Into<String>, effort: Option<&str>) -> Self {
        BackendChoice {
            id: id.into(),
            model: model.into(),
            effort: effort.map(|s| s.to_string()),
        }
    }
}

fn defaults(provider: &str) -> Vec<BackendChoice> {
    let p = provider.to_lowercase();
    if p == "codex" {
        return ["high", "medium", "low", "xhigh"]
            .iter()
            .map(|e| BackendChoice::new(format!("gpt-5.5-{e}"), "gpt-5.5", Some(e)))
            .collect();
    }
    if p.contains("claude") {
        return [
            "claude-opus-4-8",
            "claude-sonnet-4-6",
            "claude-haiku-4-5-20251001",
        ]
        .iter()
        .map(|m| BackendChoice::new(*m, *m, None))
        .collect();
    }
    if p == "grok" || p == "xai" {
        return vec![BackendChoice::new("grok-build", "grok-build", None)];
    }
    if p == "zai" {
        return vec![
            BackendChoice::new("glm-5.2", "glm-5.2", Some("high")),
            BackendChoice::new("glm-5.2-max", "glm-5.2", Some("max")),
            BackendChoice::new("glm-5.2[1m]", "glm-5.2[1m]", Some("high")),
            BackendChoice::new("glm-4.5-air", "glm-4.5-air", None),
        ];
    }
    vec![BackendChoice::new("asx-proxy", "asx-proxy", None)]
}

/// `"model"` or `"model:effort"` -> a choice. The picker id is `"model-effort"` (or `"model"`).
fn parse_spec(spec: &str) -> BackendChoice {
    let mut parts = spec.splitn(2, ':');
    let model = parts.next().unwrap_or("").to_string();
    let effort = parts.next().filter(|s| !s.is_empty());
    let id = match effort {
        Some(e) => format!("{model}-{e}"),
        None => model.clone(),
    };
    BackendChoice {
        id,
        model,
        effort: effort.map(|s| s.to_string()),
    }
}

/// A config entry may be a `"model[:effort]"` string or an object `{ id?, model, effort? }`.
fn normalize_entry(e: &Value) -> Option<BackendChoice> {
    if let Some(s) = e.as_str() {
        let s = s.trim();
        if !s.is_empty() {
            return Some(parse_spec(s));
        }
        return None;
    }
    if let Some(obj) = e.as_object() {
        let model = obj.get("model").and_then(|v| v.as_str())?;
        if model.is_empty() {
            return None;
        }
        let effort = obj
            .get("effort")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());
        let id = obj
            .get("id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| match effort {
                Some(e) => format!("{model}-{e}"),
                None => model.to_string(),
            });
        return Some(BackendChoice {
            id,
            model: model.to_string(),
            effort: effort.map(|s| s.to_string()),
        });
    }
    None
}

/// Override via env: `ASX_<PROV>_MODELS="model:effort,model:effort"`.
fn from_env(provider: &str) -> Option<Vec<BackendChoice>> {
    let raw = std::env::var(format!("ASX_{}_MODELS", provider.to_uppercase())).ok()?;
    if raw.is_empty() {
        return None;
    }
    let choices: Vec<BackendChoice> = raw
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(parse_spec)
        .collect();
    if choices.is_empty() {
        None
    } else {
        Some(choices)
    }
}

/// `ASX_MODELS_CONFIG` overrides the path (mainly for tests); default is `<asx config dir>/models.json`.
fn models_config_path() -> PathBuf {
    if let Ok(p) = std::env::var("ASX_MODELS_CONFIG") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    aas_core::platform::asx_config_dir().join("models.json")
}

fn load_config() -> Option<Value> {
    let file = models_config_path();
    let text = std::fs::read_to_string(file).ok()?;
    serde_json::from_str(&text).ok()
}

fn from_config_file(provider: &str) -> Option<Vec<BackendChoice>> {
    let cfg = load_config()?;
    let list = cfg.get(provider.to_lowercase())?.as_array()?;
    let choices: Vec<BackendChoice> = list.iter().filter_map(normalize_entry).collect();
    if choices.is_empty() {
        None
    } else {
        Some(choices)
    }
}

pub fn backend_choices(provider: &str) -> Vec<BackendChoice> {
    from_env(provider)
        .or_else(|| from_config_file(provider))
        .unwrap_or_else(|| defaults(provider))
}

/// Map an agent-requested id back to a concrete choice. Falls back to the default (first).
pub fn resolve_choice(provider: &str, id: &str) -> BackendChoice {
    let list = backend_choices(provider);
    list.iter()
        .find(|c| c.id == id)
        .cloned()
        .unwrap_or_else(|| list[0].clone())
}
