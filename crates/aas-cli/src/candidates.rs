//! Credential-free candidate discovery for agents. Usage is evidence, not an availability probe.
use aas_core::{model::AccountSort, store::AccountStore, usage::Usage};
use aas_providers::UsageCacheMode;

fn assessment(usage: &Usage, excluded: bool, cooldown: Option<i64>) -> (bool, &'static str) {
    if excluded {
        return (false, "excluded");
    }
    if cooldown.is_some() {
        return (false, "usage_rate_limited");
    }
    if usage.error.is_some() {
        return (false, "usage_error");
    }
    if usage.meters.is_empty() || usage.meters.iter().any(|m| !m.used_pct.is_finite()) {
        return (false, "usage_unknown");
    }
    if usage.meters.iter().any(|m| m.used_pct >= 100.0) {
        return (false, "quota_exhausted");
    }
    (true, "quota_available")
}

/// One ranked account. Ranking is evidence only: it is not a model-compatibility guarantee.
pub struct Candidate {
    pub provider: String,
    pub name: String,
    pub active: bool,
    pub eligible: bool,
    pub reason: &'static str,
    pub cached: bool,
    pub fetched_at_ms: Option<i64>,
    pub cooldown_until_ms: Option<i64>,
    pub score: Option<aas_core::pick::Score>,
    pub meters: Vec<aas_core::usage::Meter>,
}

impl Candidate {
    pub fn id(&self) -> String {
        format!("{}/{}", self.provider, self.name)
    }
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
}

/// Rank accounts best-first: eligible accounts by quota pace (`aas_core::pick`), then the rest.
pub async fn rank(
    store: &AccountStore,
    provider: Option<&str>,
    exclude: &[String],
    mode: UsageCacheMode,
) -> anyhow::Result<Vec<Candidate>> {
    let items =
        aas_providers::snapshot_sorted_with_cache(store, provider, AccountSort::Name, mode).await?;
    let now = now_ms();
    let mut ranked: Vec<Candidate> = items
        .into_iter()
        .map(|item| {
            let id = format!("{}/{}", item.provider, item.name);
            let cooldown = aas_core::backoff::rate_limited_until(&id);
            let (eligible, reason) =
                assessment(&item.usage, exclude.contains(&item.name), cooldown);
            Candidate {
                score: aas_core::pick::score(&item.usage.meters, now),
                provider: item.provider,
                name: item.name,
                active: item.active,
                eligible,
                reason,
                cached: item.cached,
                fetched_at_ms: item.fetched_at_ms,
                cooldown_until_ms: cooldown,
                meters: item.usage.meters,
            }
        })
        .collect();
    ranked.sort_by(|a, b| {
        b.eligible
            .cmp(&a.eligible)
            .then_with(|| match (&a.score, &b.score) {
                (Some(x), Some(y)) => aas_core::pick::compare(x, a.active, y, b.active),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            })
            .then_with(|| a.id().cmp(&b.id()))
    });
    Ok(ranked)
}

pub async fn run(
    store: &AccountStore,
    provider: Option<&str>,
    exclude: &[String],
    json: bool,
    fresh: bool,
) -> anyhow::Result<()> {
    let provider = provider
        .map(|p| {
            aas_core::naming::normalize_provider(p)
                .filter(|p| aas_providers::get_adapter(p).is_some())
                .ok_or_else(|| anyhow::anyhow!("Unknown provider: {p}"))
        })
        .transpose()?;
    let mode = if fresh {
        UsageCacheMode::Refresh
    } else {
        UsageCacheMode::PreferCache
    };
    let accounts: Vec<_> = rank(store, provider.as_deref(), exclude, mode)
        .await?
        .into_iter()
        .map(|c| {
            serde_json::json!({
                "id": c.id(), "name": c.name, "provider": c.provider,
                "active": c.active, "eligible": c.eligible, "reason": c.reason,
                "cached": c.cached, "fetchedAtMs": c.fetched_at_ms,
                "remainingPct": c.score.map(|s| s.remaining_pct),
                "paceScore": c.score.map(|s| s.pace),
                "usageCooldownUntilMs": c.cooldown_until_ms,
                "meters": c.meters,
            })
        })
        .collect();
    if json {
        println!(
            "{}",
            serde_json::json!({"schemaVersion": 1, "accounts": accounts})
        );
    } else {
        for account in accounts {
            println!(
                "{}\t{}\t{}",
                account["id"].as_str().unwrap_or_default(),
                if account["eligible"] == true {
                    "candidate"
                } else {
                    "unavailable"
                },
                account["reason"].as_str().unwrap_or_default()
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use aas_core::usage::Meter;

    #[test]
    fn unknown_errors_and_cooldowns_never_become_available() {
        let mut usage = Usage::default();
        assert_eq!(assessment(&usage, false, None), (false, "usage_unknown"));
        usage.meters.push(Meter::new("5h", 20.0, None));
        assert_eq!(assessment(&usage, false, None), (true, "quota_available"));
        assert_eq!(assessment(&usage, true, None), (false, "excluded"));
        assert_eq!(
            assessment(&usage, false, Some(123)),
            (false, "usage_rate_limited")
        );
        usage.error = Some("stale fallback after failed fetch".into());
        assert_eq!(assessment(&usage, false, None), (false, "usage_error"));
        usage.error = None;
        usage.meters.push(Meter::new("7d", 100.0, None));
        assert_eq!(assessment(&usage, false, None), (false, "quota_exhausted"));
        usage.meters[1].used_pct = f64::NAN;
        assert_eq!(assessment(&usage, false, None), (false, "usage_unknown"));
    }
}
