//! Pi coding agent adapter. Pi keeps all vendor credentials in one
//! `$PI_CODING_AGENT_DIR/auth.json` (default `~/.pi/agent/auth.json`), so AAS snapshots the
//! complete document instead of attempting to split its provider entries.

use crate::common::{set_active, store_account_secret};
use aas_core::platform::pi_auth_path;
use aas_core::secure_store::{get_secret, write_restricted_file};
use aas_core::usage::Usage;
use serde_json::Value;

fn read_system_auth() -> Option<String> {
    std::fs::read_to_string(pi_auth_path())
        .ok()
        .filter(|raw| !raw.is_empty())
}

fn validate_auth(raw: &str) -> anyhow::Result<Value> {
    let value: Value = serde_json::from_str(raw)
        .map_err(|error| anyhow::anyhow!("Pi auth.json is not valid JSON: {error}"))?;
    if !value.is_object() {
        anyhow::bail!("Pi auth.json must contain a JSON object");
    }
    Ok(value)
}

fn auth_summary(raw: &str) -> (Option<String>, Option<String>) {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return (Some("auth.json".into()), None);
    };
    let Some(object) = value.as_object() else {
        return (Some("auth.json".into()), None);
    };
    let providers: Vec<&str> = object
        .iter()
        .filter_map(|(key, value)| value.is_object().then_some(key.as_str()))
        .collect();
    let email = object.values().find_map(|entry| {
        entry
            .get("email")
            .and_then(|value| value.as_str())
            .filter(|value| !value.is_empty())
            .map(String::from)
    });
    let label = if providers.is_empty() {
        Some("empty".into())
    } else {
        Some(providers.join("+"))
    };
    (label, email)
}

fn write_system_auth(raw: &str) -> anyhow::Result<()> {
    validate_auth(raw)?;
    write_restricted_file(&pi_auth_path(), raw)?;
    Ok(())
}

pub(crate) async fn usage(account: &str) -> Usage {
    match get_secret("pi", account) {
        Some(raw) => {
            let (label, _) = auth_summary(&raw);
            Usage {
                headline: label
                    .map(|value| format!("Pi auth ({value})"))
                    .unwrap_or_else(|| "Pi auth".into()),
                notes: vec!["Pi does not expose a unified quota endpoint.".into()],
                ..Default::default()
            }
        }
        None => Usage::error("pi", "No stored credential for this account."),
    }
}

pub(crate) async fn current_credential() -> Option<String> {
    read_system_auth()
}

pub(crate) async fn current_email() -> Option<String> {
    read_system_auth().and_then(|raw| auth_summary(&raw).1)
}

pub(crate) async fn load_current(account: &str, label: Option<&str>) -> anyhow::Result<()> {
    let raw = read_system_auth().ok_or_else(|| {
        anyhow::anyhow!(
            "No Pi auth found at ~/.pi/agent/auth.json (or $PI_CODING_AGENT_DIR/auth.json). Run `pi`, complete `/login`, then retry `aas load pi`."
        )
    })?;
    load_raw(account, label, &raw)
}

pub(crate) fn load_raw(account: &str, label: Option<&str>, raw: &str) -> anyhow::Result<()> {
    validate_auth(raw)?;
    let (summary, email) = auth_summary(raw);
    let label = label.or(summary.as_deref());
    store_account_secret("pi", account, label, email, raw)
}

pub(crate) fn load_raw_and_activate(account: &str, raw: &str) -> anyhow::Result<()> {
    load_raw(account, None, raw)?;
    set_active("pi", account)
}

pub(crate) async fn switch_to(account: &str) -> anyhow::Result<()> {
    let raw = get_secret("pi", account)
        .ok_or_else(|| anyhow::anyhow!("No stored credential for pi/{account}"))?;
    let previous = read_system_auth();
    write_system_auth(&raw)?;
    if let Err(error) = set_active("pi", account) {
        let rollback = match previous {
            Some(previous) => write_system_auth(&previous),
            None => match std::fs::remove_file(pi_auth_path()) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(e.into()),
            },
        };
        anyhow::bail!("could not update active Pi marker: {error}; native rollback={rollback:?}");
    }
    Ok(())
}

pub(crate) async fn clear_current() -> anyhow::Result<()> {
    write_restricted_file(&pi_auth_path(), "{}")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarizes_provider_entries_and_email() {
        let raw = r#"{"anthropic":{"type":"oauth","email":"june@example.com"},"openai":{"type":"api_key"}}"#;
        let (label, email) = auth_summary(raw);
        assert_eq!(label.as_deref(), Some("anthropic+openai"));
        assert_eq!(email.as_deref(), Some("june@example.com"));
    }

    #[test]
    fn rejects_non_object_auth() {
        assert!(validate_auth("[]").is_err());
        assert!(validate_auth("not json").is_err());
    }
}
