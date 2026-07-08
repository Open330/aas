//! macOS Keychain **service-name derivation** (pure). The `security` CLI read/write/delete
//! lives in `aas-providers` (it shells out); this module only reproduces asx's
//! `getClaudeKeychainService` so the same entries are found byte-for-byte.
//!
//! service = `"Claude Code-credentials"` when no config dir, else
//! `"Claude Code-credentials-" + hex(sha256(configDir))[..8]`.

use sha2::{Digest, Sha256};
use std::io;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Mutex;

pub const SERVICE_PREFIX: &str = "Claude Code-credentials";

/// Serializes `security` CLI access across the process. The parallel usage fan-out
/// (`snapshot()` spawns one task per account) otherwise fires several `security
/// find-generic-password` invocations at once, and under that concurrency some spuriously
/// return `errSecItemNotFound` (44) for items that exist and read fine sequentially — surfacing
/// as a false "No stored credential". A brief lock around the CLI (reads are milliseconds)
/// removes the false negative; the slow network fetches still run fully in parallel.
static SECURITY_CLI_LOCK: Mutex<()> = Mutex::new(());

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

/// Read a generic-password credential from the macOS Keychain via the `security` CLI.
/// Returns `None` on any failure or empty value. (No-op-ish off macOS: `security` missing → None.)
pub fn read_credential(service: &str) -> Option<String> {
    let _guard = SECURITY_CLI_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let out = Command::new("security")
        .args(["find-generic-password", "-s", service, "-a", &current_user(), "-w"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Write (create or update via `-U`) a generic-password credential.
pub fn write_credential(service: &str, raw: &str) -> io::Result<()> {
    let _guard = SECURITY_CLI_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let status = Command::new("security")
        .args(["add-generic-password", "-s", service, "-a", &current_user(), "-w", raw, "-U"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("security add-generic-password failed for {service}")))
    }
}

/// Delete a generic-password credential (errors ignored, matching asx).
pub fn delete_credential(service: &str) {
    let _guard = SECURITY_CLI_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _ = Command::new("security")
        .args(["delete-generic-password", "-s", service, "-a", &current_user()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
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
