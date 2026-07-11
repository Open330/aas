//! macOS Keychain service-name derivation and serialized `security` CLI access. The naming
//! reproduces asx's `getClaudeKeychainService` so existing entries remain discoverable.
//!
//! service = `"Claude Code-credentials"` when no config dir, else
//! `"Claude Code-credentials-" + hex(sha256(configDir))[..8]`.

use sha2::{Digest, Sha256};
use std::io;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Mutex;

pub const SERVICE_PREFIX: &str = "Claude Code-credentials";

/// Serializes `security` CLI access so no two invocations touch the Keychain at once. The
/// parallel usage fan-out (and a *second* aas process, e.g. the menubar app fetching while you
/// run `aas usage`) otherwise make `security find-generic-password` spuriously return
/// `errSecItemNotFound` for items that exist and read fine alone — surfacing as a false
/// "No stored credential". A process-wide mutex covers our own threads; an advisory `flock`
/// covers other processes. Reads are milliseconds, so this is invisible; the slow network
/// fetches still run fully in parallel.
static SECURITY_CLI_LOCK: Mutex<()> = Mutex::new(());

fn with_keychain_lock<T>(f: impl FnOnce() -> T) -> T {
    let _inproc = SECURITY_CLI_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        use std::os::unix::io::AsRawFd;
        let dir = crate::platform::asx_config_dir();
        let _ = std::fs::create_dir_all(&dir);
        if let Ok(file) = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .mode(0o600)
            .open(dir.join(".keychain.lock"))
        {
            // Advisory cross-process lock; auto-released when `file`'s fd closes at scope end.
            unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
            let out = f();
            unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
            return out;
        }
    }
    f()
}

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
        .or({
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
    let user = current_user();
    with_keychain_lock(|| {
        let out = Command::new("security")
            .args(["find-generic-password", "-s", service, "-a", &user, "-w"])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (!s.is_empty()).then_some(s)
    })
}

fn quote_security_arg(value: &str) -> io::Result<String> {
    if value.chars().any(char::is_control) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "security argument contains a control character",
        ));
    }
    Ok(format!(
        "\"{}\"",
        value.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}

fn add_generic_password_command(service: &str, user: &str, raw: &str) -> io::Result<String> {
    let hex: String = raw
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok(format!(
        "add-generic-password -s {} -a {} -U -X {hex}\n",
        quote_security_arg(service)?,
        quote_security_arg(user)?
    ))
}

/// Write (create or update via `-U`) a generic-password credential.
pub fn write_credential(service: &str, raw: &str) -> io::Result<()> {
    let user = current_user();
    with_keychain_lock(|| {
        // `security ... -w` without an argv value reads directly from the terminal and prompts
        // twice; it does not consume our piped stdin. Interactive mode instead accepts an entire
        // command over stdin. `-X` carries the password as hex inside that private pipe, keeping
        // OAuth JSON/API keys out of process metadata without triggering a terminal prompt.
        let command = add_generic_password_command(service, &user, raw)?;
        let mut child = Command::new("security")
            .args(["-q", "-i"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("security stdin was not available"))?;
        stdin.write_all(command.as_bytes())?;
        drop(stdin);
        let status = child.wait()?;
        if status.success() {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "security add-generic-password failed for {service}"
            )))
        }
    })
}

/// Delete a generic-password credential and surface failures to the caller.
pub fn delete_credential(service: &str) -> io::Result<()> {
    let user = current_user();
    with_keychain_lock(|| {
        let status = Command::new("security")
            .args(["delete-generic-password", "-s", service, "-a", &user])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;
        if status.success() || status.code() == Some(44) {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "security delete-generic-password failed for {service}"
            )))
        }
    })
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
        assert_eq!(
            s,
            claude_keychain_service(Some(Path::new("/some/config/dir")))
        );
    }

    #[test]
    fn keychain_write_command_keeps_raw_secret_out_of_process_arguments() {
        let command = add_generic_password_command(
            "Claude Code-credentials-abcd1234",
            "user name",
            r#"{"token":"very-secret"}"#,
        )
        .unwrap();
        assert!(command.starts_with("add-generic-password "));
        assert!(command.contains("-a \"user name\""));
        assert!(command.contains("-X 7b22746f6b656e22"));
        assert!(!command.contains("very-secret"));
        assert!(add_generic_password_command("service", "bad\nuser", "secret").is_err());
    }
}
