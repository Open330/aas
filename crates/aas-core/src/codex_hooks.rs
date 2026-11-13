//! Codex hook-trust seeding for profile homes.
//!
//! Codex keys its `[hooks.state]` trust entries by the *absolute path of the config file* that
//! declared the hook. `aas` symlinks `config.toml` from the Codex system home into each profile
//! home, so Codex sees a different path per profile and re-prompts "Hooks need review" the first
//! time every profile runs — and again whenever an account rename changes the profile directory.
//!
//! Codex's trust hash covers only what a hook does, not where it was declared, so a grant made
//! under one path is equally valid under another. Seeding copies a hook's trust onto the profile
//! path when the user already trusted that exact hook under some path in the same file — the
//! system config or another profile — and never trusts a hook the user has not. Entries for
//! profile directories that no longer exist are pruned at the same time.
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
fn filter_state_tables(body: &str, mut drop: impl FnMut(&StateKey) -> bool) -> (String, usize) {
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

/// Codex's trust label for a `[[hooks.<Event>]]` table name.
fn event_label(event: &str) -> Option<&'static str> {
    Some(match event {
        "PreToolUse" => "pre_tool_use",
        "PermissionRequest" => "permission_request",
        "PostToolUse" => "post_tool_use",
        "PreCompact" => "pre_compact",
        "PostCompact" => "post_compact",
        "PreModelRequest" => "pre_model_request",
        "PostModelRequest" => "post_model_request",
        "SessionStart" => "session_start",
        "SessionEnd" => "session_end",
        "UserPromptSubmit" => "user_prompt_submit",
        "SubagentStart" => "subagent_start",
        "SubagentStop" => "subagent_stop",
        "Stop" => "stop",
        "Interrupt" => "interrupt",
        _ => return None,
    })
}

/// Events whose matcher takes part in the hook's identity; the rest drop it before hashing.
fn event_uses_matcher(label: &str) -> bool {
    !matches!(
        label,
        "pre_model_request" | "post_model_request" | "user_prompt_submit" | "stop" | "interrupt"
    )
}

/// Events that can emit `additionalContext`, so `additionalContextLimit` is kept for them.
fn event_takes_context_limit(label: &str) -> bool {
    matches!(
        label,
        "pre_tool_use"
            | "post_tool_use"
            | "session_start"
            | "user_prompt_submit"
            | "subagent_start"
    )
}

/// Codex's default `additionalContextLimit`; a hook that names it hashes as if it named none.
const DEFAULT_CONTEXT_LIMIT: i64 = 2_500;

/// Serialize with every object's keys sorted, the canonical form Codex hashes.
fn canonical_json(value: &serde_json::Value, out: &mut String) {
    match value {
        serde_json::Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, key) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::Value::String(key.clone()).to_string());
                out.push(':');
                canonical_json(&map[key], out);
            }
            out.push('}');
        }
        serde_json::Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                canonical_json(item, out);
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

/// The trust hash Codex computes for one command handler: sha256 over the canonical JSON of the
/// event label, the group's matcher and the normalized handler. It covers what the hook *does*,
/// never where it was declared, which is why one grant can be carried to another config path.
///
/// `None` for anything this does not model (non-command handlers, malformed values); those
/// hooks fall back to the conservative copy rule in [`seed_profile_hook_trust`].
fn command_hook_hash(label: &str, matcher: Option<&str>, handler: &toml::Value) -> Option<String> {
    use serde_json::{json, Map, Value};
    use sha2::{Digest, Sha256};

    let table = handler.as_table()?;
    if table.get("type")?.as_str()? != "command" {
        return None;
    }
    let windows = table
        .get("commandWindows")
        .or_else(|| table.get("command_windows"))
        .and_then(toml::Value::as_str);
    let command = match (cfg!(windows), windows) {
        (true, Some(windows)) => windows,
        _ => table.get("command")?.as_str()?,
    };
    if command.trim().is_empty() {
        return None;
    }
    let timeout = match table.get("timeout") {
        None => None,
        Some(value) => Some(u64::try_from(value.as_integer()?).ok()?),
    };
    let timeout = match label {
        "session_end" | "interrupt" => timeout.unwrap_or(1).clamp(1, 3),
        _ => timeout.unwrap_or(600).max(1),
    };
    let r#async = match table.get("async") {
        None => false,
        Some(value) => value.as_bool()?,
    };

    let mut normalized = Map::new();
    normalized.insert("type".into(), json!("command"));
    normalized.insert("command".into(), json!(command));
    normalized.insert("timeout".into(), json!(timeout));
    normalized.insert("async".into(), json!(r#async));
    if let Some(status) = table.get("statusMessage") {
        normalized.insert("statusMessage".into(), json!(status.as_str()?));
    }
    if let Some(limit) = table.get("additionalContextLimit") {
        let limit = limit.as_integer().filter(|limit| *limit >= 0)?;
        if event_takes_context_limit(label) && limit != DEFAULT_CONTEXT_LIMIT {
            normalized.insert("additionalContextLimit".into(), json!(limit));
        }
    }

    let mut identity = Map::new();
    identity.insert("event_name".into(), json!(label));
    if let Some(matcher) = matcher {
        identity.insert("matcher".into(), json!(matcher));
    }
    identity.insert(
        "hooks".into(),
        Value::Array(vec![Value::Object(normalized)]),
    );

    let mut text = String::new();
    canonical_json(&Value::Object(identity), &mut text);
    let digest = Sha256::digest(text.as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    Some(format!("sha256:{hex}"))
}

/// The hash Codex would expect for every command hook `body` declares, keyed `<event>:<i>:<j>`.
/// Empty when the file does not parse; callers then fall back to the conservative rule.
fn declared_hashes(body: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Ok(doc) = body.parse::<toml::Value>() else {
        return out;
    };
    let Some(hooks) = doc.get("hooks").and_then(toml::Value::as_table) else {
        return out;
    };
    for (event, groups) in hooks {
        let (Some(label), Some(groups)) = (event_label(event), groups.as_array()) else {
            continue;
        };
        for (group_index, group) in groups.iter().enumerate() {
            let matcher = group
                .get("matcher")
                .and_then(toml::Value::as_str)
                .filter(|_| event_uses_matcher(label));
            let Some(handlers) = group.get("hooks").and_then(toml::Value::as_array) else {
                continue;
            };
            for (handler_index, handler) in handlers.iter().enumerate() {
                if let Some(hash) = command_hook_hash(label, matcher, handler) {
                    out.insert(format!("{label}:{group_index}:{handler_index}"), hash);
                }
            }
        }
    }
    out
}

/// What a seeding run changed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SeedReport {
    /// Trust entries written onto the profile config path, new or refreshed.
    pub seeded: usize,
    /// Stale entries removed because their profile directory is gone.
    pub pruned: usize,
}

impl SeedReport {
    pub fn changed(&self) -> bool {
        self.seeded > 0 || self.pruned > 0
    }
}

/// Carry hook trust the user already granted onto `profile_config`, and prune entries belonging
/// to profile directories under `profiles_dir` that no longer exist.
///
/// A hook is trusted on the profile path only when the user trusted that exact hook somewhere:
/// its current hash (see [`command_hook_hash`]) must already be recorded as `trusted_hash` under
/// some config path in the same file — the system config, or another profile where the user
/// answered the review prompt. An entry left stale by a changed hook is refreshed the same way.
/// A hook nobody trusted is never touched, so Codex still asks about it.
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
    let trusted: std::collections::BTreeSet<&String> = state.values().collect();
    let declared = declared_hashes(&body);

    let mut wanted: BTreeMap<String, String> = BTreeMap::new();
    for (suffix, hash) in &declared {
        let current = state.get(&(profile_key.clone(), suffix.clone()));
        if current != Some(hash) && trusted.contains(hash) {
            wanted.insert(suffix.clone(), hash.clone());
        }
    }
    // Hooks whose identity is not modelled keep the conservative rule: a profile with no entry
    // inherits the system config's.
    for ((config, suffix), hash) in &state {
        if config == &system_key
            && !declared.contains_key(suffix)
            && !state.contains_key(&(profile_key.clone(), suffix.clone()))
        {
            wanted.insert(suffix.clone(), hash.clone());
        }
    }

    let profiles_dir = profiles_dir.to_path_buf();
    let is_stale_profile = |k: &StateKey| {
        let path = PathBuf::from(&k.config);
        path.starts_with(&profiles_dir)
            && k.config != profile_key
            && !path.parent().map(Path::is_dir).unwrap_or(false)
    };
    let mut pruned = 0usize;
    let (kept, _) = filter_state_tables(&body, |k| {
        if k.config == profile_key {
            // Replaced below with the refreshed hash.
            return wanted.contains_key(&k.suffix);
        }
        let stale = is_stale_profile(k);
        pruned += usize::from(stale);
        stale
    });

    if wanted.is_empty() && pruned == 0 {
        return Ok(SeedReport::default());
    }

    let mut out = kept;
    if !out.ends_with('\n') {
        out.push('\n');
    }
    if !wanted.is_empty() {
        out.push_str("\n# Hook trust carried over by aas exec from a path where it was granted.\n");
        for (suffix, hash) in &wanted {
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
        seeded: wanted.len(),
        pruned,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOOK: &str = "muxa hook codex";

    /// The hash Codex records for the sample `SessionStart` hook.
    fn session_start_hash(command: &str) -> String {
        let handler: toml::Value =
            toml::from_str(&format!("type = \"command\"\ncommand = \"{command}\"\n")).unwrap();
        command_hook_hash("session_start", None, &handler).unwrap()
    }

    #[cfg(unix)]
    fn sample(trusted_at: &str, command: &str, hash: &str) -> String {
        format!(
            "model = \"gpt-5\"\n\n\
             [[hooks.SessionStart]]\n\
             hooks = [{{ type = \"command\", command = \"{command}\" }}]\n\n\
             [[hooks.Stop]]\n\
             hooks = [{{ type = \"mcp_tool\", server = \"s\", tool = \"t\" }}]\n\n\
             [hooks.state]\n\n\
             [hooks.state.\"{trusted_at}:session_start:0:0\"]\n\
             trusted_hash = \"{hash}\"\n\n\
             [hooks.state.\"{trusted_at}:stop:0:0\"]\n\
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
        let body = sample(&system.to_string_lossy(), HOOK, &session_start_hash(HOOK));
        std::fs::write(&system, body).unwrap();
        std::os::unix::fs::symlink(&system, profile_home.join("config.toml")).unwrap();
        Fixture {
            system,
            profile_home,
            profiles,
        }
    }

    #[cfg(unix)]
    fn profile_state(f: &Fixture) -> BTreeMap<String, String> {
        let key = f
            .profile_home
            .join("config.toml")
            .to_string_lossy()
            .into_owned();
        collect_state(&std::fs::read_to_string(&f.system).unwrap())
            .into_iter()
            .filter(|((config, _), _)| config == &key)
            .map(|((_, suffix), hash)| (suffix, hash))
            .collect()
    }

    #[test]
    fn hashes_match_what_codex_records() {
        // Hashes taken from a real Codex config: a trivial hook trusted in a scratch home, and
        // a production SessionStart hook.
        assert_eq!(
            session_start_hash("true"),
            "sha256:696abd07fc74736ff2d809abef82925e1ea697cd2e547d73317c677b0d407441"
        );
        assert_eq!(
            session_start_hash("tend hook codex --event session_start"),
            "sha256:d5dfe8ff78f5821ef13e45eee31023f02fae0b64f5620e2219239bf02ce5c1a2"
        );
        let handler: toml::Value = toml::from_str(
            "type = \"command\"\ncommand = \"tend hook codex --event pre_tool_use\"",
        )
        .unwrap();
        assert_eq!(
            command_hook_hash("pre_tool_use", None, &handler).unwrap(),
            "sha256:270142e975acd1e3788822119c11b61b665c5db864cf75ba457d952e23e347ef"
        );
    }

    #[test]
    fn normalization_follows_codex() {
        let parse = |s: &str| -> toml::Value { toml::from_str(s).unwrap() };
        let base = parse("type = \"command\"\ncommand = \"x\"");
        // The default timeout and context limit hash the same as leaving them out.
        let explicit = parse(
            "type = \"command\"\ncommand = \"x\"\ntimeout = 600\nadditionalContextLimit = 2500",
        );
        assert_eq!(
            command_hook_hash("session_start", None, &base),
            command_hook_hash("session_start", None, &explicit)
        );
        // Stop drops its matcher; PreToolUse keeps it.
        assert_eq!(
            command_hook_hash("stop", None, &base),
            declared_hashes("[[hooks.Stop]]\nmatcher = \"m\"\nhooks = [{ type = \"command\", command = \"x\" }]\n")
                .get("stop:0:0")
                .cloned()
        );
        assert_ne!(
            command_hook_hash("pre_tool_use", None, &base),
            command_hook_hash("pre_tool_use", Some("m"), &base)
        );
        assert_eq!(
            command_hook_hash("stop", None, &parse("type = \"prompt\"")),
            None
        );
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
        let state = profile_state(&f);
        assert_eq!(
            state.get("session_start:0:0"),
            Some(&session_start_hash(HOOK))
        );
        // An unmodelled handler inherits the system entry as before.
        assert_eq!(state.get("stop:0:0"), Some(&"sha256:bbb".to_string()));
        // The user's own content is preserved verbatim.
        let body = std::fs::read_to_string(&f.system).unwrap();
        assert!(body.contains("[projects.\"/tmp\"]"));
        assert!(body.contains("model = \"gpt-5\""));
    }

    #[cfg(unix)]
    #[test]
    fn trust_granted_in_another_profile_is_carried_over() {
        let f = fixture();
        // The user answered the review prompt in a sibling profile, never in the system home.
        let sibling = f.profiles.join("codex-c_d").join("config.toml");
        std::fs::create_dir_all(sibling.parent().unwrap()).unwrap();
        let body = sample(&sibling.to_string_lossy(), HOOK, &session_start_hash(HOOK));
        std::fs::write(&f.system, body).unwrap();

        let profile_config = f.profile_home.join("config.toml");
        let report = seed_profile_hook_trust(&f.system, &profile_config, &f.profiles).unwrap();
        assert_eq!(report.seeded, 1);
        let state = profile_state(&f);
        assert_eq!(
            state.get("session_start:0:0"),
            Some(&session_start_hash(HOOK))
        );
        // Without a system entry the unmodelled hook stays for Codex to ask about.
        assert_eq!(state.get("stop:0:0"), None);
    }

    #[cfg(unix)]
    #[test]
    fn a_stale_profile_entry_is_refreshed_from_a_newer_grant() {
        let f = fixture();
        let profile_config = f.profile_home.join("config.toml");
        let sibling = f.profiles.join("codex-c_d").join("config.toml");
        std::fs::create_dir_all(sibling.parent().unwrap()).unwrap();
        // The hook was rewritten; the sibling re-trusted it, this profile still holds the old hash.
        let new_hook = "muxa hook codex --v2";
        let mut body = sample(
            &sibling.to_string_lossy(),
            new_hook,
            &session_start_hash(new_hook),
        );
        body.push_str(&format!(
            "\n[hooks.state.\"{}:session_start:0:0\"]\ntrusted_hash = \"{}\"\n",
            profile_config.to_string_lossy(),
            session_start_hash(HOOK)
        ));
        std::fs::write(&f.system, body).unwrap();

        seed_profile_hook_trust(&f.system, &profile_config, &f.profiles).unwrap();
        let state = profile_state(&f);
        assert_eq!(
            state.get("session_start:0:0"),
            Some(&session_start_hash(new_hook))
        );
        let after = std::fs::read_to_string(&f.system).unwrap();
        assert_eq!(after.matches(&session_start_hash(HOOK)).count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn a_changed_hook_nobody_retrusted_is_left_for_codex() {
        let f = fixture();
        // Every recorded grant is for the old command; the declared hook is new.
        let old = std::fs::read_to_string(&f.system).unwrap();
        std::fs::write(
            &f.system,
            old.replace(&format!("command = \"{HOOK}\""), "command = \"other\""),
        )
        .unwrap();
        let profile_config = f.profile_home.join("config.toml");
        seed_profile_hook_trust(&f.system, &profile_config, &f.profiles).unwrap();
        assert_eq!(profile_state(&f).get("session_start:0:0"), None);
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
        assert!(after.contains("sha256:bbb"));
    }

    #[cfg(unix)]
    #[test]
    fn an_isolated_profile_config_is_left_alone() {
        let f = fixture();
        let profile_config = f.profile_home.join("config.toml");
        std::fs::remove_file(&profile_config).unwrap();
        std::fs::write(&profile_config, "model = \"gpt-5\"\n").unwrap();
        let before = std::fs::read_to_string(&f.system).unwrap();
        let report = seed_profile_hook_trust(&f.system, &profile_config, &f.profiles).unwrap();
        assert_eq!(report, SeedReport::default());
        assert_eq!(std::fs::read_to_string(&f.system).unwrap(), before);
    }

    #[cfg(unix)]
    #[test]
    fn untrusted_hooks_are_not_invented() {
        let f = fixture();
        // Hooks with no trust anywhere must not gain any.
        std::fs::write(
            &f.system,
            format!("[[hooks.SessionStart]]\nhooks = [{{ type = \"command\", command = \"{HOOK}\" }}]\n"),
        )
        .unwrap();
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
