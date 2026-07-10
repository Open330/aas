//! Rendering for `list` and the `list -u` usage table (comfy-table).

use aas_core::usage::{render_bar_plain, BarLevel, Usage};
use comfy_table::modifiers::UTF8_ROUND_CORNERS;
use comfy_table::presets::UTF8_FULL;
use comfy_table::{Cell, Color, ContentArrangement, Table};
use owo_colors::OwoColorize;

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
    for r in rows {
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

fn color_on() -> bool {
    use std::io::IsTerminal;
    std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none()
}

/// Colour a whole line by level (TTY-aware). Relies on comfy-table's `custom_styling`
/// feature so the embedded ANSI is measured correctly for column widths.
fn color_line(text: String, level: BarLevel) -> String {
    if !color_on() {
        return text;
    }
    match level {
        BarLevel::Good => text.green().to_string(),
        BarLevel::Warn => text.yellow().to_string(),
        BarLevel::Bad => text.red().to_string(),
    }
}

/// Colour time-remaining: lots of time left = green, almost reset = red.
fn remaining_level(remaining_pct: f64) -> BarLevel {
    if remaining_pct <= 15.0 {
        BarLevel::Bad
    } else if remaining_pct <= 40.0 {
        BarLevel::Warn
    } else {
        BarLevel::Good
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Fraction of a time-boxed window (5h / 7d) still remaining, from its reset time.
/// Shown next to the time-left so both the amount and the % refer to what remains.
fn remaining_window_pct(label: &str, reset_ms: i64) -> Option<f64> {
    let dur_ms = match label {
        "5h" => 5.0 * 3_600_000.0,
        "7d" => 7.0 * 86_400_000.0,
        _ => return None,
    };
    let rem = reset_ms as f64 - now_ms() as f64;
    Some((rem / dur_ms).clamp(0.0, 1.0) * 100.0)
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

/// Compact time to reset without a suffix, e.g. `9m`, `7h 59m`, `now`.
fn time_amount(reset_ms: i64) -> String {
    let diff = reset_ms - now_ms();
    if diff <= 0 {
        return "now".to_string();
    }
    let mins = (diff as f64 / 60000.0).round() as i64;
    let (h, m) = (mins / 60, mins % 60);
    if h > 0 {
        format!("{h}h {m}m")
    } else {
        format!("{m}m")
    }
}

/// Combine subscription + short tier, e.g. `max · 20x`, `team · 5x`, `pro`.
pub fn plan_label(u: &Usage) -> String {
    let base = u
        .plan
        .clone()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| u.headline.clone());
    // Pull `tier=default_claude_max_20x` out of the headline and shorten to `20x`.
    if let Some(tier) = u
        .headline
        .split_whitespace()
        .find_map(|w| w.strip_prefix("tier="))
    {
        let short = tier.rsplit('_').next().unwrap_or(tier);
        if !short.is_empty() && short != "default" && short != base {
            return format!("{base} · {short}");
        }
    }
    base
}

/// Build the `Usage` and `Reset` cell contents for an account — one line per meter (5h/7d),
/// each line coloured independently (usage green→red by used %, reset green→red by time left).
fn usage_reset_cells(u: &Usage) -> (String, String) {
    if let Some(err) = &u.error {
        return (color_line(format!("⚠ {err}"), BarLevel::Bad), String::new());
    }
    let mut usage_lines: Vec<String> = Vec::new();
    let mut reset_lines: Vec<String> = Vec::new();
    for m in &u.meters {
        let used = m.used_pct.clamp(0.0, 100.0);
        // Bar fills with USED (like Claude Code's /usage): a full bar = at the limit.
        let bar = render_bar_plain(used, 8);
        usage_lines.push(color_line(
            format!("{:<3}{} {:>3.0}% used", m.label, bar, used),
            used_level(used),
        ));
        // e.g. "7h 7m (4%) left" — the time and the % both refer to what remains.
        let rline = match m.reset_ms {
            Some(ms) => {
                let amount = time_amount(ms);
                if amount == "now" {
                    color_line("now".to_string(), BarLevel::Bad)
                } else if let Some(rem) = remaining_window_pct(&m.label, ms) {
                    color_line(format!("{amount} ({rem:.0}%) left"), remaining_level(rem))
                } else {
                    format!("{amount} left")
                }
            }
            None => String::new(),
        };
        reset_lines.push(rline);
    }
    for n in &u.notes {
        usage_lines.push(n.clone());
    }
    if usage_lines.is_empty() {
        usage_lines.push(u.headline.clone());
    }
    (usage_lines.join("\n"), reset_lines.join("\n"))
}

pub fn render_usage_table(rows: &[UsageRow]) {
    let mut table = new_table();
    table.set_header(vec!["", "Provider", "Account", "Plan", "Usage", "Reset"]);

    for r in rows {
        let (usage, reset) = usage_reset_cells(&r.usage);
        table.add_row(vec![
            marker_cell(r.active, r.current_in_system),
            Cell::new(&r.provider),
            account_cell(&r.name, &r.email),
            Cell::new(plan_label(&r.usage)),
            Cell::new(usage),
            Cell::new(reset),
        ]);
    }

    tighten(&mut table);
    println!("{table}");
}
