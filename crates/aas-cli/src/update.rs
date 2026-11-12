//! `aas update`: update the native agent CLIs `aas exec` launches, in place, and drop the
//! releases the update superseded.
//!
//! The binary updated is the one `aas exec` actually runs: a shim's pinned CLI, else the first
//! real CLI on `PATH`. How to update it follows from where it lives: a package manager owns
//! `node_modules` and Homebrew paths; standalone installs (Claude native, Codex standalone)
//! update themselves.

use crate::{exec, shim, ui};
use std::path::{Path, PathBuf};
use std::time::Duration;

const VERSION_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Eq)]
enum Method {
    /// A global JS package; `manager` is `npm`, `pnpm` or `bun`.
    Package {
        manager: &'static str,
        package: String,
        /// `npm` installed beside this package's prefix, so a version manager's other Node
        /// installations are not updated by mistake.
        npm: Option<PathBuf>,
    },
    Brew {
        name: String,
        cask: bool,
    },
    /// The CLI's own `update` subcommand.
    SelfUpdate,
}

impl Method {
    fn label(&self) -> &'static str {
        match self {
            Method::Package { manager, .. } => manager,
            Method::Brew { .. } => "homebrew",
            Method::SelfUpdate => "standalone",
        }
    }

    fn command(&self, launcher: &Path) -> (PathBuf, Vec<String>) {
        match self {
            Method::Package {
                manager: "npm",
                package,
                npm,
            } => (
                npm.clone().unwrap_or_else(|| "npm".into()),
                vec!["install".into(), "-g".into(), format!("{package}@latest")],
            ),
            Method::Package {
                manager, package, ..
            } => (
                PathBuf::from(*manager),
                vec!["add".into(), "-g".into(), format!("{package}@latest")],
            ),
            Method::Brew { name, cask } => {
                let mut args = vec!["upgrade".to_string()];
                if *cask {
                    args.push("--cask".into());
                }
                args.push(name.clone());
                ("brew".into(), args)
            }
            Method::SelfUpdate => (launcher.to_path_buf(), vec!["update".into()]),
        }
    }
}

/// Segment of `path` directly after the first component named `marker`.
fn segment_after<'a>(parts: &[&'a str], marker: &str) -> Option<(usize, &'a str)> {
    let index = parts.iter().position(|part| *part == marker)?;
    parts.get(index + 1).map(|part| (index + 1, *part))
}

/// How the CLI whose resolved executable is `real` is updated, if aas knows.
fn detect(real: &Path, provider: &str) -> Option<Method> {
    let text = real.to_string_lossy().replace('\\', "/");
    let parts: Vec<&str> = text.split('/').filter(|p| !p.is_empty()).collect();
    if let Some((index, first)) = segment_after(&parts, "node_modules") {
        let package = if first.starts_with('@') {
            format!("{first}/{}", parts.get(index + 1)?)
        } else {
            first.to_string()
        };
        let manager = if parts.contains(&".bun") {
            "bun"
        } else if parts.iter().any(|p| p.eq_ignore_ascii_case("pnpm")) {
            "pnpm"
        } else {
            "npm"
        };
        // `<prefix>/lib/node_modules/<pkg>` → `<prefix>/bin/npm`.
        let npm = text.find("/lib/node_modules/").and_then(|at| {
            let candidate = PathBuf::from(format!("{}/bin/npm", &text[..at]));
            candidate.is_file().then_some(candidate)
        });
        return Some(Method::Package {
            manager,
            package,
            npm,
        });
    }
    for (marker, cask) in [("Cellar", false), ("Caskroom", true)] {
        if let Some((_, name)) = segment_after(&parts, marker) {
            return Some(Method::Brew {
                name: name.to_string(),
                cask,
            });
        }
    }
    matches!(provider, "claude" | "codex").then_some(Method::SelfUpdate)
}

/// A standalone install keeps one entry per version under `releases/` (Codex) or `versions/`
/// (Claude native) and points its launcher at one of them. Returns that directory and the entry
/// `real` lives in.
fn version_store(real: &Path) -> Option<(PathBuf, std::ffi::OsString)> {
    let mut child = real;
    for ancestor in real.ancestors().skip(1) {
        let name = ancestor.file_name()?.to_str()?;
        if name == "releases" || name == "versions" {
            return Some((ancestor.to_path_buf(), child.file_name()?.to_os_string()));
        }
        child = ancestor;
    }
    None
}

fn tree_size(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if !meta.is_dir() {
        return meta.len();
    }
    std::fs::read_dir(path)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| tree_size(&entry.path()))
                .sum()
        })
        .unwrap_or(0)
}

#[derive(Debug, Default, PartialEq)]
struct Prune {
    removed: Vec<(String, u64)>,
    in_use: Vec<String>,
    failed: Vec<String>,
}

/// Superseded entries of `store`: everything but `keep`, never a symlink, never a hidden or
/// non-version-looking name, and never an entry a running process executes from.
fn prune_candidates(store: &Path, keep: &std::ffi::OsStr) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(store) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .flatten()
        .filter(|entry| entry.file_name() != keep)
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with(|c: char| c.is_ascii_digit()))
        })
        .filter(|entry| {
            entry
                .file_type()
                .map(|kind| !kind.is_symlink())
                .unwrap_or(false)
        })
        .map(|entry| entry.path())
        .collect();
    found.sort();
    found
}

fn prune(store: &Path, keep: &std::ffi::OsStr, running: &[PathBuf], dry_run: bool) -> Prune {
    let mut report = Prune::default();
    for path in prune_candidates(store, keep) {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let canonical = path.canonicalize().unwrap_or_else(|_| path.clone());
        if running.iter().any(|exe| exe.starts_with(&canonical)) {
            report.in_use.push(name);
            continue;
        }
        let size = tree_size(&path);
        let removed = dry_run
            || if path.is_dir() {
                std::fs::remove_dir_all(&path).is_ok()
            } else {
                std::fs::remove_file(&path).is_ok()
            };
        if removed {
            report.removed.push((name, size));
        } else {
            report.failed.push(name);
        }
    }
    report
}

/// Executables of running processes, so a release a live agent still runs from is kept.
#[cfg(target_os = "macos")]
fn running_executables() -> Vec<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    if count <= 0 {
        return Vec::new();
    }
    let mut pids = vec![0 as libc::pid_t; count as usize + 64];
    let bytes = (pids.len() * std::mem::size_of::<libc::pid_t>()) as libc::c_int;
    let count = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
    pids.truncate(count.max(0) as usize);
    let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    pids.into_iter()
        .filter_map(|pid| {
            let len = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
            (len > 0).then(|| PathBuf::from(std::ffi::OsStr::from_bytes(&buf[..len as usize])))
        })
        .collect()
}

#[cfg(target_os = "linux")]
fn running_executables() -> Vec<PathBuf> {
    std::fs::read_dir("/proc")
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|entry| std::fs::read_link(entry.path().join("exe")).ok())
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn running_executables() -> Vec<PathBuf> {
    Vec::new()
}

async fn version(bin: &Path) -> Option<String> {
    let mut cmd = tokio::process::Command::new(bin);
    cmd.arg("--version")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    scrub(&mut cmd);
    let output = tokio::time::timeout(VERSION_TIMEOUT, cmd.output())
        .await
        .ok()?
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    // `codex-cli 0.167.0`, `2.1.322 (Claude Code)`: keep the first version-looking token.
    let line = text.lines().next()?.trim();
    line.split_whitespace()
        .find(|token| token.starts_with(|c: char| c.is_ascii_digit()))
        .or(Some(line))
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

/// An update run from inside `aas exec` must not see that profile's home: Codex derives its
/// package root from `CODEX_HOME`, so it would unpack into one account's profile while
/// repointing the launcher every account shares.
fn scrub(cmd: &mut tokio::process::Command) {
    for provider in shim::SHIMMABLE {
        if let Some(var) = exec::agent_home_env(provider) {
            cmd.env_remove(var);
        }
    }
    cmd.env_remove(exec::SHIM_BIN_ENV)
        .env_remove(exec::SHIM_GUARD_ENV);
}

fn megabytes(bytes: u64) -> String {
    format!("{} MB", bytes / (1024 * 1024))
}

fn targets(providers: &[String]) -> anyhow::Result<Vec<String>> {
    if providers.is_empty() {
        return Ok(shim::SHIMMABLE.iter().map(|p| p.to_string()).collect());
    }
    providers
        .iter()
        .map(|raw| {
            let key = aas_core::naming::normalize_provider_key(raw);
            anyhow::ensure!(
                shim::SHIMMABLE.contains(&key.as_str()),
                "no native CLI to update for provider '{raw}'"
            );
            Ok(key)
        })
        .collect()
}

pub async fn run(providers: &[String], dry_run: bool, no_prune: bool) -> anyhow::Result<()> {
    let explicit = !providers.is_empty();
    let mut failures = 0usize;
    let mut found = 0usize;
    for provider in targets(providers)? {
        let Some(bin) = exec::agent_bin(&provider) else {
            continue;
        };
        let Some(launcher) = shim::launched_binary(bin) else {
            if explicit {
                ui::warn(format!("{bin}: not installed — skipped"));
            }
            continue;
        };
        found += 1;
        let real = launcher.canonicalize().unwrap_or_else(|_| launcher.clone());
        let Some(method) = detect(&real, &provider) else {
            ui::warn(format!(
                "{bin}: don't know how {} was installed — update it manually",
                real.display()
            ));
            continue;
        };
        let (program, args) = method.command(&launcher);
        let before = version(&launcher).await;
        ui::step(format!(
            "{bin} {} ({}): {} {}",
            before.as_deref().unwrap_or("?"),
            method.label(),
            program.display(),
            args.join(" ")
        ));
        shim::warn_alternatives(bin, &launcher);

        if !dry_run {
            let mut cmd = tokio::process::Command::new(&program);
            cmd.args(&args);
            scrub(&mut cmd);
            match cmd.status().await {
                Ok(status) if status.success() => {}
                Ok(status) => {
                    ui::error(format!("{bin}: update exited with {status}"));
                    failures += 1;
                    continue;
                }
                Err(error) => {
                    ui::error(format!(
                        "{bin}: could not run {}: {error}",
                        program.display()
                    ));
                    failures += 1;
                    continue;
                }
            }
            let after = version(&launcher).await;
            match (&before, &after) {
                (Some(b), Some(a)) if a != b => ui::success(format!("{bin} {b} → {a}")),
                (_, Some(a)) => ui::success(format!("{bin} {a} (up to date)")),
                _ => ui::success(format!("{bin} updated")),
            }
        }

        if no_prune || method != Method::SelfUpdate {
            continue;
        }
        // Resolve again: the update repoints the launcher at the release it installed.
        let current = launcher.canonicalize().unwrap_or(real);
        let Some((store, keep)) = version_store(&current) else {
            continue;
        };
        let report = prune(&store, &keep, &running_executables(), dry_run);
        let verb = if dry_run { "would prune" } else { "pruned" };
        for (name, size) in &report.removed {
            ui::hint(format!("{verb} {name} ({})", megabytes(*size)));
        }
        for name in &report.in_use {
            ui::hint(format!("kept {name} (a running process uses it)"));
        }
        for name in &report.failed {
            ui::warn(format!(
                "{bin}: could not remove {}",
                store.join(name).display()
            ));
        }
        let freed: u64 = report.removed.iter().map(|(_, size)| size).sum();
        if freed > 0 {
            ui::hint(format!(
                "kept {}, {} {}",
                keep.to_string_lossy(),
                if dry_run {
                    "would reclaim"
                } else {
                    "reclaimed"
                },
                megabytes(freed)
            ));
        }
    }
    if found == 0 {
        ui::warn("no agent CLIs found to update.");
    }
    anyhow::ensure!(failures == 0, "{failures} update(s) failed");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_method_follows_the_resolved_path() {
        let npm = |path: &str| match detect(Path::new(path), "pi") {
            Some(Method::Package {
                manager, package, ..
            }) => Some((manager, package)),
            _ => None,
        };
        assert_eq!(
            npm("/usr/local/lib/node_modules/@mariozechner/pi-coding-agent/dist/cli.js"),
            Some(("npm", "@mariozechner/pi-coding-agent".into()))
        );
        assert_eq!(
            npm("/Users/a/.bun/install/global/node_modules/grok-cli/bin/grok"),
            Some(("bun", "grok-cli".into()))
        );
        assert_eq!(
            npm("/Users/a/Library/pnpm/global/5/node_modules/@openai/codex/bin/codex.js"),
            Some(("pnpm", "@openai/codex".into()))
        );
        assert_eq!(
            detect(
                Path::new("/opt/homebrew/Cellar/codex/0.1/bin/codex"),
                "codex"
            ),
            Some(Method::Brew {
                name: "codex".into(),
                cask: false
            })
        );
        assert_eq!(
            detect(
                Path::new("/opt/homebrew/Caskroom/claude-code/2.1/claude"),
                "claude"
            ),
            Some(Method::Brew {
                name: "claude-code".into(),
                cask: true
            })
        );
        let native = Path::new("/Users/a/.local/share/claude/versions/2.1.322");
        assert_eq!(detect(native, "claude"), Some(Method::SelfUpdate));
        assert_eq!(detect(Path::new("/usr/bin/grok"), "grok"), None);
    }

    #[test]
    fn commands_per_method() {
        let launcher = Path::new("/bin/codex");
        let pkg = Method::Package {
            manager: "npm",
            package: "@openai/codex".into(),
            npm: None,
        };
        assert_eq!(
            pkg.command(launcher),
            (
                PathBuf::from("npm"),
                vec!["install".into(), "-g".into(), "@openai/codex@latest".into()]
            )
        );
        assert_eq!(
            Method::SelfUpdate.command(launcher),
            (launcher.to_path_buf(), vec!["update".into()])
        );
        let cask = Method::Brew {
            name: "claude-code".into(),
            cask: true,
        };
        assert_eq!(
            cask.command(launcher).1,
            ["upgrade", "--cask", "claude-code"]
        );
    }

    #[test]
    fn version_store_finds_the_entry_the_launcher_resolves_into() {
        let (store, keep) = version_store(Path::new(
            "/h/.codex/packages/standalone/releases/0.167.0-arm/bin/codex",
        ))
        .unwrap();
        assert_eq!(store, Path::new("/h/.codex/packages/standalone/releases"));
        assert_eq!(keep, "0.167.0-arm");
        let (store, keep) =
            version_store(Path::new("/h/.local/share/claude/versions/2.1.322")).unwrap();
        assert_eq!(store, Path::new("/h/.local/share/claude/versions"));
        assert_eq!(keep, "2.1.322");
        assert_eq!(version_store(Path::new("/usr/bin/codex")), None);
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn running_executables_include_this_process() {
        let me = std::env::current_exe().unwrap().canonicalize().unwrap();
        assert!(running_executables()
            .iter()
            .any(|exe| exe.canonicalize().ok().as_deref() == Some(me.as_path())));
    }

    #[test]
    fn prune_keeps_current_running_symlinked_and_non_version_entries() {
        let dir = std::env::temp_dir().join(format!("aas-prune-{}", uuid::Uuid::new_v4()));
        let store = dir.join("releases");
        for release in ["1.0.0", "2.0.0", "3.0.0"] {
            std::fs::create_dir_all(store.join(release).join("bin")).unwrap();
            std::fs::write(store.join(release).join("bin/codex"), "xx").unwrap();
        }
        std::fs::write(store.join(".lock"), "").unwrap();
        std::fs::write(store.join("README"), "").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(store.join("3.0.0"), store.join("9.9.9")).unwrap();
        let running = vec![store.join("2.0.0/bin/codex").canonicalize().unwrap()];

        let dry = prune(&store, "3.0.0".as_ref(), &running, true);
        assert_eq!(dry.removed, vec![("1.0.0".to_string(), 2)]);
        assert!(store.join("1.0.0").exists(), "dry run removes nothing");

        let report = prune(&store, "3.0.0".as_ref(), &running, false);
        assert_eq!(report.removed, vec![("1.0.0".to_string(), 2)]);
        assert_eq!(report.in_use, vec!["2.0.0".to_string()]);
        assert!(!store.join("1.0.0").exists());
        for kept in ["2.0.0", "3.0.0", ".lock", "README"] {
            assert!(store.join(kept).exists(), "{kept}");
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}
