//! Per-profile credential storage. Mirrors asx `storage/secure-store.ts`.
//!
//! Each profile owns a `0700` home under the profiles dir; file providers keep the credential
//! in that home under the native filename, while **macOS Claude** keeps it in a profile-scoped
//! Keychain service (derived in [`crate::keychain`]) via the `security` CLI. Service/account
//! identifiers remain compatible with asx; secret values are sent over stdin, never argv.

use crate::keychain::{
    claude_keychain_service, delete_credential as keychain_delete,
    read_credential as keychain_read, write_credential as keychain_write,
};
use crate::naming::{profile_credential_path, profile_home};
use crate::store::AccountStore;
use std::fs::OpenOptions;
use std::io;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

static SECRET_WRITE_SEQ: AtomicU64 = AtomicU64::new(0);

fn is_mac_claude(provider: &str) -> bool {
    cfg!(target_os = "macos") && provider.to_lowercase().contains("claude")
}

fn claude_profile_service(provider: &str, name: &str) -> String {
    claude_keychain_service(Some(&profile_home(provider, name)))
}

// ---- public API ----

pub fn set_secret(provider: &str, name: &str, value: &str) -> io::Result<()> {
    validate_storage_key(provider, name)?;
    if is_mac_claude(provider) {
        keychain_write(&claude_profile_service(provider, name), value)?;
        remove_file_if_exists(&profile_credential_path(provider, name))?;
        return Ok(());
    }
    write_secret_file(provider, name, value)
}

/// Write the credential straight to the profile-home file, bypassing the keychain. Import
/// fallback for when the OS keychain isn't writable — e.g. a non-interactive SSH session, where
/// macOS keeps the login keychain locked. `get_secret` reads this file when the keychain has no
/// entry, so the credential stays usable.
pub fn set_secret_file(provider: &str, name: &str, value: &str) -> io::Result<()> {
    validate_storage_key(provider, name)?;
    write_secret_file(provider, name, value)
}

fn write_secret_file(provider: &str, name: &str, value: &str) -> io::Result<()> {
    let p = profile_credential_path(provider, name);
    write_restricted_file(&p, value)
}

/// Atomically replace a credential/config file with owner-only permissions.
pub fn write_restricted_file(path: &Path, value: &str) -> io::Result<()> {
    let home = path
        .parent()
        .ok_or_else(|| io::Error::other("credential path has no parent directory"))?;
    std::fs::create_dir_all(home)?;
    set_0700(home);
    let p = path;
    let seq = SECRET_WRITE_SEQ.fetch_add(1, Ordering::Relaxed);
    let file_name = p
        .file_name()
        .and_then(|v| v.to_str())
        .unwrap_or("credential");
    let tmp = home.join(format!(".{file_name}.{}.{}.tmp", std::process::id(), seq));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| -> io::Result<()> {
        let mut file = options.open(&tmp)?;
        file.write_all(value.as_bytes())?;
        file.sync_all()?;
        crate::store::atomic_replace(&tmp, p)?;
        set_0600(p);
        crate::store::sync_dir(home)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

fn validate_storage_key(provider: &str, name: &str) -> io::Result<()> {
    AccountStore::open_default()
        .validate_account_identity(provider, name)
        .map_err(|e| io::Error::other(e.to_string()))
}

fn remove_file_if_exists(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

fn remove_dir_if_exists(path: &Path) -> io::Result<()> {
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

fn path_exists(path: &Path) -> io::Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

fn already_exists(provider: &str, name: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!("Credential destination already exists for {provider}/{name}"),
    )
}

fn rollback_keychain(provider: &str, from: &str, to: &str, raw: &str) {
    let _ = keychain_write(&claude_profile_service(provider, from), raw);
    let _ = keychain_delete(&claude_profile_service(provider, to));
}

pub fn rename_secret(provider: &str, old_name: &str, new_name: &str) -> io::Result<()> {
    validate_storage_key(provider, new_name)?;
    rename_secret_unchecked(provider, old_name, new_name)
}

pub(crate) fn rename_secret_unchecked(
    provider: &str,
    old_name: &str,
    new_name: &str,
) -> io::Result<()> {
    if old_name.is_empty() || new_name.is_empty() || old_name == new_name {
        return Err(io::Error::other(
            "Invalid rename: old and new names must differ and be non-empty",
        ));
    }
    let from = profile_home(provider, old_name);
    let to = profile_home(provider, new_name);
    if path_exists(&to)? {
        return Err(already_exists(provider, new_name));
    }

    let raw = if is_mac_claude(provider) {
        if keychain_read(&claude_profile_service(provider, new_name)).is_some() {
            return Err(already_exists(provider, new_name));
        }
        keychain_read(&claude_profile_service(provider, old_name))
    } else {
        None
    };

    if !path_exists(&from)? && raw.is_none() {
        return Ok(());
    }

    if let Some(raw) = &raw {
        keychain_write(&claude_profile_service(provider, new_name), raw)?;
        if let Err(error) = keychain_delete(&claude_profile_service(provider, old_name)) {
            let _ = keychain_delete(&claude_profile_service(provider, new_name));
            return Err(error);
        }
    }

    if path_exists(&from)? {
        std::fs::create_dir_all(crate::platform::profiles_dir())?;
        if let Err(error) = std::fs::rename(&from, &to) {
            if let Some(raw) = &raw {
                rollback_keychain(provider, old_name, new_name, raw);
            }
            return Err(error);
        }
    }
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

pub fn delete_secret(provider: &str, name: &str) -> io::Result<()> {
    if is_mac_claude(provider) {
        keychain_delete(&claude_profile_service(provider, name))?;
    }
    // Drop the whole profile home so no native state lingers.
    remove_dir_if_exists(&profile_home(provider, name))
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
        let dir = std::env::temp_dir().join(format!(
            "aas-secure-{}-{:p}",
            std::process::id(),
            &() as *const _
        ));
        std::env::set_var("AAS_CONFIG_DIR", &dir);
        set_secret("codex", "t.codex", "hello-cred").unwrap();
        assert_eq!(
            get_secret("codex", "t.codex").as_deref(),
            Some("hello-cred")
        );
        delete_secret("codex", "t.codex").unwrap();
        assert_eq!(get_secret("codex", "t.codex"), None);
        std::env::remove_var("AAS_CONFIG_DIR");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
