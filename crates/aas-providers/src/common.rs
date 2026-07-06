//! Shared helpers for the provider adapters: HTTP client, time, JSON access, and the
//! account-store / secret-store glue that every adapter calls after a successful load.

use aas_core::model::AccountRecord;
use aas_core::store::AccountStore;
use serde_json::Value;
use std::path::Path;

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

/// asx `addAccount({provider,name,label:label||name,email})`.
pub(crate) fn add_account(
    provider: &str,
    name: &str,
    label: Option<&str>,
    email: Option<String>,
) -> anyhow::Result<()> {
    let mut rec = AccountRecord::new(provider, name);
    rec.label = Some(label.unwrap_or(name).to_string());
    rec.email = email;
    AccountStore::open_default().add(rec)?;
    Ok(())
}

/// asx `setActive(provider, name)`.
pub(crate) fn set_active(provider: &str, name: &str) -> anyhow::Result<()> {
    AccountStore::open_default().set_active(provider, name)?;
    Ok(())
}

#[cfg(unix)]
pub(crate) fn set_0600(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}
#[cfg(not(unix))]
pub(crate) fn set_0600(_path: &Path) {}
