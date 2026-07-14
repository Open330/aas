//! Selectable backend models (port of `proxy/models.ts`).
//!
//! Each choice is one entry in the agent's model picker; the agent sends the chosen `id` back and
//! the backend adapter maps it to the real upstream (model + reasoning effort).
//!
//! Precedence: `ASX_<PROV>_MODELS` env > `<asx config dir>/models.json` > live provider
//! discovery > built-in defaults. Live results are cached for the lifetime of the proxy process.

use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{OnceLock, RwLock};

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
        let mut out = vec![
            BackendChoice::new("gpt-5.6-sol-high", "gpt-5.6-sol", Some("high")),
            BackendChoice::new("gpt-5.6-sol-medium", "gpt-5.6-sol", Some("medium")),
            BackendChoice::new("gpt-5.6-sol-low", "gpt-5.6-sol", Some("low")),
            BackendChoice::new("gpt-5.6-sol-xhigh", "gpt-5.6-sol", Some("xhigh")),
        ];
        for effort in ["max", "ultra"] {
            out.push(BackendChoice::new(
                format!("gpt-5.6-sol-{effort}"),
                "gpt-5.6-sol",
                Some(effort),
            ));
        }
        for effort in ["high", "medium", "low", "xhigh", "max", "ultra"] {
            out.push(BackendChoice::new(
                format!("gpt-5.6-terra-{effort}"),
                "gpt-5.6-terra",
                Some(effort),
            ));
        }
        for effort in ["high", "medium", "low", "xhigh", "max"] {
            out.push(BackendChoice::new(
                format!("gpt-5.6-luna-{effort}"),
                "gpt-5.6-luna",
                Some(effort),
            ));
        }
        for effort in ["high", "medium", "low", "xhigh"] {
            out.push(BackendChoice::new(
                format!("gpt-5.5-{effort}"),
                "gpt-5.5",
                Some(effort),
            ));
        }
        return out;
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

fn remote_cache() -> &'static RwLock<HashMap<String, Vec<BackendChoice>>> {
    static CACHE: OnceLock<RwLock<HashMap<String, Vec<BackendChoice>>>> = OnceLock::new();
    CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

fn from_cache(provider: &str) -> Option<Vec<BackendChoice>> {
    remote_cache()
        .read()
        .ok()
        .and_then(|cache| cache.get(&provider.to_lowercase()).cloned())
        .filter(|choices| !choices.is_empty())
}

fn put_cache(provider: &str, choices: &[BackendChoice]) {
    if choices.is_empty() {
        return;
    }
    if let Ok(mut cache) = remote_cache().write() {
        cache.insert(provider.to_lowercase(), choices.to_vec());
    }
}

pub fn backend_choices(provider: &str) -> Vec<BackendChoice> {
    from_env(provider)
        .or_else(|| from_config_file(provider))
        .or_else(|| from_cache(provider))
        .unwrap_or_else(|| defaults(provider))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentTier {
    Opus,
    Sonnet,
    Haiku,
    Fable,
}

fn has_word(value: &str, wanted: &str) -> bool {
    value
        .split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
        .any(|part| part == wanted)
}

pub fn detect_agent_tier(id: &str) -> Option<AgentTier> {
    let id = id.to_lowercase();
    if has_word(&id, "haiku") {
        Some(AgentTier::Haiku)
    } else if has_word(&id, "fable") {
        Some(AgentTier::Fable)
    } else if has_word(&id, "sonnet") {
        Some(AgentTier::Sonnet)
    } else if has_word(&id, "opus") {
        Some(AgentTier::Opus)
    } else {
        None
    }
}

fn text(choice: &BackendChoice) -> String {
    format!("{}{}", choice.model, choice.id).to_lowercase()
}

pub fn pick_tier_choice(list: &[BackendChoice], tier: AgentTier) -> BackendChoice {
    let fallback = || {
        list.first()
            .cloned()
            .unwrap_or_else(|| BackendChoice::new("asx-proxy", "asx-proxy", None))
    };
    let effort = |wanted: &str| {
        list.iter()
            .find(|choice| choice.effort.as_deref() == Some(wanted))
            .cloned()
    };
    match tier {
        AgentTier::Haiku => list
            .iter()
            .find(|choice| {
                choice.effort.as_deref() == Some("low")
                    && ["sol", "gpt-5.5", "default"]
                        .iter()
                        .any(|needle| text(choice).contains(needle))
            })
            .cloned()
            .or_else(|| effort("low"))
            .or_else(|| {
                list.iter()
                    .find(|choice| {
                        ["air", "mini", "fast", "flash", "haiku"]
                            .iter()
                            .any(|needle| has_word(&choice.id.to_lowercase(), needle))
                    })
                    .cloned()
            })
            .or_else(|| {
                list.iter()
                    .find(|choice| choice.id.contains("low"))
                    .cloned()
            })
            .or_else(|| list.get(2.min(list.len().saturating_sub(1))).cloned())
            .unwrap_or_else(fallback),
        AgentTier::Sonnet => list
            .iter()
            .find(|choice| {
                choice.effort.as_deref() == Some("medium")
                    && ["sol", "gpt-5.5"]
                        .iter()
                        .any(|needle| text(choice).contains(needle))
            })
            .cloned()
            .or_else(|| effort("medium"))
            .or_else(|| {
                list.iter()
                    .find(|choice| {
                        ["terra", "sonnet", "medium"]
                            .iter()
                            .any(|needle| choice.id.to_lowercase().contains(needle))
                    })
                    .cloned()
            })
            .or_else(|| list.get(1.min(list.len().saturating_sub(1))).cloned())
            .unwrap_or_else(fallback),
        AgentTier::Fable => list
            .iter()
            .find(|choice| matches!(choice.effort.as_deref(), Some("xhigh" | "max" | "ultra")))
            .cloned()
            .or_else(|| {
                list.iter()
                    .find(|choice| {
                        ["xhigh", "max", "ultra", "fable"]
                            .iter()
                            .any(|needle| choice.id.to_lowercase().contains(needle))
                    })
                    .cloned()
            })
            .unwrap_or_else(fallback),
        AgentTier::Opus => list
            .iter()
            .find(|choice| {
                choice.effort.as_deref() == Some("high")
                    && ["sol", "opus", "gpt-5.5"]
                        .iter()
                        .any(|needle| text(choice).contains(needle))
            })
            .cloned()
            .or_else(|| effort("high"))
            .unwrap_or_else(fallback),
    }
}

/// Map an agent-requested id back to a concrete choice. Exact id/model wins, then Claude's
/// tier aliases, then the provider default.
pub fn resolve_choice(provider: &str, id: &str) -> BackendChoice {
    let list = backend_choices(provider);
    if let Some(exact) = list
        .iter()
        .find(|choice| choice.id == id || choice.model == id)
    {
        return exact.clone();
    }
    let stripped = id.strip_prefix("claude-asx-").unwrap_or(id);
    if stripped != id {
        if let Some(exact) = list
            .iter()
            .find(|choice| choice.id == stripped || choice.model == stripped)
        {
            return exact.clone();
        }
    }
    if let Some(tier) = detect_agent_tier(id).or_else(|| detect_agent_tier(stripped)) {
        return pick_tier_choice(&list, tier);
    }
    list.first()
        .cloned()
        .unwrap_or_else(|| BackendChoice::new("asx-proxy", "asx-proxy", None))
}

fn grok_token_from_credential(credential: &str) -> Option<String> {
    if credential.is_empty() {
        return None;
    }
    if let Ok(value) = serde_json::from_str::<Value>(credential) {
        if let Some(key) = value.get("key").and_then(Value::as_str) {
            return Some(key.to_string());
        }
        if let Some(key) = value.as_object().and_then(|object| {
            object
                .values()
                .find_map(|entry| entry.get("key").and_then(Value::as_str))
        }) {
            return Some(key.to_string());
        }
    }
    Some(credential.to_string())
}

pub(crate) fn grok_models_to_choices(data: &[Value]) -> Vec<BackendChoice> {
    let mut out = Vec::new();
    for entry in data {
        let Some(model) = entry
            .get("model")
            .and_then(Value::as_str)
            .or_else(|| entry.get("id").and_then(Value::as_str))
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        let base_id = entry
            .get("id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .unwrap_or(model);
        let mut efforts: Vec<(String, bool)> = entry
            .get("reasoning_efforts")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|effort| {
                effort
                    .get("value")
                    .or_else(|| effort.get("id"))
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                    .map(|value| {
                        (
                            value.to_string(),
                            effort.get("default").and_then(Value::as_bool) == Some(true),
                        )
                    })
            })
            .collect();
        if entry
            .get("supports_reasoning_effort")
            .and_then(Value::as_bool)
            == Some(true)
            && !efforts.is_empty()
        {
            efforts.sort_by_key(|(_, is_default)| !*is_default);
            for (effort, _) in efforts {
                out.push(BackendChoice::new(
                    format!("{base_id}-{effort}"),
                    model,
                    Some(&effort),
                ));
            }
        } else {
            out.push(BackendChoice::new(base_id, model, None));
        }
    }
    out
}

fn zai_models_to_choices(data: &[Value]) -> Vec<BackendChoice> {
    data.iter()
        .filter_map(|entry| {
            let id = entry
                .get("id")
                .and_then(Value::as_str)
                .or_else(|| entry.get("model").and_then(Value::as_str))?
                .trim();
            if id.is_empty() {
                return None;
            }
            let effort = (id.to_lowercase().starts_with("glm-5")
                && !id.to_lowercase().contains("air"))
            .then_some("high");
            Some(BackendChoice::new(id, id, effort))
        })
        .collect()
}

async fn fetch_remote_choices(
    client: &reqwest::Client,
    provider: &str,
    credential: &str,
) -> Option<Vec<BackendChoice>> {
    let response = match provider {
        "grok" | "xai" => {
            let token = grok_token_from_credential(credential)?;
            let version = aas_core::platform::grok_version();
            client
                .get("https://cli-chat-proxy.grok.com/v1/models")
                .timeout(std::time::Duration::from_secs(8))
                .bearer_auth(token)
                .header("X-XAI-Token-Auth", "xai-grok-cli")
                .header("x-grok-client-version", &version)
                .header("x-grok-client-identifier", "grok-shell")
                .header(
                    reqwest::header::USER_AGENT,
                    format!(
                        "grok-shell/{version} ({}; {})",
                        std::env::consts::OS,
                        std::env::consts::ARCH
                    ),
                )
                .send()
                .await
                .ok()?
        }
        "zai" => client
            .get("https://api.z.ai/api/coding/paas/v4/models")
            .timeout(std::time::Duration::from_secs(8))
            .bearer_auth(credential.trim())
            .send()
            .await
            .ok()?,
        _ => return None,
    };
    if !response.status().is_success() {
        return None;
    }
    let body: Value = response.json().await.ok()?;
    let list = body
        .get("data")
        .and_then(Value::as_array)
        .or_else(|| body.as_array())?;
    let choices = if matches!(provider, "grok" | "xai") {
        grok_models_to_choices(list)
    } else {
        zai_models_to_choices(list)
    };
    (!choices.is_empty()).then_some(choices)
}

/// Refresh the live Grok/Z.AI model catalog once for this short-lived proxy process. Explicit
/// environment/file overrides remain authoritative and suppress the network request.
pub async fn refresh_backend_choices(
    client: &reqwest::Client,
    provider: &str,
    credential: &str,
) -> Vec<BackendChoice> {
    let provider = provider.to_lowercase();
    if let Some(pinned) = from_env(&provider).or_else(|| from_config_file(&provider)) {
        put_cache(&provider, &pinned);
        return pinned;
    }
    if let Some(remote) = fetch_remote_choices(client, &provider, credential).await {
        put_cache(&provider, &remote);
        return remote;
    }
    backend_choices(&provider)
}

#[cfg(test)]
pub(crate) fn clear_remote_model_cache() {
    if let Ok(mut cache) = remote_cache().write() {
        cache.clear();
    }
}
