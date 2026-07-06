//! Inject the proxy endpoint into the launched native agent's environment + config (port of
//! `proxy/inject.ts`). Makes the agent binary talk to our local proxy and skip its own auth.

use crate::adapters::codex::codex_model_info;
use crate::models::backend_choices;
use serde_json::json;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

fn mkdir_0700(dir: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(dir)?;
    set_mode(dir, 0o700);
    Ok(())
}

fn write_file_0600(path: &Path, content: &str) {
    // Best-effort like asx (write failures are logged, not fatal).
    if fs::write(path, content).is_ok() {
        set_mode(path, 0o600);
    }
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode));
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) {}

/// Last-resort scratch home under the asx config dir (never `/tmp`).
fn fallback_agent_home(provider: &str) -> PathBuf {
    let dir = aas_core::platform::profiles_dir().join(".agents").join(format!("{provider}-adhoc"));
    let _ = mkdir_0700(&dir);
    dir
}

fn trim_trailing_slashes(s: &str) -> &str {
    s.trim_end_matches('/')
}

pub fn inject_proxy_endpoint(
    source_provider: &str,
    env: &mut HashMap<String, String>,
    proxy_base_url: &str,
    tmp_dir: Option<&Path>,
    backend_provider: Option<&str>,
) -> anyhow::Result<()> {
    let prov = source_provider.to_lowercase();
    let choices = backend_choices(backend_provider.unwrap_or(&prov));
    let models: Vec<String> = choices.iter().map(|c| c.id.clone()).collect();

    if prov == "codex" {
        inject_codex(tmp_dir, proxy_base_url, env, &models)?;
    } else if prov.contains("claude") {
        inject_claude(env, proxy_base_url, &models);
    } else if prov == "grok" {
        inject_grok(tmp_dir, proxy_base_url, env, &models)?;
    }
    Ok(())
}

fn inject_codex(tmp_dir: Option<&Path>, proxy_base_url: &str, env: &mut HashMap<String, String>, models: &[String]) -> anyhow::Result<()> {
    let codex_home = env
        .get("CODEX_HOME")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| tmp_dir.map(|d| d.join("codex")))
        .unwrap_or_else(|| fallback_agent_home("codex"));

    env.insert("CODEX_HOME".to_string(), codex_home.to_string_lossy().to_string());
    let cfg_path = codex_home.join("config.toml");
    let catalog_path = codex_home.join("models.json");
    mkdir_0700(&codex_home)?;

    let provider_id = "asx-proxy";
    let model = models.first().cloned().unwrap_or_else(|| "asx-proxy".to_string());
    let base = trim_trailing_slashes(proxy_base_url);

    let api_key = env.get("ASX_PROXY_API_KEY").filter(|s| !s.is_empty()).cloned().unwrap_or_else(|| "asx-proxy-dummy".to_string());
    env.insert("ASX_PROXY_API_KEY".to_string(), api_key);

    // JSON.stringify quoting for the two path/string values embedded in TOML.
    let model_q = serde_json::to_string(&model).unwrap_or_else(|_| "\"asx-proxy\"".to_string());
    let catalog_q = serde_json::to_string(&catalog_path.to_string_lossy().to_string()).unwrap_or_default();

    let clean_config = format!(
        "# ASX Proxy injected config for cross-provider execution\n# This file is inside a private CODEX_HOME for this run only.\nmodel = {model_q}\nmodel_provider = \"{provider_id}\"\nmodel_catalog_json = {catalog_q}\nmodel_context_window = 200000\nmodel_auto_compact_token_limit = 160000\nmodel_supports_reasoning_summaries = false\nmodel_reasoning_summary = \"none\"\n\n[model_providers.{provider_id}]\nname = \"ASX Proxy\"\nbase_url = \"{base}/v1\"\nenv_key = \"ASX_PROXY_API_KEY\"\nwire_api = \"responses\"\nrequires_openai_auth = false\n"
    );

    let catalog = json!({
        "models": models.iter().enumerate().map(|(i, m)| codex_model_info(m, i as i64, None, Some(provider_id), Some(false))).collect::<Vec<_>>(),
    });
    write_file_0600(&catalog_path, &serde_json::to_string_pretty(&catalog).unwrap_or_default());
    write_file_0600(&cfg_path, &clean_config);
    Ok(())
}

/// Claude Code's built-in model slots (exactly these four).
const CLAUDE_MODEL_SLOTS: [&str; 4] = ["OPUS", "SONNET", "HAIKU", "FABLE"];

fn inject_claude(env: &mut HashMap<String, String>, proxy_base_url: &str, models: &[String]) {
    env.insert("ANTHROPIC_BASE_URL".to_string(), trim_trailing_slashes(proxy_base_url).to_string());
    let has_token = env.get("ANTHROPIC_AUTH_TOKEN").map(|s| !s.is_empty()).unwrap_or(false);
    let has_key = env.get("ANTHROPIC_API_KEY").map(|s| !s.is_empty()).unwrap_or(false);
    if !has_token && !has_key {
        env.insert("ANTHROPIC_AUTH_TOKEN".to_string(), "asx-proxy-token".to_string());
    }
    // Remap the built-in slots onto backend models; leave gateway discovery OFF.
    env.remove("CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY");
    for (i, m) in models.iter().take(CLAUDE_MODEL_SLOTS.len()).enumerate() {
        let slot = CLAUDE_MODEL_SLOTS[i];
        env.insert(format!("ANTHROPIC_DEFAULT_{slot}_MODEL"), m.clone());
        env.insert(format!("ANTHROPIC_DEFAULT_{slot}_MODEL_NAME"), m.clone());
        env.insert(format!("ANTHROPIC_DEFAULT_{slot}_MODEL_DESCRIPTION"), "via asx proxy".to_string());
    }
    if let Some(first) = models.first() {
        if env.get("ANTHROPIC_MODEL").map(|s| s.is_empty()).unwrap_or(true) {
            env.insert("ANTHROPIC_MODEL".to_string(), first.clone());
        }
    }
    env.insert("OPENAI_BASE_URL".to_string(), proxy_base_url.to_string());
}

fn inject_grok(tmp_dir: Option<&Path>, proxy_base_url: &str, env: &mut HashMap<String, String>, models: &[String]) -> anyhow::Result<()> {
    let grok_home = env
        .get("GROK_HOME")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| tmp_dir.map(|d| d.join("grok")))
        .unwrap_or_else(|| fallback_agent_home("grok"));

    env.insert("GROK_HOME".to_string(), grok_home.to_string_lossy().to_string());
    mkdir_0700(&grok_home)?;

    let base = trim_trailing_slashes(proxy_base_url);
    let list: Vec<String> = if models.is_empty() { vec!["asx-proxy".to_string()] } else { models.to_vec() };

    let entries: String = list
        .iter()
        .map(|m| format!("[model.\"{m}\"]\nmodel = \"{m}\"\nbase_url = \"{base}/v1\"\nname = \"{m}\"\napi_backend = \"chat_completions\"\napi_key = \"asx-proxy-dummy\"\ncontext_window = 200000\n"))
        .collect::<Vec<_>>()
        .join("\n");

    let config_content = format!(
        "# ASX Proxy injected config for cross-provider execution\n[models]\ndefault = \"{}\"\n\n[ui]\npermission_mode = \"always-approve\"\n\n{entries}",
        list[0]
    );

    write_file_0600(&grok_home.join("config.toml"), &config_content);
    Ok(())
}
