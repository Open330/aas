//! Cursor adapter — a metadata-only marker provider. Mirrors asx `providers/cursor.ts`.
//! Full credential switching (state.vscdb + safe storage) is out of scope; we track accounts
//! via a stored marker + active pointer only.

use crate::common::{set_active, store_account_secret};
use crate::RefreshOutcome;
use aas_core::secure_store::get_secret;
use aas_core::usage::Usage;
use serde_json::json;

const PROVIDER: &str = "cursor";

pub(crate) fn usage(_account: &str) -> Usage {
    Usage {
        headline:
            "Cursor usage: track via Cursor UI or openusage (complex due to internal state.vscdb)"
                .into(),
        ..Default::default()
    }
}

pub(crate) async fn current_credential() -> Option<String> {
    None
}

pub(crate) async fn current_email() -> Option<String> {
    None
}

pub(crate) async fn load_current(account: &str, label: Option<&str>) -> anyhow::Result<()> {
    let marker = json!({"note": "cursor-account-marker", "name": account}).to_string();
    store_account_secret(PROVIDER, account, label, None, &marker)?;
    Ok(())
}

pub(crate) async fn switch_to(account: &str) -> anyhow::Result<()> {
    if get_secret(PROVIDER, account).is_none() {
        anyhow::bail!("No account stored for cursor");
    }
    set_active(PROVIDER, account)?;
    Ok(())
}

pub(crate) async fn clear_current() -> anyhow::Result<()> {
    // Cursor is metadata-only; nothing to clear.
    Ok(())
}

pub(crate) fn login_command() -> Option<Vec<String>> {
    None
}

pub(crate) fn refresh_outcome() -> RefreshOutcome {
    RefreshOutcome {
        ok: true,
        message: "cursor does not require refresh".into(),
        needs_relogin: false,
    }
}
