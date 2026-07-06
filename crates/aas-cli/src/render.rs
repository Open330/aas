//! Rendering for `list` and the `list -u` usage table (comfy-table).

use aas_core::usage::{render_bar_plain, BarLevel, Usage};
use comfy_table::modifiers::UTF8_ROUND_CORNERS;
use comfy_table::presets::UTF8_FULL;
use comfy_table::{Cell, Color, ContentArrangement, Table};

pub struct UsageRow {
    pub provider: String,
    pub name: String,
    pub email: Option<String>,
    pub active: bool,
    pub current_in_system: bool,
    pub usage: Usage,
}

/// Rounded, dynamically-arranged base table shared by every view.
fn new_table() -> Table {
    let mut t = Table::new();
    t.load_preset(UTF8_FULL)
        .apply_modifier(UTF8_ROUND_CORNERS)
        .set_content_arrangement(ContentArrangement::Dynamic);
    t
}

/// Trim per-column padding to 1 space (right only).
fn tighten(t: &mut Table) {
    for column in t.column_iter_mut() {
        column.set_padding((0, 1));
    }
}

fn account_cell(name: &str, email: &Option<String>) -> Cell {
    let mut s = name.to_string();
    if let Some(e) = email {
        s.push('\n');
        s.push_str(e);
    }
    Cell::new(s)
}

fn marker_cell(active: bool, current: bool) -> Cell {
    if active {
        Cell::new("●").fg(Color::Green)
    } else if current {
        Cell::new("◆").fg(Color::Cyan)
    } else {
        Cell::new(" ")
    }
}

/// How a profile shares state (for the `list` Sharing column).
pub enum Sharing {
    Categories(String),
    CurrentInSystem,
    System,
}

pub struct ListRow {
    pub provider: String,
    pub name: String,
    pub email: Option<String>,
    pub active: bool,
    pub current_in_system: bool,
    pub sharing: Sharing,
}

pub fn render_list_table(rows: &[ListRow]) {
    let mut table = new_table();
    table.set_header(vec!["", "Provider", "Account", "Sharing"]);
    let mut any_marker = false;
    for r in rows {
        any_marker |= r.active || r.current_in_system;
        let (txt, color) = match &r.sharing {
            Sharing::Categories(s) => (s.clone(), Color::Yellow),
            Sharing::CurrentInSystem => ("current in system".to_string(), Color::Cyan),
            Sharing::System => ("system".to_string(), Color::DarkGrey),
        };
        table.add_row(vec![
            marker_cell(r.active, r.current_in_system),
            Cell::new(&r.provider),
            account_cell(&r.name, &r.email),
            Cell::new(txt).fg(color),
        ]);
    }
    tighten(&mut table);
    println!("{table}");
    if any_marker {
        println!("  ● active   ◆ current in system");
    }
}

pub fn render_status_table(rows: &[(String, Option<String>)]) {
    let mut table = new_table();
    table.set_header(vec!["Provider", "Active"]);
    for (prov, active) in rows {
        let cell = match active {
            Some(n) => Cell::new(n).fg(Color::Green),
            None => Cell::new("(none)").fg(Color::DarkGrey),
        };
        table.add_row(vec![Cell::new(prov), cell]);
    }
    tighten(&mut table);
    println!("{table}");
}

fn level_color(l: BarLevel) -> Color {
    match l {
        BarLevel::Good => Color::Green,
        BarLevel::Warn => Color::Yellow,
        BarLevel::Bad => Color::Red,
    }
}

fn worse(a: BarLevel, b: BarLevel) -> BarLevel {
    let rank = |l: BarLevel| match l {
        BarLevel::Bad => 0,
        BarLevel::Warn => 1,
        BarLevel::Good => 2,
    };
    if rank(a) <= rank(b) {
        a
    } else {
        b
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Fraction of a time-boxed window (5h / 7d) that has elapsed, from its reset time.
/// Shown next to time-left so you can compare usage pace against time pace.
fn elapsed_pct(label: &str, reset_ms: i64) -> Option<f64> {
    let dur_ms = match label {
        "5h" => 5.0 * 3_600_000.0,
        "7d" => 7.0 * 86_400_000.0,
        _ => return None,
    };
    let rem = reset_ms as f64 - now_ms() as f64;
    Some((1.0 - rem / dur_ms).clamp(0.0, 1.0) * 100.0)
}

/// Colour by how close to the limit: low use green, high use red.
fn used_level(used_pct: f64) -> BarLevel {
    if used_pct >= 85.0 {
        BarLevel::Bad
    } else if used_pct >= 60.0 {
        BarLevel::Warn
    } else {
        BarLevel::Good
    }
}

/// Compact relative time to reset, e.g. `9m left`, `7h 59m left`, `now`.
fn time_left(reset_ms: i64) -> String {
    let diff = reset_ms - now_ms();
    if diff <= 0 {
        return "now".to_string();
    }
    let mins = (diff as f64 / 60000.0).round() as i64;
    let (h, m) = (mins / 60, mins % 60);
    if h > 0 {
        format!("{h}h {m}m left")
    } else {
        format!("{m}m left")
    }
}

/// Combine subscription + short tier, e.g. `max · 20x`, `team · 5x`, `pro`.
fn plan_label(u: &Usage) -> String {
    let base = u
        .plan
        .clone()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| u.headline.clone());
    // Pull `tier=default_claude_max_20x` out of the headline and shorten to `20x`.
    if let Some(tier) = u.headline.split_whitespace().find_map(|w| w.strip_prefix("tier=")) {
        let short = tier.rsplit('_').next().unwrap_or(tier);
        if !short.is_empty() && short != "default" && short != base {
            return format!("{base} · {short}");
        }
    }
    base
}

fn render_limits(u: &Usage) -> (String, Option<BarLevel>) {
    if let Some(err) = &u.error {
        return (format!("⚠ {err}"), Some(BarLevel::Bad));
    }
    let mut lines: Vec<String> = Vec::new();
    let mut worst: Option<BarLevel> = None;
    for m in &u.meters {
        let used = m.used_pct.clamp(0.0, 100.0);
        let lvl = used_level(used);
        worst = Some(match worst {
            Some(w) => worse(w, lvl),
            None => lvl,
        });
        // Bar fills with USED (like Claude Code's /usage): a full bar = at the limit.
        let bar = render_bar_plain(used, 8);
        // Time-to-reset, plus the % of the window elapsed (bare % after "left" = time pace).
        let reset = match m.reset_ms {
            Some(ms) => match elapsed_pct(&m.label, ms) {
                Some(e) => format!(" · {} · {e:.0}%", time_left(ms)),
                None => format!(" · {}", time_left(ms)),
            },
            None => String::new(),
        };
        lines.push(format!("{:<3}{} {:>3.0}% used{}", m.label, bar, used, reset));
    }
    for n in &u.notes {
        lines.push(n.clone());
    }
    if lines.is_empty() {
        lines.push(u.headline.clone());
    }
    (lines.join("\n"), worst)
}

pub fn render_usage_table(rows: &[UsageRow]) {
    let mut table = new_table();
    table.set_header(vec!["", "Provider", "Account", "Plan", "Limits"]);

    let mut any_marker = false;
    for r in rows {
        any_marker |= r.active || r.current_in_system;
        let (limits, worst) = render_limits(&r.usage);
        let limits_cell = match worst {
            Some(l) => Cell::new(limits).fg(level_color(l)),
            None => Cell::new(limits),
        };
        table.add_row(vec![
            marker_cell(r.active, r.current_in_system),
            Cell::new(&r.provider),
            account_cell(&r.name, &r.email),
            Cell::new(plan_label(&r.usage)),
            limits_cell,
        ]);
    }

    tighten(&mut table);
    println!("{table}");
    // Legend only when a marker is actually shown.
    if any_marker {
        println!("  ● active   ◆ current in system");
    }
}
