//! Claude Code adapter. Mirrors asx `providers/claude-code.ts`.
//!
//! Credential shapes: OAuth `{claudeAiOauth:{accessToken,refreshToken,subscriptionType,
//! rateLimitTier,expiresAt(ms)}}` or long-lived `{type:"claude-code-oauth-token",token}`.
//! Live credential lives in the macOS Keychain (or `~/.claude/.credentials.json` off mac).

use crate::common::{add_account, http_client, now_ms, set_0600, set_active};
use crate::RefreshOutcome;
use aas_core::keychain::{claude_keychain_service, delete_credential, read_credential, write_credential};
use aas_core::platform::{claude_config_dir, claude_credentials_path};
use aas_core::secure_store::{get_secret, set_secret};
use aas_core::usage::{Meter, Usage};
use chrono::{SecondsFormat, TimeZone, Utc};
use serde_json::{json, Value};

const PROVIDER: &str = "claude";
/// Claude Code's public OAuth client id (used for the refresh_token grant).
const CLAUDE_OAUTH_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const LONG_LIVED_TOKEN_TYPE: &str = "claude-code-oauth-token";

// ---------------------------------------------------------------------------
// Pure credential/token helpers (unit-tested).
// ---------------------------------------------------------------------------

/// asx `normalizeClaudeCodeOAuthToken`: strip an `export CLAUDE_CODE_OAUTH_TOKEN=` wrapper
/// and matching surrounding quotes.
pub(crate) fn normalize_claude_code_oauth_token(input: &str) -> String {
    let mut token = input.trim().to_string();
    // /^(?:export\s+)?CLAUDE_CODE_OAUTH_TOKEN=(.+)$/s
    {
        let s = token.as_str();
        let after_export = s
            .strip_prefix("export")
            .map(|r| r.trim_start())
            .filter(|r| r.len() < s.len()) // only when `export` was actually present
            .unwrap_or(s);
        if let Some(rest) = after_export.strip_prefix("CLAUDE_CODE_OAUTH_TOKEN=") {
            let rest = rest.trim();
            if !rest.is_empty() {
                token = rest.to_string();
            }
        }
    }
    let bytes = token.as_bytes();
    if bytes.len() >= 2 {
        let first = bytes[0];
        let last = bytes[bytes.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            token = token[1..token.len() - 1].to_string();
        }
    }
    token.trim().to_string()
}

/// asx `isClaudeCodeLongLivedToken`.
pub(crate) fn is_long_lived(raw: &str) -> bool {
    serde_json::from_str::<Value>(raw)
        .ok()
        .and_then(|v| v.get("type").and_then(|t| t.as_str()).map(|s| s == LONG_LIVED_TOKEN_TYPE))
        .unwrap_or(false)
}

/// asx `getClaudeCodeOAuthToken`.
pub(crate) fn get_claude_code_oauth_token(raw: &str) -> Option<String> {
    match serde_json::from_str::<Value>(raw) {
        Ok(parsed) => {
            if parsed.get("type").and_then(|v| v.as_str()) == Some(LONG_LIVED_TOKEN_TYPE) {
                if let Some(t) = parsed.get("token").and_then(|v| v.as_str()) {
                    return Some(normalize_claude_code_oauth_token(t));
                }
            }
            parsed
                .get("claudeAiOauth")
                .and_then(|o| o.get("accessToken"))
                .and_then(|v| v.as_str())
                .or_else(|| parsed.get("accessToken").and_then(|v| v.as_str()))
                .map(String::from)
        }
        Err(_) => {
            if raw.is_empty() {
                None
            } else {
                Some(raw.to_string())
            }
        }
    }
}

fn make_long_lived_token_credential(token: &str) -> String {
    json!({"type": LONG_LIVED_TOKEN_TYPE, "token": normalize_claude_code_oauth_token(token)}).to_string()
}

/// The stored `claudeAiOauth.expiresAt` (ms) if present as a number; `None` for long-lived
/// tokens or malformed credentials.
pub(crate) fn claude_expires_at(raw: &str) -> Option<i64> {
    if is_long_lived(raw) {
        return None;
    }
    let v: Value = serde_json::from_str(raw).ok()?;
    let e = v.get("claudeAiOauth")?.get("expiresAt")?;
    e.as_i64().or_else(|| e.as_f64().map(|f| f as i64))
}

/// asx `isExpired`: `expiresAt < now + 60_000`.
pub(crate) fn is_expired_at(raw: &str, now: i64) -> bool {
    claude_expires_at(raw).map(|e| e < now + 60_000).unwrap_or(false)
}

fn is_truthy(v: Option<&Value>) -> bool {
    match v {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map(|f| f != 0.0).unwrap_or(false),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Null) | None => false,
        Some(_) => true,
    }
}

/// asx claude `baseInfo` line (without the `(name)` suffix, which the CLI already knows).
pub(crate) fn claude_base_info(
    is_long_lived: bool,
    sub_type: &str,
    tier: &str,
    profile: Option<&Value>,
) -> String {
    if is_long_lived {
        return "long-lived token".to_string();
    }
    if let Some(prof) = profile {
        let org = prof.get("organization");
        let acc = prof.get("account");
        let org_type = org
            .and_then(|o| o.get("organization_type"))
            .and_then(|v| v.as_str())
            .or_else(|| org.and_then(|o| o.get("billing_type")).and_then(|v| v.as_str()))
            .unwrap_or("");
        let has_max = if is_truthy(acc.and_then(|a| a.get("has_claude_max")))
            || is_truthy(org.and_then(|o| o.get("has_claude_max")))
        {
            "yes"
        } else {
            "no"
        };
        format!("subscription={sub_type} tier={tier} org={org_type} has_max={has_max}")
    } else {
        format!("subscription={sub_type} tier={tier}")
    }
}

/// Parse a Claude `resets_at`, which may be epoch millis (number/string) or an ISO-8601 string.
pub(crate) fn parse_flexible_reset_ms(v: &Value) -> Option<i64> {
    if let Some(n) = v.as_i64() {
        return Some(n);
    }
    if let Some(f) = v.as_f64() {
        return Some(f as i64);
    }
    if let Some(s) = v.as_str() {
        if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
            return Some(dt.timestamp_millis());
        }
        if let Ok(n) = s.parse::<f64>() {
            return Some(n as i64);
        }
    }
    None
}

/// asx claude usage windows → meters. `five_hour|fiveHour` → "5h", `seven_day|sevenDay` → "7d".
pub(crate) fn build_claude_usage_meters(usage: &Value) -> Vec<Meter> {
    let mut meters = Vec::new();
    for (snake, camel, label) in
        [("five_hour", "fiveHour", "5h"), ("seven_day", "sevenDay", "7d")]
    {
        let Some(w) = usage.get(snake).or_else(|| usage.get(camel)) else {
            continue;
        };
        if let Some(util) = w.get("utilization").and_then(|v| v.as_f64()) {
            let used = util.clamp(0.0, 100.0);
            let reset = w.get("resets_at").and_then(parse_flexible_reset_ms);
            meters.push(Meter::new(label, used, reset));
        }
    }
    meters
}

// ---------------------------------------------------------------------------
// Live system credential (keychain / file).
// ---------------------------------------------------------------------------

fn scoped_config() -> bool {
    std::env::var("CLAUDE_CONFIG_DIR").map(|v| !v.is_empty()).unwrap_or(false)
}

/// asx `readCurrentCredentials`.
fn read_current_credentials() -> Option<String> {
    let file_path = claude_credentials_path();
    if cfg!(target_os = "macos") {
        if scoped_config() {
            let svc = claude_keychain_service(Some(&claude_config_dir()));
            if let Some(s) = read_credential(&svc) {
                return Some(s);
            }
            return std::fs::read_to_string(&file_path).ok();
        }
        let svc0 = claude_keychain_service(None);
        for svc in [svc0.as_str(), "Claude Code - credentials", "claude-code-credentials"] {
            if let Some(s) = read_credential(svc) {
                return Some(s);
            }
        }
        return std::fs::read_to_string(&file_path).ok();
    }
    std::fs::read_to_string(&file_path).ok()
}

/// asx `writeActiveCredentials`.
fn write_active_credentials(raw: &str) -> anyhow::Result<()> {
    if cfg!(target_os = "macos") {
        let svc = if scoped_config() {
            claude_keychain_service(Some(&claude_config_dir()))
        } else {
            claude_keychain_service(None)
        };
        write_credential(&svc, raw)?;
        return Ok(());
    }
    let p = claude_credentials_path();
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&p, raw)?;
    set_0600(&p);
    Ok(())
}

/// asx `readScopedAccountEmail`: `<credDir>/.claude.json` `oauthAccount.emailAddress|email`.
fn read_scoped_account_email() -> Option<String> {
    if !scoped_config() {
        return None;
    }
    let cred = claude_credentials_path();
    let dir = cred.parent()?;
    let data: Value = serde_json::from_str(&std::fs::read_to_string(dir.join(".claude.json")).ok()?).ok()?;
    let oa = data.get("oauthAccount")?;
    oa.get("emailAddress")
        .and_then(|v| v.as_str())
        .or_else(|| oa.get("email").and_then(|v| v.as_str()))
        .map(String::from)
}

// ---------------------------------------------------------------------------
// Anthropic HTTP.
// ---------------------------------------------------------------------------

/// `(status, json, retry-after)`. status 0 == network error. Mirrors asx `fetchAnthropicJson`.
async fn fetch_anthropic_json(path: &str, token: &str) -> (u16, Option<Value>, Option<String>) {
    let client = http_client();
    let url = format!("https://api.anthropic.com{path}");
    match client
        .get(&url)
        .header("Authorization", format!("Bearer {token}"))
        .header("anthropic-version", "2023-06-01")
        .header("anthropic-beta", "oauth-2025-04-20")
        .send()
        .await
    {
        Ok(res) => {
            let status = res.status().as_u16();
            let retry = res
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .map(String::from);
            if !(200..300).contains(&status) {
                return (status, None, retry);
            }
            let data = res.json::<Value>().await.ok();
            (status, data, retry)
        }
        Err(_) => (0, None, None),
    }
}

async fn extract_claude_email(cred_json: &str) -> Option<String> {
    let token = get_claude_code_oauth_token(cred_json)?;
    let (status, data, _) = fetch_anthropic_json("/api/oauth/profile", &token).await;
    if !(200..300).contains(&status) {
        return None;
    }
    let d = data?;
    d.get("email_address")
        .and_then(|v| v.as_str())
        .or_else(|| d.get("email").and_then(|v| v.as_str()))
        .or_else(|| d.get("account").and_then(|a| a.get("email_address")).and_then(|v| v.as_str()))
        .or_else(|| d.get("account").and_then(|a| a.get("email")).and_then(|v| v.as_str()))
        .map(String::from)
}

// ---------------------------------------------------------------------------
// Adapter methods.
// ---------------------------------------------------------------------------

pub(crate) async fn usage(account: &str) -> Usage {
    let Some(raw) = get_secret(PROVIDER, account) else {
        return Usage::error("claude", "No stored credential for this account.");
    };
    let data: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
    let long_lived = data.get("type").and_then(|v| v.as_str()) == Some(LONG_LIVED_TOKEN_TYPE);
    let oauth = data.get("claudeAiOauth");
    let tier = oauth
        .and_then(|o| o.get("rateLimitTier"))
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let sub_type = oauth
        .and_then(|o| o.get("subscriptionType"))
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();

    let token = get_claude_code_oauth_token(&raw);

    // Honor a prior 429's Retry-After without touching the network (see aas_core::backoff),
    // so we stop hammering (and re-extending) a rate-limited account.
    let backoff_key = format!("claude/{account}");
    if let Some(until) = aas_core::backoff::rate_limited_until(&backoff_key) {
        let secs = ((until - chrono::Utc::now().timestamp_millis()) / 1000).max(0);
        return Usage {
            headline: claude_base_info(long_lived, &sub_type, &tier, None),
            plan: if long_lived { None } else { Some(sub_type.clone()) },
            error: Some(format!("rate limited (HTTP 429) — backing off {secs}s to recover.")),
            ..Default::default()
        };
    }

    let mut profile: Option<Value> = None;
    if let Some(tok) = &token {
        let (status, prof, _) = fetch_anthropic_json("/api/oauth/profile", tok).await;
        if (200..300).contains(&status) {
            profile = prof;
        }
    }
    let headline = claude_base_info(long_lived, &sub_type, &tier, profile.as_ref());
    let plan = if long_lived { None } else { Some(sub_type) };

    let Some(token) = token else {
        return Usage {
            headline,
            plan,
            error: Some("Unable to fetch usage — no access token stored.".into()),
            ..Default::default()
        };
    };

    let (status, usage_data, retry) = fetch_anthropic_json("/api/oauth/usage", &token).await;
    if status == 401 || status == 403 {
        return Usage {
            headline,
            plan,
            error: Some(format!(
                "token expired or invalid (HTTP {status}). Re-login: aas login claude"
            )),
            ..Default::default()
        };
    }
    if status == 429 {
        // Persist the Retry-After window so subsequent fetches back off instead of re-hitting.
        let secs: i64 = retry
            .as_deref()
            .and_then(|r| r.trim().parse().ok())
            .filter(|&s: &i64| s > 0)
            .unwrap_or(300);
        aas_core::backoff::set_rate_limited(&backoff_key, chrono::Utc::now().timestamp_millis() + secs * 1000);
        return Usage {
            headline,
            plan,
            error: Some(format!("rate limited (HTTP 429), retry after {secs}s.")),
            ..Default::default()
        };
    }
    let Some(usage_data) = usage_data else {
        let why = if status == 0 { "network error".to_string() } else { format!("HTTP {status}") };
        return Usage {
            headline,
            plan,
            error: Some(format!("Unable to fetch usage ({why}).")),
            ..Default::default()
        };
    };

    let meters = build_claude_usage_meters(&usage_data);
    if meters.is_empty() {
        return Usage {
            headline,
            plan,
            error: Some("no quota data returned.".into()),
            ..Default::default()
        };
    }
    aas_core::backoff::clear(&backoff_key); // recovered — drop any stale backoff
    Usage { headline, plan, meters, ..Default::default() }
}

pub(crate) async fn current_credential() -> Option<String> {
    if let Ok(tok) = std::env::var("CLAUDE_CODE_OAUTH_TOKEN") {
        if !tok.is_empty() {
            return Some(make_long_lived_token_credential(&tok));
        }
    }
    read_current_credentials()
}

pub(crate) async fn current_email() -> Option<String> {
    if let Ok(tok) = std::env::var("CLAUDE_CODE_OAUTH_TOKEN") {
        if !tok.is_empty() {
            return extract_claude_email(&make_long_lived_token_credential(&tok)).await;
        }
    }
    let current = read_current_credentials()?;
    extract_claude_email(&current).await
}

pub(crate) async fn load_current(account: &str, label: Option<&str>) -> anyhow::Result<()> {
    if let Ok(tok) = std::env::var("CLAUDE_CODE_OAUTH_TOKEN") {
        if !tok.is_empty() {
            return load_long_lived_token(account, &tok).await;
        }
    }
    let current = read_current_credentials().ok_or_else(|| {
        anyhow::anyhow!(
            "No active Claude Code credentials found. Login with `claude` (or `claude auth login`) first, then run `aas load claude <name>`."
        )
    })?;
    let email = extract_claude_email(&current).await;
    let scoped_email = read_scoped_account_email();
    if let (Some(email), Some(scoped)) = (&email, &scoped_email) {
        if email.to_lowercase() != scoped.to_lowercase() {
            anyhow::bail!(
                "Claude scoped login mismatch: profile home says {scoped}, but credential token belongs to {email}. Refusing to save stale credentials."
            );
        }
    }
    set_secret(PROVIDER, account, &current)?;
    add_account(PROVIDER, account, label, email)?;
    Ok(())
}

pub(crate) async fn switch_to(account: &str) -> anyhow::Result<()> {
    let stored = get_secret(PROVIDER, account).ok_or_else(|| {
        anyhow::anyhow!("No credentials stored for {PROVIDER}/{account}. Use 'aas load' first.")
    })?;
    if !is_long_lived(&stored) {
        write_active_credentials(&stored)?;
    }
    set_active(PROVIDER, account)?;
    Ok(())
}

pub(crate) async fn clear_current() -> anyhow::Result<()> {
    if cfg!(target_os = "macos") {
        let services: Vec<String> = if scoped_config() {
            vec![claude_keychain_service(Some(&claude_config_dir()))]
        } else {
            vec![
                claude_keychain_service(None),
                "Claude Code - credentials".to_string(),
                "claude-code-credentials".to_string(),
            ]
        };
        for svc in services {
            delete_credential(&svc);
        }
        return Ok(());
    }
    let _ = std::fs::remove_file(claude_credentials_path());
    Ok(())
}

pub(crate) async fn is_expired(account: &str) -> bool {
    let Some(raw) = get_secret(PROVIDER, account) else {
        return false;
    };
    is_expired_at(&raw, now_ms())
}

pub(crate) async fn refresh(account: &str) -> RefreshOutcome {
    let fail = |m: String| RefreshOutcome { ok: false, message: m, needs_relogin: false };
    let done = |m: String| RefreshOutcome { ok: true, message: m, needs_relogin: false };

    let Some(raw) = get_secret(PROVIDER, account) else {
        return fail("no stored credential".into());
    };
    if is_long_lived(&raw) {
        return done("long-lived token does not need refresh".into());
    }
    let Ok(parsed) = serde_json::from_str::<Value>(&raw) else {
        return fail("stored credential is not valid JSON".into());
    };
    let Some(o) = parsed.get("claudeAiOauth").cloned() else {
        return fail("stored credential is not valid JSON".into());
    };
    let Some(refresh_token) = o.get("refreshToken").and_then(|v| v.as_str()).map(String::from) else {
        return RefreshOutcome {
            ok: false,
            message: "no refresh token stored".into(),
            needs_relogin: true,
        };
    };

    let client = http_client();
    let body = json!({
        "grant_type": "refresh_token",
        "refresh_token": refresh_token,
        "client_id": CLAUDE_OAUTH_CLIENT_ID,
    });
    let res = match client
        .post("https://console.anthropic.com/v1/oauth/token")
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return fail(format!("network error: {e}")),
    };
    let status = res.status();
    if !status.is_success() {
        let j = res.json::<Value>().await.unwrap_or_else(|_| json!({}));
        if j.get("error").and_then(|v| v.as_str()) == Some("invalid_grant") {
            return RefreshOutcome {
                ok: false,
                message: "refresh token invalid/revoked".into(),
                needs_relogin: true,
            };
        }
        let err = j.get("error").and_then(|v| v.as_str()).unwrap_or("");
        return fail(format!("refresh failed (HTTP {}: {err})", status.as_u16()));
    }
    let j = match res.json::<Value>().await {
        Ok(v) => v,
        Err(e) => return fail(format!("invalid refresh response: {e}")),
    };
    let access_token = j.get("access_token").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let new_refresh = j
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .map(String::from)
        .unwrap_or(refresh_token);
    let expires_in = j.get("expires_in").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let expires_at = now_ms() + (expires_in * 1000.0) as i64;

    let mut oauth_obj = o.as_object().cloned().unwrap_or_default();
    oauth_obj.insert("accessToken".into(), json!(access_token));
    oauth_obj.insert("refreshToken".into(), json!(new_refresh));
    oauth_obj.insert("expiresAt".into(), json!(expires_at));
    let new_raw = json!({ "claudeAiOauth": Value::Object(oauth_obj) }).to_string();

    if let Err(e) = set_secret(PROVIDER, account, &new_raw) {
        return fail(format!("failed to store refreshed credential: {e}"));
    }

    let mut synced = false;
    if read_current_credentials().as_deref() == Some(raw.as_str()) && write_active_credentials(&new_raw).is_ok() {
        synced = true;
    }
    let iso = Utc
        .timestamp_millis_opt(expires_at)
        .single()
        .map(|d| d.to_rfc3339_opts(SecondsFormat::Millis, true))
        .unwrap_or_default();
    done(format!("refreshed (expires {iso}){}", if synced { " [native synced]" } else { "" }))
}

pub(crate) fn login_command() -> Option<Vec<String>> {
    Some(vec!["claude".into(), "auth".into(), "login".into()])
}

pub(crate) async fn load_long_lived_token(account: &str, token: &str) -> anyhow::Result<()> {
    let raw = make_long_lived_token_credential(token);
    let email = extract_claude_email(&raw).await;
    set_secret(PROVIDER, account, &raw)?;
    add_account(PROVIDER, account, Some(account), email)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oauth_token_extraction() {
        let oauth = r#"{"claudeAiOauth":{"accessToken":"acc-123","refreshToken":"ref","subscriptionType":"max","rateLimitTier":"default","expiresAt":1750000000000}}"#;
        assert_eq!(get_claude_code_oauth_token(oauth).as_deref(), Some("acc-123"));

        let ll = r#"{"type":"claude-code-oauth-token","token":"sk-ll-token"}"#;
        assert_eq!(get_claude_code_oauth_token(ll).as_deref(), Some("sk-ll-token"));
        assert!(is_long_lived(ll));
        assert!(!is_long_lived(oauth));

        // bare `accessToken`
        let bare = r#"{"accessToken":"top-level"}"#;
        assert_eq!(get_claude_code_oauth_token(bare).as_deref(), Some("top-level"));

        // non-JSON → returned verbatim
        assert_eq!(get_claude_code_oauth_token("raw-token").as_deref(), Some("raw-token"));
        assert_eq!(get_claude_code_oauth_token(""), None);
    }

    #[test]
    fn token_normalization() {
        assert_eq!(normalize_claude_code_oauth_token("  sk-abc  "), "sk-abc");
        assert_eq!(
            normalize_claude_code_oauth_token("export CLAUDE_CODE_OAUTH_TOKEN=sk-xyz"),
            "sk-xyz"
        );
        assert_eq!(
            normalize_claude_code_oauth_token("CLAUDE_CODE_OAUTH_TOKEN=\"sk-quoted\""),
            "sk-quoted"
        );
        assert_eq!(normalize_claude_code_oauth_token("'sk-single'"), "sk-single");
    }

    #[test]
    fn expiry_skew() {
        let now = 1_000_000_000_000i64;
        // expiresAt within 60s → expired
        let raw = format!(r#"{{"claudeAiOauth":{{"expiresAt":{}}}}}"#, now + 30_000);
        assert!(is_expired_at(&raw, now));
        // comfortably in the future → not expired
        let raw2 = format!(r#"{{"claudeAiOauth":{{"expiresAt":{}}}}}"#, now + 120_000);
        assert!(!is_expired_at(&raw2, now));
        // long-lived → never expired
        assert!(!is_expired_at(r#"{"type":"claude-code-oauth-token","token":"x"}"#, now));
        // no expiresAt → not expired
        assert!(!is_expired_at(r#"{"claudeAiOauth":{}}"#, now));
    }

    #[test]
    fn usage_meters_snake_and_camel() {
        let snake = json!({
            "five_hour": {"utilization": 42.5, "resets_at": 1750000000000i64},
            "seven_day": {"utilization": 150.0}
        });
        let m = build_claude_usage_meters(&snake);
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].label, "5h");
        assert!((m[0].used_pct - 42.5).abs() < 1e-9);
        assert_eq!(m[0].reset_ms, Some(1750000000000));
        // over-100 utilization is clamped
        assert!((m[1].used_pct - 100.0).abs() < 1e-9);
        assert_eq!(m[1].reset_ms, None);

        let camel = json!({"fiveHour": {"utilization": 10}, "sevenDay": {"utilization": 20}});
        let m2 = build_claude_usage_meters(&camel);
        assert_eq!(m2.len(), 2);
        assert_eq!(m2[1].label, "7d");
    }

    #[test]
    fn reset_iso_and_ms() {
        assert_eq!(parse_flexible_reset_ms(&json!(1750000000000i64)), Some(1750000000000));
        let iso = parse_flexible_reset_ms(&json!("2026-07-06T00:00:00Z"));
        assert!(iso.is_some());
        assert!(parse_flexible_reset_ms(&json!(true)).is_none());
    }

    #[test]
    fn base_info_with_and_without_profile() {
        assert_eq!(
            claude_base_info(false, "max", "default", None),
            "subscription=max tier=default"
        );
        let prof = json!({
            "organization": {"organization_type": "business", "has_claude_max": true},
            "account": {"has_claude_max": false}
        });
        assert_eq!(
            claude_base_info(false, "max", "default", Some(&prof)),
            "subscription=max tier=default org=business has_max=yes"
        );
        assert_eq!(claude_base_info(true, "max", "default", None), "long-lived token");
    }
}
