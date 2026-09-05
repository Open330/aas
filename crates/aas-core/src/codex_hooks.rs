//! Codex hook-trust seeding for profile homes.
//!
//! Codex keys its `[hooks.state]` trust entries by the *absolute path of the config file* that
//! declared the hook. `aas` symlinks `config.toml` from the Codex system home into each profile
//! home, so Codex sees a different path per profile and re-prompts "Hooks need review" the first
//! time every profile runs — and again whenever an account rename changes the profile directory.
//!
//! Seeding copies the trust hashes already recorded for the system config onto the profile path,
//! so a profile inherits exactly the trust the user granted in `~/.codex/config.toml` — never more.
//! Entries for profile directories that no longer exist are pruned at the same time.
//!
//! The file is edited textually (append + line filter) rather than round-tripped through a TOML
//! serializer, so the user's comments, ordering and formatting survive untouched.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

/// A `[hooks.state."<config path>:<event>:<i>:<j>"]` table header, split at the path.
struct StateKey {
    /// The config-file path the entry is scoped to.
    config: String,
    /// `<event>:<i>:<j>` — everything after the config path.
    suffix: String,
}

fn parse_state_key(raw: &str) -> Option<StateKey> {
    // Codex writes `<abs path>:<event>:<i>:<j>`; the path itself may contain ':' only in
    // pathological cases, so split off the three trailing fields from the right.
    let mut parts = raw.rsplitn(4, ':');
    let j = parts.next()?;
    let i = parts.next()?;
    let event = parts.next()?;
    let config = parts.next()?;
    if config.is_empty() {
        return None;
    }
    Some(StateKey {
        config: config.to_string(),
        suffix: format!("{event}:{i}:{j}"),
    })
}

/// Unescape a TOML basic string body (the subset Codex emits for paths).
fn unescape_basic(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('\\') => out.push('\\'),
            Some('"') => out.push('"'),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

fn escape_basic(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// The quoted body of a `[hooks.state."…"]` header, or `None` for any other line.
fn state_header_body(line: &str) -> Option<&str> {
    let t = line.trim();
    let inner = t.strip_prefix("[hooks.state.\"")?.strip_suffix("\"]")?;
    Some(inner)
}

/// True for a line that opens any TOML table or array-of-tables.
fn is_table_header(line: &str) -> bool {
    line.trim_start().starts_with('[')
}

fn quoted_value(line: &str, key: &str) -> Option<String> {
    let t = line.trim();
    let rest = t.strip_prefix(key)?;
    let rest = rest.trim_start().strip_prefix('=')?.trim();
    let body = rest.strip_prefix('"')?.strip_suffix('"')?;
    Some(unescape_basic(body))
}

/// Every `hooks.state` entry in `body`, as `(config path, suffix) -> trusted_hash`.
fn collect_state(body: &str) -> BTreeMap<(String, String), String> {
    let mut out = BTreeMap::new();
    let mut current: Option<StateKey> = None;
    for line in body.lines() {
        if is_table_header(line) {
            current = state_header_body(line)
                .map(unescape_basic)
                .as_deref()
                .and_then(parse_state_key);
            continue;
        }
        let Some(key) = current.as_ref() else {
            continue;
        };
        if let Some(hash) = quoted_value(line, "trusted_hash") {
            out.insert((key.config.clone(), key.suffix.clone()), hash);
        }
    }
    out
}

/// Drop whole `[hooks.state."…"]` tables whose key satisfies `drop`.
fn filter_state_tables(body: &str, drop: impl Fn(&StateKey) -> bool) -> (String, usize) {
    let mut out = String::with_capacity(body.len());
    let mut dropping = false;
    let mut dropped = 0usize;
    for line in body.lines() {
        if is_table_header(line) {
            let key = state_header_body(line)
                .map(unescape_basic)
                .as_deref()
                .and_then(parse_state_key);
            dropping = key.map(|k| drop(&k)).unwrap_or(false);
            if dropping {
                dropped += 1;
                continue;
            }
        }
        if dropping {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    (out, dropped)
}

fn write_atomic(path: &Path, body: &str) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let tmp = dir.join(format!(
        ".{}.aas-{}",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("config"),
        std::process::id()
    ));
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(body.as_bytes())?;
        f.sync_all()?;
    }
    if let Ok(meta) = std::fs::metadata(path) {
        let _ = std::fs::set_permissions(&tmp, meta.permissions());
    }
    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// What a seeding run changed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SeedReport {
    /// Trust entries copied onto the profile config path.
    pub seeded: usize,
    /// Stale entries removed because their profile directory is gone.
    pub pruned: usize,
}

impl SeedReport {
    pub fn changed(&self) -> bool {
        self.seeded > 0 || self.pruned > 0
    }
}

/// Copy the system config's hook trust onto `profile_config`, and prune entries belonging to
/// profile directories under `profiles_dir` that no longer exist.
///
/// `profile_config` must be the symlink inside the profile home; it is a no-op unless that link
/// actually resolves to `system_config`. Errors are swallowed by the caller — a profile that
/// cannot be seeded simply falls back to Codex's interactive trust prompt.
pub fn seed_profile_hook_trust(
    system_config: &Path,
    profile_config: &Path,
    profiles_dir: &Path,
) -> std::io::Result<SeedReport> {
    let real = std::fs::canonicalize(system_config)?;
    // Only meaningful while the profile shares the system config; an isolated (real) config
    // carries its own trust state and must not be touched.
    if std::fs::canonicalize(profile_config)? != real {
        return Ok(SeedReport::default());
    }
    let system_key = system_config.to_string_lossy().into_owned();
    let profile_key = profile_config.to_string_lossy().into_owned();
    if system_key == profile_key {
        return Ok(SeedReport::default());
    }

    let body = std::fs::read_to_string(&real)?;
    let state = collect_state(&body);

    let profiles_dir = profiles_dir.to_path_buf();
    let is_stale_profile = |k: &StateKey| {
        let path = PathBuf::from(&k.config);
        path.starts_with(&profiles_dir)
            && k.config != profile_key
            && !path.parent().map(Path::is_dir).unwrap_or(false)
    };
    let (kept, pruned) = filter_state_tables(&body, is_stale_profile);

    let missing: Vec<(String, String)> = state
        .iter()
        .filter(|((config, _), _)| config == &system_key)
        .filter(|((_, suffix), _)| !state.contains_key(&(profile_key.clone(), suffix.clone())))
        .map(|((_, suffix), hash)| (suffix.clone(), hash.clone()))
        .collect();

    if missing.is_empty() && pruned == 0 {
        return Ok(SeedReport::default());
    }

    let mut out = kept;
    if !out.ends_with('\n') {
        out.push('\n');
    }
    if !missing.is_empty() {
        out.push_str("\n# Hook trust inherited from the Codex system config by aas exec.\n");
        for (suffix, hash) in &missing {
            out.push_str(&format!(
                "[hooks.state.\"{}:{}\"]\ntrusted_hash = \"{}\"\n",
                escape_basic(&profile_key),
                escape_basic(suffix),
                escape_basic(hash)
            ));
        }
    }
    write_atomic(&real, &out)?;
    Ok(SeedReport {
        seeded: missing.len(),
        pruned,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn sample(system: &str) -> String {
        format!(
            "model = \"gpt-5\"\n\n\
             [[hooks.SessionStart]]\n\
             hooks = [{{ type = \"command\", command = \"muxa hook codex\" }}]\n\n\
             [hooks.state]\n\n\
             [hooks.state.\"{system}:session_start:0:0\"]\n\
             trusted_hash = \"sha256:aaa\"\n\n\
             [hooks.state.\"{system}:stop:0:0\"]\n\
             trusted_hash = \"sha256:bbb\"\n\n\
             [projects.\"/tmp\"]\n\
             trust_level = \"trusted\"\n"
        )
    }

    #[cfg(unix)]
    struct Fixture {
        system: PathBuf,
        profile_home: PathBuf,
        profiles: PathBuf,
    }

    #[cfg(unix)]
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    #[cfg(unix)]
    fn fixture() -> Fixture {
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("aas-codex-hooks-{}-{seq}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let system_home = root.join("codex");
        let profiles = root.join("profiles");
        let profile_home = profiles.join("codex-a_b");
        std::fs::create_dir_all(&system_home).unwrap();
        std::fs::create_dir_all(&profile_home).unwrap();
        let system = system_home.join("config.toml");
        std::fs::write(&system, sample(&system.to_string_lossy())).unwrap();
        std::os::unix::fs::symlink(&system, profile_home.join("config.toml")).unwrap();
        Fixture {
            system,
            profile_home,
            profiles,
        }
    }

    #[cfg(unix)]
    #[test]
    fn seeds_system_trust_onto_the_profile_config_path() {
        let f = fixture();
        let profile_config = f.profile_home.join("config.toml");
        let report = seed_profile_hook_trust(&f.system, &profile_config, &f.profiles).unwrap();
        assert_eq!(
            report,
            SeedReport {
                seeded: 2,
                pruned: 0
            }
        );

        let body = std::fs::read_to_string(&f.system).unwrap();
        let state = collect_state(&body);
        let key = profile_config.to_string_lossy().into_owned();
        assert_eq!(
            state.get(&(key.clone(), "session_start:0:0".into())),
            Some(&"sha256:aaa".to_string())
        );
        assert_eq!(
            state.get(&(key, "stop:0:0".into())),
            Some(&"sha256:bbb".to_string())
        );
        // The user's own content is preserved verbatim.
        assert!(body.contains("[projects.\"/tmp\"]"));
        assert!(body.contains("model = \"gpt-5\""));
    }

    #[cfg(unix)]
    #[test]
    fn seeding_is_idempotent() {
        let f = fixture();
        let profile_config = f.profile_home.join("config.toml");
        seed_profile_hook_trust(&f.system, &profile_config, &f.profiles).unwrap();
        let once = std::fs::read_to_string(&f.system).unwrap();
        let report = seed_profile_hook_trust(&f.system, &profile_config, &f.profiles).unwrap();
        assert_eq!(report, SeedReport::default());
        assert_eq!(std::fs::read_to_string(&f.system).unwrap(), once);
    }

    #[cfg(unix)]
    #[test]
    fn renamed_profile_entries_are_pruned() {
        let f = fixture();
        let stale = f.profiles.join("codex-old_name").join("config.toml");
        let mut body = std::fs::read_to_string(&f.system).unwrap();
        body.push_str(&format!(
            "\n[hooks.state.\"{}:session_start:0:0\"]\ntrusted_hash = \"sha256:ccc\"\n",
            stale.to_string_lossy()
        ));
        std::fs::write(&f.system, &body).unwrap();

        let profile_config = f.profile_home.join("config.toml");
        let report = seed_profile_hook_trust(&f.system, &profile_config, &f.profiles).unwrap();
        assert_eq!(report.pruned, 1);
        let after = std::fs::read_to_string(&f.system).unwrap();
        assert!(!after.contains("codex-old_name"));
        assert!(after.contains("sha256:aaa"));
    }

    #[cfg(unix)]
    #[test]
    fn an_isolated_profile_config_is_left_alone() {
        let f = fixture();
        let profile_config = f.profile_home.join("config.toml");
        std::fs::remove_file(&profile_config).unwrap();
        std::fs::write(&profile_config, "model = \"gpt-5\"\n").unwrap();
        let report = seed_profile_hook_trust(&f.system, &profile_config, &f.profiles).unwrap();
        assert_eq!(report, SeedReport::default());
        assert_eq!(
            std::fs::read_to_string(&f.system).unwrap(),
            sample(&f.system.to_string_lossy())
        );
    }

    #[cfg(unix)]
    #[test]
    fn untrusted_hooks_are_not_invented() {
        let f = fixture();
        // A config with no trust at all must not gain any.
        std::fs::write(&f.system, "model = \"gpt-5\"\n").unwrap();
        let profile_config = f.profile_home.join("config.toml");
        let report = seed_profile_hook_trust(&f.system, &profile_config, &f.profiles).unwrap();
        assert_eq!(report, SeedReport::default());
    }

    #[test]
    fn paths_with_quotes_round_trip() {
        let raw = r#"/tmp/we"ird\path/config.toml"#;
        assert_eq!(unescape_basic(&escape_basic(raw)), raw);
    }
}
