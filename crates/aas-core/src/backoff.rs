//! Persisted per-account rate-limit backoff.
//!
//! The `usage` endpoints are rate-limited; when one returns `429 Retry-After`, hammering it
//! again the instant the window passes keeps — and often *extends* — the ban. Since the CLI is
//! stateless per-invocation and aas-bar shells out to it, we record the window on disk so
//! *every* caller honors it, returning a cached "rate limited" state instead of re-hitting the
//! API until the window expires.
//!
//! The gate mirrors the provider's `Retry-After` rather than inventing a longer client-side
//! window. Cross-process fetch locks and the successful-usage cache prevent local callers from
//! stampeding the endpoint. Extending the provider's window locally is actively harmful across
//! machines: one Mac can keep displaying an hour-long synthetic limit after another Mac has
//! already proved that the provider recovered.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io;
use std::path::PathBuf;

use crate::platform::asx_config_dir;

/// Floor for a backoff window when the server sends no (parsable) `Retry-After`.
const BASE_MS: i64 = 60_000; // 60s

/// One key's authoritative retry gate. The versioned outer file keeps this schema separate from
/// the synthetic escalation entries written by aas < 0.1.7.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct Entry {
    until_ms: i64,
}

#[derive(Default, Serialize, Deserialize)]
struct BackoffFile {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    entries: HashMap<String, Entry>,
}

fn version() -> u32 {
    2
}

fn path() -> PathBuf {
    asx_config_dir().join("usage-backoff.json")
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn load_unlocked() -> BackoffFile {
    // Pre-v2 files were a bare map and contained synthetic escalation state. Intentionally reject
    // that schema so upgrading immediately drops gates which may outlive the provider's limit.
    std::fs::read_to_string(path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .filter(|file: &BackoffFile| file.version == version())
        .unwrap_or_else(|| BackoffFile {
            version: version(),
            ..Default::default()
        })
}

fn store_unlocked(file: &BackoffFile) -> io::Result<()> {
    let body = serde_json::to_string(file).map_err(io::Error::other)?;
    crate::secure_store::write_restricted_file(&path(), &body)
}

fn open_lock() -> io::Result<File> {
    let dir = asx_config_dir();
    std::fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(dir.join(".usage-backoff.lock"))
}

fn with_lock<T>(exclusive: bool, f: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
    use fs2::FileExt;
    let lock = open_lock()?;
    if exclusive {
        FileExt::lock_exclusive(&lock)?;
    } else {
        FileExt::lock_shared(&lock)?;
    }
    let result = f();
    let unlock = FileExt::unlock(&lock);
    match (result, unlock) {
        (Err(e), _) | (Ok(_), Err(e)) => Err(e),
        (Ok(value), Ok(())) => Ok(value),
    }
}

/// Mirror the provider's `Retry-After`, with a small safety floor when it is absent or invalid.
fn next_backoff(hint_ms: i64, now: i64) -> Entry {
    let backoff = hint_ms.max(BASE_MS);
    Entry {
        until_ms: now.saturating_add(backoff),
    }
}

fn record_at(key: &str, hint_ms: i64, now: i64) -> i64 {
    with_lock(true, || {
        let mut file = load_unlocked();
        // Expired gates are no longer useful and only make the state file harder to inspect.
        file.entries.retain(|_, entry| entry.until_ms > now);
        let entry = next_backoff(hint_ms, now);
        file.entries.insert(key.to_string(), entry);
        store_unlocked(&file)?;
        Ok(entry.until_ms)
    })
    .unwrap_or_else(|_| next_backoff(hint_ms, now).until_ms)
}

/// Unix-ms until which `key` (e.g. `"claude/work"`) is rate-limited, if still in the future.
pub fn rate_limited_until(key: &str) -> Option<i64> {
    with_lock(false, || {
        Ok(load_unlocked()
            .entries
            .get(key)
            .map(|e| e.until_ms)
            .filter(|&until| until > now_ms()))
    })
    .unwrap_or(None)
}

/// Record a 429 for `key`. `server_hint_ms` is the parsed `Retry-After` in ms (pass `0` when
/// absent). Returns the chosen `until_ms`.
pub fn record_rate_limit(key: &str, server_hint_ms: i64) -> i64 {
    record_at(key, server_hint_ms, now_ms())
}

/// Clear a key's backoff after a successful fetch.
pub fn clear(key: &str) {
    let _ = with_lock(true, || {
        let mut file = load_unlocked();
        if file.entries.remove(key).is_some() {
            store_unlocked(&file)?;
        }
        Ok(())
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_hit_uses_server_hint() {
        let e = next_backoff(400_000, 0);
        assert_eq!(e.until_ms, 400_000);
    }

    #[test]
    fn fresh_hit_floors_at_base() {
        assert_eq!(next_backoff(5_000, 0).until_ms, BASE_MS);
        assert_eq!(next_backoff(0, 0).until_ms, BASE_MS);
    }

    #[test]
    fn consecutive_hits_do_not_invent_a_longer_window() {
        let e1 = next_backoff(400_000, 0);
        let e2 = next_backoff(400_000, e1.until_ms);
        assert_eq!(e2.until_ms - e1.until_ms, 400_000);
    }

    #[test]
    fn honors_long_server_hint_without_a_client_cap() {
        let e = next_backoff(5_000_000, 10);
        assert_eq!(e.until_ms, 5_000_010);
    }

    #[test]
    fn saturates_timestamp_addition() {
        assert_eq!(next_backoff(i64::MAX, 10).until_ms, i64::MAX);
    }

    #[test]
    fn legacy_escalation_file_is_not_the_v2_schema() {
        let legacy = r#"{"claude/work":{"until_ms":999,"backoff_ms":7200000,"last_ms":1}}"#;
        let parsed = serde_json::from_str::<BackoffFile>(legacy).unwrap();
        assert_ne!(parsed.version, version());
        assert!(parsed.entries.is_empty());
    }
}
