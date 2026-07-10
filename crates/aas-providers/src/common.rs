//! Shared helpers for the provider adapters: HTTP client, time, JSON access, and the
//! account-store / secret-store glue that every adapter calls after a successful load.

use aas_core::model::AccountRecord;
use aas_core::secure_store;
use aas_core::store::AccountStore;
use serde_json::Value;

/// A reqwest client with a sane timeout. asx's Claude fetch uses 15s; others use `fetch`
/// defaults — a single shared bound is close enough and keeps `list -u` from hanging.
pub(crate) fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .unwrap_or_default()
}

/// Milliseconds since the Unix epoch (asx `Date.now()`).
pub(crate) fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Render a JSON scalar the way asx string-interpolation would (`${value}`).
pub(crate) fn value_display(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// First finite `f64` among two candidate keys (asx `a ?? b`).
pub(crate) fn num_alt(v: &Value, a: &str, b: &str) -> Option<f64> {
    v.get(a)
        .and_then(|x| x.as_f64())
        .or_else(|| v.get(b).and_then(|x| x.as_f64()))
}

/// Reserve account identity in the locked metadata store before writing its credential. This
/// closes the cross-process gap where two colliding logical names could both validate and then
/// write the same sanitized profile path. Restore the prior record/secret on credential failure.
pub(crate) fn store_account_secret(
    provider: &str,
    name: &str,
    label: Option<&str>,
    email: Option<String>,
    secret: &str,
) -> anyhow::Result<()> {
    let store = AccountStore::open_default();
    let previous_account = store.get(provider, name)?;
    let previous_secret = secure_store::get_secret(provider, name);

    let mut record = AccountRecord::new(provider, name);
    record.label = Some(label.unwrap_or(name).to_string());
    record.email = email;
    store.add(record)?;

    if let Err(error) = secure_store::set_secret(provider, name, secret) {
        let mut rollback_errors = Vec::new();
        match previous_account {
            Some(previous) => {
                if let Err(rollback) = store.add(previous) {
                    rollback_errors.push(format!("account={rollback}"));
                }
            }
            None => {
                if let Err(rollback) = store.remove(provider, name) {
                    rollback_errors.push(format!("account={rollback}"));
                }
            }
        }
        match previous_secret {
            Some(previous) => {
                if let Err(rollback) = secure_store::set_secret(provider, name, &previous) {
                    rollback_errors.push(format!("credential={rollback}"));
                }
            }
            None => {
                if let Err(rollback) = secure_store::delete_secret(provider, name) {
                    rollback_errors.push(format!("credential={rollback}"));
                }
            }
        }
        anyhow::bail!(
            "failed to store credential for {provider}/{name}: {error}; rollback: {}",
            if rollback_errors.is_empty() {
                "completed".to_string()
            } else {
                rollback_errors.join(", ")
            }
        );
    }
    Ok(())
}

/// asx `setActive(provider, name)`.
pub(crate) fn set_active(provider: &str, name: &str) -> anyhow::Result<()> {
    AccountStore::open_default().set_active(provider, name)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier, Mutex};

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn concurrent_colliding_names_cannot_overwrite_the_winner() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir =
            std::env::temp_dir().join(format!("aas-provider-collision-{}", uuid::Uuid::new_v4()));
        std::env::set_var("AAS_CONFIG_DIR", &dir);
        let barrier = Arc::new(Barrier::new(3));
        let attempts = [("a/b", "slash-secret"), ("a?b", "question-secret")];
        let threads: Vec<_> = attempts
            .into_iter()
            .map(|(name, secret)| {
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    (
                        name,
                        secret,
                        store_account_secret("codex", name, None, None, secret),
                    )
                })
            })
            .collect();
        barrier.wait();
        let results: Vec<_> = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect();

        assert_eq!(
            results
                .iter()
                .filter(|(_, _, result)| result.is_ok())
                .count(),
            1
        );
        let accounts = AccountStore::open_default().list(None).unwrap();
        assert_eq!(accounts.len(), 1);
        let winner = &accounts[0].name;
        let expected = results
            .iter()
            .find(|(name, _, result)| result.is_ok() && name == winner)
            .map(|(_, secret, _)| *secret)
            .unwrap();
        assert_eq!(
            secure_store::get_secret("codex", winner).as_deref(),
            Some(expected)
        );

        std::env::remove_var("AAS_CONFIG_DIR");
        let _ = std::fs::remove_dir_all(dir);
    }
}
