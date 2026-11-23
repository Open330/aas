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
    SharedEntry {
        name,
        is_dir: true,
        category,
    }
}
const fn file(name: &'static str, category: &'static str) -> SharedEntry {
    SharedEntry {
        name,
        is_dir: false,
        category,
    }
}

/// Provider configs rewritten for cross-provider runs; never symlink them from the system home.
pub const INJECTED_WHEN_CROSS: &[&str] = &["config.toml", "models.json", "settings.json"];

/// asx `SHARED[providerKey]`.
pub fn shared_entries(provider: &str) -> &'static [SharedEntry] {
    match normalize_provider_key(provider).as_str() {
        "claude" => &CLAUDE,
        "codex" => &CODEX,
        "grok" => &GROK,
        "pi" => &PI,
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

static PI: [SharedEntry; 7] = [
    dir("sessions", "sessions"),
    dir("skills", "skills"),
    dir("extensions", "settings"),
    dir("prompt-templates", "settings"),
    dir("themes", "settings"),
    file("AGENTS.md", "settings"),
    file("settings.json", "settings"),
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
    let count = [
        opts.isolated,
        opts.shared,
        opts.share.is_some(),
        opts.isolate.is_some(),
    ]
    .iter()
    .filter(|b| **b)
    .count();
    if count == 0 {
        return Ok(ShareSelection {
            provided: false,
            value: None,
        });
    }
    if count > 1 {
        return Err(ShareError::Multiple);
    }
    if opts.isolated {
        return Ok(ShareSelection {
            provided: true,
            value: Some(vec![]),
        });
    }
    if opts.shared {
        return Ok(ShareSelection {
            provided: true,
            value: None,
        });
    }
    if let Some(csv) = &opts.share {
        let v = match provider {
            Some(p) => parse_categories_for_provider(csv, p)?,
            None => parse_categories(csv)?,
        };
        return Ok(ShareSelection {
            provided: true,
            value: Some(v),
        });
    }
    // isolate
    let csv = opts.isolate.as_deref().unwrap();
    let exclude = match provider {
        Some(p) => parse_categories_for_provider(csv, p)?,
        None => parse_categories(csv)?,
    };
    let base: Vec<String> = match provider {
        Some(p) => supported_share_categories(p)
            .iter()
            .map(|s| s.to_string())
            .collect(),
        None => SHARE_CATEGORIES.iter().map(|s| s.to_string()).collect(),
    };
    let value: Vec<String> = base.into_iter().filter(|c| !exclude.contains(c)).collect();
    Ok(ShareSelection {
        provided: true,
        value: Some(value),
    })
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
    let shared: Vec<&str> = categories
        .iter()
        .copied()
        .filter(|c| share.iter().any(|s| s == c))
        .collect();
    let isolated: Vec<&str> = categories
        .iter()
        .copied()
        .filter(|c| !share.iter().any(|s| s == c))
        .collect();
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
pub fn link_shared_state(
    provider: &str,
    home: &std::path::Path,
    is_cross: bool,
    categories: Option<&[String]>,
) {
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
        let target = base.join(entry.name);
        let link = home.join(entry.name);
        let selected = categories.is_none_or(|cats| cats.iter().any(|c| c == entry.category));
        if !selected || (is_cross && INJECTED_WHEN_CROSS.contains(&entry.name)) {
            // Opted out (or rewritten for cross runs): drop the link an earlier, wider sharing
            // left, so the setting takes effect instead of the profile still reading through it.
            unlink_shared_entry(provider, &target, &link);
            continue;
        }

        if entry.is_dir {
            if !target.exists() {
                let _ = std::fs::create_dir_all(&target);
            }
        } else if !target.exists() {
            // Don't fabricate empty shared files.
            continue;
        }

        if entry.name == "skills" && normalize_provider_key(provider) == "codex" {
            link_codex_skills(&target, &link);
            continue;
        }

        match std::fs::symlink_metadata(&link) {
            Ok(meta) if meta.file_type().is_symlink() && !link.exists() => {
                let _ = std::fs::remove_file(&link); // replace stale symlink
            }
            Ok(_) => continue, // preserve existing data and working links
            Err(_) => {}
        }
        let _ = symlink(&target, &link);
    }
}

/// Whether the link points to the named entry in the provider's system home.
#[cfg(unix)]
fn links_to(link: &std::path::Path, target: &std::path::Path) -> bool {
    let Ok(to) = std::fs::read_link(link) else {
        return false;
    };
    if to == target {
        return true;
    }
    let to = if to.is_absolute() {
        to
    } else {
        link.with_file_name(to)
    };
    // Resolve aliased parent paths without following the provider's entry itself.
    if to.file_name() != target.file_name() {
        return false;
    }
    let canonical_parent = |path: &std::path::Path| path.parent()?.canonicalize().ok();
    matches!(
        (canonical_parent(&to), canonical_parent(target)),
        (Some(a), Some(b)) if a == b
    )
}

/// Undo [`link_shared_state`] for one entry no longer shared: remove the link to the system
/// home, and for Codex `skills` the per-skill links inside the profile's own directory, keeping
/// its `.system`, real skills and links that point anywhere else.
#[cfg(unix)]
fn unlink_shared_entry(provider: &str, target: &std::path::Path, link: &std::path::Path) {
    if links_to(link, target) {
        let _ = std::fs::remove_file(link);
        return;
    }
    if !(link.ends_with("skills") && normalize_provider_key(provider) == "codex") {
        return;
    }
    let is_real_dir = std::fs::symlink_metadata(link).is_ok_and(|m| m.is_dir());
    if !is_real_dir {
        return;
    }
    let Ok(entries) = std::fs::read_dir(link) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name == CODEX_SYSTEM_SKILLS {
            continue;
        }
        let here = entry.path();
        if links_to(&here, &target.join(&name)) {
            let _ = std::fs::remove_file(&here);
        }
    }
}

/// The directory under `skills` that Codex owns: it writes the skills bundled into the binary
/// there, with a marker naming the bundle, on every start.
pub const CODEX_SYSTEM_SKILLS: &str = ".system";

/// Share a Codex profile's skills entry by entry, keeping [`CODEX_SYSTEM_SKILLS`] per profile.
///
/// Linking the whole `skills` directory made every profile — and the system home, which the
/// Codex desktop app and its daemon use — write the same `.system`. Two Codex builds bundle
/// different skills, so each found the other's marker on start, deleted the directory and wrote
/// its own, invalidating the skills cache of every session sharing it. A profile now gets its
/// own `.system`; only the user's skills are shared.
///
/// - A `skills` link left by an older aas becomes a real directory.
/// - Every system skill gets a link here; links to skills that were removed are dropped.
/// - A skill installed from inside the profile (a real entry the system home lacks) moves to
///   the system home and is linked back, so it reaches every profile as before. One the system
///   home also has stays here, private to the profile.
#[cfg(unix)]
fn link_codex_skills(target: &std::path::Path, link: &std::path::Path) {
    use std::os::unix::fs::symlink;

    match std::fs::symlink_metadata(link) {
        Ok(meta) if meta.file_type().is_symlink() && links_to(link, target) => {
            if std::fs::remove_file(link).is_err() || std::fs::create_dir(link).is_err() {
                return;
            }
            // Sessions already running read their bundled skills through this path and only
            // write them on start. Hand them the bundle they were reading until they restart;
            // the next start replaces it with its own if it differs.
            let _ = copy_tree(
                &target.join(CODEX_SYSTEM_SKILLS),
                &link.join(CODEX_SYSTEM_SKILLS),
            );
        }
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => return, // a file: not ours to replace
        Err(_) => {
            if std::fs::create_dir(link).is_err() {
                return;
            }
        }
    }

    if let Ok(entries) = std::fs::read_dir(link) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            if name == CODEX_SYSTEM_SKILLS {
                continue;
            }
            let here = entry.path();
            let shared = target.join(&name);
            let Ok(meta) = std::fs::symlink_metadata(&here) else {
                continue;
            };
            if meta.file_type().is_symlink() {
                let ours = std::fs::read_link(&here).is_ok_and(|to| to == shared);
                if ours && std::fs::symlink_metadata(&shared).is_err() {
                    let _ = std::fs::remove_file(&here);
                }
            } else if std::fs::symlink_metadata(&shared).is_err()
                && std::fs::rename(&here, &shared).is_ok()
            {
                let _ = symlink(&shared, &here);
            }
        }
    }

    let Ok(entries) = std::fs::read_dir(target) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name == CODEX_SYSTEM_SKILLS {
            continue;
        }
        let here = link.join(&name);
        if std::fs::symlink_metadata(&here).is_err() {
            let _ = symlink(entry.path(), &here);
        }
    }
}

/// Copy a directory tree, keeping symlinks as symlinks.
#[cfg(unix)]
fn copy_tree(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(from)?;
    if meta.file_type().is_symlink() {
        std::os::unix::fs::symlink(std::fs::read_link(from)?, to)
    } else if meta.is_dir() {
        std::fs::create_dir_all(to)?;
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            copy_tree(&entry.path(), &to.join(entry.file_name()))?;
        }
        Ok(())
    } else {
        std::fs::copy(from, to).map(|_| ())
    }
}

#[cfg(not(unix))]
pub fn link_shared_state(
    _provider: &str,
    _home: &std::path::Path,
    _is_cross: bool,
    _categories: Option<&[String]>,
) {
}

/// Point a Codex profile home at the package root the native install actually lives in.
///
/// Codex's installer derives where the standalone package goes from `CODEX_HOME`
/// (`$CODEX_HOME/packages/standalone`) while always writing the launcher to `~/.local/bin`. Run
/// `codex update` from inside `aas exec` and the update therefore lands in that one account's
/// profile, and the launcher every other account shares is repointed into it — each profile
/// drifting to whichever version it last updated itself to.
///
/// Linking the directory keeps one install behind every profile, so an in-session update moves
/// them all together. Unlike the shared-state categories this is not opt-in: a profile that opts
/// out of sharing still has to update the runtime it is running, not a private copy of it.
#[cfg(unix)]
pub fn link_codex_package_root(provider: &str, home: &std::path::Path) {
    use std::os::unix::fs::symlink;

    if crate::naming::normalize_provider_key(provider) != "codex" {
        return;
    }
    let Some(base) = crate::platform::system_home_for("codex") else {
        return;
    };
    let target = base.join("packages");
    let link = home.join("packages");
    // A system profile runs out of the native home already.
    let canon = |p: &std::path::Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    if canon(&base) == canon(home) {
        return;
    }
    // Only ever mirror an install that exists; never invent a package root.
    if !target.is_dir() {
        return;
    }
    match std::fs::symlink_metadata(&link) {
        Ok(meta) if meta.file_type().is_symlink() => {
            let _ = std::fs::remove_file(&link); // replace a stale link
        }
        Ok(_) => return, // a real directory is an update that already landed here — leave it
        Err(_) => {}
    }
    let _ = symlink(&target, &link);
}

#[cfg(not(unix))]
pub fn link_codex_package_root(_provider: &str, _home: &std::path::Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn codex_profiles_keep_their_own_system_skills() {
        use std::os::unix::fs::symlink;
        let _guard = crate::ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let root = std::env::temp_dir().join(format!("aas-codex-skills-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let codex_home = root.join("codex");
        let skills = codex_home.join("skills");
        std::fs::create_dir_all(skills.join(".system/imagegen")).unwrap();
        std::fs::write(skills.join(".system/.codex-system-skills.marker"), "app").unwrap();
        std::fs::create_dir_all(skills.join("mine")).unwrap();
        std::fs::create_dir_all(skills.join("gone")).unwrap();
        std::env::set_var("CODEX_HOME", &codex_home);

        // A profile from an older aas: the whole directory linked.
        let home = root.join("profile");
        std::fs::create_dir_all(&home).unwrap();
        symlink(&skills, home.join("skills")).unwrap();
        link_shared_state("codex", &home, false, None);
        let here = home.join("skills");
        assert!(!std::fs::symlink_metadata(&here)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            std::fs::read_link(here.join("mine")).unwrap(),
            skills.join("mine")
        );
        assert_eq!(
            std::fs::read_to_string(here.join(".system/.codex-system-skills.marker")).unwrap(),
            "app",
            "sessions already running keep the bundle they were reading"
        );
        assert!(here.join(".system/imagegen").is_dir());

        // Codex writes this profile's bundle; the system home's is untouched.
        std::fs::write(here.join(".system/.codex-system-skills.marker"), "cli").unwrap();
        // A skill installed from inside the profile, one removed from the system home, and
        // one added there since.
        std::fs::create_dir_all(here.join("installed")).unwrap();
        std::fs::write(here.join("installed/SKILL.md"), "x").unwrap();
        std::fs::remove_dir_all(skills.join("gone")).unwrap();
        std::fs::create_dir_all(skills.join("new")).unwrap();

        link_shared_state("codex", &home, false, None);
        assert_eq!(
            std::fs::read_to_string(skills.join(".system/.codex-system-skills.marker")).unwrap(),
            "app"
        );
        assert_eq!(
            std::fs::read_to_string(here.join(".system/.codex-system-skills.marker")).unwrap(),
            "cli"
        );
        assert_eq!(
            std::fs::read_to_string(skills.join("installed/SKILL.md")).unwrap(),
            "x"
        );
        assert_eq!(
            std::fs::read_link(here.join("installed")).unwrap(),
            skills.join("installed")
        );
        assert!(std::fs::symlink_metadata(here.join("gone")).is_err());
        assert_eq!(
            std::fs::read_link(here.join("new")).unwrap(),
            skills.join("new")
        );

        // An isolated profile keeps a skills directory of its own.
        let isolated = root.join("isolated");
        std::fs::create_dir_all(&isolated).unwrap();
        link_shared_state("codex", &isolated, false, Some(&[]));
        assert!(!isolated.join("skills").exists());

        std::env::remove_var("CODEX_HOME");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn narrowing_sharing_removes_only_links_aas_made() {
        use std::os::unix::fs::symlink;
        let _guard = crate::ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let root = std::env::temp_dir().join(format!("aas-unshare-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let claude_home = root.join("claude");
        std::fs::create_dir_all(claude_home.join("projects")).unwrap();
        std::fs::write(claude_home.join("settings.json"), "{}").unwrap();
        std::fs::write(claude_home.join("CLAUDE.md"), "sys").unwrap();
        let prior = std::env::var_os("CLAUDE_CONFIG_DIR");
        std::env::set_var("CLAUDE_CONFIG_DIR", &claude_home);

        let home = root.join("profile");
        std::fs::create_dir_all(&home).unwrap();
        link_shared_state("claude", &home, false, None);
        assert_eq!(
            std::fs::read_link(home.join("projects")).unwrap(),
            claude_home.join("projects")
        );
        // Real data and a link the user made elsewhere sit beside aas's links.
        std::fs::remove_file(home.join("CLAUDE.md")).unwrap();
        std::fs::write(home.join("CLAUDE.md"), "mine").unwrap();
        let elsewhere = root.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::remove_file(home.join("agents")).unwrap();
        symlink(&elsewhere, home.join("agents")).unwrap();

        // `--share sessions`: settings stop being shared, sessions stay.
        link_shared_state("claude", &home, false, Some(&["sessions".to_string()]));
        assert!(std::fs::read_link(home.join("projects")).is_ok());
        assert!(std::fs::symlink_metadata(home.join("settings.json")).is_err());
        assert!(std::fs::symlink_metadata(home.join("skills")).is_err());
        assert_eq!(
            std::fs::read_to_string(home.join("CLAUDE.md")).unwrap(),
            "mine"
        );
        assert_eq!(std::fs::read_link(home.join("agents")).unwrap(), elsewhere);

        // `--isolated`: nothing of the system home stays linked; the system home is untouched.
        link_shared_state("claude", &home, false, Some(&[]));
        assert!(std::fs::symlink_metadata(home.join("projects")).is_err());
        assert!(claude_home.join("projects").is_dir());
        assert_eq!(
            std::fs::read_to_string(claude_home.join("settings.json")).unwrap(),
            "{}"
        );
        assert_eq!(std::fs::read_link(home.join("agents")).unwrap(), elsewhere);

        match prior {
            Some(v) => std::env::set_var("CLAUDE_CONFIG_DIR", v),
            None => std::env::remove_var("CLAUDE_CONFIG_DIR"),
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn isolating_codex_skills_keeps_system_and_own_skills() {
        use std::os::unix::fs::symlink;
        let _guard = crate::ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let root = std::env::temp_dir().join(format!("aas-unshare-codex-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let codex_home = root.join("codex");
        let skills = codex_home.join("skills");
        std::fs::create_dir_all(skills.join(".system")).unwrap();
        std::fs::create_dir_all(skills.join("shared")).unwrap();
        std::fs::write(codex_home.join("config.toml"), "sys").unwrap();
        std::env::set_var("CODEX_HOME", &codex_home);

        // Per-entry links in the profile's own skills directory.
        let home = root.join("profile");
        std::fs::create_dir_all(&home).unwrap();
        link_shared_state("codex", &home, false, None);
        let here = home.join("skills");
        assert!(std::fs::read_link(here.join("shared")).is_ok());
        std::fs::create_dir_all(here.join(".system")).unwrap();
        std::fs::write(here.join(".system/marker"), "cli").unwrap();
        let elsewhere = root.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        symlink(&elsewhere, here.join("other")).unwrap();
        // One the system home also has stays private to the profile, as a real directory.
        std::fs::create_dir_all(skills.join("private")).unwrap();
        std::fs::create_dir_all(here.join("private")).unwrap();

        link_shared_state("codex", &home, false, Some(&[]));
        assert!(here.is_dir());
        assert!(std::fs::symlink_metadata(here.join("shared")).is_err());
        assert!(skills.join("shared").is_dir());
        assert_eq!(
            std::fs::read_to_string(here.join(".system/marker")).unwrap(),
            "cli"
        );
        assert_eq!(std::fs::read_link(here.join("other")).unwrap(), elsewhere);
        assert!(std::fs::read_link(here.join("private")).is_err());
        assert!(here.join("private").is_dir());
        assert!(std::fs::symlink_metadata(home.join("config.toml")).is_err());

        // A whole-directory link left by an older aas is dropped too.
        let old = root.join("old");
        std::fs::create_dir_all(&old).unwrap();
        symlink(&skills, old.join("skills")).unwrap();
        link_shared_state("codex", &old, false, Some(&[]));
        assert!(std::fs::symlink_metadata(old.join("skills")).is_err());
        assert!(skills.join("shared").is_dir());

        // A cross-provider home never keeps a link to the config the proxy rewrites.
        let cross = root.join("cross");
        std::fs::create_dir_all(&cross).unwrap();
        symlink(codex_home.join("config.toml"), cross.join("config.toml")).unwrap();
        link_shared_state("codex", &cross, true, None);
        assert!(std::fs::symlink_metadata(cross.join("config.toml")).is_err());
        assert_eq!(
            std::fs::read_to_string(codex_home.join("config.toml")).unwrap(),
            "sys"
        );

        std::env::remove_var("CODEX_HOME");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn codex_package_root_is_linked_regardless_of_sharing() {
        let _guard = crate::ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let root = std::env::temp_dir().join(format!("aas-pkgroot-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let codex_home = root.join("codex");
        let home = root.join("profile");
        std::fs::create_dir_all(codex_home.join("packages/standalone")).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("CODEX_HOME", &codex_home);

        // `Some(&[])` shares nothing; the package root is still linked.
        link_shared_state("codex", &home, false, Some(&[]));
        link_codex_package_root("codex", &home);
        assert_eq!(
            std::fs::read_link(home.join("packages")).unwrap(),
            codex_home.join("packages"),
            "an isolated profile still has to update the runtime it runs"
        );

        // A real directory is an update that already landed here — never clobbered.
        let other = root.join("profile2");
        std::fs::create_dir_all(other.join("packages/standalone")).unwrap();
        link_codex_package_root("codex", &other);
        assert!(other.join("packages").is_dir());
        assert!(std::fs::read_link(other.join("packages")).is_err());

        // Another provider's home is left alone.
        let claude = root.join("claude-profile");
        std::fs::create_dir_all(&claude).unwrap();
        link_codex_package_root("claude", &claude);
        assert!(!claude.join("packages").exists());

        std::env::remove_var("CODEX_HOME");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn supported_categories() {
        assert_eq!(
            supported_share_categories("claude"),
            vec!["sessions", "skills", "agents", "hooks", "settings"]
        );
        assert_eq!(
            supported_share_categories("codex"),
            vec!["sessions", "skills", "settings"]
        );
        assert_eq!(
            supported_share_categories("grok"),
            vec!["sessions", "skills", "settings"]
        );
        assert_eq!(
            supported_share_categories("pi"),
            vec!["sessions", "skills", "settings"]
        );
    }

    #[test]
    fn resolve_flags() {
        let iso = resolve_share_selection(
            &ShareOpts {
                isolated: true,
                ..Default::default()
            },
            None,
        )
        .unwrap();
        assert_eq!(iso.value, Some(vec![]));
        let sh = resolve_share_selection(
            &ShareOpts {
                shared: true,
                ..Default::default()
            },
            None,
        )
        .unwrap();
        assert_eq!(sh.value, None);
        let only = resolve_share_selection(
            &ShareOpts {
                share: Some("sessions,skills".into()),
                ..Default::default()
            },
            Some("codex"),
        )
        .unwrap();
        assert_eq!(only.value, Some(vec!["sessions".into(), "skills".into()]));
        let isolate = resolve_share_selection(
            &ShareOpts {
                isolate: Some("skills".into()),
                ..Default::default()
            },
            Some("codex"),
        )
        .unwrap();
        assert_eq!(
            isolate.value,
            Some(vec!["sessions".into(), "settings".into()])
        );
        assert!(resolve_share_selection(
            &ShareOpts {
                isolated: true,
                shared: true,
                ..Default::default()
            },
            None
        )
        .is_err());
    }

    #[test]
    fn describe() {
        assert_eq!(
            describe_share(None, Some("codex")),
            "shared: sessions, skills, settings"
        );
        assert_eq!(
            describe_share(Some(&[]), Some("codex")),
            "isolated: sessions, skills, settings"
        );
        assert_eq!(
            describe_share(Some(&["sessions".to_string()]), Some("codex")),
            "shared: sessions (isolated: skills, settings)"
        );
    }
}
