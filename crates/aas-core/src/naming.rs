//! Provider normalization, short names, account-name derivation, and profile-home naming.
//! Mirrors asx `providers/index.ts`, `cli.ts` (getProviderShortName/deriveAccountName), and
//! `storage/profile-home.ts`.

use std::path::PathBuf;

/// Providers with a registered adapter (asx `listKnownProviders()`), minus the `claude-code` alias.
pub const KNOWN_PROVIDERS: &[&str] = &["claude", "codex", "zai", "grok", "cursor"];

/// asx `normalizeProvider`: canonical provider id, or `None` if unrecognized.
pub fn normalize_provider(p: &str) -> Option<String> {
    let k = p.to_lowercase();
    match k.as_str() {
        "claude-code" => Some("claude".into()),
        "xai" => Some("grok".into()),
        "openai" => Some("openai".into()),
        "claude" | "codex" | "zai" | "grok" | "cursor" => Some(k),
        _ => None,
    }
}

pub fn is_known_provider(p: &str) -> bool {
    normalize_provider(p).is_some()
}

/// `norm(p) = normalizeProvider(p) || p.toLowerCase()` (used by resolveProviderName).
pub fn norm_or_lower(p: &str) -> String {
    normalize_provider(p).unwrap_or_else(|| p.to_lowercase())
}

/// asx `getProviderShortName`.
pub fn provider_short_name(p: &str) -> String {
    let k = p.to_lowercase();
    match k.as_str() {
        "claude" | "claude-code" => "claude".into(),
        "codex" => "codex".into(),
        "grok" => "grok".into(),
        "cursor" => "cursor".into(),
        "zai" => "zai".into(),
        _ => {
            let stripped = k.strip_suffix("-code").unwrap_or(&k);
            stripped.split('-').next().unwrap_or(stripped).to_string()
        }
    }
}

/// asx `normalizeProviderKey` (profile-home / sharing key): contains `claude` → `claude`,
/// `xai` → `grok`, else lowercase.
pub fn normalize_provider_key(p: &str) -> String {
    let k = p.to_lowercase();
    if k.contains("claude") {
        "claude".into()
    } else if k == "xai" {
        "grok".into()
    } else {
        k
    }
}

/// asx `deriveAccountName`: `<email-local>.<providerShort>`, or `personal.<short>`.
pub fn derive_account_name(email: Option<&str>, provider: &str) -> String {
    let local = email
        .and_then(|e| e.split('@').next())
        .filter(|s| !s.is_empty())
        .unwrap_or("personal");
    format!("{}.{}", local, provider_short_name(provider))
}

/// asx `nativeCredFile`.
pub fn native_cred_file(provider: &str) -> &'static str {
    match normalize_provider_key(provider).as_str() {
        "claude" => ".credentials.json",
        "codex" | "grok" => "auth.json",
        _ => "credential",
    }
}

/// asx `safeProfileDirName`: `{normKey}-{name}` with `[^A-Za-z0-9_.-]` → `_`.
pub fn safe_profile_dir_name(provider: &str, name: &str) -> String {
    let raw = format!("{}-{}", normalize_provider_key(provider), name);
    raw.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

pub fn profile_home(provider: &str, name: &str) -> PathBuf {
    crate::platform::profiles_dir().join(safe_profile_dir_name(provider, name))
}

pub fn profile_credential_path(provider: &str, name: &str) -> PathBuf {
    profile_home(provider, name).join(native_cred_file(provider))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_provider_aliases() {
        assert_eq!(normalize_provider("claude-code").as_deref(), Some("claude"));
        assert_eq!(normalize_provider("XAI").as_deref(), Some("grok"));
        assert_eq!(normalize_provider("Codex").as_deref(), Some("codex"));
        assert_eq!(normalize_provider("nope"), None);
    }

    #[test]
    fn derive_name_matches_asx() {
        assert_eq!(derive_account_name(Some("e-ed@callabo.ai"), "codex"), "e-ed.codex");
        assert_eq!(derive_account_name(None, "claude"), "personal.claude");
    }

    #[test]
    fn safe_dir_name_sanitizes() {
        // e-ed@callabo -> codex-e-ed_callabo (matches the on-disk homes we saw)
        assert_eq!(safe_profile_dir_name("codex", "e-ed@callabo"), "codex-e-ed_callabo");
        assert_eq!(safe_profile_dir_name("claude", "e-ed.codex"), "claude-e-ed.codex");
        // claude-containing provider keys normalize to `claude`
        assert_eq!(safe_profile_dir_name("claude-code", "june@rtzr"), "claude-june_rtzr");
    }

    #[test]
    fn native_files() {
        assert_eq!(native_cred_file("claude"), ".credentials.json");
        assert_eq!(native_cred_file("codex"), "auth.json");
        assert_eq!(native_cred_file("cursor"), "credential");
    }
}
