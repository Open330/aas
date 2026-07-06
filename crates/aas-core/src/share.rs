//! Sharing categories, share-flag resolution, and description. Mirrors asx
//! `storage/shared-state.ts` (the symlink materialization itself lands in P3 / exec).

use crate::naming::normalize_provider_key;

pub const SHARE_CATEGORIES: &[&str] = &["sessions", "skills", "agents", "hooks", "settings"];

/// A shared filesystem entry symlinked from the provider system home into a profile home.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SharedEntry {
    pub name: &'static str,
    pub is_dir: bool,
    pub category: &'static str,
}

const fn dir(name: &'static str, category: &'static str) -> SharedEntry {
    SharedEntry { name, is_dir: true, category }
}
const fn file(name: &'static str, category: &'static str) -> SharedEntry {
    SharedEntry { name, is_dir: false, category }
}

/// config.toml is provider-injected on cross-provider runs, so it is skipped there.
pub const INJECTED_WHEN_CROSS: &[&str] = &["config.toml"];

/// asx `SHARED[providerKey]`.
pub fn shared_entries(provider: &str) -> &'static [SharedEntry] {
    match normalize_provider_key(provider).as_str() {
        "claude" => &CLAUDE,
        "codex" => &CODEX,
        "grok" => &GROK,
        _ => &[],
    }
}

static CLAUDE: [SharedEntry; 14] = [
    dir("projects", "sessions"),
    dir("sessions", "sessions"),
    dir("shell-snapshots", "sessions"),
    dir("file-history", "sessions"),
    dir("plans", "sessions"),
    dir("tasks", "sessions"),
    dir("todos", "sessions"),
    file("history.jsonl", "sessions"),
    dir("skills", "skills"),
    dir("agents", "agents"),
    dir("hooks", "hooks"),
    dir("plugins", "settings"),
    file("settings.json", "settings"),
    file("CLAUDE.md", "settings"),
];

static CODEX: [SharedEntry; 9] = [
    dir("sessions", "sessions"),
    dir("archived_sessions", "sessions"),
    file("history.jsonl", "sessions"),
    file("session_index.jsonl", "sessions"),
    dir("skills", "skills"),
    dir("rules", "settings"),
    dir("plugins", "settings"),
    file("AGENTS.md", "settings"),
    // config.toml is in `settings`, but skipped on cross-provider runs (the proxy injects its own).
    file("config.toml", "settings"),
];

static GROK: [SharedEntry; 6] = [
    dir("sessions", "sessions"),
    dir("projects", "sessions"),
    file("active_sessions.json", "sessions"),
    dir("skills", "skills"),
    dir("completions", "settings"),
    file("config.toml", "settings"),
];

/// Categories a provider actually supports, ordered by SHARE_CATEGORIES. asx
/// `supportedShareCategories`.
pub fn supported_share_categories(provider: &str) -> Vec<&'static str> {
    let entries = shared_entries(provider);
    SHARE_CATEGORIES
        .iter()
        .copied()
        .filter(|cat| entries.iter().any(|e| e.category == *cat))
        .collect()
}

#[derive(Default, Clone, Debug)]
pub struct ShareOpts {
    pub isolated: bool,
    pub shared: bool,
    pub share: Option<String>,
    pub isolate: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ShareSelection {
    pub provided: bool,
    /// None = share all; Some([]) = fully isolated; Some(subset) = those categories.
    pub value: Option<Vec<String>>,
}

#[derive(Debug, thiserror::Error)]
pub enum ShareError {
    #[error("Use only one of --isolated / --shared / --share / --isolate.")]
    Multiple,
    #[error("Unknown share category: {0}")]
    Unknown(String),
    #[error("Category not supported by provider {provider}: {category}")]
    Unsupported { provider: String, category: String },
}

fn parse_categories(csv: &str) -> Result<Vec<String>, ShareError> {
    let mut out = Vec::new();
    for part in csv.split(',') {
        let c = part.trim().to_lowercase();
        if c.is_empty() {
            continue;
        }
        if !SHARE_CATEGORIES.contains(&c.as_str()) {
            return Err(ShareError::Unknown(c));
        }
        if !out.contains(&c) {
            out.push(c);
        }
    }
    Ok(out)
}

fn parse_categories_for_provider(csv: &str, provider: &str) -> Result<Vec<String>, ShareError> {
    let cats = parse_categories(csv)?;
    let supported = supported_share_categories(provider);
    for c in &cats {
        if !supported.contains(&c.as_str()) {
            return Err(ShareError::Unsupported {
                provider: provider.into(),
                category: c.clone(),
            });
        }
    }
    Ok(cats)
}

/// asx `resolveShareSelection`.
pub fn resolve_share_selection(
    opts: &ShareOpts,
    provider: Option<&str>,
) -> Result<ShareSelection, ShareError> {
    let count = [opts.isolated, opts.shared, opts.share.is_some(), opts.isolate.is_some()]
        .iter()
        .filter(|b| **b)
        .count();
    if count == 0 {
        return Ok(ShareSelection { provided: false, value: None });
    }
    if count > 1 {
        return Err(ShareError::Multiple);
    }
    if opts.isolated {
        return Ok(ShareSelection { provided: true, value: Some(vec![]) });
    }
    if opts.shared {
        return Ok(ShareSelection { provided: true, value: None });
    }
    if let Some(csv) = &opts.share {
        let v = match provider {
            Some(p) => parse_categories_for_provider(csv, p)?,
            None => parse_categories(csv)?,
        };
        return Ok(ShareSelection { provided: true, value: Some(v) });
    }
    // isolate
    let csv = opts.isolate.as_deref().unwrap();
    let exclude = match provider {
        Some(p) => parse_categories_for_provider(csv, p)?,
        None => parse_categories(csv)?,
    };
    let base: Vec<String> = match provider {
        Some(p) => supported_share_categories(p).iter().map(|s| s.to_string()).collect(),
        None => SHARE_CATEGORIES.iter().map(|s| s.to_string()).collect(),
    };
    let value: Vec<String> = base.into_iter().filter(|c| !exclude.contains(c)).collect();
    Ok(ShareSelection { provided: true, value: Some(value) })
}

/// asx `describeShare`.
pub fn describe_share(share: Option<&[String]>, provider: Option<&str>) -> String {
    let categories: Vec<&str> = match provider {
        Some(p) => supported_share_categories(p),
        None => SHARE_CATEGORIES.to_vec(),
    };
    let Some(share) = share else {
        return format!("shared: {}", categories.join(", "));
    };
    let shared: Vec<&str> = categories.iter().copied().filter(|c| share.iter().any(|s| s == c)).collect();
    let isolated: Vec<&str> = categories.iter().copied().filter(|c| !share.iter().any(|s| s == c)).collect();
    if shared.is_empty() {
        return format!("isolated: {}", categories.join(", "));
    }
    let mut out = format!("shared: {}", shared.join(", "));
    if !isolated.is_empty() {
        out.push_str(&format!(" (isolated: {})", isolated.join(", ")));
    }
    out
}

/// asx `linkSharedState`: symlink shared state from the provider system home into `home`.
/// `categories = None` shares every category; `Some([])` shares nothing; `Some(subset)` those.
/// Best-effort — individual link failures are ignored. Mirrors shared-state.ts:118-148.
#[cfg(unix)]
pub fn link_shared_state(provider: &str, home: &std::path::Path, is_cross: bool, categories: Option<&[String]>) {
    use std::os::unix::fs::symlink;

    let Some(base) = crate::platform::system_home_for(provider) else {
        return;
    };
    // Never self-link (system profile pointing at its own home).
    let canon = |p: &std::path::Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    if canon(&base) == canon(home) {
        return;
    }

    for entry in shared_entries(provider) {
        if let Some(cats) = categories {
            if !cats.iter().any(|c| c == entry.category) {
                continue;
            }
        }
        if is_cross && INJECTED_WHEN_CROSS.contains(&entry.name) {
            continue;
        }
        let target = base.join(entry.name);
        let link = home.join(entry.name);

        if entry.is_dir {
            if !target.exists() {
                let _ = std::fs::create_dir_all(&target);
            }
        } else if !target.exists() {
            // Don't fabricate empty shared files.
            continue;
        }

        match std::fs::symlink_metadata(&link) {
            Ok(meta) if meta.file_type().is_symlink() => {
                let _ = std::fs::remove_file(&link); // replace stale symlink
            }
            Ok(_) => continue, // real data already present — never clobber
            Err(_) => {}
        }
        let _ = symlink(&target, &link);
    }
}

#[cfg(not(unix))]
pub fn link_shared_state(_provider: &str, _home: &std::path::Path, _is_cross: bool, _categories: Option<&[String]>) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_categories() {
        assert_eq!(supported_share_categories("claude"), vec!["sessions", "skills", "agents", "hooks", "settings"]);
        assert_eq!(supported_share_categories("codex"), vec!["sessions", "skills", "settings"]);
        assert_eq!(supported_share_categories("grok"), vec!["sessions", "skills", "settings"]);
    }

    #[test]
    fn resolve_flags() {
        let iso = resolve_share_selection(&ShareOpts { isolated: true, ..Default::default() }, None).unwrap();
        assert_eq!(iso.value, Some(vec![]));
        let sh = resolve_share_selection(&ShareOpts { shared: true, ..Default::default() }, None).unwrap();
        assert_eq!(sh.value, None);
        let only = resolve_share_selection(&ShareOpts { share: Some("sessions,skills".into()), ..Default::default() }, Some("codex")).unwrap();
        assert_eq!(only.value, Some(vec!["sessions".into(), "skills".into()]));
        let isolate = resolve_share_selection(&ShareOpts { isolate: Some("skills".into()), ..Default::default() }, Some("codex")).unwrap();
        assert_eq!(isolate.value, Some(vec!["sessions".into(), "settings".into()]));
        assert!(resolve_share_selection(&ShareOpts { isolated: true, shared: true, ..Default::default() }, None).is_err());
    }

    #[test]
    fn describe() {
        assert_eq!(describe_share(None, Some("codex")), "shared: sessions, skills, settings");
        assert_eq!(describe_share(Some(&[]), Some("codex")), "isolated: sessions, skills, settings");
        assert_eq!(
            describe_share(Some(&["sessions".to_string()]), Some("codex")),
            "shared: sessions (isolated: skills, settings)"
        );
    }
}
