//! Pace-based account ranking for `aas exec <provider>` auto-pick.
//!
//! A meter's *pace* is the share of quota left divided by the share of its window left:
//! `1.0` means spending at an even rate exhausts it exactly at reset, `> 1.0` means quota will
//! go unused before reset (spend it first), `< 1.0` means the account will hit its limit early.
//! An account is bound by its tightest window, so its score is the minimum pace over its meters.

use crate::usage::Meter;
use std::cmp::Ordering;

/// Floor on the window fraction left, so a meter seconds from reset cannot dominate the ranking.
const MIN_TIME_FRACTION: f64 = 0.02;
/// Below this much quota left a meter counts as nearly exhausted…
const LOW_REMAINING_PCT: f64 = 5.0;
/// …unless it resets within this long anyway.
const LOW_GRACE_MS: i64 = 10 * 60_000;

/// Window length from a meter label such as `5h`, `7d`, `90m` or `1w`. `None` for windowless
/// meters such as `credits`.
pub fn window_ms(label: &str) -> Option<i64> {
    let split = label.len().checked_sub(1)?;
    let (number, unit) = label.split_at(split);
    let n: i64 = number.parse().ok().filter(|n| *n > 0)?;
    let unit_ms = match unit {
        "m" => 60_000,
        "h" => 3_600_000,
        "d" => 86_400_000,
        "w" => 7 * 86_400_000,
        _ => return None,
    };
    Some(n * unit_ms)
}

/// Pace of one meter at `now_ms`. A cached meter whose reset time has already passed is treated
/// as freshly reset. Meters without a known window or reset time use a time fraction of 1, so
/// their pace equals the share of quota left.
pub fn meter_pace(meter: &Meter, now_ms: i64) -> f64 {
    let time_fraction = match (meter.reset_ms, window_ms(&meter.label)) {
        (Some(reset), _) if reset <= now_ms => return 1.0,
        (Some(reset), Some(window)) => {
            ((reset - now_ms) as f64 / window as f64).clamp(MIN_TIME_FRACTION, 1.0)
        }
        _ => 1.0,
    };
    meter.remaining_pct() / 100.0 / time_fraction
}

fn effective_remaining(meter: &Meter, now_ms: i64) -> f64 {
    match meter.reset_ms {
        Some(reset) if reset <= now_ms => 100.0,
        _ => meter.remaining_pct(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Score {
    /// Minimum pace over the account's meters.
    pub pace: f64,
    /// Minimum quota left (percent) over the account's meters.
    pub remaining_pct: f64,
    /// Some meter is nearly exhausted and not about to reset.
    pub low: bool,
}

/// Score an account's meters. `None` when there is nothing usable to score.
pub fn score(meters: &[Meter], now_ms: i64) -> Option<Score> {
    if meters.is_empty() || meters.iter().any(|m| !m.used_pct.is_finite()) {
        return None;
    }
    let pace = meters
        .iter()
        .map(|m| meter_pace(m, now_ms))
        .fold(f64::INFINITY, f64::min);
    let remaining_pct = meters
        .iter()
        .map(|m| effective_remaining(m, now_ms))
        .fold(f64::INFINITY, f64::min);
    let low = meters.iter().any(|m| {
        effective_remaining(m, now_ms) < LOW_REMAINING_PCT
            && m.reset_ms.is_none_or(|reset| reset - now_ms > LOW_GRACE_MS)
    });
    Some(Score {
        pace,
        remaining_pct,
        low,
    })
}

/// The meter that binds `score.pace`, for explaining a pick.
pub fn binding_meter(meters: &[Meter], now_ms: i64) -> Option<&Meter> {
    meters.iter().min_by(|a, b| {
        meter_pace(a, now_ms)
            .partial_cmp(&meter_pace(b, now_ms))
            .unwrap_or(Ordering::Equal)
    })
}

/// Best-first order: not-low, higher pace, more quota left, then the active account.
pub fn compare(a: &Score, a_active: bool, b: &Score, b_active: bool) -> Ordering {
    a.low
        .cmp(&b.low)
        .then_with(|| b.pace.partial_cmp(&a.pace).unwrap_or(Ordering::Equal))
        .then_with(|| {
            b.remaining_pct
                .partial_cmp(&a.remaining_pct)
                .unwrap_or(Ordering::Equal)
        })
        .then_with(|| b_active.cmp(&a_active))
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: i64 = 3_600_000;
    const D: i64 = 24 * H;
    const NOW: i64 = 1_000 * D;

    fn m(label: &str, used: f64, reset_in: Option<i64>) -> Meter {
        Meter::new(label, used, reset_in.map(|d| NOW + d))
    }

    #[test]
    fn window_labels_parse() {
        assert_eq!(window_ms("5h"), Some(5 * H));
        assert_eq!(window_ms("7d"), Some(7 * D));
        assert_eq!(window_ms("90m"), Some(90 * 60_000));
        assert_eq!(window_ms("1w"), Some(7 * D));
        for label in ["credits", "", "h", "0h", "-1d", "5x"] {
            assert_eq!(window_ms(label), None, "{label}");
        }
    }

    #[test]
    fn quota_expiring_soon_beats_more_quota_reset_later() {
        // A: 40% left, resets in 1 day. B: 70% left, resets in 6 days.
        let a = score(&[m("7d", 60.0, Some(D))], NOW).unwrap();
        let b = score(&[m("7d", 30.0, Some(6 * D))], NOW).unwrap();
        assert!((a.pace - 2.8).abs() < 1e-9);
        assert!((b.pace - 0.7 / (6.0 / 7.0)).abs() < 1e-9);
        assert_eq!(compare(&a, false, &b, false), Ordering::Less);
    }

    #[test]
    fn the_tightest_window_binds_the_account() {
        // Weekly is generous, but the 5h window has 10% left with 4h to go.
        let meters = [m("5h", 90.0, Some(4 * H)), m("7d", 60.0, Some(D))];
        let s = score(&meters, NOW).unwrap();
        assert!((s.pace - 0.125).abs() < 1e-9);
        assert_eq!(binding_meter(&meters, NOW).unwrap().label, "5h");
    }

    #[test]
    fn passed_resets_count_as_fresh_and_windowless_meters_use_remaining() {
        let s = score(&[m("5h", 100.0, Some(-H))], NOW).unwrap();
        assert_eq!((s.pace, s.remaining_pct, s.low), (1.0, 100.0, false));
        let s = score(&[m("credits", 25.0, None)], NOW).unwrap();
        assert!((s.pace - 0.75).abs() < 1e-9);
    }

    #[test]
    fn time_fraction_is_floored() {
        let s = score(&[m("5h", 50.0, Some(1))], NOW).unwrap();
        assert!((s.pace - 0.5 / MIN_TIME_FRACTION).abs() < 1e-9);
    }

    #[test]
    fn nearly_exhausted_accounts_sink_unless_about_to_reset() {
        let low = score(&[m("7d", 97.0, Some(H))], NOW).unwrap();
        assert!(low.low);
        let resetting = score(&[m("5h", 97.0, Some(5 * 60_000))], NOW).unwrap();
        assert!(!resetting.low);
        let steady = score(&[m("7d", 90.0, Some(6 * D))], NOW).unwrap();
        assert!(low.pace > steady.pace);
        assert_eq!(compare(&steady, false, &low, false), Ordering::Less);
    }

    #[test]
    fn ties_prefer_more_quota_then_the_active_account() {
        let a = score(&[m("credits", 50.0, None)], NOW).unwrap();
        assert_eq!(compare(&a, false, &a, true), Ordering::Greater);
        assert_eq!(compare(&a, true, &a, false), Ordering::Less);
    }

    #[test]
    fn unusable_meters_have_no_score() {
        assert_eq!(score(&[], NOW), None);
        assert_eq!(score(&[m("5h", f64::NAN, None)], NOW), None);
    }
}
