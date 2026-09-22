//! Path roots and provider-home resolution. Mirrors asx `utils/platform.ts`.
//!
//! `dirs::config_dir()` matches asx's `getConfigBaseDir()` exactly on every platform:
//! win `%APPDATA%`, macOS `~/Library/Application Support`, linux `$XDG_CONFIG_HOME | ~/.config`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

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

/// Provider homes this process picked for itself, keyed by the variable that names them.
///
/// `aas login` signs in *into* a profile home and then reads the fresh credential back out of
/// it, so that read has to resolve to the profile and not to the system install. An override
/// records the choice this process made, which an inherited variable cannot express.
fn home_overrides() -> &'static Mutex<HashMap<String, PathBuf>> {
    static OVERRIDES: OnceLock<Mutex<HashMap<String, PathBuf>>> = OnceLock::new();
    OVERRIDES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lock_overrides() -> std::sync::MutexGuard<'static, HashMap<String, PathBuf>> {
    home_overrides()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Resolve `var` to `dir` for the rest of this process, until [`clear_home_override`].
pub fn set_home_override(var: &str, dir: &Path) {
    lock_overrides().insert(var.to_string(), dir.to_path_buf());
}

pub fn clear_home_override(var: &str) {
    lock_overrides().remove(var);
}

/// True when `dir` sits inside aas's own profiles root.
///
/// `aas exec` points the agent at a profile home through the provider's home variable, so every
/// shell opened inside that agent inherits it — and a tmux server started there copies it into
/// the global environment of every shell it will ever spawn. The value then comes back to the
/// next `aas` run describing a directory aas chose, never the user's native install, so
/// resolving a provider's system home has to look past it.
pub fn is_managed_home(dir: &Path) -> bool {
    let profiles = profiles_dir();
    if dir.starts_with(&profiles) {
        return true;
    }
    match (dir.canonicalize(), profiles.canonicalize()) {
        (Ok(dir), Ok(profiles)) => dir.starts_with(profiles),
        _ => false,
    }
}

/// The home named for `var` by this process's override or the caller's own environment.
/// `None` means the provider's default applies — including when the variable only carries a
/// profile home aas itself handed out.
fn chosen_home(var: &str) -> Option<PathBuf> {
    if let Some(dir) = lock_overrides().get(var) {
        return Some(dir.clone());
    }
    let raw = std::env::var(var).ok().filter(|v| !v.is_empty())?;
    let dir = expand_home(&raw);
    (!is_managed_home(&dir)).then_some(dir)
}

fn env_home_or(var: &str, dot: &str) -> PathBuf {
    chosen_home(var).unwrap_or_else(|| home_dot_dir(dot))
}

pub fn claude_config_dir() -> PathBuf {
    env_home_or("CLAUDE_CONFIG_DIR", "claude")
}

/// `CLAUDE_CONFIG_DIR` as the Claude install actually sees it. `Some` means the Keychain service
/// is scoped to that directory; `None` means the plain, unscoped service.
pub fn claude_scoped_config_dir() -> Option<PathBuf> {
    chosen_home("CLAUDE_CONFIG_DIR")
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
    chosen_home("PI_CODING_AGENT_DIR").unwrap_or_else(|| home_dot_dir("pi").join("agent"))
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
        let _guard = crate::ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        std::env::set_var("AAS_CONFIG_DIR", "/tmp/aas-test-cfg");
        assert_eq!(asx_config_dir(), PathBuf::from("/tmp/aas-test-cfg"));
        std::env::remove_var("AAS_CONFIG_DIR");
    }

    #[test]
    fn inherited_profile_home_is_not_the_system_home() {
        let _guard = crate::ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        std::env::set_var("AAS_CONFIG_DIR", "/tmp/aas-test-managed-home");
        let profile = profiles_dir().join("claude-someone_claude");

        // What `aas exec` exported for an earlier launch, inherited through a shell or a tmux
        // server started inside it: a profile aas chose, not the user's install.
        std::env::set_var("CLAUDE_CONFIG_DIR", &profile);
        assert!(is_managed_home(&profile));
        assert_eq!(claude_config_dir(), home_dir().join(".claude"));
        assert_eq!(claude_scoped_config_dir(), None);
        assert_eq!(system_home_for("claude"), Some(home_dir().join(".claude")));

        // A home the caller picked for themselves still decides where the install lives.
        std::env::set_var("CLAUDE_CONFIG_DIR", "/tmp/my-own-claude");
        assert_eq!(claude_config_dir(), PathBuf::from("/tmp/my-own-claude"));
        assert_eq!(
            claude_scoped_config_dir(),
            Some(PathBuf::from("/tmp/my-own-claude"))
        );

        // An override is this process's own decision (`aas login`), profile home or not.
        set_home_override("CLAUDE_CONFIG_DIR", &profile);
        assert_eq!(claude_config_dir(), profile);
        assert_eq!(claude_scoped_config_dir(), Some(profile));
        clear_home_override("CLAUDE_CONFIG_DIR");

        std::env::remove_var("CLAUDE_CONFIG_DIR");
        std::env::remove_var("AAS_CONFIG_DIR");
    }

    #[test]
    fn pi_defaults_to_nested_agent_dir() {
        std::env::remove_var("PI_CODING_AGENT_DIR");
        assert_eq!(pi_agent_dir(), home_dir().join(".pi/agent"));
    }
}
