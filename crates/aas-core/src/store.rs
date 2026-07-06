//! Account store + active-marker operations. Mirrors asx `storage/account-store.ts`.
//!
//! `AccountStore` is parameterized by its config directory so it is trivially testable; use
//! [`AccountStore::open_default`] in the app and [`AccountStore::at`] in tests.

use crate::model::{now_iso, AccountRecord, ProfileType, Store};
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("Name \"{name}\" is already used by provider \"{provider}\". Account names must be unique.")]
    NameConflict { name: String, provider: String },
    #[error("Name \"{0}\" is ambiguous across providers; specify the provider.")]
    Ambiguous(String),
    #[error("Account {provider}/{name} not found")]
    NotFound { provider: String, name: String },
    #[error("Invalid rename: old and new names must be different and non-empty")]
    InvalidRename,
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
        Self { dir: crate::platform::asx_config_dir() }
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

    pub fn load(&self) -> Store {
        match std::fs::read_to_string(self.accounts_path()) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
            Err(_) => Store::default(),
        }
    }

    fn save(&self, store: &Store) -> Result<(), StoreError> {
        std::fs::create_dir_all(&self.dir)?;
        let path = self.accounts_path();
        let body = serde_json::to_string_pretty(store).unwrap_or_else(|_| "{}".into());
        std::fs::write(&path, body)?;
        set_0600(&path);
        Ok(())
    }

    pub fn list(&self, provider: Option<&str>) -> Vec<AccountRecord> {
        let store = self.load();
        match provider {
            // asx `listAccounts` uses exact-string provider match.
            Some(p) => store.accounts.into_iter().filter(|a| a.provider == p).collect(),
            None => store.accounts,
        }
    }

    /// asx `addAccount`: global name uniqueness across providers; dedupe by (provider, name).
    pub fn add(&self, mut rec: AccountRecord) -> Result<AccountRecord, StoreError> {
        if rec.added_at.is_empty() {
            rec.added_at = now_iso();
        }
        let mut store = self.load();
        if let Some(c) = store
            .accounts
            .iter()
            .find(|a| a.name == rec.name && a.provider != rec.provider)
        {
            return Err(StoreError::NameConflict {
                name: rec.name.clone(),
                provider: c.provider.clone(),
            });
        }
        store
            .accounts
            .retain(|a| !(a.provider == rec.provider && a.name == rec.name));
        store.accounts.push(rec.clone());
        self.save(&store)?;
        Ok(rec)
    }

    /// asx `removeAccount` (canonical/lowercased provider match). Returns whether it shrank.
    pub fn remove(&self, provider: &str, name: &str) -> Result<bool, StoreError> {
        let prov = canonical_provider(provider);
        let mut store = self.load();
        let before = store.accounts.len();
        store
            .accounts
            .retain(|a| !(canonical_provider(&a.provider) == prov && a.name == name));
        let changed = store.accounts.len() < before;
        if changed {
            self.save(&store)?;
        }
        Ok(changed)
    }

    pub fn get(&self, provider: &str, name: &str) -> Option<AccountRecord> {
        let prov = canonical_provider(provider);
        self.load()
            .accounts
            .into_iter()
            .find(|a| canonical_provider(&a.provider) == prov && a.name == name)
    }

    /// asx `getAccountByName`: unique-by-name lookup, error if >1 provider matches.
    pub fn get_by_name(&self, name: &str) -> Result<Option<AccountRecord>, StoreError> {
        let matches: Vec<AccountRecord> =
            self.load().accounts.into_iter().filter(|a| a.name == name).collect();
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
        let prov = canonical_provider(provider);
        let mut store = self.load();
        let acc = store
            .accounts
            .iter_mut()
            .find(|a| canonical_provider(&a.provider) == prov && a.name == name)
            .ok_or_else(|| StoreError::NotFound {
                provider: provider.into(),
                name: name.into(),
            })?;
        acc.share = share;
        self.save(&store)
    }

    /// asx `setProfileType`: setting `System` also clears `share`.
    pub fn set_profile_type(
        &self,
        provider: &str,
        name: &str,
        t: ProfileType,
    ) -> Result<(), StoreError> {
        let prov = canonical_provider(provider);
        let mut store = self.load();
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
        self.save(&store)
    }

    // ---- active marker (.active.json) ----

    fn load_active(&self) -> serde_json::Map<String, serde_json::Value> {
        std::fs::read_to_string(self.active_path())
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Map<_, _>>(&s).ok())
            .unwrap_or_default()
    }

    pub fn get_active(&self, provider: &str) -> Option<String> {
        self.load_active()
            .get(&canonical_provider(provider))
            .and_then(|v| v.as_str())
            .map(String::from)
    }

    pub fn set_active(&self, provider: &str, name: &str) -> Result<(), StoreError> {
        std::fs::create_dir_all(&self.dir)?;
        let mut m = self.load_active();
        m.insert(
            canonical_provider(provider),
            serde_json::Value::String(name.to_string()),
        );
        m.insert("updated".into(), serde_json::Value::String(now_iso()));
        let path = self.active_path();
        std::fs::write(&path, serde_json::to_string_pretty(&m).unwrap_or_default())?;
        set_0600(&path);
        Ok(())
    }

    /// asx `renameAccount`: rename in the store and fix any active markers pointing at `old`.
    pub fn rename(&self, old: &str, new: &str) -> Result<(), StoreError> {
        if old.is_empty() || new.is_empty() || old == new {
            return Err(StoreError::InvalidRename);
        }
        let mut store = self.load();
        let Some(idx) = store.accounts.iter().position(|a| a.name == old) else {
            return Err(StoreError::NotFound {
                provider: "*".into(),
                name: old.into(),
            });
        };
        let provider = store.accounts[idx].provider.clone();
        if let Some(c) = store
            .accounts
            .iter()
            .find(|a| a.name == new && a.provider != provider)
        {
            return Err(StoreError::NameConflict {
                name: new.into(),
                provider: c.provider.clone(),
            });
        }
        store.accounts[idx].name = new.to_string();
        if store.accounts[idx].label.as_deref() == Some(old) {
            store.accounts[idx].label = Some(new.to_string());
        }
        self.save(&store)?;

        // update active markers
        let mut m = self.load_active();
        let mut dirty = false;
        for (_k, v) in m.iter_mut() {
            if v.as_str() == Some(old) {
                *v = serde_json::Value::String(new.to_string());
                dirty = true;
            }
        }
        if dirty {
            let path = self.active_path();
            std::fs::write(&path, serde_json::to_string_pretty(&m).unwrap_or_default())?;
            set_0600(&path);
        }
        Ok(())
    }
}

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
        // unique-ish per test invocation without Date/rand: use the accounts len trick via nanos-free id
        p.push(format!("aas-store-test-{}", std::process::id()));
        p.push(format!("{:p}", &p as *const _));
        let _ = std::fs::remove_dir_all(&p);
        p
    }

    #[test]
    fn add_list_remove() {
        let s = AccountStore::at(tmp());
        s.add(AccountRecord::new("claude", "a@x")).unwrap();
        s.add(AccountRecord::new("codex", "b.codex")).unwrap();
        assert_eq!(s.list(None).len(), 2);
        assert_eq!(s.list(Some("claude")).len(), 1);
        assert!(s.remove("claude", "a@x").unwrap());
        assert_eq!(s.list(None).len(), 1);
    }

    #[test]
    fn global_name_uniqueness() {
        let s = AccountStore::at(tmp());
        s.add(AccountRecord::new("claude", "e-ed@callabo")).unwrap();
        let err = s.add(AccountRecord::new("codex", "e-ed@callabo")).unwrap_err();
        assert!(matches!(err, StoreError::NameConflict { .. }));
    }

    #[test]
    fn dedupe_same_provider_name() {
        let s = AccountStore::at(tmp());
        s.add(AccountRecord::new("codex", "x")).unwrap();
        s.add(AccountRecord::new("codex", "x")).unwrap();
        assert_eq!(s.list(Some("codex")).len(), 1);
    }

    #[test]
    fn active_marker_and_rename() {
        let s = AccountStore::at(tmp());
        s.add(AccountRecord::new("codex", "old")).unwrap();
        s.set_active("codex", "old").unwrap();
        assert_eq!(s.get_active("codex").as_deref(), Some("old"));
        s.rename("old", "new").unwrap();
        assert!(s.get("codex", "new").is_some());
        assert_eq!(s.get_active("codex").as_deref(), Some("new"));
    }

    #[test]
    fn set_profile_type_system_clears_share() {
        let s = AccountStore::at(tmp());
        let mut r = AccountRecord::new("claude", "z");
        r.share = Some(vec!["sessions".into()]);
        s.add(r).unwrap();
        s.set_profile_type("claude", "z", ProfileType::System).unwrap();
        assert!(s.get("claude", "z").unwrap().share.is_none());
    }

    #[test]
    fn get_by_name_ambiguous() {
        let s = AccountStore::at(tmp());
        // craft two providers with same name by bypassing add()'s uniqueness via direct writes
        let store = Store {
            version: 1,
            accounts: vec![
                AccountRecord::new("claude", "dup"),
                AccountRecord::new("codex", "dup"),
            ],
        };
        s.save(&store).unwrap();
        assert!(matches!(s.get_by_name("dup"), Err(StoreError::Ambiguous(_))));
    }
}
