//! Account store + active-marker operations. Mirrors asx `storage/account-store.ts`.
//!
//! `AccountStore` is parameterized by its config directory so it is trivially testable; use
//! [`AccountStore::open_default`] in the app and [`AccountStore::at`] in tests.

use crate::model::{now_iso, AccountRecord, ProfileType, Store};
use fs2::FileExt;
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static WRITE_SEQ: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error(
        "Name \"{name}\" is already used by provider \"{provider}\". Account names must be unique."
    )]
    NameConflict { name: String, provider: String },
    #[error("Name \"{0}\" is ambiguous across providers; specify the provider.")]
    Ambiguous(String),
    #[error("Account {provider}/{name} not found")]
    NotFound { provider: String, name: String },
    #[error("Invalid rename: old and new names must be different and non-empty")]
    InvalidRename,
    #[error("Account store {path} is corrupt: {source}")]
    Corrupt {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("Unsupported account store version {0}; expected version 1")]
    UnsupportedVersion(u32),
    #[error(
        "Account {provider}/{name} maps to the same profile directory as {existing_provider}/{existing_name}"
    )]
    StorageConflict {
        provider: String,
        name: String,
        existing_provider: String,
        existing_name: String,
    },
    #[error("Account store invariant violation: {0}")]
    Invariant(String),
    #[error("Account transaction failed: {0}")]
    Transaction(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

fn canonical_provider(p: &str) -> String {
    p.to_lowercase()
}

pub struct AccountStore {
    dir: PathBuf,
}

impl AccountStore {
    pub fn open_default() -> Self {
        Self {
            dir: crate::platform::asx_config_dir(),
        }
    }

    pub fn at(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    fn accounts_path(&self) -> PathBuf {
        self.dir.join("accounts.json")
    }

    fn active_path(&self) -> PathBuf {
        self.dir.join(".active.json")
    }

    fn lock_path(&self) -> PathBuf {
        self.dir.join(".accounts.lock")
    }

    fn ensure_dir(&self) -> Result<(), StoreError> {
        std::fs::create_dir_all(&self.dir)?;
        set_0700(&self.dir);
        Ok(())
    }

    fn open_lock(&self) -> Result<File, StoreError> {
        self.ensure_dir()?;
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        Ok(options.open(self.lock_path())?)
    }

    fn with_shared<T>(&self, f: impl FnOnce() -> Result<T, StoreError>) -> Result<T, StoreError> {
        let lock = self.open_lock()?;
        FileExt::lock_shared(&lock)?;
        let result = f();
        let unlock = FileExt::unlock(&lock).map_err(StoreError::Io);
        match (result, unlock) {
            (Err(e), _) => Err(e),
            (Ok(_), Err(e)) => Err(e),
            (Ok(value), Ok(())) => Ok(value),
        }
    }

    fn with_exclusive<T>(
        &self,
        f: impl FnOnce() -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        let lock = self.open_lock()?;
        FileExt::lock_exclusive(&lock)?;
        let result = f();
        let unlock = FileExt::unlock(&lock).map_err(StoreError::Io);
        match (result, unlock) {
            (Err(e), _) => Err(e),
            (Ok(_), Err(e)) => Err(e),
            (Ok(value), Ok(())) => Ok(value),
        }
    }

    fn load_unlocked(&self) -> Result<Store, StoreError> {
        let path = self.accounts_path();
        let body = match std::fs::read_to_string(&path) {
            Ok(body) => body,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Store::default()),
            Err(e) => return Err(StoreError::Io(e)),
        };
        let store: Store =
            serde_json::from_str(&body).map_err(|source| StoreError::Corrupt { path, source })?;
        self.validate_store(&store)?;
        Ok(store)
    }

    pub fn load(&self) -> Result<Store, StoreError> {
        self.with_shared(|| self.load_unlocked())
    }

    fn validate_store(&self, store: &Store) -> Result<(), StoreError> {
        if store.version != 1 {
            return Err(StoreError::UnsupportedVersion(store.version));
        }
        let mut names: HashMap<&str, (&str, &str)> = HashMap::new();
        let mut storage: HashMap<String, (&str, &str)> = HashMap::new();
        for account in &store.accounts {
            if account.name.is_empty() || account.provider.is_empty() {
                return Err(StoreError::Invariant(
                    "provider and account name must be non-empty".into(),
                ));
            }
            if let Some((provider, name)) = names.insert(
                account.name.as_str(),
                (account.provider.as_str(), account.name.as_str()),
            ) {
                return Err(StoreError::Invariant(format!(
                    "duplicate account name {} for {provider}/{name} and {}/{}",
                    account.name, account.provider, account.name
                )));
            }
            let key = crate::naming::safe_profile_dir_name(&account.provider, &account.name);
            if let Some((provider, name)) =
                storage.insert(key, (account.provider.as_str(), account.name.as_str()))
            {
                return Err(StoreError::StorageConflict {
                    provider: account.provider.clone(),
                    name: account.name.clone(),
                    existing_provider: provider.to_string(),
                    existing_name: name.to_string(),
                });
            }
        }
        Ok(())
    }

    fn validate_storage_key_in(
        &self,
        store: &Store,
        provider: &str,
        name: &str,
    ) -> Result<(), StoreError> {
        let requested = crate::naming::safe_profile_dir_name(provider, name);
        if let Some(existing) = store.accounts.iter().find(|account| {
            !(canonical_provider(&account.provider) == canonical_provider(provider)
                && account.name == name)
                && crate::naming::safe_profile_dir_name(&account.provider, &account.name)
                    == requested
        }) {
            return Err(StoreError::StorageConflict {
                provider: provider.to_string(),
                name: name.to_string(),
                existing_provider: existing.provider.clone(),
                existing_name: existing.name.clone(),
            });
        }
        Ok(())
    }

    pub fn validate_account_identity(&self, provider: &str, name: &str) -> Result<(), StoreError> {
        self.with_shared(|| {
            let store = self.load_unlocked()?;
            self.validate_storage_key_in(&store, provider, name)?;
            if let Some(conflict) = store.accounts.iter().find(|account| {
                account.name == name
                    && canonical_provider(&account.provider) != canonical_provider(provider)
            }) {
                return Err(StoreError::NameConflict {
                    name: name.to_string(),
                    provider: conflict.provider.clone(),
                });
            }
            Ok(())
        })
    }

    fn save_unlocked(&self, store: &Store) -> Result<(), StoreError> {
        self.validate_store(store)?;
        let body = serde_json::to_vec_pretty(store).map_err(|e| {
            StoreError::Invariant(format!("could not serialize account store: {e}"))
        })?;
        self.write_atomic(&self.accounts_path(), &body)
    }

    #[cfg(test)]
    fn save(&self, store: &Store) -> Result<(), StoreError> {
        self.with_exclusive(|| self.save_unlocked(store))
    }

    fn write_atomic(&self, path: &Path, body: &[u8]) -> Result<(), StoreError> {
        if path.exists() {
            let current = std::fs::read(path)?;
            let file_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("store");
            let backup = self.dir.join(format!("{file_name}.bak"));
            self.write_atomic_raw(&backup, &current)?;
        }
        self.write_atomic_raw(path, body)
    }

    fn write_atomic_raw(&self, path: &Path, body: &[u8]) -> Result<(), StoreError> {
        self.ensure_dir()?;
        let seq = WRITE_SEQ.fetch_add(1, Ordering::Relaxed);
        let tmp = self.dir.join(format!(
            ".{}.{}.{}.tmp",
            path.file_name().and_then(|n| n.to_str()).unwrap_or("store"),
            std::process::id(),
            seq
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let write_result = (|| -> Result<(), StoreError> {
            let mut file = options.open(&tmp)?;
            file.write_all(body)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            atomic_replace(&tmp, path)?;
            set_0600(path);
            sync_dir(&self.dir)?;
            Ok(())
        })();
        if write_result.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        write_result
    }

    pub fn list(&self, provider: Option<&str>) -> Result<Vec<AccountRecord>, StoreError> {
        let store = self.load()?;
        Ok(match provider {
            // asx `listAccounts` uses exact-string provider match.
            Some(p) => store
                .accounts
                .into_iter()
                .filter(|a| a.provider == p)
                .collect(),
            None => store.accounts,
        })
    }

    /// asx `addAccount`: global name uniqueness across providers; dedupe by (provider, name).
    pub fn add(&self, mut rec: AccountRecord) -> Result<AccountRecord, StoreError> {
        self.with_exclusive(|| {
            if rec.added_at.is_empty() {
                rec.added_at = now_iso();
            }
            let mut store = self.load_unlocked()?;
            self.validate_storage_key_in(&store, &rec.provider, &rec.name)?;
            if let Some(c) = store.accounts.iter().find(|a| {
                a.name == rec.name
                    && canonical_provider(&a.provider) != canonical_provider(&rec.provider)
            }) {
                return Err(StoreError::NameConflict {
                    name: rec.name.clone(),
                    provider: c.provider.clone(),
                });
            }
            if let Some(existing) = store.accounts.iter_mut().find(|a| {
                canonical_provider(&a.provider) == canonical_provider(&rec.provider)
                    && a.name == rec.name
            }) {
                rec.added_at = existing.added_at.clone();
                rec.label = rec.label.or_else(|| existing.label.clone());
                rec.email = rec.email.or_else(|| existing.email.clone());
                rec.share = rec.share.or_else(|| existing.share.clone());
                rec.profile_type = rec.profile_type.or(existing.profile_type);
                rec.meta = rec.meta.or_else(|| existing.meta.clone());
                *existing = rec.clone();
            } else {
                store.accounts.push(rec.clone());
            }
            self.save_unlocked(&store)?;
            Ok(rec.clone())
        })
    }

    /// asx `removeAccount` (canonical/lowercased provider match). Returns whether it shrank.
    pub fn remove(&self, provider: &str, name: &str) -> Result<bool, StoreError> {
        self.with_exclusive(|| {
            let prov = canonical_provider(provider);
            let mut store = self.load_unlocked()?;
            let before = store.accounts.len();
            store
                .accounts
                .retain(|a| !(canonical_provider(&a.provider) == prov && a.name == name));
            let changed = store.accounts.len() < before;
            if changed {
                self.save_unlocked(&store)?;
                let mut active = self.load_active_unlocked()?;
                if active.get(&prov).and_then(|v| v.as_str()) == Some(name) {
                    active.remove(&prov);
                    active.insert("updated".into(), serde_json::Value::String(now_iso()));
                    self.save_active_unlocked(&active)?;
                }
            }
            Ok(changed)
        })
    }

    pub fn get(&self, provider: &str, name: &str) -> Result<Option<AccountRecord>, StoreError> {
        let prov = canonical_provider(provider);
        Ok(self
            .load()?
            .accounts
            .into_iter()
            .find(|a| canonical_provider(&a.provider) == prov && a.name == name))
    }

    /// asx `getAccountByName`: unique-by-name lookup, error if >1 provider matches.
    pub fn get_by_name(&self, name: &str) -> Result<Option<AccountRecord>, StoreError> {
        let matches: Vec<AccountRecord> = self
            .load()?
            .accounts
            .into_iter()
            .filter(|a| a.name == name)
            .collect();
        match matches.len() {
            0 => Ok(None),
            1 => Ok(Some(matches.into_iter().next().unwrap())),
            _ => Err(StoreError::Ambiguous(name.to_string())),
        }
    }

    pub fn set_share(
        &self,
        provider: &str,
        name: &str,
        share: Option<Vec<String>>,
    ) -> Result<(), StoreError> {
        self.with_exclusive(|| {
            let prov = canonical_provider(provider);
            let mut store = self.load_unlocked()?;
            let acc = store
                .accounts
                .iter_mut()
                .find(|a| canonical_provider(&a.provider) == prov && a.name == name)
                .ok_or_else(|| StoreError::NotFound {
                    provider: provider.into(),
                    name: name.into(),
                })?;
            acc.share = share;
            self.save_unlocked(&store)
        })
    }

    /// asx `setProfileType`: setting `System` also clears `share`.
    pub fn set_profile_type(
        &self,
        provider: &str,
        name: &str,
        t: ProfileType,
    ) -> Result<(), StoreError> {
        self.with_exclusive(|| {
            let prov = canonical_provider(provider);
            let mut store = self.load_unlocked()?;
            let acc = store
                .accounts
                .iter_mut()
                .find(|a| canonical_provider(&a.provider) == prov && a.name == name)
                .ok_or_else(|| StoreError::NotFound {
                    provider: provider.into(),
                    name: name.into(),
                })?;
            acc.profile_type = Some(t);
            if t == ProfileType::System {
                acc.share = None;
            }
            self.save_unlocked(&store)
        })
    }

    // ---- active marker (.active.json) ----

    fn load_active_unlocked(
        &self,
    ) -> Result<serde_json::Map<String, serde_json::Value>, StoreError> {
        let path = self.active_path();
        let body = match std::fs::read_to_string(&path) {
            Ok(body) => body,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Default::default()),
            Err(e) => return Err(StoreError::Io(e)),
        };
        serde_json::from_str(&body).map_err(|source| StoreError::Corrupt { path, source })
    }

    fn save_active_unlocked(
        &self,
        active: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), StoreError> {
        let body = serde_json::to_vec_pretty(active)
            .map_err(|e| StoreError::Invariant(format!("could not serialize active store: {e}")))?;
        self.write_atomic(&self.active_path(), &body)
    }

    pub fn get_active(&self, provider: &str) -> Result<Option<String>, StoreError> {
        self.with_shared(|| {
            Ok(self
                .load_active_unlocked()?
                .get(&canonical_provider(provider))
                .and_then(|v| v.as_str())
                .map(String::from))
        })
    }

    pub fn set_active(&self, provider: &str, name: &str) -> Result<(), StoreError> {
        self.with_exclusive(|| {
            let store = self.load_unlocked()?;
            if !store.accounts.iter().any(|a| {
                canonical_provider(&a.provider) == canonical_provider(provider) && a.name == name
            }) {
                return Err(StoreError::NotFound {
                    provider: provider.into(),
                    name: name.into(),
                });
            }
            let mut active = self.load_active_unlocked()?;
            active.insert(
                canonical_provider(provider),
                serde_json::Value::String(name.to_string()),
            );
            active.insert("updated".into(), serde_json::Value::String(now_iso()));
            self.save_active_unlocked(&active)
        })
    }

    /// asx `renameAccount`: rename in the store and fix any active markers pointing at `old`.
    pub fn rename(&self, old: &str, new: &str) -> Result<(), StoreError> {
        self.with_exclusive(|| {
            if old.is_empty() || new.is_empty() || old == new {
                return Err(StoreError::InvalidRename);
            }
            let original_store = self.load_unlocked()?;
            let Some(idx) = original_store.accounts.iter().position(|a| a.name == old) else {
                return Err(StoreError::NotFound {
                    provider: "*".into(),
                    name: old.into(),
                });
            };
            if let Some(conflict) = original_store.accounts.iter().find(|a| a.name == new) {
                return Err(StoreError::NameConflict {
                    name: new.into(),
                    provider: conflict.provider.clone(),
                });
            }
            let provider = original_store.accounts[idx].provider.clone();
            self.validate_storage_key_in(&original_store, &provider, new)?;

            let original_active = self.load_active_unlocked()?;
            crate::secure_store::rename_secret_unchecked(&provider, old, new)?;

            let mut updated_store = original_store.clone();
            updated_store.accounts[idx].name = new.to_string();
            if updated_store.accounts[idx].label.as_deref() == Some(old) {
                updated_store.accounts[idx].label = Some(new.to_string());
            }
            if let Err(error) = self.save_unlocked(&updated_store) {
                let rollback = crate::secure_store::rename_secret_unchecked(&provider, new, old);
                return Err(transaction_error(error, rollback.err()));
            }

            let mut updated_active = original_active.clone();
            let mut active_dirty = false;
            for value in updated_active.values_mut() {
                if value.as_str() == Some(old) {
                    *value = serde_json::Value::String(new.to_string());
                    active_dirty = true;
                }
            }
            if active_dirty {
                updated_active.insert("updated".into(), serde_json::Value::String(now_iso()));
                if let Err(error) = self.save_active_unlocked(&updated_active) {
                    let store_rollback = self.save_unlocked(&original_store).err();
                    let secret_rollback =
                        crate::secure_store::rename_secret_unchecked(&provider, new, old).err();
                    return Err(StoreError::Transaction(format!(
                        "{error}; store rollback={store_rollback:?}; secret rollback={secret_rollback:?}"
                    )));
                }
            }
            Ok(())
        })
    }
}

fn transaction_error(error: StoreError, rollback: Option<std::io::Error>) -> StoreError {
    StoreError::Transaction(format!("{error}; secret rollback={rollback:?}"))
}

#[cfg(unix)]
pub(crate) fn atomic_replace(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::rename(from, to)
}

#[cfg(windows)]
pub(crate) fn atomic_replace(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };
    let from_wide: Vec<u16> = from.as_os_str().encode_wide().chain(Some(0)).collect();
    let to_wide: Vec<u16> = to.as_os_str().encode_wide().chain(Some(0)).collect();
    let result = unsafe {
        MoveFileExW(
            from_wide.as_ptr(),
            to_wide.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn atomic_replace(from: &Path, to: &Path) -> std::io::Result<()> {
    if to.exists() {
        std::fs::remove_file(to)?;
    }
    std::fs::rename(from, to)
}

#[cfg(unix)]
pub(crate) fn sync_dir(dir: &Path) -> std::io::Result<()> {
    File::open(dir)?.sync_all()
}

#[cfg(not(unix))]
pub(crate) fn sync_dir(_dir: &Path) -> std::io::Result<()> {
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
    use crate::model::AccountRecord;

    fn tmp() -> PathBuf {
        let mut p = std::env::temp_dir();
        let seq = WRITE_SEQ.fetch_add(1, Ordering::Relaxed);
        p.push(format!("aas-store-test-{}-{seq}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        p
    }

    #[test]
    fn add_list_remove() {
        let s = AccountStore::at(tmp());
        s.add(AccountRecord::new("claude", "a@x")).unwrap();
        s.add(AccountRecord::new("codex", "b.codex")).unwrap();
        assert_eq!(s.list(None).unwrap().len(), 2);
        assert_eq!(s.list(Some("claude")).unwrap().len(), 1);
        assert!(s.remove("claude", "a@x").unwrap());
        assert_eq!(s.list(None).unwrap().len(), 1);
    }

    #[test]
    fn global_name_uniqueness() {
        let s = AccountStore::at(tmp());
        s.add(AccountRecord::new("claude", "e-ed@callabo")).unwrap();
        let err = s
            .add(AccountRecord::new("codex", "e-ed@callabo"))
            .unwrap_err();
        assert!(matches!(err, StoreError::NameConflict { .. }));
    }

    #[test]
    fn dedupe_same_provider_name() {
        let s = AccountStore::at(tmp());
        s.add(AccountRecord::new("codex", "x")).unwrap();
        s.add(AccountRecord::new("codex", "x")).unwrap();
        assert_eq!(s.list(Some("codex")).unwrap().len(), 1);
    }

    #[test]
    fn active_marker_and_rename() {
        let s = AccountStore::at(tmp());
        s.add(AccountRecord::new("codex", "old")).unwrap();
        s.set_active("codex", "old").unwrap();
        assert_eq!(s.get_active("codex").unwrap().as_deref(), Some("old"));
        s.rename("old", "new").unwrap();
        assert!(s.get("codex", "new").unwrap().is_some());
        assert_eq!(s.get_active("codex").unwrap().as_deref(), Some("new"));
    }

    #[test]
    fn set_profile_type_system_clears_share() {
        let s = AccountStore::at(tmp());
        let mut r = AccountRecord::new("claude", "z");
        r.share = Some(vec!["sessions".into()]);
        s.add(r).unwrap();
        s.set_profile_type("claude", "z", ProfileType::System)
            .unwrap();
        assert!(s.get("claude", "z").unwrap().unwrap().share.is_none());
    }

    #[test]
    fn duplicate_name_store_is_rejected() {
        let s = AccountStore::at(tmp());
        let store = Store {
            version: 1,
            accounts: vec![
                AccountRecord::new("claude", "dup"),
                AccountRecord::new("codex", "dup"),
            ],
        };
        assert!(matches!(s.save(&store), Err(StoreError::Invariant(_))));
    }

    #[test]
    fn storage_key_collision_is_rejected() {
        let s = AccountStore::at(tmp());
        s.add(AccountRecord::new("codex", "a/b")).unwrap();
        let error = s.add(AccountRecord::new("codex", "a?b")).unwrap_err();
        assert!(matches!(error, StoreError::StorageConflict { .. }));
        assert_eq!(s.list(None).unwrap().len(), 1);
    }

    #[test]
    fn malformed_store_fails_closed() {
        let dir = tmp();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("accounts.json"), b"{not-json").unwrap();
        let s = AccountStore::at(&dir);
        assert!(matches!(s.load(), Err(StoreError::Corrupt { .. })));
        assert!(matches!(
            s.add(AccountRecord::new("codex", "new")),
            Err(StoreError::Corrupt { .. })
        ));
        assert_eq!(
            std::fs::read(dir.join("accounts.json")).unwrap(),
            b"{not-json"
        );
    }

    #[test]
    fn successful_replace_keeps_last_valid_backup() {
        let dir = tmp();
        let s = AccountStore::at(&dir);
        s.add(AccountRecord::new("codex", "first")).unwrap();
        s.add(AccountRecord::new("codex", "second")).unwrap();

        let current: Store =
            serde_json::from_str(&std::fs::read_to_string(dir.join("accounts.json")).unwrap())
                .unwrap();
        let backup: Store =
            serde_json::from_str(&std::fs::read_to_string(dir.join("accounts.json.bak")).unwrap())
                .unwrap();
        assert_eq!(current.accounts.len(), 2);
        assert_eq!(backup.accounts.len(), 1);
        assert_eq!(backup.accounts[0].name, "first");
    }

    #[test]
    fn upsert_preserves_profile_metadata() {
        let s = AccountStore::at(tmp());
        let mut original = AccountRecord::new("codex", "work");
        original.share = Some(vec![]);
        original.profile_type = Some(ProfileType::Isolated);
        let added_at = original.added_at.clone();
        s.add(original).unwrap();

        let mut refreshed = AccountRecord::new("codex", "work");
        refreshed.email = Some("new@example.com".into());
        let updated = s.add(refreshed).unwrap();
        assert_eq!(updated.share, Some(vec![]));
        assert_eq!(updated.profile_type, Some(ProfileType::Isolated));
        assert_eq!(updated.added_at, added_at);
    }

    #[test]
    fn concurrent_adds_do_not_lose_records() {
        let dir = tmp();
        let threads: Vec<_> = (0..40)
            .map(|index| {
                let dir = dir.clone();
                std::thread::spawn(move || {
                    AccountStore::at(dir)
                        .add(AccountRecord::new("codex", format!("account-{index}")))
                        .unwrap();
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(AccountStore::at(dir).list(None).unwrap().len(), 40);
    }
}
