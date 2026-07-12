//! Adopt existing `asx` state, and export/import a portable credential **bundle** for moving
//! all accounts to another host.
//!
//! Because `aas` defaults to asx's on-disk locations, plain adoption (`inspect`) is usually a
//! no-op. The bundle is for host-to-host migration: `export_bundle` collects every account +
//! its credential; `import_bundle` recreates them (writing each secret to the local keychain /
//! profile home).

use aas_core::model::AccountRecord;
use aas_core::secure_store;
use aas_core::store::AccountStore;
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
        .map(|a| {
            let credential = secure_store::get_secret(&a.provider, &a.name);
            BundleAccount {
                record: a,
                credential,
            }
        })
        .collect();
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

/// Recreate accounts + credentials from a bundle on this host.
pub fn import_bundle(bundle: &Bundle) -> RestoreReport {
    let store = AccountStore::open_default();
    let mut report = RestoreReport::default();
    for ba in &bundle.accounts {
        let id = format!("{}/{}", ba.record.provider, ba.record.name);
        match store.add(ba.record.clone()) {
            Ok(_) => {
                report.accounts += 1;
                match &ba.credential {
                    Some(c) => {
                        // Prefer native storage (keychain on macOS Claude); if that fails — e.g.
                        // a locked keychain over a non-interactive SSH session — fall back to the
                        // profile-home file, which get_secret reads when the keychain is empty.
                        let ok = secure_store::set_secret(&ba.record.provider, &ba.record.name, c)
                            .is_ok()
                            || secure_store::set_secret_file(
                                &ba.record.provider,
                                &ba.record.name,
                                c,
                            )
                            .is_ok();
                        if ok {
                            aas_core::usage_cache::clear(&format!(
                                "{}/{}",
                                ba.record.provider, ba.record.name
                            ));
                            report.credentials += 1;
                        } else {
                            report.failed.push(id);
                        }
                    }
                    None => report.without_credential.push(id),
                }
            }
            Err(_) => report.conflicts.push(id),
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
