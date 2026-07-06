//! Rendering for `list` and the `list -u` usage table (comfy-table).

use aas_core::usage::{bar_level, format_reset, render_bar_plain, BarLevel, Usage};
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

fn render_limits(u: &Usage) -> (String, Option<BarLevel>) {
    if let Some(err) = &u.error {
        return (format!("⚠ {err}"), Some(BarLevel::Bad));
    }
    let mut lines: Vec<String> = Vec::new();
    let mut worst: Option<BarLevel> = None;
    for m in &u.meters {
        let rem = m.remaining_pct();
        let lvl = bar_level(rem);
        worst = Some(match worst {
            Some(w) => worse(w, lvl),
            None => lvl,
        });
        let bar = render_bar_plain(rem, 16);
        let reset = m
            .reset_ms
            .map(|ms| format!("  · {}", format_reset(ms)))
            .unwrap_or_default();
        lines.push(format!("{:<7}{} {:>3.0}% used{}", m.label, bar, m.used_pct, reset));
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
    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .apply_modifier(UTF8_ROUND_CORNERS)
        .set_content_arrangement(ContentArrangement::Dynamic);
    table.set_header(vec!["", "Provider", "Account", "Plan", "Limits"]);

    for r in rows {
        let mark_cell = if r.active {
            Cell::new("●").fg(Color::Green)
        } else if r.current_in_system {
            Cell::new("◆").fg(Color::Cyan)
        } else {
            Cell::new(" ")
        };

        let mut acct = r.name.clone();
        if let Some(e) = &r.email {
            acct.push('\n');
            acct.push_str(e);
        }

        let plan = r
            .usage
            .plan
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| r.usage.headline.clone());

        let (limits, worst) = render_limits(&r.usage);
        let limits_cell = match worst {
            Some(l) => Cell::new(limits).fg(level_color(l)),
            None => Cell::new(limits),
        };

        table.add_row(vec![
            mark_cell,
            Cell::new(&r.provider),
            Cell::new(acct),
            Cell::new(plan),
            limits_cell,
        ]);
    }

    println!("{table}");
}
