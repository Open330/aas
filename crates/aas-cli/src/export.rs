//! `aas export <name>` — print the shell env needed to use a profile in the current shell.
//!
//!   eval "$(aas export zai work)"      # POSIX
//!   aas export codex work | source     # (fish)
//!
//! Only the `export`/`set` lines go to stdout, so it is safe to `eval`. Hints go to stderr.

use crate::ui;
use aas_core::model::ProfileType;
use aas_core::naming::{normalize_provider_key, profile_home};
use aas_core::secure_store;
use aas_core::store::AccountStore;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

#[derive(clap::ValueEnum, Clone, Copy, Debug, Default)]
pub enum Shell {
    #[default]
    Posix,
    Powershell,
    Fish,
}

fn esc_posix(v: &str) -> String {
    v.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('$', "\\$")
        .replace('`', "\\`")
}

fn esc_ps(v: &str) -> String {
    v.replace('`', "``").replace('"', "`\"").replace('$', "`$")
}

fn fmt_line(shell: Shell, k: &str, v: &str) -> String {
    match shell {
        Shell::Posix => format!("export {k}=\"{}\"", esc_posix(v)),
        Shell::Fish => format!("set -gx {k} \"{}\"", esc_posix(v)),
        Shell::Powershell => format!("$env:{k} = \"{}\"", esc_ps(v)),
    }
}

fn claude_long_lived_token(raw: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    if v.get("type").and_then(|t| t.as_str()) == Some("claude-code-oauth-token") {
        return v.get("token").and_then(|t| t.as_str()).map(String::from);
    }
    None
}

/// Extract the Grok bearer key from the stored auth (JSON `{key}` or `{name:{key}}`, else raw).
fn grok_bearer(raw: &str) -> Option<String> {
    match serde_json::from_str::<serde_json::Value>(raw) {
        Ok(v) => {
            if let Some(k) = v.get("key").and_then(|x| x.as_str()) {
                return Some(k.to_string());
            }
            if let Some(obj) = v.as_object() {
                for (_, val) in obj {
                    if let Some(k) = val.get("key").and_then(|x| x.as_str()) {
                        return Some(k.to_string());
                    }
                }
            }
            None
        }
        Err(_) => Some(raw.to_string()),
    }
}

pub fn cmd_export(store: &AccountStore, name: &str, shell: Shell) -> anyhow::Result<()> {
    let Some(acct) = store.get_by_name(name)? else {
        anyhow::bail!("Account not found: {name}");
    };
    let key = normalize_provider_key(&acct.provider);
    let system = acct.profile_type == Some(ProfileType::System);
    let home = profile_home(&acct.provider, &acct.name)
        .display()
        .to_string();
    let secret = secure_store::get_secret(&acct.provider, &acct.name);

    let mut vars: Vec<(&str, String)> = Vec::new();
    match key.as_str() {
        "claude" => {
            if let Some(tok) = secret.as_deref().and_then(claude_long_lived_token) {
                vars.push(("CLAUDE_CODE_OAUTH_TOKEN", tok));
            } else if !system {
                vars.push(("CLAUDE_CONFIG_DIR", home));
            }
        }
        "codex" => {
            if !system {
                vars.push(("CODEX_HOME", home));
            }
        }
        "grok" => {
            if !system {
                vars.push(("GROK_HOME", home));
            }
            if let Some(k) = secret.as_deref().and_then(grok_bearer) {
                vars.push(("XAI_API_KEY", k));
            }
        }
        "zai" => {
            if let Some(k) = &secret {
                vars.push(("ZAI_API_KEY", k.clone()));
                vars.push(("ZAI_KEY", k.clone()));
            }
        }
        _ => {}
    }

    if vars.is_empty() {
        if system {
            ui::warn(format!(
                "{}/{} is a system profile — it uses the provider's default home; nothing to export.",
                acct.provider, acct.name
            ));
        } else {
            ui::warn(format!(
                "Nothing to export for {}/{}.",
                acct.provider, acct.name
            ));
        }
        return Ok(());
    }

    for (k, v) in &vars {
        println!("{}", fmt_line(shell, k, v));
    }
    // Only nudge when run interactively (not when the output is being eval'd/piped).
    if std::io::stdout().is_terminal() {
        ui::hint(format!("apply with:  eval \"$(aas export {name})\""));
    }
    Ok(())
}

#[cfg(unix)]
fn set_0600(p: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o600));
}
#[cfg(not(unix))]
fn set_0600(_p: &Path) {}

/// Export every account + credential as a portable JSON bundle (for host-to-host migration).
fn export_all(out: Option<&Path>) -> anyhow::Result<()> {
    let bundle = aas_import::export_bundle()?;
    let n = bundle.accounts.len();
    let json = serde_json::to_string_pretty(&bundle)?;
    match out {
        Some(path) => {
            std::fs::write(path, format!("{json}\n"))?;
            set_0600(path);
            ui::success(format!(
                "Exported {n} accounts (with credentials) → {}",
                path.display()
            ));
            ui::warn("this file holds plaintext credentials — transfer securely, then delete it.");
        }
        None => {
            println!("{json}");
            if std::io::stdout().is_terminal() {
                ui::warn(
                    "this bundle holds plaintext credentials — pipe it, don't leave it on screen.",
                );
                ui::hint("migrate:  aas export --all | ssh other-host aas import -");
            }
        }
    }
    Ok(())
}

/// Dispatch for the `export` command: `--all` → bundle, otherwise per-account shell env.
pub fn run(
    store: &AccountStore,
    name: Option<String>,
    account: Option<String>,
    all: bool,
    shell: Shell,
    out: Option<PathBuf>,
) -> anyhow::Result<()> {
    if all {
        if name.is_some() || account.is_some() {
            anyhow::bail!("--all cannot be combined with an account name");
        }
        return export_all(out.as_deref());
    }
    let resolved = match (name, account) {
        (Some(provider), Some(account)) => {
            let provider = normalize_provider_key(&provider);
            if store.get(&provider, &account)?.is_none() {
                anyhow::bail!("Account not found: {provider}/{account}");
            }
            Some(account)
        }
        (Some(name), None) => Some(name),
        (None, Some(_)) => unreachable!("clap cannot populate the second positional alone"),
        (None, None) => None,
    };
    match resolved {
        Some(n) => cmd_export(store, &n, shell),
        None => {
            ui::error("specify an account, or --all to export every account");
            ui::hint("e.g.  aas export codex work   |   aas export --all");
            std::process::exit(2);
        }
    }
}
