//! Consistent, TTY-aware terminal output: colored status lines + small styling helpers.
//! Colors are applied only on a real terminal and are disabled when `NO_COLOR` is set.

use owo_colors::OwoColorize;
use std::fmt::Display;
use std::io::IsTerminal;

fn enabled(is_tty: bool) -> bool {
    is_tty && std::env::var_os("NO_COLOR").is_none()
}
fn out() -> bool {
    enabled(std::io::stdout().is_terminal())
}
fn eout() -> bool {
    enabled(std::io::stderr().is_terminal())
}

/// `✗ …` in red, to stderr.
pub fn error(msg: impl Display) {
    if eout() {
        eprintln!("{} {msg}", "✗".red().bold());
    } else {
        eprintln!("✗ {msg}");
    }
}

/// `✓ …` in green, to stdout.
pub fn success(msg: impl Display) {
    if out() {
        println!("{} {msg}", "✓".green().bold());
    } else {
        println!("✓ {msg}");
    }
}

/// `! …` in yellow, to stderr.
pub fn warn(msg: impl Display) {
    if eout() {
        eprintln!("{} {msg}", "!".yellow().bold());
    } else {
        eprintln!("! {msg}");
    }
}

/// Dimmed, indented hint line to stderr.
pub fn hint(msg: impl Display) {
    if eout() {
        eprintln!("{}", format!("  {msg}").dimmed());
    } else {
        eprintln!("  {msg}");
    }
}

// ---- inline string styling (stdout) ----

pub fn heading(s: &str) -> String {
    if out() { s.bold().to_string() } else { s.to_string() }
}
pub fn dim(s: &str) -> String {
    if out() { s.dimmed().to_string() } else { s.to_string() }
}
pub fn green(s: &str) -> String {
    if out() { s.green().to_string() } else { s.to_string() }
}
pub fn cyan(s: &str) -> String {
    if out() { s.cyan().to_string() } else { s.to_string() }
}
pub fn yellow(s: &str) -> String {
    if out() { s.yellow().to_string() } else { s.to_string() }
}
