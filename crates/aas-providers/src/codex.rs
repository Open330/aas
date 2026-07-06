//! Codex adapter. Mirrors asx `providers/codex.ts`.
//!
//! Native credential is `<CODEX_HOME>/auth.json` = `{email?, tokens:{access_token,
//! refresh_token,id_token,account_id}, account_id?}`. Refresh is delegated to the `codex`
//! binary via the `codex doctor --summary` trick (the profile home *is* that account's
//! `CODEX_HOME`, so the tokens refresh in place).

use crate::common::{add_account, http_client, now_ms, set_0600, set_active, value_display};
use crate::RefreshOutcome;
use aas_core::jwt::decode_jwt_claims;
use aas_core::naming::profile_home;
use aas_core::platform::codex_auth_path;
use aas_core::secure_store::{get_secret, set_secret};
use aas_core::usage::{Meter, Usage};
use serde_json::Value;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const PROVIDER: &str = "codex";

// ---------------------------------------------------------------------------
// Pure helpers (unit-tested).
// ---------------------------------------------------------------------------

/// asx `extractCodexEmail`: `email` else `jwt(id_token).email|email_address`.
pub(crate) fn extract_codex_email(auth_json: &str) -> Option<String> {
    let data: Value = serde_json::from_str(auth_json).ok()?;
    if let Some(e) = data.get("email").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        return Some(e.to_string());
    }
    let id_token = data.get("tokens").and_then(|t| t.get("id_token")).and_then(|v| v.as_str())?;
    let claims = decode_jwt_claims(id_token)?;
    claims
        .get("email")
        .and_then(|v| v.as_str())
        .or_else(|| claims.get("email_address").and_then(|v| v.as_str()))
        .map(String::from)
}

/// asx `extractPlanFromIdToken`: `(plan_type, active_until)` from the OpenAI auth claim.
pub(crate) fn extract_plan_from_id_token(id_token: &str) -> Option<(Option<String>, Option<String>)> {
    let claims = decode_jwt_claims(id_token)?;
    let auth = claims.get("https://api.openai.com/auth");
    let plan = auth
        .and_then(|a| a.get("chatgpt_plan_type"))
        .and_then(|v| v.as_str())
        .map(String::from);
    let until = auth
        .and_then(|a| a.get("chatgpt_subscription_active_until"))
        .map(value_display)
        .filter(|s| !s.is_empty());
    Some((plan, until))
}

/// asx `codexReset`: `reset_at*1000` or `now + reset_after_seconds*1000` (epoch ms).
pub(crate) fn codex_reset_ms(w: &Value, now: i64) -> Option<i64> {
    if let Some(ra) = w.get("reset_at").and_then(|v| v.as_f64()) {
        if ra != 0.0 {
            return Some((ra * 1000.0) as i64);
        }
    }
    if let Some(ras) = w.get("reset_after_seconds").and_then(|v| v.as_f64()) {
        if ras != 0.0 {
            return Some(now + (ras * 1000.0) as i64);
        }
    }
    None
}

/// The access/id-token `exp` (epoch ms) if decodable. asx `isExpired` uses this.
pub(crate) fn codex_access_exp_ms(raw: &str) -> Option<i64> {
    let v: Value = serde_json::from_str(raw).ok()?;
    let t = v.get("tokens")?;
    let tok = t
        .get("access_token")
        .and_then(|x| x.as_str())
        .or_else(|| t.get("id_token").and_then(|x| x.as_str()))?;
    let claims = decode_jwt_claims(tok)?;
    let exp = claims.get("exp")?.as_f64()?;
    Some((exp * 1000.0) as i64)
}

pub(crate) fn is_expired_at(raw: &str, now: i64) -> bool {
    codex_access_exp_ms(raw).map(|e| e < now + 60_000).unwrap_or(false)
}

fn codex_account_id(data: &Value) -> Option<String> {
    data.get("tokens")
        .and_then(|t| t.get("account_id"))
        .and_then(|v| v.as_str())
        .or_else(|| data.get("account_id").and_then(|v| v.as_str()))
        .map(String::from)
}

/// asx usage parse: `(headline, plan, meters)` from the wham/usage payload.
pub(crate) fn parse_codex_usage(
    payload: &Value,
    id_token: Option<&str>,
    now: i64,
) -> (String, Option<String>, Vec<Meter>) {
    let rl = payload.get("rate_limit").or_else(|| payload.get("rate_limits"));
    let primary = rl.and_then(|r| r.get("primary_window").or_else(|| r.get("primary")));
    let secondary = rl.and_then(|r| r.get("secondary_window").or_else(|| r.get("secondary")));

    let plan_type = payload
        .get("plan_type")
        .and_then(|v| v.as_str())
        .map(String::from)
        .or_else(|| rl.and_then(|r| r.get("plan_type")).and_then(|v| v.as_str()).map(String::from))
        .or_else(|| id_token.and_then(extract_plan_from_id_token).and_then(|(p, _)| p));

    let headline = match &plan_type {
        Some(p) => format!("plan={p}"),
        None => "subscription-based (5h reasoning windows)".to_string(),
    };

    let mut meters = Vec::new();
    if let Some(p) = primary {
        if let Some(up) = p.get("used_percent").and_then(|v| v.as_f64()) {
            meters.push(Meter::new("5h", up, codex_reset_ms(p, now)));
        }
    }
    if let Some(s) = secondary {
        if let Some(up) = s.get("used_percent").and_then(|v| v.as_f64()) {
            meters.push(Meter::new("7d", up, codex_reset_ms(s, now)));
        }
    }
    (headline, plan_type, meters)
}

// ---------------------------------------------------------------------------
// Native auth.json IO + refresh trick.
// ---------------------------------------------------------------------------

fn read_codex_auth_native() -> Option<String> {
    std::fs::read_to_string(codex_auth_path()).ok()
}

fn write_codex_auth_native(raw: &str) {
    let p = codex_auth_path();
    if let Some(parent) = p.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&p, raw);
    set_0600(&p);
}

/// Run `codex <args>` with `CODEX_HOME` set, killing it after `timeout_secs`. Returns whether
/// it exited successfully within the window.
fn run_codex_command(home: &Path, args: &[&str], timeout_secs: u64) -> bool {
    let mut child = match Command::new("codex")
        .args(args)
        .env("CODEX_HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) => {
                if start.elapsed().as_secs() >= timeout_secs {
                    let _ = child.kill();
                    let _ = child.wait();
                    return false;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(_) => return false,
        }
    }
}

/// asx `attemptCodexNativeRefresh` — the doctor trick (blocking; call via spawn_blocking).
fn codex_native_refresh_blocking(account: &str) -> bool {
    let Some(stored) = get_secret(PROVIDER, account) else {
        return false;
    };
    let home = profile_home(PROVIDER, account);
    let auth_path = home.join("auth.json");

    if !run_codex_command(&home, &["doctor", "--summary"], 20) {
        let _ = run_codex_command(&home, &["login", "status"], 8);
    }

    let Some(fresh) = std::fs::read_to_string(&auth_path).ok() else {
        return false;
    };
    let email = extract_codex_email(&fresh);
    let _ = add_account(PROVIDER, account, Some(account), email);
    if read_codex_auth_native().as_deref() == Some(stored.as_str()) {
        write_codex_auth_native(&fresh);
    }
    true
}

async fn attempt_codex_native_refresh(account: &str) -> bool {
    let account = account.to_string();
    tokio::task::spawn_blocking(move || codex_native_refresh_blocking(&account))
        .await
        .unwrap_or(false)
}

/// One usage fetch. Returns `(Usage, auth_fail)`; `auth_fail` triggers the refresh+retry path.
async fn fetch_codex_usage(token: &str, account_id: Option<&str>, data: &Value) -> (Usage, bool) {
    let client = http_client();
    let mut req = client
        .get("https://chatgpt.com/backend-api/wham/usage")
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/json")
        .header("User-Agent", "codex-cli");
    if let Some(aid) = account_id {
        req = req.header("ChatGPT-Account-Id", aid);
    }
    match req.send().await {
        Ok(res) => {
            let status = res.status().as_u16();
            if !(200..300).contains(&status) {
                let auth_fail = status == 401 || status == 403;
                return (Usage::error("codex", format!("live usage fetch failed: {status}")), auth_fail);
            }
            let payload = res.json::<Value>().await.unwrap_or(Value::Null);
            let id_token = data.get("tokens").and_then(|t| t.get("id_token")).and_then(|v| v.as_str());
            let (headline, plan, meters) = parse_codex_usage(&payload, id_token, now_ms());
            (Usage { headline, plan, meters, ..Default::default() }, false)
        }
        Err(_) => (Usage::error("codex", "live usage fetch failed: network error"), false),
    }
}

// ---------------------------------------------------------------------------
// Adapter methods.
// ---------------------------------------------------------------------------

pub(crate) async fn usage(account: &str) -> Usage {
    let Some(raw) = get_secret(PROVIDER, account) else {
        return Usage::error("codex", "No stored credential for this account.");
    };
    let data: Value = match serde_json::from_str(&raw) {
        Ok(d) => d,
        Err(_) => return Usage::error("codex", "stored credential is not valid JSON"),
    };

    let token = data
        .get("tokens")
        .and_then(|t| t.get("access_token"))
        .and_then(|v| v.as_str())
        .map(String::from);

    let Some(token) = token else {
        // No access token → best-effort plan info from the id_token.
        let id_token = data.get("tokens").and_then(|t| t.get("id_token")).and_then(|v| v.as_str());
        if let Some(idt) = id_token {
            if let Some((plan, until)) = extract_plan_from_id_token(idt) {
                let p = plan.clone().unwrap_or_else(|| "unknown".into());
                let u = until.unwrap_or_else(|| "unknown".into());
                return Usage {
                    headline: format!("plan={p} active_until={u}"),
                    plan,
                    ..Default::default()
                };
            }
        }
        return Usage {
            headline: "subscription-based (5h reasoning windows)".into(),
            ..Default::default()
        };
    };

    let account_id = codex_account_id(&data);
    let (mut result, auth_fail) = fetch_codex_usage(&token, account_id.as_deref(), &data).await;

    if auth_fail && attempt_codex_native_refresh(account).await {
        if let Some(r2) = get_secret(PROVIDER, account) {
            if let Ok(d2) = serde_json::from_str::<Value>(&r2) {
                if let Some(t2) =
                    d2.get("tokens").and_then(|t| t.get("access_token")).and_then(|v| v.as_str())
                {
                    let aid2 = codex_account_id(&d2);
                    let (retry, _) = fetch_codex_usage(t2, aid2.as_deref(), &d2).await;
                    result = retry;
                }
            }
        }
    }
    result
}

pub(crate) async fn current_credential() -> Option<String> {
    read_codex_auth_native()
}

pub(crate) async fn current_email() -> Option<String> {
    read_codex_auth_native().and_then(|c| extract_codex_email(&c))
}

pub(crate) async fn load_current(account: &str, label: Option<&str>) -> anyhow::Result<()> {
    let cur = read_codex_auth_native()
        .ok_or_else(|| anyhow::anyhow!("No ~/.codex/auth.json found. Login with `codex` first."))?;
    let email = extract_codex_email(&cur);
    set_secret(PROVIDER, account, &cur)?;
    add_account(PROVIDER, account, label, email)?;
    Ok(())
}

pub(crate) async fn switch_to(account: &str) -> anyhow::Result<()> {
    let s = get_secret(PROVIDER, account).ok_or_else(|| anyhow::anyhow!("Account not found"))?;
    write_codex_auth_native(&s);
    set_active(PROVIDER, account)?;
    Ok(())
}

pub(crate) async fn clear_current() -> anyhow::Result<()> {
    let _ = std::fs::remove_file(codex_auth_path());
    Ok(())
}

pub(crate) async fn is_expired(account: &str) -> bool {
    let Some(raw) = get_secret(PROVIDER, account) else {
        return false;
    };
    is_expired_at(&raw, now_ms())
}

pub(crate) async fn refresh(account: &str) -> RefreshOutcome {
    if attempt_codex_native_refresh(account).await {
        RefreshOutcome { ok: true, message: "refreshed via native codex".into(), needs_relogin: false }
    } else {
        RefreshOutcome { ok: false, message: "native refresh failed".into(), needs_relogin: true }
    }
}

pub(crate) fn login_command() -> Option<Vec<String>> {
    Some(vec!["codex".into(), "login".into()])
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use serde_json::json;

    fn make_jwt(payload: &Value) -> String {
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b"{\"alg\":\"none\"}");
        let body =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(serde_json::to_vec(payload).unwrap());
        format!("{header}.{body}.sig")
    }

    #[test]
    fn email_from_field_and_id_token() {
        let direct = r#"{"email":"a@b.com","tokens":{}}"#;
        assert_eq!(extract_codex_email(direct).as_deref(), Some("a@b.com"));

        let id = make_jwt(&json!({"email_address": "jwt@x.com"}));
        let auth = json!({"tokens": {"id_token": id}}).to_string();
        assert_eq!(extract_codex_email(&auth).as_deref(), Some("jwt@x.com"));

        assert_eq!(extract_codex_email("{}"), None);
    }

    #[test]
    fn plan_from_id_token() {
        let id = make_jwt(&json!({
            "https://api.openai.com/auth": {
                "chatgpt_plan_type": "plus",
                "chatgpt_subscription_active_until": 1750000000i64
            }
        }));
        let (plan, until) = extract_plan_from_id_token(&id).unwrap();
        assert_eq!(plan.as_deref(), Some("plus"));
        assert_eq!(until.as_deref(), Some("1750000000"));
    }

    #[test]
    fn reset_absolute_and_relative() {
        let now = 1_000_000_000_000i64;
        assert_eq!(codex_reset_ms(&json!({"reset_at": 1700}), now), Some(1_700_000));
        assert_eq!(
            codex_reset_ms(&json!({"reset_after_seconds": 3600}), now),
            Some(now + 3_600_000)
        );
        assert_eq!(codex_reset_ms(&json!({}), now), None);
    }

    #[test]
    fn expiry_from_access_token() {
        let now = 1_000_000_000_000i64; // ms
        let exp_soon = (now / 1000) + 30; // seconds, 30s out
        let tok = make_jwt(&json!({"exp": exp_soon}));
        let raw = json!({"tokens": {"access_token": tok}}).to_string();
        assert!(is_expired_at(&raw, now));

        let exp_far = (now / 1000) + 3600;
        let tok2 = make_jwt(&json!({"exp": exp_far}));
        let raw2 = json!({"tokens": {"access_token": tok2}}).to_string();
        assert!(!is_expired_at(&raw2, now));
    }

    #[test]
    fn usage_parse_snake_and_camel_windows() {
        let now = 1_000_000_000_000i64;
        let payload = json!({
            "plan_type": "pro",
            "rate_limit": {
                "primary_window": {"used_percent": 30.0, "reset_at": 1700},
                "secondary_window": {"used_percent": 5.0, "reset_after_seconds": 3600}
            }
        });
        let (headline, plan, meters) = parse_codex_usage(&payload, None, now);
        assert_eq!(headline, "plan=pro");
        assert_eq!(plan.as_deref(), Some("pro"));
        assert_eq!(meters.len(), 2);
        assert_eq!(meters[0].label, "5h");
        assert!((meters[0].used_pct - 30.0).abs() < 1e-9);
        assert_eq!(meters[0].reset_ms, Some(1_700_000));
        assert_eq!(meters[1].reset_ms, Some(now + 3_600_000));

        // alternate keys `rate_limits` / `primary` / `secondary`, no plan
        let alt = json!({
            "rate_limits": {"primary": {"used_percent": 12.0}, "secondary": {"used_percent": 3.0}}
        });
        let (h2, p2, m2) = parse_codex_usage(&alt, None, now);
        assert_eq!(h2, "subscription-based (5h reasoning windows)");
        assert!(p2.is_none());
        assert_eq!(m2.len(), 2);
    }

    #[test]
    fn plan_falls_back_to_id_token() {
        let now = 0;
        let id = make_jwt(&json!({
            "https://api.openai.com/auth": {"chatgpt_plan_type": "team"}
        }));
        let payload = json!({"rate_limit": {}});
        let (headline, plan, _) = parse_codex_usage(&payload, Some(&id), now);
        assert_eq!(headline, "plan=team");
        assert_eq!(plan.as_deref(), Some("team"));
    }
}
