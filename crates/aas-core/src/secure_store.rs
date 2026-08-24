//! Per-profile credential storage. Mirrors asx `storage/secure-store.ts`.
//!
//! Each profile owns a `0700` home under the profiles dir; file providers keep the credential
//! in that home under the native filename, while **macOS Claude** prefers a profile-scoped
//! Keychain service (derived in [`crate::keychain`]) via the `security` CLI. Large credentials
//! that cannot safely pass through its bounded interactive parser use Claude's owner-only profile
//! file instead. Service/account identifiers remain compatible with asx; secret values are sent
//! over stdin, never argv.

use crate::keychain::{
    claude_keychain_service, credential_fits_security_cli, delete_credential as keychain_delete,
    read_credential_result as keychain_read_result, write_credential as keychain_write,
};
use crate::naming::{profile_credential_path, profile_home};
use crate::store::AccountStore;
use std::fs::OpenOptions;
use std::io;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static SECRET_WRITE_SEQ: AtomicU64 = AtomicU64::new(0);
static SECRET_REMOVE_SEQ: AtomicU64 = AtomicU64::new(0);

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
        let service = claude_profile_service(provider, name);
        let file = profile_credential_path(provider, name);
        let keychain_value = keychain_read_result(&service)?;

        // Native Claude login has already written this exact profile credential, either to its
        // scoped Keychain item or (on newer/headless setups) to `.credentials.json`. Avoid an
        // unnecessary rewrite after `aas login`; it can change ACL ownership and large OAuth JSON
        // exceeds the `security -i` parser limit.
        if keychain_value.as_deref() == Some(value) {
            remove_file_if_exists(&file)?;
            return Ok(());
        }
        let file_matches = std::fs::read_to_string(&file)
            .map(|current| current == value)
            .unwrap_or(false);
        if keychain_value.is_none() && file_matches {
            return Ok(());
        }

        if !credential_fits_security_cli(value) {
            // Claude reads this owner-only fallback whenever the scoped Keychain entry is absent.
            // Write first, then remove any stale entry so readers never observe no credential.
            write_secret_file(provider, name, value)?;
            if let Err(error) = keychain_delete(&service) {
                let _ = remove_file_if_exists(&file);
                return Err(error);
            }
            return Ok(());
        }

        keychain_write(&service, value)?;
        remove_file_if_exists(&file)?;
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

/// Import-only fallback. A profile file is safe only when Keychain explicitly reports that no
/// item exists; a locked/unavailable Keychain may contain a stale value that would win later.
pub fn set_secret_with_safe_fallback(provider: &str, name: &str, value: &str) -> io::Result<()> {
    match set_secret(provider, name, value) {
        Ok(()) => Ok(()),
        Err(primary) if is_mac_claude(provider) => {
            let service = claude_profile_service(provider, name);
            match keychain_read_result(&service) {
                Ok(None) => set_secret_file(provider, name, value),
                Ok(Some(_)) => Err(io::Error::other(format!(
                    "{primary}; refusing file fallback because an existing Keychain credential would take precedence"
                ))),
                Err(read_error) => Err(io::Error::other(format!(
                    "{primary}; refusing file fallback because Keychain state could not be verified: {read_error}"
                ))),
            }
        }
        Err(primary) => Err(primary),
    }
}

fn write_secret_file(provider: &str, name: &str, value: &str) -> io::Result<()> {
    let p = profile_credential_path(provider, name);
    write_restricted_file(&p, value)
}

/// Atomically replace a credential/config file with owner-only permissions.
pub fn write_restricted_file(path: &Path, value: &str) -> io::Result<()> {
    write_restricted_bytes(path, value.as_bytes())
}

/// Atomically replace a binary credential/config file with owner-only permissions.
pub fn write_restricted_bytes(path: &Path, value: &[u8]) -> io::Result<()> {
    write_restricted_bytes_inner(path, value, true)
}

/// Write an explicit user-selected output without changing permissions on its existing parent
/// directory (which may be shared, such as `/tmp`). Newly created parents remain owner-only.
pub fn write_private_output(path: &Path, value: &[u8]) -> io::Result<()> {
    write_restricted_bytes_inner(path, value, false)
}

/// Create a new owner-only output without following symlinks or overwriting an existing path.
/// The final path is mode 0600 from its first byte, closing the permission and TOCTOU windows used
/// by portable credential exports.
pub fn write_private_new(path: &Path, value: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("output path has no parent directory"))?;
    let parent_existed = parent.exists();
    std::fs::create_dir_all(parent)?;
    if !parent_existed {
        set_0700(parent)?;
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut created = false;
    let result = (|| -> io::Result<()> {
        let mut file = options.open(path)?;
        created = true;
        file.write_all(value)?;
        file.sync_all()?;
        set_0600(path)?;
        crate::store::sync_dir(parent)
    })();
    if result.is_err() && created {
        let _ = std::fs::remove_file(path);
    }
    result
}

fn write_restricted_bytes_inner(
    path: &Path,
    value: &[u8],
    protect_existing_parent: bool,
) -> io::Result<()> {
    let home = path
        .parent()
        .ok_or_else(|| io::Error::other("credential path has no parent directory"))?;
    let parent_existed = home.exists();
    std::fs::create_dir_all(home)?;
    if protect_existing_parent || !parent_existed {
        set_0700(home)?;
    }
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
        file.write_all(value)?;
        file.sync_all()?;
        crate::store::atomic_replace(&tmp, p)?;
        set_0600(p)?;
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
        if keychain_read_result(&claude_profile_service(provider, new_name))?.is_some() {
            return Err(already_exists(provider, new_name));
        }
        keychain_read_result(&claude_profile_service(provider, old_name))?
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

pub fn get_secret_result(provider: &str, name: &str) -> io::Result<Option<String>> {
    if is_mac_claude(provider) {
        if let Some(v) = keychain_read_result(&claude_profile_service(provider, name))? {
            return Ok(Some(v));
        }
    }
    match std::fs::read_to_string(profile_credential_path(provider, name)) {
        Ok(s) if !s.is_empty() => Ok(Some(s)),
        Ok(_) => Ok(None),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

pub fn get_secret(provider: &str, name: &str) -> Option<String> {
    get_secret_result(provider, name).ok().flatten()
}

/// Remove only the native credential value while preserving the rest of the profile home. This is
/// used by transaction rollback; account removal uses `quarantine_secret` for the full tree.
pub fn clear_secret_value(provider: &str, name: &str) -> io::Result<()> {
    if is_mac_claude(provider) {
        keychain_delete(&claude_profile_service(provider, name))?;
    }
    remove_file_if_exists(&profile_credential_path(provider, name))
}

/// Reversible removal handle. The live profile is atomically moved out of its public location;
/// recursive cleanup happens only after metadata commits. A cleanup failure therefore leaves a
/// private tombstone instead of a half-deleted live profile.
pub struct SecretQuarantine {
    provider: String,
    name: String,
    original_home: PathBuf,
    tombstone: Option<PathBuf>,
    keychain_value: Option<String>,
}

impl SecretQuarantine {
    pub fn rollback(mut self) -> io::Result<()> {
        let mut errors = Vec::new();
        if let Some(tombstone) = self.tombstone.take() {
            if let Err(error) = std::fs::rename(&tombstone, &self.original_home) {
                errors.push(format!("profile={error}"));
            }
        }
        if let Some(raw) = self.keychain_value.take() {
            if let Err(error) =
                keychain_write(&claude_profile_service(&self.provider, &self.name), &raw)
            {
                errors.push(format!("keychain={error}"));
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(io::Error::other(errors.join(", ")))
        }
    }

    /// Permanently clean the quarantined tree. If an open NFS file prevents cleanup, the error
    /// includes the tombstone path; the logical credential remains removed and can be collected
    /// after the process holding the file exits.
    pub fn commit(mut self) -> io::Result<()> {
        let Some(tombstone) = self.tombstone.take() else {
            return Ok(());
        };
        remove_dir_if_exists(&tombstone).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!(
                    "deferred profile cleanup at {}: {error}",
                    tombstone.display()
                ),
            )
        })
    }
}

pub fn quarantine_secret(provider: &str, name: &str) -> io::Result<SecretQuarantine> {
    let original_home = profile_home(provider, name);
    let keychain_value = if is_mac_claude(provider) {
        keychain_read_result(&claude_profile_service(provider, name))?
    } else {
        None
    };
    let tombstone = if path_exists(&original_home)? {
        let parent = crate::platform::profiles_dir();
        std::fs::create_dir_all(&parent)?;
        set_0700(&parent)?;
        let seq = SECRET_REMOVE_SEQ.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!(
            ".deleted-{}.{}.{}",
            crate::naming::safe_profile_dir_name(provider, name),
            std::process::id(),
            seq
        ));
        std::fs::rename(&original_home, &path)?;
        Some(path)
    } else {
        None
    };

    if is_mac_claude(provider) {
        if let Err(error) = keychain_delete(&claude_profile_service(provider, name)) {
            let mut rollback_errors = Vec::new();
            if let Some(path) = &tombstone {
                if let Err(rollback) = std::fs::rename(path, &original_home) {
                    rollback_errors.push(format!("profile={rollback}"));
                }
            }
            if let Some(raw) = &keychain_value {
                if let Err(rollback) = keychain_write(&claude_profile_service(provider, name), raw)
                {
                    rollback_errors.push(format!("keychain={rollback}"));
                }
            }
            return Err(io::Error::other(format!(
                "{error}; rollback: {}",
                if rollback_errors.is_empty() {
                    "completed".to_string()
                } else {
                    rollback_errors.join(", ")
                }
            )));
        }
    }

    Ok(SecretQuarantine {
        provider: provider.to_string(),
        name: name.to_string(),
        original_home,
        tombstone,
        keychain_value,
    })
}

/// Retry cleanup of tombstones left by an earlier logical removal. Callers must hold the same
/// provider lifecycle lock used by `quarantine_secret`, so an in-flight rollback cannot race this
/// garbage collection.
pub fn cleanup_quarantines(provider: &str, name: &str) -> io::Result<usize> {
    let parent = crate::platform::profiles_dir();
    let prefix = format!(
        ".deleted-{}.",
        crate::naming::safe_profile_dir_name(provider, name)
    );
    let entries = match std::fs::read_dir(&parent) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    let mut removed = 0;
    for entry in entries {
        let entry = entry?;
        let file_name = entry.file_name();
        if !file_name.to_string_lossy().starts_with(&prefix) {
            continue;
        }
        let file_type = entry.file_type()?;
        if !file_type.is_dir() || file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        remove_dir_if_exists(&path).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("deferred profile cleanup at {}: {error}", path.display()),
            )
        })?;
        removed += 1;
    }
    Ok(removed)
}

pub fn delete_secret(provider: &str, name: &str) -> io::Result<()> {
    quarantine_secret(provider, name)?.commit()
}

#[cfg(unix)]
fn set_0700(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
}
#[cfg(not(unix))]
fn set_0700(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn set_0600(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}
#[cfg(not(unix))]
fn set_0600(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn file_provider_roundtrip() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
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

        #[cfg(target_os = "macos")]
        {
            let store = AccountStore::open_default();
            store
                .add(crate::model::AccountRecord::new("claude", "large.test"))
                .unwrap();
            let raw = "a".repeat(crate::keychain::SECURITY_CLI_MAX_PASSWORD_BYTES + 1);
            set_secret("claude", "large.test", &raw).unwrap();
            assert_eq!(
                std::fs::read_to_string(profile_credential_path("claude", "large.test")).unwrap(),
                raw
            );
            // The idempotent path used immediately after native Claude login must also succeed.
            set_secret("claude", "large.test", &raw).unwrap();
            assert_eq!(
                get_secret("claude", "large.test").as_deref(),
                Some(raw.as_str())
            );
            delete_secret("claude", "large.test").unwrap();
        }

        std::env::remove_var("AAS_CONFIG_DIR");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn quarantined_profile_can_be_fully_rolled_back() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let dir = std::env::temp_dir().join(format!(
            "aas-quarantine-test-{}-{}",
            std::process::id(),
            SECRET_REMOVE_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::env::set_var("AAS_CONFIG_DIR", &dir);
        let store = AccountStore::open_default();
        store
            .add(crate::model::AccountRecord::new("codex", "victim"))
            .unwrap();
        set_secret("codex", "victim", "credential").unwrap();
        let home = profile_home("codex", "victim");
        std::fs::write(home.join("settings.json"), "keep-me").unwrap();

        let quarantine = quarantine_secret("codex", "victim").unwrap();
        assert!(!home.exists());
        quarantine.rollback().unwrap();
        assert_eq!(get_secret("codex", "victim").as_deref(), Some("credential"));
        assert_eq!(
            std::fs::read_to_string(home.join("settings.json")).unwrap(),
            "keep-me"
        );

        std::env::remove_var("AAS_CONFIG_DIR");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn deferred_quarantine_cleanup_is_scoped_to_one_account() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let dir = std::env::temp_dir().join(format!(
            "aas-quarantine-cleanup-test-{}-{}",
            std::process::id(),
            SECRET_REMOVE_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::env::set_var("AAS_CONFIG_DIR", &dir);
        let profiles = crate::platform::profiles_dir();
        let target = profiles.join(".deleted-codex-victim.1.1");
        let unrelated = profiles.join(".deleted-codex-other.1.1");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::create_dir_all(&unrelated).unwrap();
        std::fs::write(target.join("stale"), "x").unwrap();

        assert_eq!(cleanup_quarantines("codex", "victim").unwrap(), 1);
        assert!(!target.exists());
        assert!(unrelated.exists());

        std::env::remove_var("AAS_CONFIG_DIR");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn restricted_writer_publishes_mode_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!(
            "aas-restricted-test-{}-{}",
            std::process::id(),
            SECRET_WRITE_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let path = dir.join("bundle.age");
        write_restricted_bytes(&path, b"secret").unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn private_new_output_never_overwrites_or_follows_symlinks() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let dir = std::env::temp_dir().join(format!(
            "aas-private-output-test-{}-{}",
            std::process::id(),
            SECRET_WRITE_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let output = dir.join("bundle.json");
        write_private_new(&output, b"first").unwrap();
        assert!(write_private_new(&output, b"second").is_err());
        assert_eq!(std::fs::read(&output).unwrap(), b"first");
        assert_eq!(
            std::fs::metadata(&output).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o755
        );

        let target = dir.join("target");
        std::fs::write(&target, "untouched").unwrap();
        let link = dir.join("linked-export");
        symlink(&target, &link).unwrap();
        assert!(write_private_new(&link, b"secret").is_err());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "untouched");
        let _ = std::fs::remove_dir_all(dir);
    }
}
