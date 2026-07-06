//! Adopt existing `asx` state. Because `aas` defaults to asx's on-disk locations
//! (`<config>/asx/…`, same keychain scheme), adoption is usually a **no-op** — `aas` reads the
//! same `accounts.json`, profile homes, and keychain entries. This crate provides an explicit
//! `import` that validates what is present and reports what (if anything) needs re-login.
//! Implemented in **P1**.

use aas_core::store::AccountStore;

/// Summary of what an adopt/import pass found.
#[derive(Debug, Default)]
pub struct ImportReport {
    pub accounts: usize,
    pub with_profile_home: usize,
    pub missing_credential: Vec<String>,
}

/// Inspect the current (shared) asx config and report adoptable state. Non-destructive.
pub fn inspect() -> anyhow::Result<ImportReport> {
    let store = AccountStore::open_default();
    let accounts = store.list(None);
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
