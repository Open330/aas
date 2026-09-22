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
use std::io::{IsTerminal, Write};
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

/// The variables a shell needs in order to *be* `<provider>/<name>`.
///
/// Whatever `aas exec` installs for the same account, since both answer the same question — a
/// credential the agent takes from the environment where one exists, and the profile home, which
/// is where the session itself lives. A system profile uses the provider's default home and so
/// names none.
fn profile_env_vars(
    key: &str,
    system: bool,
    home: String,
    secret: Option<&str>,
) -> Vec<(&'static str, String)> {
    let mut vars: Vec<(&'static str, String)> = Vec::new();
    match key {
        "claude" => {
            // A long-lived token reaches Claude Code only through the environment, and wins over
            // whatever the config dir holds — so it accompanies the home rather than replacing it.
            // Exporting it alone used to leave the agent authenticated as the account while
            // writing its history and settings into `~/.claude`.
            if let Some(tok) = secret.and_then(claude_long_lived_token) {
                vars.push(("CLAUDE_CODE_OAUTH_TOKEN", tok));
            }
            if !system {
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
            if let Some(k) = secret.and_then(grok_bearer) {
                vars.push(("XAI_API_KEY", k));
            }
        }
        "zai" => {
            if let Some(k) = secret {
                vars.push(("ZAI_API_KEY", k.to_string()));
                vars.push(("ZAI_KEY", k.to_string()));
            }
        }
        "kimi" => {
            if let Some(k) = secret {
                // Both names are in circulation: Kimi's own docs use the Moonshot spelling.
                vars.push(("KIMI_API_KEY", k.to_string()));
                vars.push(("MOONSHOT_API_KEY", k.to_string()));
            }
        }
        "pi" if !system => vars.push(("PI_CODING_AGENT_DIR", home)),
        _ => {}
    }
    vars
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

    let vars = profile_env_vars(&key, system, home, secret.as_deref());

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

fn vault_passphrase() -> anyhow::Result<String> {
    if let Ok(passphrase) = std::env::var("AAS_VAULT_PASSPHRASE") {
        if passphrase.is_empty() {
            anyhow::bail!("AAS_VAULT_PASSPHRASE cannot be empty");
        }
        return Ok(passphrase);
    }
    let first = rpassword::prompt_password("Vault passphrase: ")?;
    if first.is_empty() {
        anyhow::bail!("vault passphrase cannot be empty");
    }
    let second = rpassword::prompt_password("Confirm passphrase: ")?;
    if first != second {
        anyhow::bail!("vault passphrases do not match");
    }
    Ok(first)
}

/// Export every account + credential as a portable bundle (for host-to-host migration).
fn export_all(out: Option<&Path>, vault: bool) -> anyhow::Result<()> {
    #[cfg(windows)]
    if out.is_some() && !vault {
        anyhow::bail!(
            "refusing plaintext credential-file export on Windows because owner-only ACLs cannot be guaranteed; use --vault or pipe stdout directly"
        );
    }
    let bundle = aas_import::export_bundle()?;
    let n = bundle.accounts.len();
    if vault && out.is_none() && std::io::stdout().is_terminal() {
        anyhow::bail!(
            "refusing to print an encrypted vault to the terminal; use -o <file> or pipe stdout"
        );
    }
    let bytes = if vault {
        aas_import::encrypt_bundle(&bundle, &vault_passphrase()?)?
    } else {
        format!("{}\n", serde_json::to_string_pretty(&bundle)?).into_bytes()
    };
    match out {
        Some(path) => {
            secure_store::write_private_new(path, &bytes).map_err(|error| {
                anyhow::anyhow!(
                    "could not create export destination {}: {error}",
                    path.display()
                )
            })?;
            ui::success(format!(
                "Exported {n} accounts (with credentials{}) → {}",
                if vault { ", encrypted" } else { "" },
                path.display()
            ));
            if !vault {
                ui::warn(
                    "this file holds plaintext credentials — transfer securely, then delete it.",
                );
            }
        }
        None => {
            std::io::stdout().write_all(&bytes)?;
            if !vault && std::io::stdout().is_terminal() {
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
    vault: bool,
    shell: Shell,
    out: Option<PathBuf>,
) -> anyhow::Result<()> {
    if all {
        if name.is_some() || account.is_some() {
            anyhow::bail!("--all cannot be combined with an account name");
        }
        return export_all(out.as_deref(), vault);
    }
    if vault {
        anyhow::bail!("--vault requires --all");
    }
    if out.is_some() {
        anyhow::bail!("--out requires --all");
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

#[cfg(test)]
mod tests {
    use super::*;

    const LONG_LIVED: &str = r#"{"type":"claude-code-oauth-token","token":"sk-ant-oat01-test"}"#;
    const OAUTH: &str = r#"{"claudeAiOauth":{"accessToken":"sk-ant-oat02-test"}}"#;

    fn names(vars: &[(&'static str, String)]) -> Vec<&'static str> {
        vars.iter().map(|(k, _)| *k).collect()
    }

    #[test]
    fn a_long_lived_claude_token_ships_with_the_profile_home() {
        let vars = profile_env_vars("claude", false, "/p/claude-a".into(), Some(LONG_LIVED));
        assert_eq!(
            names(&vars),
            ["CLAUDE_CODE_OAUTH_TOKEN", "CLAUDE_CONFIG_DIR"],
            "the token says who you are; the home says where the session lives"
        );
        assert_eq!(vars[0].1, "sk-ant-oat01-test");
        assert_eq!(vars[1].1, "/p/claude-a");
    }

    #[test]
    fn a_claude_system_profile_keeps_the_providers_default_home() {
        assert_eq!(
            names(&profile_env_vars(
                "claude",
                true,
                "/p/claude-a".into(),
                Some(LONG_LIVED)
            )),
            ["CLAUDE_CODE_OAUTH_TOKEN"]
        );
        assert!(profile_env_vars("claude", true, "/p/claude-a".into(), Some(OAUTH)).is_empty());
    }

    #[test]
    fn an_ordinary_claude_credential_travels_in_the_profile_home() {
        assert_eq!(
            names(&profile_env_vars(
                "claude",
                false,
                "/p/claude-a".into(),
                Some(OAUTH)
            )),
            ["CLAUDE_CONFIG_DIR"]
        );
    }

    #[test]
    fn grok_already_pairs_its_home_with_its_key() {
        assert_eq!(
            names(&profile_env_vars(
                "grok",
                false,
                "/p/grok-a".into(),
                Some(r#"{"key":"xai-test"}"#)
            )),
            ["GROK_HOME", "XAI_API_KEY"]
        );
    }
}
