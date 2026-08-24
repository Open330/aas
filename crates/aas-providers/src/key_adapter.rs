//! Grok + Z.AI adapter. Mirrors asx `providers/key-adapter.ts` (`createKeyAdapter`).
//!
//! Z.AI is an API-key provider. Grok additionally understands native OIDC credentials in
//! `~/.grok/auth.json`, including access/refresh-token rotation.

use crate::common::{http_client, num_alt, set_active, store_account_secret, value_display};
use crate::RefreshOutcome;
use aas_core::jwt::decode_jwt_claims;
use aas_core::model::ProfileType;
use aas_core::platform::grok_auth_path;
use aas_core::secure_store::{get_secret, set_secret, write_restricted_file};
use aas_core::store::AccountStore;
use aas_core::usage::{Meter, Usage};
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// Provider table. Everything that differs between the API-key providers lives here, so adding a
// third-party provider is one entry rather than another arm in every function below.
// ---------------------------------------------------------------------------

/// How a key is presented to a provider's REST API.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum AuthStyle {
    /// `Authorization: Bearer <key>`.
    Bearer,
    /// `Authorization: <key>` — Z.AI's quota endpoint rejects the `Bearer` prefix.
    RawKey,
}

impl AuthStyle {
    fn header(self, key: &str) -> String {
        match self {
            AuthStyle::Bearer => format!("Bearer {key}"),
            AuthStyle::RawKey => key.to_string(),
        }
    }
}

/// One selectable API host. Kimi runs several platforms whose keys are NOT interchangeable (a key
/// from one returns 401 on the others), so the host is recorded per account. Single-host providers
/// simply have one entry.
///
/// Quota reporting hangs off the endpoint rather than the provider: Kimi's per-token platform
/// serves a balance, while its subscription console serves no quota API at all.
#[derive(Clone, Copy, Debug)]
pub struct KeyEndpoint {
    pub id: &'static str,
    pub label: &'static str,
    pub base: &'static str,
    quota: QuotaStyle,
}

/// How live quota is reported.
#[derive(Clone, Copy, Debug)]
enum QuotaStyle {
    /// Grok's native multi-endpoint + JWT introspection.
    GrokNative,
    /// Percent-of-quota endpoint. Absolute, because Z.AI serves it off a different host than its API.
    PercentQuota { url: &'static str, auth: AuthStyle },
    /// Currency balance relative to the account's endpoint (Kimi `/v1/users/me/balance`). There is
    /// no denominator, so this renders as a note rather than a percentage meter.
    CurrencyBalance { path: &'static str, auth: AuthStyle },
    /// The host exposes identity but no quota (Kimi Code Console). Report the plan the account is
    /// on and say plainly that no quota is published, rather than failing a call that cannot work.
    IdentityOnly { path: &'static str, auth: AuthStyle },
}

/// Validation call issued before a pasted key is stored.
#[derive(Clone, Copy, Debug)]
struct KeyTest {
    path: &'static str,
    auth: AuthStyle,
}

pub(crate) struct KeyProviderSpec {
    pub id: &'static str,
    pub display: &'static str,
    /// Env var names checked after `<ID>_API_KEY` / `<ID>_KEY`.
    extra_env: &'static [&'static str],
    /// Env vars exported when this provider's account becomes active.
    activate_env: &'static [&'static str],
    /// Selectable API hosts; the first is the default for a new account.
    pub endpoints: &'static [KeyEndpoint],
    key_test: Option<KeyTest>,
    /// Provider keeps native OAuth credentials on disk (Grok's `~/.grok/auth.json`).
    native_oidc: bool,
    /// Native login argv, when the provider ships its own login command.
    login_command: Option<&'static [&'static str]>,
}

impl KeyProviderSpec {
    /// Whether a pasted API key is the login method (as opposed to a native OAuth command).
    fn takes_api_key_login(&self) -> bool {
        self.key_test.is_some()
    }
}

const KEY_PROVIDERS: &[KeyProviderSpec] = &[
    KeyProviderSpec {
        id: "grok",
        display: "Grok",
        extra_env: &["XAI_API_KEY"],
        activate_env: &["XAI_API_KEY"],
        endpoints: &[KeyEndpoint {
            id: "xai",
            label: "api.x.ai",
            base: "https://api.x.ai",
            quota: QuotaStyle::GrokNative,
        }],
        key_test: None,
        native_oidc: true,
        login_command: Some(&["grok", "login"]),
    },
    KeyProviderSpec {
        id: "zai",
        display: "Z.AI",
        extra_env: &[],
        activate_env: &["ZAI_API_KEY"],
        endpoints: &[KeyEndpoint {
            id: "coding",
            label: "api.z.ai coding plan",
            base: "https://api.z.ai/api/coding/paas/v4",
            // Z.AI quota uses `Authorization: <raw key>` with NO `Bearer` prefix.
            quota: QuotaStyle::PercentQuota {
                url: "https://api.z.ai/api/monitor/usage/quota/limit",
                auth: AuthStyle::RawKey,
            },
        }],
        key_test: Some(KeyTest {
            path: "/models",
            auth: AuthStyle::Bearer,
        }),
        native_oidc: false,
        login_command: None,
    },
    KeyProviderSpec {
        id: "kimi",
        display: "Kimi",
        extra_env: &["MOONSHOT_API_KEY", "MOONSHOT_KEY"],
        activate_env: &["KIMI_API_KEY", "MOONSHOT_API_KEY"],
        // Keys are platform-scoped; `aas login kimi --endpoint <id>` picks one. Restricting the
        // stored value to this list means a tampered accounts.json cannot redirect a key to an
        // attacker-controlled host.
        endpoints: &[
            KeyEndpoint {
                id: "moonshot-ai",
                label: "platform.kimi.ai — per-token billing",
                base: "https://api.moonshot.ai",
                quota: QuotaStyle::CurrencyBalance {
                    path: "/v1/users/me/balance",
                    auth: AuthStyle::Bearer,
                },
            },
            KeyEndpoint {
                id: "kimi-code",
                label: "Kimi Code Console — subscription",
                base: "https://api.kimi.com/coding",
                // Verified against the live host: every balance/quota/usage path 404s, while
                // `/v1/me` returns the account's plan tier. Report that instead of failing.
                quota: QuotaStyle::IdentityOnly {
                    path: "/v1/me",
                    auth: AuthStyle::Bearer,
                },
            },
            KeyEndpoint {
                id: "moonshot-cn",
                label: "api.moonshot.cn — mainland China",
                base: "https://api.moonshot.cn",
                quota: QuotaStyle::CurrencyBalance {
                    path: "/v1/users/me/balance",
                    auth: AuthStyle::Bearer,
                },
            },
        ],
        key_test: Some(KeyTest {
            path: "/v1/models",
            auth: AuthStyle::Bearer,
        }),
        native_oidc: false,
        login_command: None,
    },
];

/// The table entry for a provider id (accepts asx aliases such as `moonshot` and `xai`).
pub(crate) fn spec(provider: &str) -> Option<&'static KeyProviderSpec> {
    let id = aas_core::naming::normalize_provider_key(provider);
    KEY_PROVIDERS.iter().find(|entry| entry.id == id)
}

fn require_spec(provider: &str) -> anyhow::Result<&'static KeyProviderSpec> {
    spec(provider).ok_or_else(|| anyhow::anyhow!("'{provider}' is not an API-key provider"))
}

/// Resolve an endpoint id (`--endpoint kimi-code`) or a full base URL to a table entry.
pub(crate) fn resolve_endpoint(
    spec: &'static KeyProviderSpec,
    requested: Option<&str>,
) -> anyhow::Result<&'static KeyEndpoint> {
    let Some(requested) = requested.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(&spec.endpoints[0]);
    };
    spec.endpoints
        .iter()
        .find(|entry| entry.id == requested || entry.base == requested)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "unknown {} endpoint '{requested}'; expected one of: {}",
                spec.display,
                spec.endpoints
                    .iter()
                    .map(|entry| entry.id)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
}

/// The API base for one account: its recorded endpoint, else the provider default. Returns the
/// `&'static` table value rather than the stored string, so a tampered store cannot point a
/// credential at an arbitrary host.
pub(crate) fn account_endpoint(
    spec: &'static KeyProviderSpec,
    account: &str,
) -> anyhow::Result<&'static KeyEndpoint> {
    let stored = AccountStore::open_default()
        .get(spec.id, account)
        .ok()
        .flatten()
        .and_then(|record| record.endpoint().map(str::to_string));
    match stored {
        Some(value) => resolve_endpoint(spec, Some(&value)).map_err(|_| {
            anyhow::anyhow!(
                "{}/{account} names endpoint '{value}', which is not a known {} host",
                spec.id,
                spec.display
            )
        }),
        None => Ok(&spec.endpoints[0]),
    }
}

fn join_url(base: &str, path: &str) -> String {
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

// ---------------------------------------------------------------------------
// Pure helpers (unit-tested).
// ---------------------------------------------------------------------------

/// asx `getEnvKey`: `<PFX>_API_KEY`, then `<PFX>_KEY`, then the provider's table aliases
/// (`XAI_API_KEY` for Grok, `MOONSHOT_API_KEY` for Kimi).
fn get_env_key(provider: &str) -> Option<String> {
    let pfx = aas_core::naming::normalize_provider_key(provider).to_uppercase();
    let read = |name: &str| std::env::var(name).ok().filter(|value| !value.is_empty());
    read(&format!("{pfx}_API_KEY"))
        .or_else(|| read(&format!("{pfx}_KEY")))
        .or_else(|| spec(provider)?.extra_env.iter().find_map(|name| read(name)))
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

#[derive(Clone, Debug)]
struct GrokStoredEntry {
    document: Value,
    wrapper_key: Option<String>,
    entry: Value,
}

fn grok_stored_entry(raw: &str) -> Option<GrokStoredEntry> {
    let document: Value = serde_json::from_str(raw).ok()?;
    let object = document.as_object()?;
    if object.get("key").and_then(Value::as_str).is_some() {
        return Some(GrokStoredEntry {
            document: document.clone(),
            wrapper_key: None,
            entry: document,
        });
    }
    object.iter().find_map(|(key, value)| {
        value
            .get("key")
            .and_then(Value::as_str)
            .map(|_| GrokStoredEntry {
                document: document.clone(),
                wrapper_key: Some(key.clone()),
                entry: value.clone(),
            })
    })
}

fn update_grok_document(stored: &GrokStoredEntry, updated_entry: Value) -> Value {
    match &stored.wrapper_key {
        Some(key) => {
            let mut document = stored.document.clone();
            if let Some(object) = document.as_object_mut() {
                object.insert(key.clone(), updated_entry);
            }
            document
        }
        None => updated_entry,
    }
}

struct GrokRefresh {
    access_token: String,
    refresh_token: String,
    expires_in: i64,
}

fn grok_token_endpoint(entry: &Value) -> Result<reqwest::Url, String> {
    let issuer = entry
        .get("oidc_issuer")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .unwrap_or("https://auth.x.ai");
    let mut url = reqwest::Url::parse(issuer)
        .map_err(|error| format!("invalid Grok OIDC issuer: {error}"))?;
    let valid_origin = url.scheme() == "https"
        && url.host_str() == Some("auth.x.ai")
        && url.port_or_known_default() == Some(443)
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && matches!(url.path(), "" | "/");
    if !valid_origin {
        return Err("refusing untrusted Grok OIDC issuer; expected https://auth.x.ai".into());
    }
    url.set_path("/oauth2/token");
    Ok(url)
}

async fn grok_refresh_grant(entry: &Value) -> Result<GrokRefresh, String> {
    let refresh_token = entry
        .get("refresh_token")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "no refresh token stored".to_string())?;
    let client_id = entry
        .get("oidc_client_id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "no OIDC client id stored".to_string())?;
    let endpoint = grok_token_endpoint(entry)?;
    let version = aas_core::platform::grok_version();
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| format!("could not build Grok refresh client: {error}"))?;
    let response = client
        .post(endpoint)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("x-grok-client-version", &version)
        .header("x-grok-client-surface", "grok-build")
        .header("x-grok-client-identifier", "grok-shell")
        .header(
            reqwest::header::USER_AGENT,
            format!(
                "grok-shell/{version} ({}; {})",
                std::env::consts::OS,
                std::env::consts::ARCH
            ),
        )
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", client_id),
        ])
        .send()
        .await
        .map_err(|error| format!("refresh network error: {error}"))?;
    let status = response.status();
    if !status.is_success() {
        let detail: String = response
            .text()
            .await
            .unwrap_or_default()
            .chars()
            .take(240)
            .collect();
        return Err(format!(
            "refresh endpoint returned HTTP {}{}",
            status.as_u16(),
            if detail.is_empty() {
                String::new()
            } else {
                format!(": {detail}")
            }
        ));
    }
    let payload: Value = response
        .json()
        .await
        .map_err(|error| format!("refresh endpoint returned invalid JSON: {error}"))?;
    let access_token = payload
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "refresh response did not contain access_token".to_string())?
        .to_string();
    Ok(GrokRefresh {
        access_token,
        refresh_token: payload
            .get("refresh_token")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .unwrap_or(refresh_token)
            .to_string(),
        expires_in: payload
            .get("expires_in")
            .and_then(Value::as_i64)
            .unwrap_or(21_600),
    })
}

pub(crate) fn is_grok_expired(account: &str) -> bool {
    let Some(raw) = get_secret("grok", account) else {
        return false;
    };
    let Some(stored) = grok_stored_entry(&raw) else {
        return false;
    };
    if stored
        .entry
        .get("refresh_token")
        .and_then(Value::as_str)
        .is_none()
    {
        return false;
    }
    let Some(exp) = stored
        .entry
        .get("key")
        .and_then(Value::as_str)
        .and_then(decode_jwt_claims)
        .and_then(|claims| claims.get("exp").and_then(Value::as_i64))
    else {
        return false;
    };
    exp * 1000 < chrono::Utc::now().timestamp_millis() + 60_000
}

pub(crate) async fn refresh_grok(account: &str) -> RefreshOutcome {
    let Some(raw) = get_secret("grok", account) else {
        return RefreshOutcome {
            ok: false,
            message: "no stored credential".into(),
            needs_relogin: false,
        };
    };
    let Some(stored) = grok_stored_entry(&raw) else {
        return RefreshOutcome {
            ok: false,
            message: "no refresh token stored".into(),
            needs_relogin: true,
        };
    };
    if stored
        .entry
        .get("refresh_token")
        .and_then(Value::as_str)
        .is_none()
    {
        return RefreshOutcome {
            ok: false,
            message: "no refresh token stored".into(),
            needs_relogin: true,
        };
    }
    let refreshed = match grok_refresh_grant(&stored.entry).await {
        Ok(refreshed) => refreshed,
        Err(message) => {
            return RefreshOutcome {
                ok: false,
                message,
                needs_relogin: true,
            }
        }
    };
    let expires_at = chrono::Utc::now()
        .checked_add_signed(chrono::Duration::seconds(refreshed.expires_in.max(0)))
        .unwrap_or_else(chrono::Utc::now)
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let mut updated_entry = stored.entry.clone();
    let Some(object) = updated_entry.as_object_mut() else {
        return RefreshOutcome {
            ok: false,
            message: "stored Grok OIDC entry is malformed".into(),
            needs_relogin: true,
        };
    };
    object.insert("key".into(), Value::String(refreshed.access_token));
    object.insert(
        "refresh_token".into(),
        Value::String(refreshed.refresh_token),
    );
    object.insert("expires_at".into(), Value::String(expires_at.clone()));
    let new_raw = update_grok_document(&stored, updated_entry).to_string();
    if let Err(error) = set_secret("grok", account, &new_raw) {
        return RefreshOutcome {
            ok: false,
            message: format!("could not store refreshed credential: {error}"),
            needs_relogin: false,
        };
    }
    aas_core::usage_cache::clear(&format!("grok/{account}"));

    let is_system = AccountStore::open_default()
        .get("grok", account)
        .ok()
        .flatten()
        .and_then(|record| record.profile_type)
        == Some(ProfileType::System);
    if is_system {
        if let Err(error) = write_grok_auth(&new_raw) {
            let _ = set_secret("grok", account, &raw);
            return RefreshOutcome {
                ok: false,
                message: format!("refreshed vault but native sync failed; rolled back: {error}"),
                needs_relogin: false,
            };
        }
    }
    RefreshOutcome {
        ok: true,
        message: format!(
            "refreshed (expires {expires_at}){}",
            if is_system { " [native synced]" } else { "" }
        ),
        needs_relogin: false,
    }
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

/// Percent-of-quota usage (Z.AI). The endpoint is absolute because Z.AI serves quota off a
/// different host than its API.
async fn percent_quota_usage(
    spec: &'static KeyProviderSpec,
    account: &str,
    url: &str,
    auth: AuthStyle,
) -> Usage {
    let Some(key) = get_secret(spec.id, account) else {
        return Usage {
            headline: "API key (no live quota data)".into(),
            ..Default::default()
        };
    };
    let res = http_client()
        .get(url)
        .header("Authorization", auth.header(&key))
        .header("Accept-Language", "en-US,en")
        .header("Content-Type", "application/json")
        .send()
        .await;
    match res {
        Ok(res) => {
            let status = res.status().as_u16();
            if !(200..300).contains(&status) {
                return Usage::error(
                    spec.id,
                    format!("{} usage fetch failed: {status}", spec.display),
                );
            }
            let payload = res.json::<Value>().await.unwrap_or(Value::Null);
            match parse_zai_quota_used_pct(&payload) {
                Some(used) => Usage {
                    headline: spec.display.into(),
                    meters: vec![Meter::new("5h", used.clamp(0.0, 100.0), None)],
                    ..Default::default()
                },
                None => Usage {
                    headline: spec.display.into(),
                    error: Some("no token quota returned".into()),
                    ..Default::default()
                },
            }
        }
        Err(_) => Usage::error(
            spec.id,
            format!("{} usage fetch: network error", spec.display),
        ),
    }
}

/// Kimi's balance payload: `{"code":0,"data":{"available_balance":..,"voucher_balance":..,
/// "cash_balance":..},"status":true}`. Returns the three figures when the call succeeded.
pub(crate) fn parse_currency_balance(payload: &Value) -> Option<(f64, Option<f64>, Option<f64>)> {
    let data = payload.get("data").unwrap_or(payload);
    let available = data.get("available_balance").and_then(Value::as_f64)?;
    Some((
        available,
        data.get("cash_balance").and_then(Value::as_f64),
        data.get("voucher_balance").and_then(Value::as_f64),
    ))
}

/// Render a currency balance. `Meter` is percentage-only and a balance has no denominator, so this
/// reports the figures as notes instead of inventing a full-scale meter.
pub(crate) fn currency_balance_usage_from(
    spec: &'static KeyProviderSpec,
    endpoint_label: &str,
    payload: &Value,
) -> Usage {
    match parse_currency_balance(payload) {
        Some((available, cash, voucher)) => {
            let mut notes = vec![format!("available balance {available:.2}")];
            if let (Some(cash), Some(voucher)) = (cash, voucher) {
                notes.push(format!("cash {cash:.2} · voucher {voucher:.2}"));
            }
            if available <= 0.0 {
                notes.push("balance exhausted — requests will be rejected".into());
            }
            Usage {
                headline: spec.display.into(),
                plan: Some(endpoint_label.to_string()),
                notes,
                ..Default::default()
            }
        }
        None => Usage {
            headline: spec.display.into(),
            plan: Some(endpoint_label.to_string()),
            error: Some("no balance returned".into()),
            ..Default::default()
        },
    }
}

async fn currency_balance_usage(
    spec: &'static KeyProviderSpec,
    endpoint: &'static KeyEndpoint,
    account: &str,
    path: &str,
    auth: AuthStyle,
) -> Usage {
    let Some(key) = get_secret(spec.id, account) else {
        return Usage {
            headline: "API key (no live quota data)".into(),
            ..Default::default()
        };
    };
    let res = http_client()
        .get(join_url(endpoint.base, path))
        .header("Authorization", auth.header(&key))
        .send()
        .await;
    match res {
        Ok(res) => {
            let status = res.status().as_u16();
            if !(200..300).contains(&status) {
                // A key used against the wrong Kimi platform fails exactly here.
                let hint = if status == 401 {
                    format!(" (is this key issued by {}?)", endpoint.label)
                } else {
                    String::new()
                };
                return Usage::error(
                    spec.id,
                    format!("{} usage fetch failed: {status}{hint}", spec.display),
                );
            }
            let payload = res.json::<Value>().await.unwrap_or(Value::Null);
            currency_balance_usage_from(spec, endpoint.label, &payload)
        }
        Err(_) => Usage::error(
            spec.id,
            format!("{} usage fetch: network error", spec.display),
        ),
    }
}

/// Kimi Code Console's `/v1/me`: identity plus the account's plan tier. There is no quota field.
pub(crate) fn parse_identity_plan(payload: &Value) -> Option<String> {
    let data = payload.get("data").unwrap_or(payload);
    data.get("user_level_name")
        .or_else(|| data.get("plan"))
        .or_else(|| data.get("subscription"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// Report what the host actually publishes. This endpoint has no quota API at all — every
/// balance/quota/usage path returns 404 — so saying so is the correct answer, not an error.
pub(crate) fn identity_only_usage_from(
    spec: &'static KeyProviderSpec,
    endpoint: &'static KeyEndpoint,
    payload: &Value,
) -> Usage {
    Usage {
        headline: spec.display.into(),
        plan: parse_identity_plan(payload).or_else(|| Some(endpoint.label.to_string())),
        notes: vec!["subscription plan — this host publishes no quota endpoint".into()],
        ..Default::default()
    }
}

async fn identity_only_usage(
    spec: &'static KeyProviderSpec,
    endpoint: &'static KeyEndpoint,
    account: &str,
    path: &str,
    auth: AuthStyle,
) -> Usage {
    let Some(key) = get_secret(spec.id, account) else {
        return Usage {
            headline: "API key (no live quota data)".into(),
            ..Default::default()
        };
    };
    match http_client()
        .get(join_url(endpoint.base, path))
        .header("Authorization", auth.header(&key))
        .send()
        .await
    {
        Ok(res) if res.status().is_success() => {
            let payload = res.json::<Value>().await.unwrap_or(Value::Null);
            identity_only_usage_from(spec, endpoint, &payload)
        }
        // Identity is a nicety here; failing to fetch it must not look like a quota failure.
        Ok(res) => {
            let status = res.status().as_u16();
            let hint = if status == 401 {
                format!(" (is this key issued by {}?)", endpoint.label)
            } else {
                String::new()
            };
            Usage {
                headline: spec.display.into(),
                plan: Some(endpoint.label.to_string()),
                notes: vec![format!(
                    "subscription plan — no quota endpoint; identity lookup returned {status}{hint}"
                )],
                ..Default::default()
            }
        }
        Err(_) => Usage {
            headline: spec.display.into(),
            plan: Some(endpoint.label.to_string()),
            notes: vec!["subscription plan — no quota endpoint; identity lookup failed".into()],
            ..Default::default()
        },
    }
}

/// Validate a pasted key against the account's endpoint before it is stored.
async fn test_key(
    spec: &'static KeyProviderSpec,
    endpoint: &'static KeyEndpoint,
    key: &str,
) -> anyhow::Result<()> {
    let Some(test) = spec.key_test else {
        return Ok(());
    };
    let res = http_client()
        .get(join_url(endpoint.base, test.path))
        .header("Authorization", test.auth.header(key))
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("{} endpoint test failed: {e}", spec.display))?;
    if !res.status().is_success() {
        let status = res.status();
        let detail: String = res
            .text()
            .await
            .unwrap_or_default()
            .chars()
            .take(240)
            .collect();
        // Keys are platform-scoped; the most common cause of a 401 here is the wrong platform.
        let hint = if status.as_u16() == 401 && spec.endpoints.len() > 1 {
            format!(
                "; this key must be issued by {} — pick another with --endpoint <{}>",
                endpoint.label,
                spec.endpoints
                    .iter()
                    .map(|entry| entry.id)
                    .collect::<Vec<_>>()
                    .join("|")
            )
        } else {
            String::new()
        };
        anyhow::bail!(
            "{} endpoint test failed ({}{}{}){hint}",
            spec.display,
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
    let Some(spec) = spec(provider) else {
        return Usage::error(provider, format!("'{provider}' is not an API-key provider"));
    };
    let endpoint = match account_endpoint(spec, account) {
        Ok(endpoint) => endpoint,
        Err(error) => return Usage::error(spec.id, error.to_string()),
    };
    match endpoint.quota {
        QuotaStyle::GrokNative => grok_usage(account).await,
        QuotaStyle::PercentQuota { url, auth } => {
            percent_quota_usage(spec, account, url, auth).await
        }
        QuotaStyle::CurrencyBalance { path, auth } => {
            currency_balance_usage(spec, endpoint, account, path, auth).await
        }
        QuotaStyle::IdentityOnly { path, auth } => {
            identity_only_usage(spec, endpoint, account, path, auth).await
        }
    }
}

pub(crate) async fn current_credential(provider: &str) -> Option<String> {
    if spec(provider)?.native_oidc {
        return get_grok_auth_file().map(|a| a.to_string());
    }
    get_env_key(provider)
}

pub(crate) async fn current_email(provider: &str) -> Option<String> {
    spec(provider)?
        .native_oidc
        .then(try_extract_grok_email)
        .flatten()
}

pub(crate) async fn load_current(
    provider: &str,
    account: &str,
    label: Option<&str>,
) -> anyhow::Result<()> {
    let spec = require_spec(provider)?;
    let mut val = get_env_key(provider);
    if val.is_none() && spec.native_oidc {
        if let Some(auth) = get_grok_auth_file() {
            val = Some(auth.to_string());
        }
    }
    let val = val.ok_or_else(|| {
        anyhow::anyhow!(
            "No live {} credential found. Set the provider API key or log in first.",
            spec.display
        )
    })?;
    let email = spec.native_oidc.then(try_extract_grok_email).flatten();
    store_account_secret(spec.id, account, label, email, &val)?;
    Ok(())
}

pub(crate) async fn switch_to(provider: &str, account: &str) -> anyhow::Result<()> {
    let spec = require_spec(provider)?;
    let v = get_secret(spec.id, account)
        .ok_or_else(|| anyhow::anyhow!("No key for {}/{account}", spec.id))?;
    let previous_env: Vec<(&str, Option<std::ffi::OsString>)> = spec
        .activate_env
        .iter()
        .map(|name| (*name, std::env::var_os(name)))
        .collect();
    let previous_grok = spec.native_oidc.then(get_grok_auth_file).flatten();

    let exported = if spec.native_oidc {
        write_grok_auth(&v)?;
        grok_bearer(&v)
    } else {
        v.clone()
    };
    for name in spec.activate_env {
        std::env::set_var(name, &exported);
    }

    if let Err(error) = set_active(spec.id, account) {
        for (name, previous) in previous_env {
            match previous {
                Some(previous) => std::env::set_var(name, previous),
                None => std::env::remove_var(name),
            }
        }
        let rollback = if spec.native_oidc {
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
            "could not update active {} marker: {error}; native rollback={rollback:?}",
            spec.id
        );
    }
    Ok(())
}

pub(crate) async fn clear_current(provider: &str) -> anyhow::Result<()> {
    if require_spec(provider)?.native_oidc {
        match std::fs::remove_file(grok_auth_path()) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

pub(crate) fn login_command(provider: &str) -> Option<Vec<String>> {
    let argv = spec(provider)?.login_command?;
    Some(argv.iter().map(|s| s.to_string()).collect())
}

/// Whether `aas login <provider>` should prompt for an API key rather than shelling out.
pub(crate) fn takes_api_key_login(provider: &str) -> bool {
    spec(provider).is_some_and(KeyProviderSpec::takes_api_key_login)
}

/// The endpoints a provider's login may choose between (empty when it has no table entry).
pub(crate) fn endpoints(provider: &str) -> &'static [KeyEndpoint] {
    spec(provider).map(|entry| entry.endpoints).unwrap_or(&[])
}

/// asx key-adapter `login`: validate the key against the selected endpoint, then store + activate.
/// `endpoint` names a table entry (`--endpoint kimi-code`); `None` uses the provider default.
pub(crate) async fn validate_and_store_key(
    provider: &str,
    account: &str,
    key: &str,
    endpoint: Option<&str>,
) -> anyhow::Result<()> {
    let spec = require_spec(provider)?;
    if !spec.takes_api_key_login() {
        anyhow::bail!("{} does not support API-key login", spec.display);
    }
    let key = key.trim();
    if key.is_empty() {
        anyhow::bail!("No {} API key provided.", spec.display);
    }
    let endpoint = resolve_endpoint(spec, endpoint)?;
    test_key(spec, endpoint, key).await?;
    store_account_secret(spec.id, account, None, None, key)?;
    // Record which platform issued this key. Written after the credential so a failed store never
    // leaves an endpoint pointing at a credential that does not exist.
    if spec.endpoints.len() > 1 {
        let store = AccountStore::open_default();
        if let Some(mut record) = store.get(spec.id, account)? {
            record.set_endpoint(Some(endpoint.base));
            store.add(record)?;
        }
    }
    set_active(spec.id, account)?;
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
    fn provider_table_covers_every_key_provider() {
        for id in ["grok", "zai", "kimi"] {
            assert!(spec(id).is_some(), "{id} missing from the table");
        }
        // Aliases resolve to the same entry.
        assert_eq!(spec("moonshot").map(|s| s.id), Some("kimi"));
        assert_eq!(spec("xai").map(|s| s.id), Some("grok"));
        // Providers with their own adapters are not key providers.
        assert!(spec("claude").is_none());
        assert!(spec("codex").is_none());

        // Grok logs in natively; the others take a pasted key.
        assert!(!takes_api_key_login("grok"));
        assert!(takes_api_key_login("zai"));
        assert!(takes_api_key_login("kimi"));
        assert_eq!(
            login_command("grok"),
            Some(vec!["grok".into(), "login".into()])
        );
        assert_eq!(login_command("kimi"), None);
    }

    #[test]
    fn endpoint_resolution_accepts_ids_and_bases_but_rejects_foreign_hosts() {
        let kimi = spec("kimi").unwrap();
        assert_eq!(resolve_endpoint(kimi, None).unwrap().id, "moonshot-ai");
        assert_eq!(
            resolve_endpoint(kimi, Some("kimi-code")).unwrap().id,
            "kimi-code"
        );
        assert_eq!(
            resolve_endpoint(kimi, Some("https://api.moonshot.cn"))
                .unwrap()
                .id,
            "moonshot-cn"
        );
        // A blank value is not a selection.
        assert_eq!(
            resolve_endpoint(kimi, Some("  ")).unwrap().id,
            "moonshot-ai"
        );
        // An arbitrary host must never become a credential destination.
        assert!(resolve_endpoint(kimi, Some("https://evil.example")).is_err());
        assert!(resolve_endpoint(kimi, Some("moonshot-ai.evil")).is_err());

        // Single-host providers still resolve to their one entry.
        let zai = spec("zai").unwrap();
        assert_eq!(resolve_endpoint(zai, None).unwrap().id, "coding");
        assert!(resolve_endpoint(zai, Some("kimi-code")).is_err());
    }

    #[test]
    fn auth_style_matches_each_provider_contract() {
        // Z.AI's quota endpoint rejects a `Bearer` prefix; everything else expects one.
        assert_eq!(AuthStyle::RawKey.header("k"), "k");
        assert_eq!(AuthStyle::Bearer.header("k"), "Bearer k");
    }

    #[test]
    fn join_url_does_not_double_or_drop_separators() {
        assert_eq!(join_url("https://h", "/v1/x"), "https://h/v1/x");
        assert_eq!(join_url("https://h/", "/v1/x"), "https://h/v1/x");
        assert_eq!(
            join_url("https://h/coding/", "v1/x"),
            "https://h/coding/v1/x"
        );
    }

    #[test]
    fn quota_strategy_is_per_endpoint_not_per_provider() {
        let kimi = spec("kimi").unwrap();
        let by_id = |id: &str| kimi.endpoints.iter().find(|e| e.id == id).unwrap();
        // Verified against the live hosts: the per-token platform serves a balance, the
        // subscription console 404s every quota path but answers /v1/me.
        assert!(matches!(
            by_id("moonshot-ai").quota,
            QuotaStyle::CurrencyBalance { .. }
        ));
        assert!(matches!(
            by_id("kimi-code").quota,
            QuotaStyle::IdentityOnly { .. }
        ));
        assert!(matches!(
            by_id("moonshot-cn").quota,
            QuotaStyle::CurrencyBalance { .. }
        ));
        assert!(matches!(
            spec("zai").unwrap().endpoints[0].quota,
            QuotaStyle::PercentQuota { .. }
        ));
        assert!(matches!(
            spec("grok").unwrap().endpoints[0].quota,
            QuotaStyle::GrokNative
        ));
    }

    #[test]
    fn identity_only_host_reports_its_plan_instead_of_a_quota_failure() {
        let kimi = spec("kimi").unwrap();
        let endpoint = kimi.endpoints.iter().find(|e| e.id == "kimi-code").unwrap();
        // Shape taken from the live /v1/me response.
        let payload = json!({
            "user_id": "x", "status": "USER_STATUS_NORMAL", "region": "REGION_OVERSEA",
            "user_level": 30, "user_level_name": "Vivace"
        });
        assert_eq!(parse_identity_plan(&payload).as_deref(), Some("Vivace"));

        let usage = identity_only_usage_from(kimi, endpoint, &payload);
        assert_eq!(usage.headline, "Kimi");
        assert_eq!(usage.plan.as_deref(), Some("Vivace"));
        // No quota exists on this host, so there must be no meter and no error.
        assert!(usage.meters.is_empty());
        assert!(usage.error.is_none());
        assert!(usage.notes.iter().any(|n| n.contains("no quota endpoint")));

        // Missing plan falls back to the host label rather than going blank.
        let usage = identity_only_usage_from(kimi, endpoint, &json!({}));
        assert_eq!(usage.plan.as_deref(), Some(endpoint.label));
        assert!(usage.error.is_none());
    }

    #[test]
    fn currency_balance_reports_figures_without_faking_a_meter() {
        let kimi = spec("kimi").unwrap();
        let payload = json!({
            "code": 0,
            "data": { "available_balance": 49.58894, "voucher_balance": 46.58893, "cash_balance": 3.00001 },
            "status": true
        });
        assert_eq!(
            parse_currency_balance(&payload).map(|(a, _, _)| a),
            Some(49.58894)
        );
        let usage = currency_balance_usage_from(kimi, "platform.kimi.ai", &payload);
        assert_eq!(usage.headline, "Kimi");
        assert_eq!(usage.plan.as_deref(), Some("platform.kimi.ai"));
        // A balance has no denominator, so it must not be rendered as a percentage meter.
        assert!(usage.meters.is_empty());
        assert!(usage.notes.iter().any(|n| n.contains("49.59")));
        assert!(usage.error.is_none());

        // An exhausted balance is called out, because Kimi rejects requests at zero.
        let empty = json!({ "data": { "available_balance": 0.0 } });
        let usage = currency_balance_usage_from(kimi, "platform.kimi.ai", &empty);
        assert!(usage.notes.iter().any(|n| n.contains("exhausted")));

        // A shape we do not recognise is an error, never a silent "0 used".
        let usage = currency_balance_usage_from(kimi, "platform.kimi.ai", &json!({"data": {}}));
        assert!(usage.error.is_some());
        assert!(usage.meters.is_empty());
    }

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
    fn grok_refresh_endpoint_rejects_untrusted_issuers() {
        assert_eq!(
            grok_token_endpoint(&json!({})).unwrap().as_str(),
            "https://auth.x.ai/oauth2/token"
        );
        assert!(grok_token_endpoint(&json!({"oidc_issuer":"http://auth.x.ai"})).is_err());
        assert!(grok_token_endpoint(&json!({"oidc_issuer":"https://evil.example"})).is_err());
        assert!(
            grok_token_endpoint(&json!({"oidc_issuer":"https://auth.x.ai@evil.example"})).is_err()
        );
        assert!(grok_token_endpoint(&json!({"oidc_issuer":"https://auth.x.ai/redirect"})).is_err());
    }

    #[test]
    fn grok_refresh_preserves_issuer_wrapper() {
        let stored = grok_stored_entry(
            r#"{"https://auth.x.ai::device":{"key":"old","refresh_token":"refresh","oidc_client_id":"client"},"other":{"value":1}}"#,
        )
        .unwrap();
        assert_eq!(
            stored.wrapper_key.as_deref(),
            Some("https://auth.x.ai::device")
        );
        let updated = update_grok_document(
            &stored,
            json!({"key":"new","refresh_token":"rotated","oidc_client_id":"client"}),
        );
        assert_eq!(
            updated["https://auth.x.ai::device"]["refresh_token"],
            "rotated"
        );
        assert_eq!(updated["other"]["value"], 1);
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
