//! Moving state from the old `<base>/asx` directory to `<base>/aas`.
//!
//! [`ensure_config_dir`] renames the directory in one step and leaves `asx` behind as a
//! symlink to `aas`. Agents already running were started with a profile path under `asx`
//! (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`, …) and keep working through that link.
//!
//! The link is not enough for macOS Claude profiles. Claude Code names its Keychain item after
//! a hash of the `CLAUDE_CONFIG_DIR` string, so a session started from the old path keeps
//! refreshing its token under the old item, while new launches read the item for the new path.
//! The move copies every old item to the new name, and [`crate::secure_store`] keeps the two
//! in step while the link exists, so a token rotated on one side is not lost on the other.
//!
//! Once no running agent still points into the old directory, [`finish`] deletes the old
//! Keychain items and the link.

use crate::platform;
use std::io;
use std::path::{Path, PathBuf};

/// The state directory was moved by this call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Moved {
    pub from: PathBuf,
    pub to: PathBuf,
    /// Claude Keychain items copied to the name for the new profile path.
    pub keychain_copied: usize,
}

/// Move `<base>/asx` to `<base>/aas` if that has not happened yet.
///
/// `Ok(None)` when there is nothing to do: `AAS_CONFIG_DIR` is set, the old directory does not
/// exist (or is already the link), or `aas` already exists. A failed rename leaves everything
/// where it was, and [`platform::config_dir`] keeps returning the old directory.
pub fn ensure_config_dir() -> io::Result<Option<Moved>> {
    let Some(from) = platform::legacy_config_dir() else {
        return Ok(None);
    };
    let to = platform::config_base_dir().join(platform::CONFIG_DIR_NAME);
    match std::fs::symlink_metadata(&from) {
        Ok(meta) if meta.is_dir() => {}
        _ => return Ok(None),
    }
    if std::fs::symlink_metadata(&to).is_ok() {
        return Ok(None);
    }
    if let Err(error) = std::fs::rename(&from, &to) {
        // Another aas process may have moved it first.
        if to.is_dir() && !from.is_dir() {
            return Ok(None);
        }
        return Err(error);
    }
    link_legacy(&from);
    let keychain_copied = copy_claude_keychain_items();
    Ok(Some(Moved {
        from,
        to,
        keychain_copied,
    }))
}

#[cfg(unix)]
fn link_legacy(from: &Path) {
    // Relative, so the link survives the base directory being reached through another mount.
    let _ = std::os::unix::fs::symlink(platform::CONFIG_DIR_NAME, from);
}

#[cfg(not(unix))]
fn link_legacy(_from: &Path) {}

/// The old directory while it is a link to the current one — the period in which agents
/// started from old paths may still be running.
pub fn legacy_link() -> Option<PathBuf> {
    let legacy = platform::legacy_config_dir()?;
    let meta = std::fs::symlink_metadata(&legacy).ok()?;
    meta.file_type().is_symlink().then_some(legacy)
}

/// A profile's home as agents started before the move see it.
pub fn legacy_profile_home(provider: &str, name: &str) -> Option<PathBuf> {
    Some(
        legacy_link()?
            .join("profiles")
            .join(crate::naming::safe_profile_dir_name(provider, name)),
    )
}

fn claude_accounts() -> Vec<crate::AccountRecord> {
    crate::store::AccountStore::open_default()
        .list(None)
        .unwrap_or_default()
        .into_iter()
        .filter(|a| crate::naming::normalize_provider_key(&a.provider) == "claude")
        .collect()
}

fn copy_claude_keychain_items() -> usize {
    claude_accounts()
        .iter()
        .filter(|a| crate::secure_store::adopt_legacy_keychain_item(&a.provider, &a.name))
        .count()
}

/// Variables through which an agent is pointed at a profile home.
const PROFILE_VARS: &[&str] = &[
    "CLAUDE_CONFIG_DIR",
    "CODEX_HOME",
    "GROK_HOME",
    "PI_CODING_AGENT_DIR",
];

/// A running process started with a profile home under the old directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyUser {
    pub pid: u32,
    pub command: String,
}

/// Running processes whose profile variable still points into the old directory. Processes
/// that only carry the old shim directory on `PATH` are not counted: they lose shim routing
/// when the link goes, but keep working.
pub fn legacy_users() -> Vec<LegacyUser> {
    let Some(legacy) = platform::legacy_config_dir() else {
        return Vec::new();
    };
    let prefix = format!("{}/", legacy.to_string_lossy().trim_end_matches('/'));
    let needles: Vec<String> = PROFILE_VARS
        .iter()
        .map(|var| format!("{var}={prefix}"))
        .collect();
    let me = std::process::id();
    process_environments()
        .into_iter()
        .filter(|(pid, _, env)| *pid != me && needles.iter().any(|n| env.contains(n.as_str())))
        .map(|(pid, command, _)| LegacyUser { pid, command })
        .collect()
}

/// `(pid, command, environment text)` for the current user's processes.
#[cfg(target_os = "linux")]
fn process_environments() -> Vec<(u32, String, String)> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return out;
    };
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        // Other users' environments are unreadable, which filters them out.
        let Ok(env) = std::fs::read(entry.path().join("environ")) else {
            continue;
        };
        let command = std::fs::read(entry.path().join("cmdline"))
            .map(|raw| {
                String::from_utf8_lossy(&raw)
                    .split('\0')
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default();
        let env = String::from_utf8_lossy(&env).replace('\0', "\n");
        out.push((pid, command, format!("\n{env}")));
    }
    out
}

/// `(pid, command, environment text)` for the current user's processes. `ps -E` appends the
/// environment to the command line, so the environment text is the whole line.
#[cfg(target_os = "macos")]
fn process_environments() -> Vec<(u32, String, String)> {
    let Ok(output) = std::process::Command::new("/bin/ps")
        .args(["-E", "-ww", "-x", "-o", "pid=,command="])
        .output()
    else {
        return Vec::new();
    };
    let names = std::process::Command::new("/bin/ps")
        .args(["-ww", "-x", "-o", "pid=,command="])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    let commands: std::collections::HashMap<u32, String> = names
        .lines()
        .filter_map(|line| {
            let (pid, command) = line.trim_start().split_once(' ')?;
            Some((pid.parse().ok()?, command.trim().to_string()))
        })
        .collect();
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let (pid, rest) = line.trim_start().split_once(' ')?;
            let pid: u32 = pid.parse().ok()?;
            let command = commands.get(&pid).cloned().unwrap_or_default();
            // `ps -E` separates variables with spaces; a leading space anchors the name.
            Some((pid, command, format!(" {rest}")))
        })
        .collect()
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn process_environments() -> Vec<(u32, String, String)> {
    Vec::new()
}

/// What [`finish`] removed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Finished {
    pub keychain_removed: usize,
    pub hook_trust_pruned: usize,
    pub link_removed: bool,
}

/// Remove what the move left behind for agents started from the old directory: the old Claude
/// Keychain items, Codex hook trust recorded for old profile paths, and the link itself.
///
/// Refuses while [`legacy_users`] is non-empty unless `force` is set; those agents would lose
/// their credential and their profile directory.
pub fn finish(force: bool) -> Result<Finished, FinishError> {
    let Some(link) = legacy_link() else {
        return Ok(Finished::default());
    };
    let users = legacy_users();
    if !users.is_empty() && !force {
        return Err(FinishError::InUse(users));
    }
    let keychain_removed = claude_accounts()
        .iter()
        .filter(|a| crate::secure_store::remove_legacy_keychain_item(&a.provider, &a.name))
        .count();
    let hook_trust_pruned = platform::system_home_for("codex")
        .map(|home| {
            crate::codex_hooks::prune_entries_under(
                &home.join("config.toml"),
                &link.join("profiles"),
            )
            .unwrap_or(0)
        })
        .unwrap_or(0);
    std::fs::remove_file(&link).map_err(FinishError::Io)?;
    Ok(Finished {
        keychain_removed,
        hook_trust_pruned,
        link_removed: true,
    })
}

#[derive(Debug)]
pub enum FinishError {
    /// Agents still running from the old directory.
    InUse(Vec<LegacyUser>),
    Io(io::Error),
}

impl std::fmt::Display for FinishError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FinishError::InUse(users) => write!(
                f,
                "{} running process(es) still use a profile under the old directory",
                users.len()
            ),
            FinishError::Io(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for FinishError {}

// The move itself is covered end to end in `aas-cli/tests/agent_interfaces.rs`, which can give
// the binary its own HOME.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_explicit_config_dir_never_migrates() {
        let _guard = crate::ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        std::env::set_var("AAS_CONFIG_DIR", "/tmp/aas-explicit-never-migrates");
        assert_eq!(platform::legacy_config_dir(), None);
        assert_eq!(ensure_config_dir().unwrap(), None);
        assert_eq!(legacy_link(), None);
        std::env::remove_var("AAS_CONFIG_DIR");
    }
}
