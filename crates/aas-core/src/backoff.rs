//! Persisted per-account rate-limit backoff.
//!
//! The `usage` endpoints are rate-limited; when one returns `429 Retry-After`, hammering it
//! again before the window passes keeps (and often extends) the ban. Since the CLI is
//! stateless per-invocation and aas-bar shells out to it, we record the window on disk so
//! *every* caller honors it — returning a cached "rate limited" state instead of re-hitting
//! the API until the window expires.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::platform::asx_config_dir;

fn path() -> PathBuf {
    asx_config_dir().join("usage-backoff.json")
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn load() -> HashMap<String, i64> {
    std::fs::read_to_string(path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn store(map: &HashMap<String, i64>) {
    let dir = asx_config_dir();
    let _ = std::fs::create_dir_all(&dir);
    let Ok(s) = serde_json::to_string(map) else { return };
    // Atomic replace: write a uniquely-named temp file, then rename it over the target. This
    // keeps concurrent readers (parallel fetches, aas-bar subprocesses) from ever seeing a
    // half-written or truncated file. A lost update between two simultaneous 429s is benign —
    // the dropped account just re-records its window on the next fetch.
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let uniq = format!("{}.{}", std::process::id(), SEQ.fetch_add(1, Ordering::Relaxed));
    let tmp = dir.join(format!("usage-backoff.{uniq}.tmp"));
    if std::fs::write(&tmp, s).is_ok() && std::fs::rename(&tmp, path()).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// Unix-ms until which `key` (e.g. `"claude/work"`) is rate-limited, if still in the future.
pub fn rate_limited_until(key: &str) -> Option<i64> {
    load().get(key).copied().filter(|&until| until > now_ms())
}

/// Record that `key` is rate-limited until `until_ms` (also prunes expired entries).
pub fn set_rate_limited(key: &str, until_ms: i64) {
    let now = now_ms();
    let mut map = load();
    map.retain(|_, &mut until| until > now);
    map.insert(key.to_string(), until_ms);
    store(&map);
}

/// Clear a key's backoff after a successful fetch.
pub fn clear(key: &str) {
    let mut map = load();
    if map.remove(key).is_some() {
        store(&map);
    }
}
