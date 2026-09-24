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
    let items = aas_providers::snapshot_sorted_with_cache(
        store,
        provider.as_deref(),
        AccountSort::Name,
        mode,
    )
    .await?;
    let mut accounts = Vec::new();
    for item in &items {
        let id = format!("{}/{}", item.provider, item.name);
        let cooldown = aas_core::backoff::rate_limited_until(&id);
        let (eligible, reason) = assessment(&item.usage, exclude.contains(&item.name), cooldown);
        let remaining = super::remaining_pct(&item.usage.meters);
        accounts.push(serde_json::json!({
            "id": id, "name": item.name, "provider": item.provider,
            "active": item.active, "eligible": eligible, "reason": reason,
            "cached": item.cached, "fetchedAtMs": item.fetched_at_ms,
            "remainingPct": remaining, "usageCooldownUntilMs": cooldown,
            "meters": item.usage.meters,
        }));
    }
    accounts.sort_by(|a, b| {
        b["eligible"]
            .as_bool()
            .cmp(&a["eligible"].as_bool())
            .then_with(|| {
                b["remainingPct"]
                    .as_f64()
                    .partial_cmp(&a["remainingPct"].as_f64())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| a["id"].as_str().cmp(&b["id"].as_str()))
    });
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
