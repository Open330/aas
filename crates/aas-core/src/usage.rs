//! Structured usage model. asx's providers return a preformatted color string; aas returns
//! data and lets the CLI render bars/tables/colors. This is what enables the parallel
//! `list -u` fan-out + single-render, and a provider-agnostic table.

use chrono::{Local, TimeZone, Utc};

#[derive(Clone, Debug, Default)]
pub struct Usage {
    /// e.g. `subscription=max tier=... org=... has_max=yes`.
    pub headline: String,
    pub plan: Option<String>,
    pub meters: Vec<Meter>,
    /// Free-form extra lines (grok rate limits, billing period, etc.).
    pub notes: Vec<String>,
    /// Set when usage could not be fetched (token expired, network, etc.).
    pub error: Option<String>,
}

impl Usage {
    pub fn error(headline: impl Into<String>, msg: impl Into<String>) -> Self {
        Usage {
            headline: headline.into(),
            error: Some(msg.into()),
            ..Default::default()
        }
    }
}

#[derive(Clone, Debug)]
pub struct Meter {
    /// "5h", "7d", "credits", ...
    pub label: String,
    pub used_pct: f64,
    /// Reset time, ms since epoch.
    pub reset_ms: Option<i64>,
}

impl Meter {
    pub fn new(label: impl Into<String>, used_pct: f64, reset_ms: Option<i64>) -> Self {
        Meter {
            label: label.into(),
            used_pct,
            reset_ms,
        }
    }
    pub fn remaining_pct(&self) -> f64 {
        (100.0 - self.used_pct).clamp(0.0, 100.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarLevel {
    Good,
    Warn,
    Bad,
}

/// asx color thresholds on *remaining* percentage.
pub fn bar_level(remaining_pct: f64) -> BarLevel {
    if remaining_pct >= 90.0 {
        BarLevel::Good
    } else if remaining_pct >= 70.0 {
        BarLevel::Warn
    } else {
        BarLevel::Bad
    }
}

/// asx `renderBar` without color (the CLI applies color via [`bar_level`]).
pub fn render_bar_plain(remaining_pct: f64, width: usize) -> String {
    let pct = remaining_pct.clamp(0.0, 100.0);
    let filled = ((pct / 100.0) * width as f64).round() as usize;
    let filled = filled.min(width);
    let empty = width - filled;
    format!("[{}{}]", "█".repeat(filled), "░".repeat(empty))
}

/// asx `formatReset`: `resets Jul 6, 03:26 PM (2h 41m left)` / `(now)`, in local time.
pub fn format_reset(reset_ms: i64) -> String {
    let Some(dt) = Utc.timestamp_millis_opt(reset_ms).single() else {
        return String::new();
    };
    let local = dt.with_timezone(&Local);
    let stamp = local.format("%b %-d, %I:%M %p").to_string();
    let now = Utc::now().timestamp_millis();
    let diff = reset_ms - now;
    if diff <= 0 {
        return format!("resets {stamp} (now)");
    }
    let mins = ((diff as f64) / 60000.0).round() as i64;
    let (h, m) = (mins / 60, mins % 60);
    let left = if h > 0 {
        format!("{h}h {m}m")
    } else {
        format!("{m}m")
    };
    format!("resets {stamp} ({left} left)")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bar_widths_and_levels() {
        assert_eq!(render_bar_plain(100.0, 20), format!("[{}]", "█".repeat(20)));
        assert_eq!(render_bar_plain(0.0, 20), format!("[{}]", "░".repeat(20)));
        assert_eq!(
            render_bar_plain(50.0, 20),
            format!("[{}{}]", "█".repeat(10), "░".repeat(10))
        );
        assert_eq!(bar_level(95.0), BarLevel::Good);
        assert_eq!(bar_level(75.0), BarLevel::Warn);
        assert_eq!(bar_level(10.0), BarLevel::Bad);
    }

    #[test]
    fn meter_remaining() {
        let m = Meter::new("5h", 96.0, None);
        assert!((m.remaining_pct() - 4.0).abs() < 1e-9);
    }

    #[test]
    fn reset_now_and_future() {
        let now = Utc::now().timestamp_millis();
        assert!(format_reset(now - 1000).contains("(now)"));
        assert!(format_reset(now + 3_600_000).contains("left"));
    }
}
