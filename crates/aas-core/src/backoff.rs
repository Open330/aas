//! Persisted per-account rate-limit backoff with **client-side escalation**.
//!
//! The `usage` endpoints are rate-limited; when one returns `429 Retry-After`, hammering it
//! again the instant the window passes keeps — and often *extends* — the ban. Since the CLI is
//! stateless per-invocation and aas-bar shells out to it, we record the window on disk so
//! *every* caller honors it, returning a cached "rate limited" state instead of re-hitting the
//! API until the window expires.
//!
//! **Why escalation.** Merely mirroring the server's `Retry-After` and retrying once per window
//! never converges when the server escalates: each retry-at-expiry earns a *longer* ban, so a
//! user who "waits then checks" loops forever. Instead we keep a per-key backoff *duration* and
//! **double it on every consecutive 429** (honoring a larger server hint), capped at `CAP_MS`.
//! Repeated failures therefore widen the gap geometrically and settle at the cap, and a stray
//! `aas usage` can't keep re-arming a short window. The escalation memory **decays**: once a key
//! has gone `CAP_MS` without a fresh 429 (a genuine hands-off cooldown), it resets to the
//! server hint, and a *successful* fetch clears it outright.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io;
use std::path::PathBuf;

use crate::platform::asx_config_dir;

/// Floor for a backoff window when the server sends no (parsable) `Retry-After`.
const BASE_MS: i64 = 60_000; // 60s
/// Ceiling for the escalating window, and the no-429 span after which escalation memory decays.
const CAP_MS: i64 = 2 * 60 * 60 * 1000; // 2h

/// One key's backoff state. `until_ms` is the honored retry gate; `backoff_ms` is the duration
/// that produced it (doubled on the next consecutive 429); `last_ms` is when it was recorded
/// (used to decay stale escalation).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct Entry {
    until_ms: i64,
    backoff_ms: i64,
    last_ms: i64,
}

fn path() -> PathBuf {
    asx_config_dir().join("usage-backoff.json")
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn load_unlocked() -> HashMap<String, Entry> {
    // A parse failure (missing file, or the pre-escalation `{"key": <i64>}` format) is benign —
    // we start empty and re-record on the next 429.
    std::fs::read_to_string(path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn store_unlocked(map: &HashMap<String, Entry>) -> io::Result<()> {
    let body = serde_json::to_string(map).map_err(io::Error::other)?;
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

/// Pure escalation math: given the prior entry (if any), the server's `Retry-After` hint (ms;
/// `<= 0` when absent), and the current time, choose the next backoff window.
///
/// - **fresh / decayed** (no live prior): `max(hint, BASE_MS)`.
/// - **consecutive 429** (prior still within `CAP_MS`): `max(prior.backoff_ms * 2, hint)` — double
///   our own window but never undercut a server asking for longer.
/// - always clamped to `CAP_MS`.
fn next_backoff(prev: Option<Entry>, hint_ms: i64, now: i64) -> Entry {
    let hint = hint_ms.max(0);
    let escalated = match prev {
        Some(p) if now.saturating_sub(p.last_ms) <= CAP_MS => {
            p.backoff_ms.saturating_mul(2).max(hint)
        }
        _ => hint.max(BASE_MS),
    };
    let backoff = escalated.clamp(0, CAP_MS);
    Entry {
        until_ms: now + backoff,
        backoff_ms: backoff,
        last_ms: now,
    }
}

fn record_at(key: &str, hint_ms: i64, now: i64) -> i64 {
    with_lock(true, || {
        let mut map = load_unlocked();
        // Decay: forget escalation for any key untouched for CAP_MS (also keeps the file small).
        map.retain(|_, e| now.saturating_sub(e.last_ms) <= CAP_MS);
        let entry = next_backoff(map.get(key).copied(), hint_ms, now);
        map.insert(key.to_string(), entry);
        store_unlocked(&map)?;
        Ok(entry.until_ms)
    })
    .unwrap_or_else(|_| next_backoff(None, hint_ms, now).until_ms)
}

/// Unix-ms until which `key` (e.g. `"claude/work"`) is rate-limited, if still in the future.
pub fn rate_limited_until(key: &str) -> Option<i64> {
    with_lock(false, || {
        Ok(load_unlocked()
            .get(key)
            .map(|e| e.until_ms)
            .filter(|&until| until > now_ms()))
    })
    .unwrap_or(None)
}

/// Record a 429 for `key`, escalating the backoff on consecutive hits. `server_hint_ms` is the
/// parsed `Retry-After` in ms (pass `0` when absent). Returns the chosen `until_ms`.
pub fn record_rate_limit(key: &str, server_hint_ms: i64) -> i64 {
    record_at(key, server_hint_ms, now_ms())
}

/// Clear a key's backoff after a successful fetch — resets escalation to zero.
pub fn clear(key: &str) {
    let _ = with_lock(true, || {
        let mut map = load_unlocked();
        if map.remove(key).is_some() {
            store_unlocked(&map)?;
        }
        Ok(())
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_hit_uses_server_hint() {
        let e = next_backoff(None, 400_000, 0);
        assert_eq!(e.backoff_ms, 400_000);
        assert_eq!(e.until_ms, 400_000);
    }

    #[test]
    fn fresh_hit_floors_at_base() {
        // Server hint smaller than BASE (or absent) → at least BASE_MS.
        assert_eq!(next_backoff(None, 5_000, 0).backoff_ms, BASE_MS);
        assert_eq!(next_backoff(None, 0, 0).backoff_ms, BASE_MS);
    }

    #[test]
    fn consecutive_hits_double() {
        // Simulate the "wait for the window, then get 429 again" loop: each retry lands at the
        // prior until_ms and must escalate ×2, not restart at the server hint.
        let e1 = next_backoff(None, 400_000, 0); // 400s
        let e2 = next_backoff(Some(e1), 400_000, e1.until_ms); // at expiry → 800s
        let e3 = next_backoff(Some(e2), 400_000, e2.until_ms); // → 1600s
        assert_eq!(e1.backoff_ms, 400_000);
        assert_eq!(e2.backoff_ms, 800_000);
        assert_eq!(e3.backoff_ms, 1_600_000);
    }

    #[test]
    fn honors_larger_server_hint_over_doubling() {
        let e1 = next_backoff(None, 400_000, 0);
        // Server suddenly asks for 2h-ish, more than our doubled 800s → take the server's.
        let e2 = next_backoff(Some(e1), 5_000_000, e1.until_ms);
        assert_eq!(e2.backoff_ms, 5_000_000.min(CAP_MS));
    }

    #[test]
    fn clamps_at_cap() {
        let mut e = next_backoff(None, CAP_MS, 0);
        for _ in 0..5 {
            e = next_backoff(Some(e), 0, e.until_ms);
            assert!(e.backoff_ms <= CAP_MS);
        }
        assert_eq!(e.backoff_ms, CAP_MS);
    }

    #[test]
    fn decays_after_long_silence() {
        let e1 = next_backoff(None, 1_000_000, 0); // 1000s
                                                   // A genuine hands-off cooldown longer than CAP_MS resets to the server hint, not ×2.
        let long_gap = e1.last_ms + CAP_MS + 1;
        let e2 = next_backoff(Some(e1), 300_000, long_gap);
        assert_eq!(e2.backoff_ms, 300_000); // fresh, escalation forgotten
    }
}
