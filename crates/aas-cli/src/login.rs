//! Login flow. Mirrors asx `runLoginFlow` (cli.ts:348-502): long-lived Claude token, Z.AI API
//! key, and native OAuth login into an isolated (or system) profile home.

use crate::ui;
use aas_core::naming::{derive_account_name, native_cred_file, normalize_provider_key, profile_home};
use aas_providers::Provider;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Native login argv, with the provider's headless/device flag appended when requested.
fn build_login_command(provider: Provider, device_auth: bool) -> anyhow::Result<Vec<String>> {
    let mut cmd = provider
        .login_command()
        .ok_or_else(|| anyhow::anyhow!("Login flow is not supported for provider '{}'.", provider.id()))?;
    if device_auth && provider == Provider::Codex {
        cmd.push("--device-auth".to_string());
    }
    Ok(cmd)
}

fn home_env_var(provider_key: &str) -> Option<&'static str> {
    match provider_key {
        "claude" => Some("CLAUDE_CONFIG_DIR"),
        "codex" => Some("CODEX_HOME"),
        "grok" => Some("GROK_HOME"),
        _ => None,
    }
}

#[cfg(unix)]
fn ensure_700(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::create_dir_all(dir);
    let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
}
#[cfg(not(unix))]
fn ensure_700(dir: &Path) {
    let _ = std::fs::create_dir_all(dir);
}

#[cfg(unix)]
fn set_0600(p: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o600));
}
#[cfg(not(unix))]
fn set_0600(_p: &Path) {}

/// asx `seedAgentHome` (claude only): merge `{hasCompletedOnboarding:true}` into `.claude.json`.
fn seed_agent_home(provider_key: &str, dir: &Path) {
    if provider_key != "claude" {
        return;
    }
    let p = dir.join(".claude.json");
    let mut json = std::fs::read_to_string(&p)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    if let Some(obj) = json.as_object_mut() {
        obj.insert("hasCompletedOnboarding".into(), serde_json::Value::Bool(true));
    }
    if std::fs::write(&p, serde_json::to_string(&json).unwrap_or_default()).is_ok() {
        set_0600(&p);
    }
}

fn prompt(msg: &str) -> anyhow::Result<String> {
    print!("{msg}");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(line.trim().to_string())
}

fn run_native(cmd: &[String], env: Option<(&str, &Path)>) -> anyhow::Result<i32> {
    let mut c = Command::new(&cmd[0]);
    c.args(&cmd[1..]);
    if let Some((k, v)) = env {
        c.env(k, v);
    }
    // stdio inherited by default → interactive login works.
    let status = c.status()?;
    Ok(status.code().unwrap_or(0))
}

async fn login_in_home(
    provider: Provider,
    target: &str,
    home: Option<&Path>,
    device_auth: bool,
) -> anyhow::Result<Option<String>> {
    let key = normalize_provider_key(provider.id());
    let env_var = home_env_var(&key);
    let cmd = build_login_command(provider, device_auth)?;

    let env = match (home, env_var) {
        (Some(h), Some(ev)) => Some((ev, h)),
        _ => None,
    };
    ui::step(format!(
        "Signing in to {} as \"{target}\"{}",
        provider.id(),
        if device_auth { " (headless)" } else { "" }
    ));
    if device_auth {
        ui::hint("follow the device-code prompt below");
    } else {
        ui::hint("a browser will open — finish the sign-in there");
    }
    let code = run_native(&cmd, env)?;
    if code != 0 {
        ui::warn(format!("native login exited with code {code}"));
    }

    // Load the newly logged-in session, with the home env var pointed at the profile home.
    let restore = env.map(|(ev, h)| {
        let prev = std::env::var(ev).ok();
        std::env::set_var(ev, h);
        (ev, prev)
    });
    let res = provider.load_current(target, None).await;
    if let Some((ev, prev)) = restore {
        match prev {
            Some(p) => std::env::set_var(ev, p),
            None => std::env::remove_var(ev),
        }
    }
    res?;
    Ok(Some(target.to_string()))
}

/// Returns the final account name on success, or `None` if the flow was aborted.
pub async fn run_login_flow(
    provider: Provider,
    name: Option<&str>,
    long_lived: bool,
    device_auth: bool,
    system_home: bool,
) -> anyhow::Result<Option<String>> {
    let key = normalize_provider_key(provider.id());
    let target = name
        .map(String::from)
        .unwrap_or_else(|| derive_account_name(None, provider.id()));

    if device_auth && provider == Provider::Claude && !long_lived {
        ui::hint("claude has no device flow — use `--long-lived` for headless setups.");
    }

    // 1. Claude long-lived token (claude setup-token).
    if long_lived && provider == Provider::Claude {
        let cmd = vec!["claude".to_string(), "setup-token".to_string()];
        ui::step(format!("Setting up a long-lived token for claude as \"{target}\""));
        let code = run_native(&cmd, None)?;
        if code != 0 {
            ui::warn(format!("setup-token exited with code {code}"));
            return Ok(None);
        }
        let token = match std::env::var("ASX_CLAUDE_CODE_OAUTH_TOKEN") {
            Ok(t) if !t.trim().is_empty() => t.trim().to_string(),
            _ => prompt("Paste the long-lived token (CLAUDE_CODE_OAUTH_TOKEN): ")?,
        };
        if token.is_empty() {
            anyhow::bail!("No token provided.");
        }
        provider.load_long_lived_token(&target, &token).await?;
        return Ok(Some(target));
    }

    // 2. Z.AI API key.
    if provider == Provider::Zai {
        let key_val = match std::env::var("ASX_ZAI_API_KEY") {
            Ok(k) if !k.trim().is_empty() => k.trim().to_string(),
            _ => prompt("Paste Z.AI API key: ")?,
        };
        if key_val.is_empty() {
            anyhow::bail!("No API key provided.");
        }
        provider.validate_and_store_key(&target, &key_val).await?;
        return Ok(Some(target));
    }

    // 3. Providers without a native login flow.
    if provider.login_command().is_none() {
        eprintln!("Login flow is not supported for provider '{}'.", provider.id());
        return Ok(None);
    }

    // 4. system profile → login into the provider's normal home.
    if system_home {
        return login_in_home(provider, &target, None, device_auth).await;
    }

    // 5. Isolated agent profile → login into a per-profile home.
    let dir: PathBuf = profile_home(provider.id(), &target);
    ensure_700(&dir);
    seed_agent_home(&key, &dir);
    let _ = std::fs::remove_file(dir.join(native_cred_file(provider.id())));
    login_in_home(provider, &target, Some(&dir), device_auth).await
}
