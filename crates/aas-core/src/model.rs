//! On-disk data model — byte-compatible with asx `accounts.json` (`version: 1`).

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ProfileType {
    System,
    Isolated,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AccountRecord {
    pub provider: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub email: Option<String>,
    pub added_at: String,
    /// Shared state categories. Absent = share all; `[]` = fully isolated; subset = those.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub share: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub profile_type: Option<ProfileType>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub meta: Option<serde_json::Value>,
}

/// Display order for accounts within a provider. Provider grouping/order is owned by the
/// provider registry; this only controls the accounts inside each group.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AccountSort {
    /// Case-insensitive account-name order. This is the stable default for every UI.
    #[default]
    Name,
    /// Oldest account first, using the persisted ISO-8601 `addedAt` value.
    Added,
    /// Preserve the account array order from `accounts.json`.
    Stored,
}

/// Sort one provider's accounts without changing the persisted store.
pub fn sort_accounts(accounts: &mut [AccountRecord], order: AccountSort) {
    match order {
        AccountSort::Name => accounts.sort_by(|a, b| {
            a.name
                .to_ascii_lowercase()
                .cmp(&b.name.to_ascii_lowercase())
                .then_with(|| a.name.cmp(&b.name))
                .then_with(|| a.added_at.cmp(&b.added_at))
        }),
        AccountSort::Added => accounts.sort_by(|a, b| {
            a.added_at
                .cmp(&b.added_at)
                .then_with(|| {
                    a.name
                        .to_ascii_lowercase()
                        .cmp(&b.name.to_ascii_lowercase())
                })
                .then_with(|| a.name.cmp(&b.name))
        }),
        AccountSort::Stored => {}
    }
}

impl AccountRecord {
    pub fn new(provider: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            name: name.into(),
            label: None,
            email: None,
            added_at: now_iso(),
            share: None,
            profile_type: None,
            meta: None,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Store {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub accounts: Vec<AccountRecord>,
}

impl Default for Store {
    fn default() -> Self {
        Self {
            version: 1,
            accounts: Vec::new(),
        }
    }
}

fn default_version() -> u32 {
    1
}

/// asx `new Date().toISOString()` → e.g. `2026-07-06T02:05:06.244Z`.
pub fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_asx_shape() {
        let json = r#"{"version":1,"accounts":[
          {"provider":"claude","name":"june@rtzr","label":"june@rtzr","email":"june@rtzr.ai",
           "addedAt":"2026-07-06T02:02:35.648Z","profileType":"isolated"}]}"#;
        let store: Store = serde_json::from_str(json).unwrap();
        assert_eq!(store.version, 1);
        let a = &store.accounts[0];
        assert_eq!(a.name, "june@rtzr");
        assert_eq!(a.profile_type, Some(ProfileType::Isolated));
        assert!(a.share.is_none());
        // re-serialize: camelCase key preserved, None fields omitted
        let out = serde_json::to_string(&store).unwrap();
        assert!(out.contains("\"profileType\":\"isolated\""));
        assert!(!out.contains("\"share\""));
        assert!(!out.contains("\"meta\""));
    }

    #[test]
    fn now_iso_ends_with_z() {
        let s = now_iso();
        assert!(s.ends_with('Z'), "{s}");
    }

    #[test]
    fn account_sort_supports_name_added_and_stored_order() {
        let mut beta = AccountRecord::new("codex", "Beta");
        beta.added_at = "2026-02-01T00:00:00.000Z".into();
        let mut alpha = AccountRecord::new("codex", "alpha");
        alpha.added_at = "2026-03-01T00:00:00.000Z".into();
        let mut older = AccountRecord::new("codex", "zeta");
        older.added_at = "2026-01-01T00:00:00.000Z".into();

        let stored = vec![beta, alpha, older];

        let mut by_name = stored.clone();
        sort_accounts(&mut by_name, AccountSort::Name);
        assert_eq!(
            by_name.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(),
            ["alpha", "Beta", "zeta"]
        );

        let mut by_added = stored.clone();
        sort_accounts(&mut by_added, AccountSort::Added);
        assert_eq!(
            by_added.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(),
            ["zeta", "Beta", "alpha"]
        );

        let mut unchanged = stored.clone();
        sort_accounts(&mut unchanged, AccountSort::Stored);
        assert_eq!(unchanged, stored);
    }
}
