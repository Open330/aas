//! macOS Keychain **service-name derivation** (pure). The `security` CLI read/write/delete
//! lives in `aas-providers` (it shells out); this module only reproduces asx's
//! `getClaudeKeychainService` so the same entries are found byte-for-byte.
//!
//! service = `"Claude Code-credentials"` when no config dir, else
//! `"Claude Code-credentials-" + hex(sha256(configDir))[..8]`.

use sha2::{Digest, Sha256};
use std::path::Path;

pub const SERVICE_PREFIX: &str = "Claude Code-credentials";

pub fn claude_keychain_service(config_dir: Option<&Path>) -> String {
    match config_dir {
        None => SERVICE_PREFIX.to_string(),
        Some(dir) => {
            let mut hasher = Sha256::new();
            // asx hashes the string form of the path (Node `crypto.createHash('sha256').update(dir)`).
            hasher.update(dir.to_string_lossy().as_bytes());
            let digest = hasher.finalize();
            let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
            format!("{SERVICE_PREFIX}-{}", &hex[..8])
        }
    }
}

/// asx `currentUser()` — the keychain account name.
pub fn current_user() -> String {
    std::env::var("USER")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            #[cfg(unix)]
            {
                // best-effort fallback via getlogin-equivalent is not in std; USER covers macOS.
                None
            }
            #[cfg(not(unix))]
            {
                std::env::var("USERNAME").ok()
            }
        })
        .unwrap_or_else(|| "user".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_dir_is_bare_prefix() {
        assert_eq!(claude_keychain_service(None), "Claude Code-credentials");
    }

    #[test]
    fn scoped_service_is_prefix_plus_8_hex() {
        let s = claude_keychain_service(Some(Path::new("/some/config/dir")));
        assert!(s.starts_with("Claude Code-credentials-"));
        let suffix = &s["Claude Code-credentials-".len()..];
        assert_eq!(suffix.len(), 8);
        assert!(suffix.chars().all(|c| c.is_ascii_hexdigit()));
        // deterministic
        assert_eq!(s, claude_keychain_service(Some(Path::new("/some/config/dir"))));
    }
}
