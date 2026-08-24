//! Adopt existing `asx` state, and export/import a portable credential **bundle** for moving
//! all accounts to another host.
//!
//! Because `aas` defaults to asx's on-disk locations, plain adoption (`inspect`) is usually a
//! no-op. The bundle is for host-to-host migration: `export_bundle` collects every account +
//! its credential; `import_bundle` recreates them (writing each secret to the local keychain /
//! profile home).

use aas_core::model::AccountRecord;
use aas_core::naming::normalize_provider_key;
use aas_core::secure_store;
use aas_core::store::{AccountStore, StoreError};
use age::secrecy::SecretString;
use serde::{Deserialize, Serialize};

/// Summary of what an adopt/inspect pass found.
#[derive(Debug, Default)]
pub struct ImportReport {
    pub accounts: usize,
    pub with_profile_home: usize,
    pub missing_credential: Vec<String>,
}

/// Inspect the current (shared) asx config and report adoptable state. Non-destructive.
pub fn inspect() -> anyhow::Result<ImportReport> {
    let store = AccountStore::open_default();
    let accounts = store.list(None)?;
    let mut report = ImportReport {
        accounts: accounts.len(),
        ..Default::default()
    };
    for a in &accounts {
        let home = aas_core::naming::profile_home(&a.provider, &a.name);
        if home.exists() {
            report.with_profile_home += 1;
        }
    }
    Ok(report)
}

// ---- portable credential bundle (host → host migration) ----

#[derive(Serialize, Deserialize)]
pub struct BundleAccount {
    #[serde(flatten)]
    pub record: AccountRecord,
    /// The raw stored credential (OAuth JSON / auth.json / API key). May be absent.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub credential: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct Bundle {
    pub version: u32,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub exported_at: Option<String>,
    pub accounts: Vec<BundleAccount>,
}

/// Prefix emitted by the age file format. Used only to decide whether a passphrase prompt is
/// needed; [`decrypt_bundle`] still performs the authenticated format validation.
pub const AGE_HEADER: &[u8] = b"age-encryption.org/v1\n";

pub fn is_encrypted_bundle(data: &[u8]) -> bool {
    data.starts_with(AGE_HEADER)
}

/// Encrypt a portable bundle with age's passphrase recipient (scrypt + authenticated
/// encryption). The result is compatible with the `age` / `rage` command-line tools.
pub fn encrypt_bundle(bundle: &Bundle, passphrase: &str) -> anyhow::Result<Vec<u8>> {
    if passphrase.is_empty() {
        anyhow::bail!("vault passphrase cannot be empty");
    }
    let plaintext = serde_json::to_vec_pretty(bundle)?;
    let recipient = age::scrypt::Recipient::new(SecretString::from(passphrase.to_owned()));
    age::encrypt(&recipient, &plaintext)
        .map_err(|error| anyhow::anyhow!("could not encrypt vault: {error}"))
}

/// Decrypt and parse a passphrase-encrypted age bundle.
pub fn decrypt_bundle(data: &[u8], passphrase: &str) -> anyhow::Result<Bundle> {
    if passphrase.is_empty() {
        anyhow::bail!("vault passphrase cannot be empty");
    }
    let identity = age::scrypt::Identity::new(SecretString::from(passphrase.to_owned()));
    let plaintext = age::decrypt(&identity, data)
        .map_err(|error| anyhow::anyhow!("could not decrypt vault: {error}"))?;
    serde_json::from_slice(&plaintext).map_err(Into::into)
}

/// Collect every account + its credential into a portable bundle.
pub fn export_bundle() -> anyhow::Result<Bundle> {
    let store = AccountStore::open_default();
    let accounts = store
        .list(None)?
        .into_iter()
        .map(|a| -> anyhow::Result<BundleAccount> {
            let credential = secure_store::get_secret_result(&a.provider, &a.name)?;
            Ok(BundleAccount {
                record: a,
                credential,
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    Ok(Bundle {
        version: 1,
        exported_at: Some(aas_core::model::now_iso()),
        accounts,
    })
}

#[derive(Debug, Default)]
pub struct RestoreReport {
    pub accounts: usize,
    pub credentials: usize,
    /// Accounts skipped because the name is already used by a different provider locally.
    pub conflicts: Vec<String>,
    /// Imported accounts whose bundle entry had no credential.
    pub without_credential: Vec<String>,
    /// Imported accounts whose credential could not be stored at all.
    pub failed: Vec<String>,
}

fn restore_secret(provider: &str, name: &str, previous: Option<&str>) -> std::io::Result<()> {
    match previous {
        Some(raw) => secure_store::set_secret(provider, name, raw),
        None => secure_store::clear_secret_value(provider, name),
    }
}

fn restore_account(
    store: &AccountStore,
    provider: &str,
    name: &str,
    previous: Option<AccountRecord>,
) -> Result<(), StoreError> {
    match previous {
        Some(record) => store.add(record).map(|_| ()),
        None => store.remove(provider, name).map(|_| ()),
    }
}

/// Recreate accounts + credentials from a bundle on this host.
pub fn import_bundle(bundle: &Bundle) -> RestoreReport {
    let store = AccountStore::open_default();
    let mut report = RestoreReport::default();
    for ba in &bundle.accounts {
        let id = format!("{}/{}", ba.record.provider, ba.record.name);
        let provider_key = normalize_provider_key(&ba.record.provider);
        let _lifecycle = match aas_core::keyed_lock::acquire("credential-lifecycle", &provider_key)
        {
            Ok(lock) => lock,
            Err(error) => {
                report
                    .failed
                    .push(format!("{id}: could not acquire lifecycle lock: {error}"));
                continue;
            }
        };
        if let Err(error) = store.validate_account_identity(&ba.record.provider, &ba.record.name) {
            let detail = format!("{id}: {error}");
            if matches!(
                error,
                StoreError::NameConflict { .. } | StoreError::StorageConflict { .. }
            ) {
                report.conflicts.push(detail);
            } else {
                report.failed.push(detail);
            }
            continue;
        }
        let previous_account = match store.get(&ba.record.provider, &ba.record.name) {
            Ok(record) => record,
            Err(error) => {
                report.failed.push(format!("{id}: {error}"));
                continue;
            }
        };
        let previous_secret =
            match secure_store::get_secret_result(&ba.record.provider, &ba.record.name) {
                Ok(secret) => secret,
                Err(error) => {
                    report.failed.push(format!(
                        "{id}: could not read existing credential before import: {error}"
                    ));
                    continue;
                }
            };

        if let Some(credential) = &ba.credential {
            if let Err(error) = secure_store::set_secret_with_safe_fallback(
                &ba.record.provider,
                &ba.record.name,
                credential,
            ) {
                let rollback = restore_secret(
                    &ba.record.provider,
                    &ba.record.name,
                    previous_secret.as_deref(),
                )
                .err();
                report.failed.push(format!(
                    "{id}: could not store credential: {error}; rollback={rollback:?}"
                ));
                continue;
            }
        }

        match store.add(ba.record.clone()) {
            Ok(_) => {
                report.accounts += 1;
                if ba.credential.is_some() {
                    report.credentials += 1;
                    aas_core::usage_cache::clear(&id);
                } else {
                    report.without_credential.push(id);
                }
            }
            Err(error) => {
                let account_rollback = restore_account(
                    &store,
                    &ba.record.provider,
                    &ba.record.name,
                    previous_account,
                )
                .err();
                let secret_rollback = if ba.credential.is_some() {
                    restore_secret(
                        &ba.record.provider,
                        &ba.record.name,
                        previous_secret.as_deref(),
                    )
                    .err()
                } else {
                    None
                };
                let detail = format!(
                    "{id}: {error}; account rollback={account_rollback:?}; credential rollback={secret_rollback:?}"
                );
                if matches!(
                    error,
                    StoreError::NameConflict { .. } | StoreError::StorageConflict { .. }
                ) {
                    report.conflicts.push(detail);
                } else {
                    report.failed.push(detail);
                }
            }
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn bundle_round_trip_preserves_account_metadata_and_credential() {
        let mut record = AccountRecord::new("codex", "work");
        record.share = Some(Vec::new());
        record.profile_type = Some(aas_core::model::ProfileType::Isolated);
        let bundle = Bundle {
            version: 1,
            exported_at: Some("2026-07-10T00:00:00.000Z".into()),
            accounts: vec![BundleAccount {
                record,
                credential: Some("secret".into()),
            }],
        };

        let json = serde_json::to_string(&bundle).unwrap();
        let decoded: Bundle = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.version, 1);
        assert_eq!(decoded.accounts[0].record.share, Some(Vec::new()));
        assert_eq!(
            decoded.accounts[0].record.profile_type,
            Some(aas_core::model::ProfileType::Isolated)
        );
        assert_eq!(decoded.accounts[0].credential.as_deref(), Some("secret"));
    }

    #[test]
    fn missing_optional_bundle_fields_are_backward_compatible() {
        let json = r#"{"version":1,"accounts":[{"provider":"zai","name":"work","addedAt":"2026-07-10T00:00:00.000Z"}]}"#;
        let decoded: Bundle = serde_json::from_str(json).unwrap();
        assert!(decoded.exported_at.is_none());
        assert!(decoded.accounts[0].credential.is_none());
    }

    #[test]
    fn encrypted_bundle_round_trip() {
        let bundle = Bundle {
            version: 1,
            exported_at: Some("2026-07-11T00:00:00.000Z".into()),
            accounts: vec![BundleAccount {
                record: AccountRecord::new("codex", "work"),
                credential: Some("very-secret".into()),
            }],
        };

        let encrypted = encrypt_bundle(&bundle, "correct horse battery staple").unwrap();
        assert!(is_encrypted_bundle(&encrypted));
        assert!(!String::from_utf8_lossy(&encrypted).contains("very-secret"));

        let decoded = decrypt_bundle(&encrypted, "correct horse battery staple").unwrap();
        assert_eq!(decoded.accounts[0].record.name, "work");
        assert_eq!(
            decoded.accounts[0].credential.as_deref(),
            Some("very-secret")
        );
        assert!(decrypt_bundle(&encrypted, "wrong passphrase").is_err());
    }

    #[test]
    fn credential_storage_failure_does_not_leave_account_metadata() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "aas-import-rollback-{}-{}",
            std::process::id(),
            nonce
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("profiles"), "blocks profile creation").unwrap();
        std::env::set_var("AAS_CONFIG_DIR", &dir);
        let bundle = Bundle {
            version: 1,
            exported_at: None,
            accounts: vec![BundleAccount {
                record: AccountRecord::new("codex", "victim"),
                credential: Some("secret".into()),
            }],
        };

        let report = import_bundle(&bundle);
        assert_eq!(report.accounts, 0);
        assert_eq!(report.credentials, 0);
        assert_eq!(report.failed.len(), 1);
        assert!(AccountStore::open_default().list(None).unwrap().is_empty());

        std::env::remove_var("AAS_CONFIG_DIR");
        let _ = std::fs::remove_dir_all(dir);
    }
}
