//! Shared usage fetch for CLI and UI callers.
//!
//! Successful results are cached across processes, and a per-account fetch lock coalesces a
//! terminal, BarShelf, and editor asking for the same data at once. A live rate-limit gate is
//! checked before credential refresh, guaranteeing that backoff means zero provider requests.

use aas_core::model::{sort_accounts, AccountSort};
use aas_core::store::AccountStore;
use aas_core::usage::Usage;
use aas_core::usage_cache::{self, Entry as CacheEntry, DEFAULT_MAX_AGE_MS};
use std::fs::File;

use crate::{all_providers, get_adapter, Provider};

type PendingUsage = (
    String,
    String,
    Option<String>,
    bool,
    tokio::task::JoinHandle<FetchedUsage>,
);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UsageCacheMode {
    /// Reuse a successful result for ten minutes.
    #[default]
    PreferCache,
    /// Request live data, while still honoring rate-limit gates and coalescing simultaneous calls.
    Refresh,
}

/// One account's usage plus the bits a UI needs to group and mark it.
#[derive(Debug)]
pub struct AccountUsage {
    pub provider: String,
    pub name: String,
    pub email: Option<String>,
    /// The aas-tracked active account for this provider.
    pub active: bool,
    pub usage: Usage,
    /// True when `usage` came from the shared last-known-good cache.
    pub cached: bool,
    /// Epoch milliseconds of the successful provider response, when available.
    pub fetched_at_ms: Option<i64>,
}

struct FetchedUsage {
    usage: Usage,
    cached: bool,
    fetched_at_ms: Option<i64>,
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn cache_key(provider: Provider, account: &str) -> String {
    format!("{}/{account}", provider.id())
}

fn backoff_message(until_ms: i64) -> String {
    let seconds = ((until_ms - now_ms()) / 1000).max(0);
    format!("rate limited (HTTP 429) — backing off {seconds}s to recover.")
}

fn from_cache(mut entry: CacheEntry, error: Option<String>) -> FetchedUsage {
    if let Some(error) = error {
        entry.usage.error = Some(error);
        entry
            .usage
            .notes
            .push("showing the last successful usage snapshot".into());
    }
    FetchedUsage {
        usage: entry.usage,
        cached: true,
        fetched_at_ms: Some(entry.fetched_at_ms),
    }
}

fn failure_with_cache(key: &str, headline: &str, error: String) -> FetchedUsage {
    usage_cache::get(key)
        .map(|entry| from_cache(entry, Some(error.clone())))
        .unwrap_or_else(|| FetchedUsage {
            usage: Usage::error(headline, error),
            cached: false,
            fetched_at_ms: None,
        })
}

fn permits_stale_fallback(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    ![
        "http 401",
        "http 403",
        "no stored credential",
        "re-login required",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

async fn fetch_lock(key: String) -> Result<File, String> {
    tokio::task::spawn_blocking(move || aas_core::keyed_lock::acquire("usage", &key))
        .await
        .map_err(|error| format!("usage lock task failed: {error}"))?
        .map_err(|error| format!("could not acquire usage lock: {error}"))
}

async fn fetch_account(provider: Provider, account: &str, mode: UsageCacheMode) -> FetchedUsage {
    let key = cache_key(provider, account);
    let requested_at_ms = now_ms();

    if mode == UsageCacheMode::PreferCache {
        if let Some(entry) = usage_cache::get(&key)
            .filter(|entry| entry.is_fresh(requested_at_ms, DEFAULT_MAX_AGE_MS))
        {
            return from_cache(entry, None);
        }
    }

    // Backoff is authoritative and precedes both the token endpoint and usage endpoint.
    if let Some(until) = aas_core::backoff::rate_limited_until(&key) {
        return failure_with_cache(&key, provider.id(), backoff_message(until));
    }

    let _fetch_lock = match fetch_lock(key.clone()).await {
        Ok(lock) => lock,
        Err(error) => return failure_with_cache(&key, provider.id(), error),
    };

    // Another process may have completed while this caller waited. Even `--fresh` callers reuse
    // a result newer than their own start time, coalescing simultaneous manual refreshes.
    if let Some(entry) = usage_cache::get(&key) {
        if entry.fetched_at_ms >= requested_at_ms
            || (mode == UsageCacheMode::PreferCache && entry.is_fresh(now_ms(), DEFAULT_MAX_AGE_MS))
        {
            return from_cache(entry, None);
        }
    }

    if let Some(until) = aas_core::backoff::rate_limited_until(&key) {
        return failure_with_cache(&key, provider.id(), backoff_message(until));
    }

    let refresh_failure = provider
        .refresh_if_expired(account)
        .await
        .filter(|outcome| !outcome.ok)
        .map(|outcome| {
            format!(
                "credential refresh failed: {}{}",
                outcome.message,
                if outcome.needs_relogin {
                    "; re-login required"
                } else {
                    ""
                }
            )
        });

    let mut usage = provider.usage(account).await;
    let fetched_at_ms = now_ms();
    if usage.error.is_none() {
        let _ = usage_cache::put(
            &key,
            CacheEntry {
                fetched_at_ms,
                usage: usage.clone(),
            },
        );
    }

    if let Some(refresh_failure) = refresh_failure {
        if let Some(provider_error) = usage.error.take() {
            usage.error = Some(format!("{refresh_failure}; {provider_error}"));
        } else {
            usage.notes.push(refresh_failure);
        }
    }

    if let Some(error) = usage.error.clone() {
        return usage_cache::get(&key)
            .filter(|entry| entry.fetched_at_ms < requested_at_ms && permits_stale_fallback(&error))
            .map(|entry| from_cache(entry, Some(error)))
            .unwrap_or(FetchedUsage {
                usage,
                cached: false,
                fetched_at_ms: None,
            });
    }

    FetchedUsage {
        usage,
        cached: false,
        fetched_at_ms: Some(fetched_at_ms),
    }
}

/// Resolve `filter` (a provider id, an account name, or `None` for everything) to the
/// providers to scan and an optional single-account restriction.
pub fn resolve_scope(
    store: &AccountStore,
    filter: Option<&str>,
) -> anyhow::Result<(Vec<Provider>, Option<String>)> {
    match filter {
        Some(value) => {
            if let Some(adapter) = get_adapter(value) {
                Ok((vec![adapter], None))
            } else if let Some(account) = store.get_by_name(value)? {
                match get_adapter(&account.provider) {
                    Some(adapter) => Ok((vec![adapter], Some(value.to_string()))),
                    None => anyhow::bail!("Unknown provider or name: {value}"),
                }
            } else {
                anyhow::bail!("Unknown provider or name: {value}")
            }
        }
        None => Ok((all_providers().to_vec(), None)),
    }
}

pub async fn snapshot(
    store: &AccountStore,
    filter: Option<&str>,
) -> anyhow::Result<Vec<AccountUsage>> {
    snapshot_sorted(store, filter, AccountSort::Name).await
}

pub async fn snapshot_sorted(
    store: &AccountStore,
    filter: Option<&str>,
    sort: AccountSort,
) -> anyhow::Result<Vec<AccountUsage>> {
    snapshot_sorted_with_cache(store, filter, sort, UsageCacheMode::PreferCache).await
}

pub async fn snapshot_sorted_with_cache(
    store: &AccountStore,
    filter: Option<&str>,
    sort: AccountSort,
    cache_mode: UsageCacheMode,
) -> anyhow::Result<Vec<AccountUsage>> {
    let (providers, single_name) = resolve_scope(store, filter)?;
    let mut pending: Vec<PendingUsage> = Vec::new();

    for provider in providers {
        let active = store.get_active(provider.id())?;
        let mut accounts: Vec<_> = store
            .list(Some(provider.id()))?
            .into_iter()
            .filter(|account| {
                single_name
                    .as_ref()
                    .is_none_or(|name| &account.name == name)
            })
            .collect();
        sort_accounts(&mut accounts, sort);
        for account in accounts {
            let is_active = active.as_deref() == Some(account.name.as_str());
            let task_name = account.name.clone();
            let handle =
                tokio::spawn(async move { fetch_account(provider, &task_name, cache_mode).await });
            pending.push((
                provider.id().to_string(),
                account.name,
                account.email,
                is_active,
                handle,
            ));
        }
    }

    let mut output = Vec::with_capacity(pending.len());
    for (provider, name, email, active, handle) in pending {
        let fetched = handle.await.unwrap_or_else(|_| FetchedUsage {
            usage: Usage::error("", "usage fetch task failed"),
            cached: false,
            fetched_at_ms: None,
        });
        output.push(AccountUsage {
            provider,
            name,
            email,
            active,
            usage: fetched.usage,
            cached: fetched.cached,
            fetched_at_ms: fetched.fetched_at_ms,
        });
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_failure_keeps_meters_and_surfaces_error() {
        let entry = CacheEntry {
            fetched_at_ms: 42,
            usage: Usage {
                headline: "plan=max".into(),
                meters: vec![aas_core::usage::Meter::new("5h", 12.0, None)],
                ..Default::default()
            },
        };
        let fetched = from_cache(entry, Some("rate limited".into()));
        assert!(fetched.cached);
        assert_eq!(fetched.usage.meters.len(), 1);
        assert_eq!(fetched.usage.error.as_deref(), Some("rate limited"));
        assert!(fetched.usage.notes[0].contains("last successful"));
    }

    #[test]
    fn auth_failures_never_fall_back_to_stale_usage() {
        assert!(!permits_stale_fallback("token invalid (HTTP 401)"));
        assert!(!permits_stale_fallback(
            "credential refresh failed; re-login required"
        ));
        assert!(permits_stale_fallback("network error"));
        assert!(permits_stale_fallback("rate limited (HTTP 429)"));
    }
}
