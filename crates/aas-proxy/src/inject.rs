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

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode));
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) {}

/// Last-resort scratch home under the asx config dir (never `/tmp`).
fn fallback_agent_home(provider: &str) -> PathBuf {
    let dir = aas_core::platform::profiles_dir()
        .join(".agents")
        .join(format!("{provider}-adhoc"));
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
    proxy_auth_token: &str,
    tmp_dir: Option<&Path>,
    backend_provider: Option<&str>,
    bypass: bool,
) -> anyhow::Result<()> {
    let prov = source_provider.to_lowercase();
    let choices = backend_choices(backend_provider.unwrap_or(&prov));
    let models: Vec<String> = choices.iter().map(|c| c.id.clone()).collect();

    if prov == "codex" {
        inject_codex(tmp_dir, proxy_base_url, proxy_auth_token, env, &models)?;
    } else if prov.contains("claude") {
        inject_claude(env, proxy_base_url, proxy_auth_token, &models);
    } else if prov == "grok" {
        inject_grok(
            tmp_dir,
            proxy_base_url,
            proxy_auth_token,
            env,
            &models,
            bypass,
        )?;
    }
    Ok(())
}

fn inject_codex(
    tmp_dir: Option<&Path>,
    proxy_base_url: &str,
    proxy_auth_token: &str,
    env: &mut HashMap<String, String>,
    models: &[String],
) -> anyhow::Result<()> {
    let codex_home = tmp_dir
        .map(PathBuf::from)
        .or_else(|| {
            env.get("CODEX_HOME")
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
        })
        .unwrap_or_else(|| fallback_agent_home("codex"));

    env.insert(
        "CODEX_HOME".to_string(),
        codex_home.to_string_lossy().to_string(),
    );
    let cfg_path = codex_home.join("config.toml");
    let catalog_path = codex_home.join("models.json");
    mkdir_0700(&codex_home)?;

    let provider_id = "asx-proxy";
    let model = models
        .first()
        .cloned()
        .unwrap_or_else(|| "asx-proxy".to_string());
    let base = trim_trailing_slashes(proxy_base_url);

    env.insert(
        "ASX_PROXY_API_KEY".to_string(),
        proxy_auth_token.to_string(),
    );

    // JSON.stringify quoting for the two path/string values embedded in TOML.
    let model_q = serde_json::to_string(&model).unwrap_or_else(|_| "\"asx-proxy\"".to_string());
    let catalog_q =
        serde_json::to_string(&catalog_path.to_string_lossy().to_string()).unwrap_or_default();

    let clean_config = format!(
        "# ASX Proxy injected config for cross-provider execution\n# This file is inside a private CODEX_HOME for this run only.\nmodel = {model_q}\nmodel_provider = \"{provider_id}\"\nmodel_catalog_json = {catalog_q}\nmodel_context_window = 200000\nmodel_auto_compact_token_limit = 160000\nmodel_supports_reasoning_summaries = false\nmodel_reasoning_summary = \"none\"\n\n[model_providers.{provider_id}]\nname = \"ASX Proxy\"\nbase_url = \"{base}/v1\"\nenv_key = \"ASX_PROXY_API_KEY\"\nwire_api = \"responses\"\nrequires_openai_auth = false\n"
    );

    let catalog = json!({
        "models": models.iter().enumerate().map(|(i, m)| codex_model_info(m, i as i64, None, Some(provider_id), Some(false))).collect::<Vec<_>>(),
    });
    aas_core::secure_store::write_restricted_file(
        &catalog_path,
        &serde_json::to_string_pretty(&catalog)?,
    )?;
    aas_core::secure_store::write_restricted_file(&cfg_path, &clean_config)?;
    Ok(())
}

/// Claude Code's built-in model slots (exactly these four).
const CLAUDE_MODEL_SLOTS: [&str; 4] = ["OPUS", "SONNET", "HAIKU", "FABLE"];

fn inject_claude(
    env: &mut HashMap<String, String>,
    proxy_base_url: &str,
    proxy_auth_token: &str,
    models: &[String],
) {
    env.insert(
        "ANTHROPIC_BASE_URL".to_string(),
        trim_trailing_slashes(proxy_base_url).to_string(),
    );
    env.insert(
        "ANTHROPIC_AUTH_TOKEN".to_string(),
        proxy_auth_token.to_string(),
    );
    env.remove("ANTHROPIC_API_KEY");
    // Remap the built-in slots onto backend models; leave gateway discovery OFF.
    env.remove("CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY");
    for (i, m) in models.iter().take(CLAUDE_MODEL_SLOTS.len()).enumerate() {
        let slot = CLAUDE_MODEL_SLOTS[i];
        env.insert(format!("ANTHROPIC_DEFAULT_{slot}_MODEL"), m.clone());
        env.insert(format!("ANTHROPIC_DEFAULT_{slot}_MODEL_NAME"), m.clone());
        env.insert(
            format!("ANTHROPIC_DEFAULT_{slot}_MODEL_DESCRIPTION"),
            "via asx proxy".to_string(),
        );
    }
    if let Some(first) = models.first() {
        if env
            .get("ANTHROPIC_MODEL")
            .map(|s| s.is_empty())
            .unwrap_or(true)
        {
            env.insert("ANTHROPIC_MODEL".to_string(), first.clone());
        }
    }
    env.insert("OPENAI_BASE_URL".to_string(), proxy_base_url.to_string());
    env.insert("OPENAI_API_KEY".to_string(), proxy_auth_token.to_string());
}

fn inject_grok(
    tmp_dir: Option<&Path>,
    proxy_base_url: &str,
    proxy_auth_token: &str,
    env: &mut HashMap<String, String>,
    models: &[String],
    bypass: bool,
) -> anyhow::Result<()> {
    let grok_home = tmp_dir
        .map(PathBuf::from)
        .or_else(|| {
            env.get("GROK_HOME")
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
        })
        .unwrap_or_else(|| fallback_agent_home("grok"));

    env.insert(
        "GROK_HOME".to_string(),
        grok_home.to_string_lossy().to_string(),
    );
    mkdir_0700(&grok_home)?;

    let base = trim_trailing_slashes(proxy_base_url);
    let list: Vec<String> = if models.is_empty() {
        vec!["asx-proxy".to_string()]
    } else {
        models.to_vec()
    };
    let proxy_auth_token = serde_json::to_string(proxy_auth_token)?;

    let entries: String = list
        .iter()
        .map(|m| format!("[model.\"{m}\"]\nmodel = \"{m}\"\nbase_url = \"{base}/v1\"\nname = \"{m}\"\napi_backend = \"chat_completions\"\napi_key = {proxy_auth_token}\ncontext_window = 200000\n"))
        .collect::<Vec<_>>()
        .join("\n");

    let permission_config = if bypass {
        "[ui]\npermission_mode = \"always-approve\"\n\n"
    } else {
        ""
    };
    let config_content = format!(
        "# ASX Proxy injected config for cross-provider execution\n[models]\ndefault = \"{}\"\n\n{permission_config}{entries}",
        list[0],
    );

    aas_core::secure_store::write_restricted_file(&grok_home.join("config.toml"), &config_content)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dir(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!("aas-proxy-{label}-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn explicit_codex_home_wins_over_inherited_environment() {
        let scratch = test_dir("codex-scratch");
        let inherited = test_dir("codex-inherited");
        let mut env = HashMap::from([("CODEX_HOME".to_string(), inherited.display().to_string())]);

        inject_proxy_endpoint(
            "codex",
            &mut env,
            "http://127.0.0.1:1234",
            "secret-token",
            Some(&scratch),
            Some("zai"),
            false,
        )
        .unwrap();

        assert_eq!(env.get("CODEX_HOME"), Some(&scratch.display().to_string()));
        assert_eq!(
            env.get("ASX_PROXY_API_KEY").map(String::as_str),
            Some("secret-token")
        );
        assert!(scratch.join("config.toml").is_file());
        assert!(!inherited.join("config.toml").exists());
        let _ = fs::remove_dir_all(scratch);
    }

    #[test]
    fn grok_permission_bypass_is_opt_in() {
        let safe_home = test_dir("grok-safe");
        let bypass_home = test_dir("grok-bypass");
        let mut safe_env = HashMap::new();
        let mut bypass_env = HashMap::new();

        inject_proxy_endpoint(
            "grok",
            &mut safe_env,
            "http://127.0.0.1:1234",
            "safe-token",
            Some(&safe_home),
            Some("zai"),
            false,
        )
        .unwrap();
        inject_proxy_endpoint(
            "grok",
            &mut bypass_env,
            "http://127.0.0.1:1234",
            "bypass-token",
            Some(&bypass_home),
            Some("zai"),
            true,
        )
        .unwrap();

        let safe = fs::read_to_string(safe_home.join("config.toml")).unwrap();
        let bypass = fs::read_to_string(bypass_home.join("config.toml")).unwrap();
        assert!(!safe.contains("always-approve"));
        assert!(safe.contains("api_key = \"safe-token\""));
        assert!(bypass.contains("permission_mode = \"always-approve\""));
        assert!(bypass.contains("api_key = \"bypass-token\""));
        let _ = fs::remove_dir_all(safe_home);
        let _ = fs::remove_dir_all(bypass_home);
    }

    #[test]
    fn claude_injection_replaces_inherited_credentials() {
        let mut env = HashMap::from([
            ("ANTHROPIC_AUTH_TOKEN".to_string(), "old-token".to_string()),
            ("ANTHROPIC_API_KEY".to_string(), "old-key".to_string()),
        ]);

        inject_proxy_endpoint(
            "claude",
            &mut env,
            "http://127.0.0.1:1234",
            "run-token",
            None,
            Some("zai"),
            false,
        )
        .unwrap();

        assert_eq!(
            env.get("ANTHROPIC_AUTH_TOKEN").map(String::as_str),
            Some("run-token")
        );
        assert!(!env.contains_key("ANTHROPIC_API_KEY"));
        assert_eq!(
            env.get("OPENAI_API_KEY").map(String::as_str),
            Some("run-token")
        );
    }
}
