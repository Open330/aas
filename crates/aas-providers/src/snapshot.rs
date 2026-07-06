//! Shared live-usage fetch: the parallel fan-out behind `aas usage` and `aas-bar`.
//!
//! Lifts what used to be inlined in `aas-cli`'s `cmd_list` so both the CLI table and the
//! menubar app hit one code path — refresh expired creds, then fetch every account's
//! [`Usage`] concurrently, preserving input order (provider order, then store order).

use aas_core::store::AccountStore;
use aas_core::usage::Usage;

use crate::{all_providers, get_adapter, Provider};

/// One account's live usage plus the bits a UI needs to group and mark it.
#[derive(Debug)]
pub struct AccountUsage {
    pub provider: String,
    pub name: String,
    pub email: Option<String>,
    /// The aas-tracked active account for this provider.
    pub active: bool,
    pub usage: Usage,
}

/// Refresh a stored credential if it's expired (best-effort), mirroring `aas usage`.
async fn ensure_fresh(p: Provider, name: &str) {
    if p.is_expired(name).await {
        let _ = p.refresh(name).await;
    }
}

/// Resolve `filter` (a provider id, an account name, or `None` for everything) to the
/// providers to scan and an optional single-account restriction.
fn resolve_scope(store: &AccountStore, filter: Option<&str>) -> anyhow::Result<(Vec<Provider>, Option<String>)> {
    match filter {
        Some(p) => {
            if let Some(a) = get_adapter(p) {
                Ok((vec![a], None))
            } else if let Some(acct) = store.get_by_name(p)? {
                match get_adapter(&acct.provider) {
                    Some(a) => Ok((vec![a], Some(p.to_string()))),
                    None => anyhow::bail!("Unknown provider or name: {p}"),
                }
            } else {
                anyhow::bail!("Unknown provider or name: {p}")
            }
        }
        None => Ok((all_providers().to_vec(), None)),
    }
}

/// Live usage for every stored account (optionally scoped by `filter`), fetched in parallel.
///
/// Tasks are spawned up front and awaited in order, so the network calls overlap while the
/// returned `Vec` stays in a stable, deterministic order.
pub async fn snapshot(store: &AccountStore, filter: Option<&str>) -> anyhow::Result<Vec<AccountUsage>> {
    let (provs, single_name) = resolve_scope(store, filter)?;

    let mut pending: Vec<(String, String, Option<String>, bool, tokio::task::JoinHandle<Usage>)> = Vec::new();
    for p in provs {
        let active = store.get_active(p.id());
        let accts: Vec<_> = store
            .list(Some(p.id()))
            .into_iter()
            .filter(|a| single_name.as_ref().is_none_or(|n| &a.name == n))
            .collect();
        for a in accts {
            let is_active = active.as_deref() == Some(a.name.as_str());
            let task_name = a.name.clone();
            let handle = tokio::spawn(async move {
                ensure_fresh(p, &task_name).await;
                p.usage(&task_name).await
            });
            pending.push((p.id().to_string(), a.name, a.email, is_active, handle));
        }
    }

    let mut out = Vec::with_capacity(pending.len());
    for (provider, name, email, active, handle) in pending {
        let usage = handle.await.unwrap_or_else(|_| Usage::error("", "usage fetch task failed"));
        out.push(AccountUsage { provider, name, email, active, usage });
    }
    Ok(out)
}
