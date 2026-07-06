//! Upstream fetch with retry (port of `fetchUpstreamWithRetry` from `proxy/server.ts`).
//!
//! Backends transiently reject with overload/rate errors. HTTP-level failures (network errors,
//! 429/5xx) are universal and retried here; provider-specific body cases (z.ai's 200-with-1305)
//! are delegated to `BackendAdapter::is_retryable`. Retries use exponential backoff + jitter.

use crate::types::{BackendAdapter, UpstreamRequest};
use std::time::Duration;

/// HTTP statuses worth retrying (transient overload / transport).
pub const RETRYABLE_STATUS: [u16; 6] = [408, 429, 500, 502, 503, 504];

/// HTTP statuses never worth retrying — the request itself is invalid or auth is wrong.
pub const FATAL_STATUS: [u16; 7] = [400, 401, 403, 404, 405, 410, 422];

/// Distinguish a transient thrown fetch error (network timeout, DNS hiccup, connection reset) from
/// a permanent one (invalid URL, auth/permission). We err on the side of retrying.
pub fn is_retryable_fetch_error(msg: &str) -> bool {
    let m = msg.to_lowercase();
    if m.contains("auth")
        || m.contains("forbidden")
        || m.contains("invalid url")
        || m.contains("invalid api key")
        || m.contains("cert")
        || m.contains("hostname")
    {
        return false;
    }
    true
}

/// Happy-path check: return the streaming body untouched (no body read) when the response is OK and
/// either it is an event-stream or the backend has no body-level retry hook. Mirrors asx's
/// `res.ok && (ct.includes('event-stream') || !backend?.isRetryable)`.
pub fn should_return_stream(ok: bool, content_type: &str, backend_has_is_retryable: bool) -> bool {
    ok && (content_type.contains("event-stream") || !backend_has_is_retryable)
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum BodyDecision {
    /// Fatal HTTP status — return the error, never retry.
    Fatal,
    /// Retryable and attempts remain — continue the loop.
    Retry,
    /// Non-retryable (or out of attempts) — return the error.
    GiveUp,
}

/// Decide what to do after reading a non-stream / error body.
pub fn classify_body(status: u16, backend_retryable: bool, attempt: u32, retries: u32) -> BodyDecision {
    if FATAL_STATUS.contains(&status) {
        return BodyDecision::Fatal;
    }
    let retryable = RETRYABLE_STATUS.contains(&status) || backend_retryable;
    if attempt < retries && retryable {
        BodyDecision::Retry
    } else {
        BodyDecision::GiveUp
    }
}

/// `min(30_000, 500 * 2^(attempt-1)) + jitter`. `attempt` is 1-based (only called on retries).
pub fn backoff_ms(attempt: u32, jitter: u64) -> u64 {
    let base = std::cmp::min(30_000u64, 500u64.saturating_mul(1u64 << (attempt - 1)));
    base + jitter
}

/// Jitter in 0..500 without `Math.random` — derived from a fresh v4 UUID (determinism not required).
fn jitter() -> u64 {
    let b = uuid::Uuid::new_v4().into_bytes();
    (u16::from_le_bytes([b[0], b[1]]) as u64) % 500
}

/// Outcome of the upstream fetch. `Stream` carries an unread response body ready to be piped;
/// `Error` carries a status + already-consumed body detail.
pub enum UpstreamOutcome {
    Stream(reqwest::Response),
    Error { status: u16, detail: String },
}

const MAX_RETRIES: u32 = 4;
const PER_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(120);

pub async fn fetch_upstream_with_retry(
    client: &reqwest::Client,
    up: &UpstreamRequest,
    backend: &dyn BackendAdapter,
) -> anyhow::Result<UpstreamOutcome> {
    let retries = MAX_RETRIES;
    let mut last_text = String::new();
    let mut last_status: Option<u16> = None;

    for attempt in 0..=retries {
        if attempt > 0 {
            let ms = backoff_ms(attempt, jitter());
            tokio::time::sleep(Duration::from_millis(ms)).await;
        }
        let mut req = client.post(&up.url).timeout(PER_ATTEMPT_TIMEOUT);
        for (k, v) in &up.headers {
            req = req.header(k.as_str(), v.as_str());
        }
        let res = match req.body(up.body.clone()).send().await {
            Ok(r) => r,
            Err(e) => {
                last_text = e.to_string();
                if !is_retryable_fetch_error(&last_text) {
                    break;
                }
                continue;
            }
        };
        let status = res.status().as_u16();
        last_status = Some(status);
        let ok = res.status().is_success();
        let ct = res
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        if should_return_stream(ok, &ct, backend.has_is_retryable()) {
            return Ok(UpstreamOutcome::Stream(res));
        }
        let text = res.text().await.unwrap_or_default();
        last_text = text.clone();
        let backend_retryable = backend.is_retryable(status, &text);
        match classify_body(status, backend_retryable, attempt, retries) {
            BodyDecision::Retry => continue,
            _ => return Ok(UpstreamOutcome::Error { status, detail: text }),
        }
    }

    match last_status {
        Some(status) => Ok(UpstreamOutcome::Error { status, detail: last_text }),
        None => Err(anyhow::anyhow!(if last_text.is_empty() {
            "upstream fetch failed".to_string()
        } else {
            last_text
        })),
    }
}
