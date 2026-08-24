//! Login flow. Mirrors asx `runLoginFlow` (cli.ts:348-502): long-lived Claude token, Z.AI API
//! key, and native OAuth login into an isolated (or system) profile home.

use crate::ui;
use aas_core::naming::{derive_account_name, normalize_provider_key, profile_home};
use aas_core::secure_store;
use aas_core::store::AccountStore;
use aas_providers::Provider;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Native login argv, with the provider's headless/device flag appended when requested.
fn build_login_command(provider: Provider, device_auth: bool) -> anyhow::Result<Vec<String>> {
    let mut cmd = provider.login_command().ok_or_else(|| {
        anyhow::anyhow!(
            "Login flow is not supported for provider '{}'.",
            provider.id()
        )
    })?;
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
        "pi" => Some("PI_CODING_AGENT_DIR"),
        _ => None,
    }
}

#[cfg(unix)]
fn ensure_700(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}
#[cfg(not(unix))]
fn ensure_700(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)
}

/// asx `seedAgentHome` (claude only): merge `{hasCompletedOnboarding:true}` into `.claude.json`.
fn seed_agent_home(provider_key: &str, dir: &Path) -> anyhow::Result<()> {
    if provider_key != "claude" {
        return Ok(());
    }
    let p = dir.join(".claude.json");
    let mut json = std::fs::read_to_string(&p)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    if let Some(obj) = json.as_object_mut() {
        obj.insert(
            "hasCompletedOnboarding".into(),
            serde_json::Value::Bool(true),
        );
    }
    secure_store::write_restricted_file(&p, &serde_json::to_string(&json)?)?;
    Ok(())
}

fn prompt_secret(msg: &str) -> anyhow::Result<String> {
    Ok(rpassword::prompt_password(msg)?.trim().to_string())
}

fn run_native(
    cmd: &[String],
    env: Option<(&str, &Path)>,
) -> anyhow::Result<std::process::ExitStatus> {
    let mut c = Command::new(&cmd[0]);
    c.args(&cmd[1..]);
    if let Some((k, v)) = env {
        c.env(k, v);
    }
    // stdio inherited by default → interactive login works.
    let status = c.status()?;
    Ok(status)
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
    let status = run_native(&cmd, env)?;
    if !status.success() {
        let message = match status.code() {
            Some(code) => format!("native login exited with code {code}"),
            None => "native login was terminated by a signal".to_string(),
        };
        anyhow::bail!(message);
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
    AccountStore::open_default().validate_account_identity(provider.id(), &target)?;

    if device_auth && provider == Provider::Claude && !long_lived {
        ui::hint("claude has no device flow — use `--long-lived` for headless setups.");
    }

    // 1. Claude long-lived token (claude setup-token).
    if long_lived && provider == Provider::Claude {
        let cmd = vec!["claude".to_string(), "setup-token".to_string()];
        ui::step(format!(
            "Setting up a long-lived token for claude as \"{target}\""
        ));
        let status = run_native(&cmd, None)?;
        if !status.success() {
            let message = match status.code() {
                Some(code) => format!("setup-token exited with code {code}"),
                None => "setup-token was terminated by a signal".to_string(),
            };
            anyhow::bail!(message);
        }
        let token = match std::env::var("ASX_CLAUDE_CODE_OAUTH_TOKEN") {
            Ok(t) if !t.trim().is_empty() => t.trim().to_string(),
            _ => prompt_secret("Paste the long-lived token (CLAUDE_CODE_OAUTH_TOKEN): ")?,
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
            _ => prompt_secret("Paste Z.AI API key: ")?,
        };
        if key_val.is_empty() {
            anyhow::bail!("No API key provided.");
        }
        provider.validate_and_store_key(&target, &key_val).await?;
        return Ok(Some(target));
    }

    // 3. Pi authenticates interactively inside its TUI (`/login`) and has no standalone login
    // subcommand. A complete auth.json can still be supplied for a headless/import flow.
    if provider == Provider::Pi {
        if let Ok(raw) = std::env::var("PI_AUTH_JSON") {
            if !raw.trim().is_empty() {
                provider.load_pi_auth_json(&target, raw.trim()).await?;
                return Ok(Some(target));
            }
        }
        anyhow::bail!(
            "Pi has no non-interactive login command. Run `pi`, complete `/login`, then `aas load pi {target}`. Or set PI_AUTH_JSON to a complete auth.json document."
        );
    }

    // 4. Providers without a native login flow.
    if provider.login_command().is_none() {
        anyhow::bail!(
            "Login flow is not supported for provider '{}'.",
            provider.id()
        );
    }

    // 5. system profile → login into the provider's normal home.
    if system_home {
        return login_in_home(provider, &target, None, device_auth).await;
    }

    // 6. Isolated agent profile → authenticate in a fresh sibling home. The existing profile is
    // never touched until provider.load_current has validated the new credential and atomically
    // committed it under `target`.
    let stage_name = format!(
        ".login-stage-{}-{}-{}",
        target,
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    );
    let dir: PathBuf = profile_home(provider.id(), &stage_name);
    ensure_700(&dir)?;
    seed_agent_home(&key, &dir)?;
    let login_result = login_in_home(provider, &target, Some(&dir), device_auth).await;
    let cleanup_result = secure_store::delete_secret(provider.id(), &stage_name);
    match (login_result, cleanup_result) {
        (Ok(account), Ok(())) => Ok(account),
        (Ok(account), Err(cleanup)) => {
            ui::warn(format!(
                "login succeeded, but staging profile cleanup was deferred: {cleanup}"
            ));
            Ok(account)
        }
        (Err(error), Ok(())) => Err(error),
        (Err(error), Err(cleanup)) => {
            Err(error.context(format!("staging profile cleanup also failed: {cleanup}")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    #[test]
    fn codex_device_login_adds_headless_flag() {
        let command = build_login_command(Provider::Codex, true).unwrap();
        assert_eq!(command, ["codex", "login", "--device-auth"]);
    }

    #[test]
    fn grok_login_does_not_invent_a_device_flag() {
        let command = build_login_command(Provider::Grok, true).unwrap();
        assert_eq!(command, ["grok", "login"]);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn failed_isolated_relogin_preserves_existing_profile() {
        use std::os::unix::fs::PermissionsExt;

        let _guard = ENV_LOCK.lock().await;
        let root = std::env::temp_dir().join(format!(
            "aas-login-rollback-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let fake = bin.join("codex");
        std::fs::write(&fake, "#!/bin/sh\nexit 42\n").unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o700)).unwrap();

        let previous_path = std::env::var_os("PATH");
        std::env::set_var("PATH", &bin);
        std::env::set_var("AAS_CONFIG_DIR", root.join("config"));
        let store = AccountStore::open_default();
        store
            .add(aas_core::model::AccountRecord::new("codex", "existing"))
            .unwrap();
        secure_store::set_secret("codex", "existing", "old-credential").unwrap();
        let home = profile_home("codex", "existing");
        std::fs::write(home.join("settings.json"), "keep-settings").unwrap();

        let error = run_login_flow(Provider::Codex, Some("existing"), false, false, false)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("exited with code 42"));
        assert_eq!(
            secure_store::get_secret("codex", "existing").as_deref(),
            Some("old-credential")
        );
        assert_eq!(
            std::fs::read_to_string(home.join("settings.json")).unwrap(),
            "keep-settings"
        );

        match previous_path {
            Some(path) => std::env::set_var("PATH", path),
            None => std::env::remove_var("PATH"),
        }
        std::env::remove_var("AAS_CONFIG_DIR");
        let _ = std::fs::remove_dir_all(root);
    }
}
