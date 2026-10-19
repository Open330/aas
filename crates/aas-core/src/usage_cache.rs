//! Shared usage cache for CLI and UI callers.
//!
//! Success observations and short-lived failures are stored separately, allowing last-known-good
//! quota to survive transient failures without repeatedly probing broken credentials.

use crate::usage::Usage;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io;

/// Default cache lifetime used by `aas usage` and integrations.
pub const DEFAULT_MAX_AGE_MS: i64 = 10 * 60 * 1000;
pub const FAILURE_MAX_AGE_MS: i64 = 60 * 1000;
const RETAIN_MS: i64 = 7 * 24 * 60 * 60 * 1000;

pub fn credential_fingerprint(raw: Option<&str>) -> String {
    let hash = Sha256::digest(raw.unwrap_or_default().as_bytes());
    hash.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub fetched_at_ms: i64,
    pub usage: Usage,
}

impl Entry {
    pub fn is_fresh(&self, now_ms: i64, max_age_ms: i64) -> bool {
        let age = now_ms.saturating_sub(self.fetched_at_ms);
        age >= 0 && age <= max_age_ms
    }
}

/// Retry suppression kept separately from the last successful quota observation. A credential
/// digest ensures a native token rotation immediately bypasses an earlier failure.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FailureEntry {
    pub observed_at_ms: i64,
    pub credential_fingerprint: String,
    pub usage: Usage,
}

impl FailureEntry {
    pub fn is_fresh(&self, now_ms: i64, fingerprint: &str) -> bool {
        let age = now_ms.saturating_sub(self.observed_at_ms);
        self.credential_fingerprint == fingerprint && (0..FAILURE_MAX_AGE_MS).contains(&age)
    }
}

#[derive(Default, Serialize, Deserialize)]
struct CacheFile {
    #[serde(default = "version")]
    version: u32,
    #[serde(default)]
    entries: HashMap<String, Entry>,
    #[serde(default)]
    failures: HashMap<String, FailureEntry>,
}

fn version() -> u32 {
    1
}

fn path() -> std::path::PathBuf {
    crate::platform::asx_config_dir().join("usage-cache.json")
}

fn open_lock() -> io::Result<File> {
    let dir = crate::platform::asx_config_dir();
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
    options.open(dir.join(".usage-cache.lock"))
}

fn with_lock<T>(exclusive: bool, f: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
    let lock = open_lock()?;
    if exclusive {
        lock.lock_exclusive()?;
    } else {
        lock.lock_shared()?;
    }
    let result = f();
    let unlock = FileExt::unlock(&lock);
    match (result, unlock) {
        (Err(error), _) | (Ok(_), Err(error)) => Err(error),
        (Ok(value), Ok(())) => Ok(value),
    }
}

fn load_unlocked() -> CacheFile {
    std::fs::read_to_string(path())
        .ok()
        .and_then(|body| serde_json::from_str(&body).ok())
        .filter(|cache: &CacheFile| cache.version == version())
        .unwrap_or_else(|| CacheFile {
            version: version(),
            ..Default::default()
        })
}

pub fn get(key: &str) -> Option<Entry> {
    with_lock(false, || Ok(load_unlocked().entries.get(key).cloned()))
        .ok()
        .flatten()
}

pub fn put(key: &str, entry: Entry) -> io::Result<()> {
    with_lock(true, || {
        let mut cache = load_unlocked();
        cache.entries.retain(|_, existing| {
            entry.fetched_at_ms.saturating_sub(existing.fetched_at_ms) <= RETAIN_MS
        });
        cache.entries.insert(key.to_string(), entry);
        cache.failures.remove(key);
        let body = serde_json::to_string(&cache).map_err(io::Error::other)?;
        crate::secure_store::write_restricted_file(&path(), &body)
    })
}

pub fn get_failure(key: &str) -> Option<FailureEntry> {
    with_lock(false, || Ok(load_unlocked().failures.get(key).cloned()))
        .ok()
        .flatten()
}

pub fn put_failure(key: &str, entry: FailureEntry) -> io::Result<()> {
    with_lock(true, || {
        let mut cache = load_unlocked();
        cache.failures.retain(|_, old| {
            (0..FAILURE_MAX_AGE_MS)
                .contains(&entry.observed_at_ms.saturating_sub(old.observed_at_ms))
        });
        cache.failures.insert(key.into(), entry);
        let body = serde_json::to_string(&cache).map_err(io::Error::other)?;
        crate::secure_store::write_restricted_file(&path(), &body)
    })
}

pub fn clear(key: &str) {
    let _ = with_lock(true, || {
        let mut cache = load_unlocked();
        let removed_success = cache.entries.remove(key).is_some();
        let removed_failure = cache.failures.remove(key).is_some();
        if removed_success || removed_failure {
            let body = serde_json::to_string(&cache).map_err(io::Error::other)?;
            crate::secure_store::write_restricted_file(&path(), &body)?;
        }
        Ok(())
    });
}

/// Drop every cached usage state for `key`: the snapshot **and** any recorded rate-limit backoff.
///
/// Use this whenever the credential or the account behind `key` changes. Clearing only the
/// snapshot leaves a recorded 429 in force, so a credential that has since been replaced (or an
/// account that no longer exists) keeps being reported as rate limited until the backoff happens
/// to expire — the backoff outlives the failure that produced it.
pub fn invalidate(key: &str) {
    clear(key);
    crate::backoff::clear(key);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failures_expire_and_are_bound_to_the_exact_credential() {
        let first = credential_fingerprint(Some("first"));
        let next = credential_fingerprint(Some("next"));
        let entry = FailureEntry {
            observed_at_ms: 1000,
            credential_fingerprint: first.clone(),
            usage: Usage::error("codex", "HTTP 401"),
        };
        assert!(entry.is_fresh(1000, &first));
        assert!(entry.is_fresh(60_999, &first));
        assert!(!entry.is_fresh(61_000, &first));
        assert!(!entry.is_fresh(999, &first));
        assert!(!entry.is_fresh(1000, &next));
    }

    #[test]
    fn freshness_rejects_future_and_old_entries() {
        let entry = Entry {
            fetched_at_ms: 1_000,
            usage: Usage::default(),
        };
        assert!(entry.is_fresh(1_500, 500));
        assert!(!entry.is_fresh(1_501, 500));
        assert!(!entry.is_fresh(999, 500));
    }

    #[test]
    fn invalidate_drops_the_snapshot_and_the_rate_limit_backoff() {
        let _guard = crate::ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let dir = std::env::temp_dir().join(format!("aas-usage-cache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("AAS_CONFIG_DIR", &dir);

        let key = "claude/work";
        put(
            key,
            Entry {
                fetched_at_ms: 1,
                usage: Usage::default(),
            },
        )
        .unwrap();
        crate::backoff::record_rate_limit(key, 600_000);
        assert!(get(key).is_some());
        assert!(crate::backoff::rate_limited_until(key).is_some());

        invalidate(key);

        assert!(get(key).is_none(), "the usage snapshot should be gone");
        assert!(
            crate::backoff::rate_limited_until(key).is_none(),
            "a recorded 429 must not outlive the credential it was recorded for"
        );

        std::env::remove_var("AAS_CONFIG_DIR");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cache_schema_round_trips_usage() {
        let entry = Entry {
            fetched_at_ms: 123,
            usage: Usage::error("claude", "example"),
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert_eq!(serde_json::from_str::<Entry>(&json).unwrap(), entry);
    }
}
