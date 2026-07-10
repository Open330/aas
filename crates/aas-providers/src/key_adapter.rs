//! Grok + Z.AI adapter. Mirrors asx `providers/key-adapter.ts` (`createKeyAdapter`).
//!
//! Both are API-key providers with no OAuth refresh. Grok additionally understands a native
//! `~/.grok/auth.json` (`{key}` or `{<issuer>:{key}}`) and JWT subscription tokens. The two
//! providers share this module; `provider` is `"grok"` or `"zai"`.

use crate::common::{http_client, num_alt, set_active, store_account_secret, value_display};
use crate::RefreshOutcome;
use aas_core::jwt::decode_jwt_claims;
use aas_core::platform::grok_auth_path;
use aas_core::secure_store::{get_secret, write_restricted_file};
use aas_core::usage::{Meter, Usage};
use serde_json::{json, Value};

const ZAI_BASE_URL: &str = "https://api.z.ai/api/coding/paas/v4";
const ZAI_QUOTA_URL: &str = "https://api.z.ai/api/monitor/usage/quota/limit";

// ---------------------------------------------------------------------------
// Pure helpers (unit-tested).
// ---------------------------------------------------------------------------

/// asx `getEnvKey`: `<PFX>_API_KEY | <PFX>_KEY | (grok) XAI_API_KEY`.
fn get_env_key(provider: &str) -> Option<String> {
    let pfx = provider.to_uppercase();
    std::env::var(format!("{pfx}_API_KEY"))
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            std::env::var(format!("{pfx}_KEY"))
                .ok()
                .filter(|s| !s.is_empty())
        })
        .or_else(|| {
            if provider == "grok" {
                std::env::var("XAI_API_KEY").ok().filter(|s| !s.is_empty())
            } else {
                None
            }
        })
}

/// asx `getGrokAuthFile`: parse `~/.grok/auth.json`.
fn get_grok_auth_file() -> Option<Value> {
    let raw = std::fs::read_to_string(grok_auth_path()).ok()?;
    serde_json::from_str(&raw).ok()
}

/// asx `getGrokAuth`: the auth object with a `.key`, or the first sub-object.
fn get_grok_auth() -> Option<Value> {
    let data = get_grok_auth_file()?;
    if !data.is_object() {
        return None;
    }
    if data.get("key").is_some() {
        return Some(data);
    }
    data.as_object().and_then(|m| m.values().next().cloned())
}

/// asx `grokAuthFileFromCredential`: normalize a stored credential into the on-disk file shape.
pub(crate) fn grok_auth_file_from_credential(raw: &str) -> Value {
    if let Ok(data) = serde_json::from_str::<Value>(raw) {
        if data.is_object() {
            if data.get("key").is_some() {
                return json!({ "asx": data });
            }
            return data;
        }
    }
    json!({ "asx": { "key": raw } })
}

/// asx `grokBearer`: the bearer token from a credential (`.key`, first `{key}`, or raw).
pub(crate) fn grok_bearer(raw: &str) -> String {
    if let Ok(data) = serde_json::from_str::<Value>(raw) {
        if let Some(obj) = data.as_object() {
            if let Some(k) = obj.get("key").and_then(|v| v.as_str()) {
                return k.to_string();
            }
            for v in obj.values() {
                if let Some(k) = v.get("key").and_then(|k| k.as_str()) {
                    return k.to_string();
                }
            }
        }
    }
    raw.to_string()
}

/// asx `parseGrokTokenInfo`: JWT claims, but only for tokens that look like a JWT (`ey…`).
pub(crate) fn parse_grok_token_info(token: &str) -> Option<Value> {
    if !token.starts_with("ey") {
        return None;
    }
    decode_jwt_claims(token)
}

/// JS `parseFloat`: parse the leading numeric prefix, ignoring trailing junk (`"42%"` → 42).
fn js_parse_float(s: &str) -> Option<f64> {
    let s = s.trim_start();
    let bytes = s.as_bytes();
    let n = bytes.len();
    let mut i = 0;
    if i < n && (bytes[i] == b'+' || bytes[i] == b'-') {
        i += 1;
    }
    let mut seen_digit = false;
    while i < n && bytes[i].is_ascii_digit() {
        i += 1;
        seen_digit = true;
    }
    if i < n && bytes[i] == b'.' {
        i += 1;
        while i < n && bytes[i].is_ascii_digit() {
            i += 1;
            seen_digit = true;
        }
    }
    if !seen_digit {
        return None;
    }
    if i < n && (bytes[i] == b'e' || bytes[i] == b'E') {
        let mut j = i + 1;
        if j < n && (bytes[j] == b'+' || bytes[j] == b'-') {
            j += 1;
        }
        let mut exp_digit = false;
        while j < n && bytes[j].is_ascii_digit() {
            j += 1;
            exp_digit = true;
        }
        if exp_digit {
            i = j;
        }
    }
    s[..i].parse::<f64>().ok()
}

/// asx `parsePercent`: numbers/strings; fractions `<= 1` (with no `%`) are scaled to 0..100.
pub(crate) fn parse_percent(value: &Value) -> Option<f64> {
    let (n, s) = match value {
        Value::Number(num) => (num.as_f64()?, num.to_string()),
        Value::String(st) => (js_parse_float(st)?, st.clone()),
        _ => return None,
    };
    if !n.is_finite() {
        return None;
    }
    if n <= 1.0 && !s.trim().ends_with('%') {
        Some(n * 100.0)
    } else {
        Some(n)
    }
}

/// asx Z.AI quota parse → used percentage from the `TOKENS_LIMIT` entry.
pub(crate) fn parse_zai_quota_used_pct(payload: &Value) -> Option<f64> {
    let limits = payload
        .get("data")
        .and_then(|d| d.get("limits"))
        .or_else(|| payload.get("limits"))?;
    let arr = limits.as_array()?;
    let token_limit = arr
        .iter()
        .find(|x| x.get("type").and_then(|t| t.as_str()) == Some("TOKENS_LIMIT"))?;
    parse_percent(token_limit.get("percentage")?)
}

/// asx Grok CLI billing (`/v1/billing`) → `(credits meter, notes)`.
pub(crate) fn parse_grok_billing(binfo: &Value) -> (Option<Meter>, Vec<String>) {
    let mut notes = Vec::new();
    let config = binfo.get("config");
    let monthly = config
        .and_then(|c| c.get("monthlyLimit"))
        .and_then(|m| m.get("val"))
        .and_then(|v| v.as_f64())
        .or_else(|| {
            config
                .and_then(|c| c.get("monthly_limit"))
                .and_then(|m| m.get("val"))
                .and_then(|v| v.as_f64())
        });
    let used = config
        .and_then(|c| c.get("used"))
        .and_then(|m| m.get("val"))
        .and_then(|v| v.as_f64());

    let mut meter = None;
    if let (Some(limit), Some(used)) = (monthly, used) {
        let used_pct = (used / limit * 100.0).min(100.0);
        meter = Some(Meter::new("credits", used_pct, None));
        notes.push(format!("credits {used}/{limit}"));
    }
    if let Some(end) = binfo.get("billingPeriodEnd") {
        if !end.is_null() {
            notes.push(format!("billingPeriodEnd={}", value_display(end)));
        }
    }
    (meter, notes)
}

/// asx Grok API key (`/v1/api-key`) → `(credits meter, notes, key name)`.
pub(crate) fn parse_grok_apikey(kinfo: &Value) -> (Option<Meter>, Vec<String>, Option<String>) {
    let mut notes = Vec::new();
    let rem = num_alt(kinfo, "remaining_balance", "remainingBalance");
    let total = num_alt(kinfo, "total_granted", "totalGranted");

    let mut meter = None;
    match (rem, total) {
        (Some(rem), Some(total)) if total > 0.0 => {
            let used = (total - rem).max(0.0);
            let used_pct = (used / total * 100.0).min(100.0);
            meter = Some(Meter::new("credits", used_pct, None));
            notes.push(format!("${rem:.2} left"));
        }
        (Some(rem), _) => notes.push(format!("credits_remaining=${rem}")),
        _ => {}
    }

    let key_name = kinfo.get("name").and_then(|v| v.as_str()).map(String::from);
    if let Some(kn) = &key_name {
        notes.push(format!("key={kn}"));
    }
    (meter, notes, key_name)
}

// ---------------------------------------------------------------------------
// Native grok auth IO.
// ---------------------------------------------------------------------------

fn try_extract_grok_email() -> Option<String> {
    get_grok_auth().and_then(|a| a.get("email").and_then(|v| v.as_str()).map(String::from))
}

fn write_grok_auth(raw: &str) -> std::io::Result<()> {
    let p = grok_auth_path();
    write_restricted_file(&p, &grok_auth_file_from_credential(raw).to_string())
}

// ---------------------------------------------------------------------------
// Usage.
// ---------------------------------------------------------------------------

async fn grok_rate_limit_note(
    client: &reqwest::Client,
    bearer: &str,
    probe: bool,
) -> Result<Option<String>, String> {
    let res = if probe {
        let body = json!({
            "model": "grok-4.20-non-reasoning",
            "messages": [{"role": "user", "content": "hi"}],
            "max_tokens": 1
        });
        client
            .post("https://api.x.ai/v1/chat/completions")
            .header("Authorization", format!("Bearer {bearer}"))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("rate-limit probe network error: {e}"))?
    } else {
        client
            .get("https://api.x.ai/v1/models")
            .header("Authorization", format!("Bearer {bearer}"))
            .send()
            .await
            .map_err(|e| format!("models network error: {e}"))?
    };
    if !res.status().is_success() {
        return Err(format!(
            "rate-limit endpoint returned HTTP {}",
            res.status()
        ));
    }
    let h = res.headers();
    let req = h
        .get("x-ratelimit-remaining-requests")
        .and_then(|v| v.to_str().ok());
    let tok = h
        .get("x-ratelimit-remaining-tokens")
        .and_then(|v| v.to_str().ok());
    if req.is_some() || tok.is_some() {
        Ok(Some(format!(
            "rate remaining req={} tok={}",
            req.unwrap_or("?"),
            tok.unwrap_or("?")
        )))
    } else {
        Ok(None)
    }
}

async fn grok_usage(account: &str) -> Usage {
    // Resolve the key: stored secret → XAI_API_KEY env → ~/.grok/auth.json.
    let mut key = get_secret("grok", account);
    if key.is_none() {
        key = std::env::var("XAI_API_KEY").ok().filter(|s| !s.is_empty());
        if key.is_none() {
            key = get_grok_auth()
                .and_then(|a| a.get("key").and_then(|v| v.as_str()).map(String::from));
        }
    }
    let Some(key) = key else {
        return Usage {
            headline: "API key (no live quota data)".into(),
            ..Default::default()
        };
    };
    let bearer = grok_bearer(&key);
    let client = http_client();

    let mut meters: Vec<Meter> = Vec::new();
    let mut notes: Vec<String> = Vec::new();
    let mut key_name: Option<String> = None;
    let mut successful_responses = 0usize;
    let mut errors: Vec<String> = Vec::new();

    if bearer.starts_with("ey") {
        // Subscription / CLI token → billing + settings.
        match client
            .get("https://cli-chat-proxy.grok.com/v1/billing")
            .header("Authorization", format!("Bearer {bearer}"))
            .send()
            .await
        {
            Ok(res) if res.status().is_success() => match res.json::<Value>().await {
                Ok(binfo) => {
                    successful_responses += 1;
                    let (m, ns) = parse_grok_billing(&binfo);
                    meters.extend(m);
                    notes.extend(ns);
                }
                Err(e) => errors.push(format!("billing returned invalid JSON: {e}")),
            },
            Ok(res) => errors.push(format!("billing returned HTTP {}", res.status())),
            Err(e) => errors.push(format!("billing network error: {e}")),
        }
        match client
            .get("https://cli-chat-proxy.grok.com/v1/settings")
            .header("Authorization", format!("Bearer {bearer}"))
            .send()
            .await
        {
            Ok(res) if res.status().is_success() => match res.json::<Value>().await {
                Ok(sinfo) => {
                    successful_responses += 1;
                    key_name = sinfo
                        .get("plan")
                        .and_then(|v| v.as_str())
                        .or_else(|| sinfo.get("subscription").and_then(|v| v.as_str()))
                        .map(String::from);
                }
                Err(e) => errors.push(format!("settings returned invalid JSON: {e}")),
            },
            Ok(res) => errors.push(format!("settings returned HTTP {}", res.status())),
            Err(e) => errors.push(format!("settings network error: {e}")),
        }
    } else {
        // Pure xAI API key → /api-key credits.
        match client
            .get("https://api.x.ai/v1/api-key")
            .header("Authorization", format!("Bearer {bearer}"))
            .send()
            .await
        {
            Ok(res) if res.status().is_success() => match res.json::<Value>().await {
                Ok(kinfo) => {
                    successful_responses += 1;
                    let (m, ns, kn) = parse_grok_apikey(&kinfo);
                    meters.extend(m);
                    notes.extend(ns);
                    if kn.is_some() {
                        key_name = kn;
                    }
                }
                Err(e) => errors.push(format!("api-key returned invalid JSON: {e}")),
            },
            Ok(res) => errors.push(format!("api-key returned HTTP {}", res.status())),
            Err(e) => errors.push(format!("api-key network error: {e}")),
        }
    }

    // Rate limits: header probe via /models, else a tiny chat/completions probe.
    let mut rate = match grok_rate_limit_note(&client, &bearer, false).await {
        Ok(note) => {
            successful_responses += 1;
            note
        }
        Err(e) => {
            errors.push(e);
            None
        }
    };
    if rate.is_none() {
        match grok_rate_limit_note(&client, &bearer, true).await {
            Ok(note) => {
                successful_responses += 1;
                rate = note;
            }
            Err(e) => errors.push(e),
        }
    }
    if let Some(rn) = rate {
        notes.push(rn);
    }

    // Tier/team from the JWT, if any.
    if let Some(info) = parse_grok_token_info(&bearer) {
        let tier = info
            .get("tier")
            .map(value_display)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "?".into());
        let team = info
            .get("team_id")
            .and_then(|v| v.as_str())
            .map(|t| format!(" team={t}"))
            .unwrap_or_default();
        notes.push(format!("tier={tier}{team}"));
    }

    if let Some(failure) = grok_failure_if_no_success(successful_responses, &errors) {
        return failure;
    }

    let headline = match &key_name {
        Some(kn) => format!("Grok {kn}"),
        None => "Grok key".into(),
    };
    Usage {
        headline,
        plan: key_name,
        meters,
        notes,
        ..Default::default()
    }
}

fn grok_failure_if_no_success(successful_responses: usize, errors: &[String]) -> Option<Usage> {
    (successful_responses == 0).then(|| {
        Usage::error(
            "Grok",
            if errors.is_empty() {
                "Grok usage endpoints were unavailable".to_string()
            } else {
                errors.join("; ")
            },
        )
    })
}

async fn zai_usage(account: &str) -> Usage {
    let Some(key) = get_secret("zai", account) else {
        return Usage {
            headline: "API key (no live quota data)".into(),
            ..Default::default()
        };
    };
    let client = http_client();
    // ⚠ Z.AI quota uses `Authorization: <raw key>` with NO `Bearer` prefix.
    let res = client
        .get(ZAI_QUOTA_URL)
        .header("Authorization", &key)
        .header("Accept-Language", "en-US,en")
        .header("Content-Type", "application/json")
        .send()
        .await;
    match res {
        Ok(res) => {
            let status = res.status().as_u16();
            if !(200..300).contains(&status) {
                return Usage::error("zai", format!("ZAI usage fetch failed: {status}"));
            }
            let payload = res.json::<Value>().await.unwrap_or(Value::Null);
            match parse_zai_quota_used_pct(&payload) {
                Some(used) => Usage {
                    headline: "Z.AI".into(),
                    meters: vec![Meter::new("5h", used.clamp(0.0, 100.0), None)],
                    ..Default::default()
                },
                None => Usage {
                    headline: "Z.AI".into(),
                    error: Some("no token quota returned".into()),
                    ..Default::default()
                },
            }
        }
        Err(_) => Usage::error("zai", "ZAI usage fetch: network error"),
    }
}

async fn test_zai_key(key: &str) -> anyhow::Result<()> {
    let client = http_client();
    let res = client
        .get(format!("{ZAI_BASE_URL}/models"))
        .header("Authorization", format!("Bearer {key}"))
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("ZAI endpoint test failed: {e}"))?;
    if !res.status().is_success() {
        let status = res.status();
        let detail: String = res
            .text()
            .await
            .unwrap_or_default()
            .chars()
            .take(240)
            .collect();
        anyhow::bail!(
            "ZAI endpoint test failed ({}{}{})",
            status.as_u16(),
            status
                .canonical_reason()
                .map(|r| format!(" {r}"))
                .unwrap_or_default(),
            if detail.is_empty() {
                String::new()
            } else {
                format!(": {detail}")
            }
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Adapter methods (dispatched from `Provider` with `provider` = "grok" | "zai").
// ---------------------------------------------------------------------------

pub(crate) async fn usage(provider: &str, account: &str) -> Usage {
    if provider == "grok" {
        grok_usage(account).await
    } else {
        zai_usage(account).await
    }
}

pub(crate) async fn current_credential(provider: &str) -> Option<String> {
    if provider == "grok" {
        get_grok_auth_file().map(|a| a.to_string())
    } else {
        get_env_key(provider)
    }
}

pub(crate) async fn current_email(provider: &str) -> Option<String> {
    if provider == "grok" {
        try_extract_grok_email()
    } else {
        None
    }
}

pub(crate) async fn load_current(
    provider: &str,
    account: &str,
    label: Option<&str>,
) -> anyhow::Result<()> {
    let mut val = get_env_key(provider);
    if val.is_none() && provider == "grok" {
        if let Some(auth) = get_grok_auth_file() {
            val = Some(auth.to_string());
        }
    }
    let val = val.ok_or_else(|| {
        anyhow::anyhow!(
            "No live {provider} credential found. Set the provider API key or log in first."
        )
    })?;
    let email = if provider == "grok" {
        try_extract_grok_email()
    } else {
        None
    };
    store_account_secret(provider, account, label, email, &val)?;
    Ok(())
}

pub(crate) async fn switch_to(provider: &str, account: &str) -> anyhow::Result<()> {
    let v = get_secret(provider, account)
        .ok_or_else(|| anyhow::anyhow!("No key for {provider}/{account}"))?;
    let env_name = if provider == "grok" {
        "XAI_API_KEY".to_string()
    } else {
        format!("{}_API_KEY", provider.to_uppercase())
    };
    let previous_env = std::env::var_os(&env_name);
    let previous_grok = (provider == "grok").then(get_grok_auth_file).flatten();
    if provider == "grok" {
        write_grok_auth(&v)?;
        std::env::set_var("XAI_API_KEY", grok_bearer(&v));
    } else {
        std::env::set_var(&env_name, &v);
    }
    if let Err(error) = set_active(provider, account) {
        match previous_env {
            Some(previous) => std::env::set_var(&env_name, previous),
            None => std::env::remove_var(&env_name),
        }
        let rollback = if provider == "grok" {
            match previous_grok {
                Some(previous) => write_restricted_file(&grok_auth_path(), &previous.to_string()),
                None => match std::fs::remove_file(grok_auth_path()) {
                    Ok(()) => Ok(()),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                    Err(e) => Err(e),
                },
            }
        } else {
            Ok(())
        };
        anyhow::bail!(
            "could not update active {provider} marker: {error}; native rollback={rollback:?}"
        );
    }
    Ok(())
}

pub(crate) async fn clear_current(provider: &str) -> anyhow::Result<()> {
    if provider == "grok" {
        match std::fs::remove_file(grok_auth_path()) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

pub(crate) fn login_command(provider: &str) -> Option<Vec<String>> {
    if provider == "grok" {
        Some(vec!["grok".into(), "login".into()])
    } else {
        None
    }
}

/// asx key-adapter `login` (Z.AI only): validate the key, then store + activate.
pub(crate) async fn validate_and_store_key(
    provider: &str,
    account: &str,
    key: &str,
) -> anyhow::Result<()> {
    if provider != "zai" {
        anyhow::bail!("validate_and_store_key is only supported for zai");
    }
    let key = key.trim();
    if key.is_empty() {
        anyhow::bail!("No Z.AI API key provided.");
    }
    test_zai_key(key).await?;
    store_account_secret(provider, account, None, None, key)?;
    set_active(provider, account)?;
    Ok(())
}

pub(crate) fn refresh_outcome(provider: &str) -> RefreshOutcome {
    // Key providers have no OAuth refresh; nothing to do.
    RefreshOutcome {
        ok: true,
        message: format!("{provider} does not require refresh"),
        needs_relogin: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_scaling() {
        assert_eq!(parse_percent(&json!(42.0)), Some(42.0));
        assert_eq!(parse_percent(&json!(0.42)), Some(42.0)); // fraction scaled
        assert_eq!(parse_percent(&json!("42%")), Some(42.0));
        assert_eq!(parse_percent(&json!("0.5")), Some(50.0));
        assert_eq!(parse_percent(&json!("0.5%")), Some(0.5)); // explicit % → not scaled
        assert_eq!(parse_percent(&json!(true)), None);
    }

    #[test]
    fn zai_quota_nested_and_flat() {
        let nested = json!({"data": {"limits": [
            {"type": "REQUESTS_LIMIT", "percentage": 10},
            {"type": "TOKENS_LIMIT", "percentage": 0.42}
        ]}});
        assert_eq!(parse_zai_quota_used_pct(&nested), Some(42.0));

        let flat = json!({"limits": [{"type": "TOKENS_LIMIT", "percentage": "73%"}]});
        assert_eq!(parse_zai_quota_used_pct(&flat), Some(73.0));

        assert_eq!(parse_zai_quota_used_pct(&json!({"limits": []})), None);
    }

    #[test]
    fn grok_bearer_from_shapes() {
        assert_eq!(grok_bearer(r#"{"key":"tok-1"}"#), "tok-1");
        assert_eq!(grok_bearer(r#"{"issuer":{"key":"tok-2"}}"#), "tok-2");
        assert_eq!(grok_bearer("raw-token"), "raw-token");
    }

    #[test]
    fn grok_auth_file_normalization() {
        // bare `{key}` gets wrapped under `asx`
        assert_eq!(
            grok_auth_file_from_credential(r#"{"key":"k"}"#),
            json!({"asx": {"key": "k"}})
        );
        // already-wrapped map is preserved
        assert_eq!(
            grok_auth_file_from_credential(r#"{"issuer":{"key":"k"}}"#),
            json!({"issuer": {"key": "k"}})
        );
        // raw string becomes asx.key
        assert_eq!(
            grok_auth_file_from_credential("raw"),
            json!({"asx": {"key": "raw"}})
        );
    }

    #[test]
    fn grok_billing_meter() {
        let binfo = json!({
            "config": {"monthlyLimit": {"val": 100}, "used": {"val": 25}},
            "billingPeriodEnd": "2026-08-01"
        });
        let (meter, notes) = parse_grok_billing(&binfo);
        let m = meter.unwrap();
        assert_eq!(m.label, "credits");
        assert!((m.used_pct - 25.0).abs() < 1e-9);
        assert!(notes
            .iter()
            .any(|n| n.contains("billingPeriodEnd=2026-08-01")));
    }

    #[test]
    fn grok_apikey_meter_and_fallback() {
        let kinfo = json!({"remaining_balance": 7.5, "total_granted": 10.0, "name": "mykey"});
        let (meter, notes, name) = parse_grok_apikey(&kinfo);
        let m = meter.unwrap();
        assert!((m.used_pct - 25.0).abs() < 1e-9);
        assert_eq!(name.as_deref(), Some("mykey"));
        assert!(notes.iter().any(|n| n == "$7.50 left"));
        assert!(notes.iter().any(|n| n == "key=mykey"));

        // no total → credits_remaining fallback
        let kinfo2 = json!({"remaining_balance": 3.0});
        let (meter2, notes2, _) = parse_grok_apikey(&kinfo2);
        assert!(meter2.is_none());
        assert!(notes2.iter().any(|n| n == "credits_remaining=$3"));
    }

    #[test]
    fn grok_all_endpoint_failures_are_not_reported_healthy() {
        let errors = vec!["api-key returned HTTP 401 Unauthorized".to_string()];
        let usage = grok_failure_if_no_success(0, &errors).unwrap();
        assert!(usage.meters.is_empty());
        assert!(usage.error.as_deref().unwrap().contains("401"));
        assert!(grok_failure_if_no_success(1, &errors).is_none());
    }
}
