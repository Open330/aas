//! Path roots and provider-home resolution. Mirrors asx `utils/platform.ts`.
//!
//! `dirs::config_dir()` matches asx's `getConfigBaseDir()` exactly on every platform:
//! win `%APPDATA%`, macOS `~/Library/Application Support`, linux `$XDG_CONFIG_HOME | ~/.config`.

use std::path::PathBuf;

pub fn home_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

/// Expand a leading `~/` or `~\` to the home directory (asx `expandHome`).
pub fn expand_home(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/").or_else(|| p.strip_prefix("~\\")) {
        home_dir().join(rest)
    } else {
        PathBuf::from(p)
    }
}

pub fn config_base_dir() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| home_dir().join(".config"))
}

/// asx config dir. Defaults to `<base>/asx` (shared with asx for zero-migration adoption);
/// override with `AAS_CONFIG_DIR` for a clean-slate install.
pub fn asx_config_dir() -> PathBuf {
    if let Ok(d) = std::env::var("AAS_CONFIG_DIR") {
        if !d.is_empty() {
            return expand_home(&d);
        }
    }
    config_base_dir().join("asx")
}

pub fn accounts_path() -> PathBuf {
    asx_config_dir().join("accounts.json")
}

pub fn active_path() -> PathBuf {
    asx_config_dir().join(".active.json")
}

pub fn profiles_dir() -> PathBuf {
    asx_config_dir().join("profiles")
}

fn home_dot_dir(name: &str) -> PathBuf {
    home_dir().join(format!(".{name}"))
}

fn env_home_or(var: &str, dot: &str) -> PathBuf {
    match std::env::var(var) {
        Ok(v) if !v.is_empty() => expand_home(&v),
        _ => home_dot_dir(dot),
    }
}

pub fn claude_config_dir() -> PathBuf {
    env_home_or("CLAUDE_CONFIG_DIR", "claude")
}

pub fn claude_credentials_path() -> PathBuf {
    claude_config_dir().join(".credentials.json")
}

pub fn codex_home() -> PathBuf {
    env_home_or("CODEX_HOME", "codex")
}

pub fn codex_auth_path() -> PathBuf {
    codex_home().join("auth.json")
}

pub fn grok_home() -> PathBuf {
    env_home_or("GROK_HOME", "grok")
}

pub fn grok_auth_path() -> PathBuf {
    grok_home().join("auth.json")
}

/// Pi coding agent config root. Unlike the other agents this is `~/.pi/agent`, not `~/.pi`.
pub fn pi_agent_dir() -> PathBuf {
    match std::env::var("PI_CODING_AGENT_DIR") {
        Ok(v) if !v.is_empty() => expand_home(&v),
        _ => home_dot_dir("pi").join("agent"),
    }
}

pub fn pi_auth_path() -> PathBuf {
    pi_agent_dir().join("auth.json")
}

/// Installed Grok CLI version used by the cloud proxy's client-identification headers.
pub fn grok_version() -> String {
    std::fs::read_to_string(grok_home().join("version.json"))
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .and_then(|value| {
            value
                .get("version")
                .or_else(|| value.get("stable_version"))
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(String::from)
        })
        .unwrap_or_else(|| "0.2.77".to_string())
}

/// The provider's system (native) home dir: `~/.claude` / `~/.codex` / `~/.grok`.
pub fn system_home_for(provider: &str) -> Option<PathBuf> {
    match crate::naming::normalize_provider_key(provider).as_str() {
        "claude" => Some(claude_config_dir()),
        "codex" => Some(codex_home()),
        "grok" => Some(grok_home()),
        "pi" => Some(pi_agent_dir()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_home_replaces_tilde() {
        assert_eq!(expand_home("~/x/y"), home_dir().join("x/y"));
        assert_eq!(expand_home("/abs/path"), PathBuf::from("/abs/path"));
    }

    #[test]
    fn asx_config_dir_env_override() {
        std::env::set_var("AAS_CONFIG_DIR", "/tmp/aas-test-cfg");
        assert_eq!(asx_config_dir(), PathBuf::from("/tmp/aas-test-cfg"));
        std::env::remove_var("AAS_CONFIG_DIR");
    }

    #[test]
    fn pi_defaults_to_nested_agent_dir() {
        std::env::remove_var("PI_CODING_AGENT_DIR");
        assert_eq!(pi_agent_dir(), home_dir().join(".pi/agent"));
    }
}
