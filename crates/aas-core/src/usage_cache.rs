//! Shared successful-usage cache for CLI and UI callers.
//!
//! The cache is deliberately provider-agnostic. Network callers keep errors out of it, allowing a
//! stale last-known-good value to remain available during rate limits and transient failures.

use crate::usage::Usage;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io;

/// Default cache lifetime used by `aas usage` and integrations.
pub const DEFAULT_MAX_AGE_MS: i64 = 10 * 60 * 1000;
const RETAIN_MS: i64 = 7 * 24 * 60 * 60 * 1000;

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

#[derive(Default, Serialize, Deserialize)]
struct CacheFile {
    #[serde(default = "version")]
    version: u32,
    #[serde(default)]
    entries: HashMap<String, Entry>,
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
        let body = serde_json::to_string(&cache).map_err(io::Error::other)?;
        crate::secure_store::write_restricted_file(&path(), &body)
    })
}

pub fn clear(key: &str) {
    let _ = with_lock(true, || {
        let mut cache = load_unlocked();
        if cache.entries.remove(key).is_some() {
            let body = serde_json::to_string(&cache).map_err(io::Error::other)?;
            crate::secure_store::write_restricted_file(&path(), &body)?;
        }
        Ok(())
    });
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn cache_schema_round_trips_usage() {
        let entry = Entry {
            fetched_at_ms: 123,
            usage: Usage::error("claude", "example"),
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert_eq!(serde_json::from_str::<Entry>(&json).unwrap(), entry);
    }
}
