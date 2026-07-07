//! Provider adapters (claude/codex/grok/zai/cursor): credential storage, auth/switch/refresh,
//! and structured [`Usage`]. Ported from asx `src/providers/*`. See `docs/PARITY_SPEC.md` §F.
//!
//! Unlike asx (which returns a preformatted color string), `usage()` returns structured
//! [`Usage`]/[`Meter`] data so the CLI can render bars/tables/colors and fan out `list -u`.

use aas_core::naming::normalize_provider;
use aas_core::usage::Usage;

mod claude;
mod codex;
mod common;
mod cursor;
mod key_adapter;
mod snapshot;

pub use snapshot::{resolve_scope, snapshot, AccountUsage};

/// The five adapters the CLI drives. `Grok`/`Zai` share `key_adapter`; `Cursor` is a marker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provider {
    Claude,
    Codex,
    Grok,
    Zai,
    Cursor,
}

/// Result of a credential refresh. `needs_relogin` = the refresh token is revoked/absent, so
/// the caller should fall back to the native login flow.
#[derive(Debug, Clone)]
pub struct RefreshOutcome {
    pub ok: bool,
    pub message: String,
    pub needs_relogin: bool,
}

/// Resolve a provider name (with asx aliases: `claude-code`→claude, `xai`→grok) to a
/// [`Provider`]. `openai` and unknown names return `None`.
pub fn get_adapter(name: &str) -> Option<Provider> {
    match normalize_provider(name)?.as_str() {
        "claude" => Some(Provider::Claude),
        "codex" => Some(Provider::Codex),
        "grok" => Some(Provider::Grok),
        "zai" => Some(Provider::Zai),
        "cursor" => Some(Provider::Cursor),
        _ => None,
    }
}

/// asx `listKnownProviders()` order: `[claude, codex, zai, grok, cursor]`.
pub fn all_providers() -> [Provider; 5] {
    [Provider::Claude, Provider::Codex, Provider::Zai, Provider::Grok, Provider::Cursor]
}

impl Provider {
    /// Canonical provider id.
    pub fn id(&self) -> &'static str {
        match self {
            Provider::Claude => "claude",
            Provider::Codex => "codex",
            Provider::Grok => "grok",
            Provider::Zai => "zai",
            Provider::Cursor => "cursor",
        }
    }

    /// Structured usage for the stored account (live fetch; failures land in `Usage::error`).
    pub async fn usage(&self, account: &str) -> Usage {
        match self {
            Provider::Claude => claude::usage(account).await,
            Provider::Codex => codex::usage(account).await,
            Provider::Grok => key_adapter::usage("grok", account).await,
            Provider::Zai => key_adapter::usage("zai", account).await,
            Provider::Cursor => cursor::usage(account),
        }
    }

    /// The credential currently live in the *system* (keychain / auth.json / env), if any.
    pub async fn current_credential(&self) -> Option<String> {
        match self {
            Provider::Claude => claude::current_credential().await,
            Provider::Codex => codex::current_credential().await,
            Provider::Grok => key_adapter::current_credential("grok").await,
            Provider::Zai => key_adapter::current_credential("zai").await,
            Provider::Cursor => cursor::current_credential().await,
        }
    }

    /// Email of the currently active login (used for auto-naming / metadata).
    pub async fn current_email(&self) -> Option<String> {
        match self {
            Provider::Claude => claude::current_email().await,
            Provider::Codex => codex::current_email().await,
            Provider::Grok => key_adapter::current_email("grok").await,
            Provider::Zai => key_adapter::current_email("zai").await,
            Provider::Cursor => cursor::current_email().await,
        }
    }

    /// Snapshot the live system credential into the vault under `account`.
    pub async fn load_current(&self, account: &str, label: Option<&str>) -> anyhow::Result<()> {
        match self {
            Provider::Claude => claude::load_current(account, label).await,
            Provider::Codex => codex::load_current(account, label).await,
            Provider::Grok => key_adapter::load_current("grok", account, label).await,
            Provider::Zai => key_adapter::load_current("zai", account, label).await,
            Provider::Cursor => cursor::load_current(account, label).await,
        }
    }

    /// Make `account` the active credential for this provider (writes native store + marker).
    pub async fn switch_to(&self, account: &str) -> anyhow::Result<()> {
        match self {
            Provider::Claude => claude::switch_to(account).await,
            Provider::Codex => codex::switch_to(account).await,
            Provider::Grok => key_adapter::switch_to("grok", account).await,
            Provider::Zai => key_adapter::switch_to("zai", account).await,
            Provider::Cursor => cursor::switch_to(account).await,
        }
    }

    /// Clear the *local* provider session (keychain entry / auth file) without revoking tokens.
    pub async fn clear_current(&self) -> anyhow::Result<()> {
        match self {
            Provider::Claude => claude::clear_current().await,
            Provider::Codex => codex::clear_current().await,
            Provider::Grok => key_adapter::clear_current("grok").await,
            Provider::Zai => key_adapter::clear_current("zai").await,
            Provider::Cursor => cursor::clear_current().await,
        }
    }

    /// True if the stored credential is expired (within the +60s refresh skew).
    pub async fn is_expired(&self, account: &str) -> bool {
        match self {
            Provider::Claude => claude::is_expired(account).await,
            Provider::Codex => codex::is_expired(account).await,
            // Key/marker providers have no expiry.
            Provider::Grok | Provider::Zai | Provider::Cursor => false,
        }
    }

    /// Refresh (rotate) the stored credential.
    pub async fn refresh(&self, account: &str) -> RefreshOutcome {
        match self {
            Provider::Claude => claude::refresh(account).await,
            Provider::Codex => codex::refresh(account).await,
            Provider::Grok => key_adapter::refresh_outcome("grok"),
            Provider::Zai => key_adapter::refresh_outcome("zai"),
            Provider::Cursor => cursor::refresh_outcome(),
        }
    }

    /// The native login command (argv), or `None` if the provider has no CLI login.
    pub fn login_command(&self) -> Option<Vec<String>> {
        match self {
            Provider::Claude => claude::login_command(),
            Provider::Codex => codex::login_command(),
            Provider::Grok => key_adapter::login_command("grok"),
            Provider::Zai => key_adapter::login_command("zai"),
            Provider::Cursor => cursor::login_command(),
        }
    }

    /// Store a manually issued long-lived token (Claude only; error otherwise).
    pub async fn load_long_lived_token(&self, account: &str, token: &str) -> anyhow::Result<()> {
        match self {
            Provider::Claude => claude::load_long_lived_token(account, token).await,
            other => anyhow::bail!("{} does not support long-lived tokens", other.id()),
        }
    }

    /// Validate an API key against the provider and store it (Z.AI only; error otherwise).
    pub async fn validate_and_store_key(&self, account: &str, key: &str) -> anyhow::Result<()> {
        match self {
            Provider::Zai => key_adapter::validate_and_store_key("zai", account, key).await,
            other => anyhow::bail!("{} does not support API-key login", other.id()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_resolution_and_aliases() {
        assert_eq!(get_adapter("claude"), Some(Provider::Claude));
        assert_eq!(get_adapter("claude-code"), Some(Provider::Claude));
        assert_eq!(get_adapter("Codex"), Some(Provider::Codex));
        assert_eq!(get_adapter("xai"), Some(Provider::Grok)); // alias
        assert_eq!(get_adapter("zai"), Some(Provider::Zai));
        assert_eq!(get_adapter("cursor"), Some(Provider::Cursor));
        assert_eq!(get_adapter("openai"), None); // known target, but no local adapter
        assert_eq!(get_adapter("nope"), None);
    }

    #[test]
    fn ids_and_order() {
        assert_eq!(Provider::Claude.id(), "claude");
        assert_eq!(Provider::Grok.id(), "grok");
        assert_eq!(Provider::Zai.id(), "zai");
        let order: Vec<&str> = all_providers().iter().map(|p| p.id()).collect();
        assert_eq!(order, ["claude", "codex", "zai", "grok", "cursor"]);
    }

    #[tokio::test]
    async fn long_lived_and_key_login_are_provider_gated() {
        assert!(Provider::Codex.load_long_lived_token("x", "t").await.is_err());
        assert!(Provider::Claude.validate_and_store_key("x", "k").await.is_err());
    }
}
