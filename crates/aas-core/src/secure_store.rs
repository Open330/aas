//! Per-profile credential storage. Mirrors asx `storage/secure-store.ts`.
//!
//! Each profile owns a `0700` home under the profiles dir; file providers keep the credential
//! in that home under the native filename, while **macOS Claude** keeps it in a profile-scoped
//! Keychain service (derived in [`crate::keychain`]) via the `security` CLI. The `security`
//! argv is byte-identical to asx so existing entries are found.

use crate::keychain::{claude_keychain_service, delete_credential as keychain_delete, read_credential as keychain_read, write_credential as keychain_write};
use crate::naming::{profile_credential_path, profile_home};
use std::io;
use std::path::Path;

fn is_mac_claude(provider: &str) -> bool {
    cfg!(target_os = "macos") && provider.to_lowercase().contains("claude")
}

fn claude_profile_service(provider: &str, name: &str) -> String {
    claude_keychain_service(Some(&profile_home(provider, name)))
}

// ---- public API ----

pub fn set_secret(provider: &str, name: &str, value: &str) -> io::Result<()> {
    if is_mac_claude(provider) {
        keychain_write(&claude_profile_service(provider, name), value)?;
        let _ = std::fs::remove_file(profile_credential_path(provider, name));
        return Ok(());
    }
    let home = profile_home(provider, name);
    std::fs::create_dir_all(&home)?;
    set_0700(&home);
    let p = profile_credential_path(provider, name);
    std::fs::write(&p, value)?;
    set_0600(&p);
    Ok(())
}

/// Write the credential straight to the profile-home file, bypassing the keychain. Import
/// fallback for when the OS keychain isn't writable — e.g. a non-interactive SSH session, where
/// macOS keeps the login keychain locked. `get_secret` reads this file when the keychain has no
/// entry, so the credential stays usable.
pub fn set_secret_file(provider: &str, name: &str, value: &str) -> io::Result<()> {
    let home = profile_home(provider, name);
    std::fs::create_dir_all(&home)?;
    set_0700(&home);
    let p = profile_credential_path(provider, name);
    std::fs::write(&p, value)?;
    set_0600(&p);
    Ok(())
}

pub fn get_secret(provider: &str, name: &str) -> Option<String> {
    if is_mac_claude(provider) {
        if let Some(v) = keychain_read(&claude_profile_service(provider, name)) {
            return Some(v);
        }
    }
    match std::fs::read_to_string(profile_credential_path(provider, name)) {
        Ok(s) if !s.is_empty() => Some(s),
        _ => None,
    }
}

pub fn delete_secret(provider: &str, name: &str) {
    if is_mac_claude(provider) {
        keychain_delete(&claude_profile_service(provider, name));
    }
    // Drop the whole profile home so no native state lingers.
    let _ = std::fs::remove_dir_all(profile_home(provider, name));
}

pub fn rename_secret(provider: &str, old_name: &str, new_name: &str) -> io::Result<()> {
    if old_name.is_empty() || new_name.is_empty() || old_name == new_name {
        return Err(io::Error::other("Invalid rename: old and new names must differ and be non-empty"));
    }
    let from = profile_home(provider, old_name);
    let to = profile_home(provider, new_name);

    let raw = if is_mac_claude(provider) {
        keychain_read(&claude_profile_service(provider, old_name))
    } else {
        None
    };

    if !from.exists() && raw.is_none() {
        return Err(io::Error::other(format!("No secret found for {provider}/{old_name}")));
    }

    if let Some(raw) = &raw {
        keychain_write(&claude_profile_service(provider, new_name), raw)?;
        keychain_delete(&claude_profile_service(provider, old_name));
    }

    std::fs::create_dir_all(crate::platform::profiles_dir())?;
    let _ = std::fs::remove_dir_all(&to);
    if from.exists() {
        std::fs::rename(&from, &to)?;
    }
    Ok(())
}

#[cfg(unix)]
fn set_0700(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
}
#[cfg(not(unix))]
fn set_0700(_path: &Path) {}

#[cfg(unix)]
fn set_0600(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}
#[cfg(not(unix))]
fn set_0600(_path: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_provider_roundtrip() {
        // Use a codex-style (file-backed) provider under a temp AAS_CONFIG_DIR.
        let dir = std::env::temp_dir().join(format!("aas-secure-{}-{:p}", std::process::id(), &() as *const _));
        std::env::set_var("AAS_CONFIG_DIR", &dir);
        set_secret("codex", "t.codex", "hello-cred").unwrap();
        assert_eq!(get_secret("codex", "t.codex").as_deref(), Some("hello-cred"));
        delete_secret("codex", "t.codex");
        assert_eq!(get_secret("codex", "t.codex"), None);
        std::env::remove_var("AAS_CONFIG_DIR");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
