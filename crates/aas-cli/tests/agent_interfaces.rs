use serde_json::{json, Value};
use std::{path::PathBuf, process::Command};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("aas-agent-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let accounts: Vec<_> = [("codex", "primary"), ("codex", "backup"), ("kimi", "other")]
            .into_iter().map(|(provider, name)| json!({"provider":provider,"name":name,"addedAt":"2026-01-01","profileType":"isolated"})).collect();
        std::fs::write(
            dir.join("accounts.json"),
            json!({"version":1,"accounts":accounts}).to_string(),
        )
        .unwrap();
        Self(dir)
    }
    fn command(&self) -> Command {
        // A private HOME keeps the run off the real agent homes: `exec` links a profile's
        // shared state from the system home and records hook trust in its config.
        let home = self.0.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_aas"));
        cmd.env("AAS_CONFIG_DIR", &self.0)
            .env("AAS_NO_KEYCHAIN", "1")
            .env("HOME", &home)
            .env_remove("CODEX_HOME")
            .env_remove("CLAUDE_CONFIG_DIR")
            .env_remove("GROK_HOME")
            .env_remove("PI_CODING_AGENT_DIR");
        cmd
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(unix)]
#[test]
fn shim_selection_and_status_follow_the_pinned_launcher() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let fixture = Fixture::new();
    let release = fixture.0.join("release");
    std::fs::write(&release, "#!/bin/sh\necho chosen-version\n").unwrap();
    std::fs::set_permissions(&release, std::fs::Permissions::from_mode(0o755)).unwrap();
    let launcher = fixture.0.join("codex");
    symlink(&release, &launcher).unwrap();
    let output = fixture
        .command()
        .args(["shim", "install", "codex", "--bin"])
        .arg(&launcher)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let shim = fixture.0.join("shims/codex");
    let output = Command::new(&shim)
        .env_clear()
        .env("AAS_CONFIG_DIR", &fixture.0)
        .env("AAS_NO_KEYCHAIN", "1")
        .arg("--version")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "chosen-version\n");
    // Status must still show the recorded launcher even when PATH cannot resolve it.
    let output = fixture
        .command()
        .env("PATH", "")
        .args(["shim", "status"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stdout)
        .contains(&format!("{} (pinned)", launcher.display())));
    std::fs::remove_file(release).unwrap();
    let output = fixture.command().args(["shim", "status"]).output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let report = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        report.contains("pinned CLI is missing or not executable"),
        "{report}"
    );
}

#[cfg(unix)]
#[test]
fn codex_long_profile_launch_uses_supported_fallback_without_moving_home() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let bin = fixture.0.join("codex");
    std::fs::write(
        &bin,
        "#!/bin/sh\nif [ \"$1\" = --help ]; then\n  printf '  --no-daemon  Run without server\\n'\nelse\n  printf '%s\\n' \"$CODEX_HOME\" \"$@\"\nfi\n",
    ).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o700)).unwrap();
    let native = fixture.0.join("native");
    std::fs::create_dir(&native).unwrap();
    let output = fixture
        .command()
        .env("AAS_SHIM_BIN", &bin)
        .env("CODEX_HOME", &native)
        .args(["e", "primary", "resume", "--last"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected_home = fixture.0.join("profiles/codex-primary");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        format!("{}\n--no-daemon\nresume\n--last\n", expected_home.display())
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("running with --no-daemon"));
}

#[test]
fn candidates_json_ranks_cached_accounts_and_honors_exclusions() {
    let fixture = Fixture::new();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let entry = |used| json!({"fetchedAtMs":now,"usage":{"headline":"test","plan":null,"meters":[{"label":"5h","used_pct":used,"reset_ms":null}],"notes":[],"error":null}});
    std::fs::write(
        fixture.0.join("usage-cache.json"),
        json!({"version":1,"entries":{"codex/primary":entry(80),"codex/backup":entry(10)}})
            .to_string(),
    )
    .unwrap();
    let output = fixture
        .command()
        .args(["candidates", "--provider", "codex", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let body: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(body["schemaVersion"], 1);
    assert_eq!(body["accounts"].as_array().unwrap().len(), 2);
    assert_eq!(body["accounts"][0]["name"], "backup");
    assert_eq!(body["accounts"][0]["cached"], true);
    let output = fixture
        .command()
        .args([
            "candidates",
            "--provider",
            "codex",
            "--exclude",
            "backup",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let body: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(body["accounts"][0]["name"], "primary");
    assert_eq!(body["accounts"][1]["reason"], "excluded");
}

#[cfg(unix)]
#[test]
fn provider_exec_auto_picks_the_best_paced_account_unless_told_active() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let bin = fixture.0.join("codex");
    std::fs::write(&bin, "#!/bin/sh\nprintf '%s\\n' \"$CODEX_HOME\" \"$@\"\n").unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o700)).unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let day = 86_400_000_i64;
    // primary: 70% of the week left but 6 days to go (pace ~0.82).
    // backup: 40% left, expiring in 1 day (pace 2.8) — spend it first.
    let entry = |used: f64, reset: i64| json!({"fetchedAtMs":now,"usage":{"headline":"test","plan":null,"meters":[{"label":"7d","used_pct":used,"reset_ms":reset}],"notes":[],"error":null}});
    std::fs::write(
        fixture.0.join("usage-cache.json"),
        json!({"version":1,"entries":{
            "codex/primary":entry(30.0, now + 6 * day),
            "codex/backup":entry(60.0, now + day),
        }})
        .to_string(),
    )
    .unwrap();
    std::fs::write(fixture.0.join(".active.json"), r#"{"codex":"primary"}"#).unwrap();
    let run = |args: &[&str]| {
        let output = fixture
            .command()
            .env("AAS_SHIM_BIN", &bin)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        (
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    };
    let home = |name: &str| fixture.0.join(format!("profiles/codex-{name}"));

    let (stdout, stderr) = run(&["codex", "resume", "--last"]);
    assert_eq!(
        stdout,
        format!("{}\nresume\n--last\n", home("backup").display())
    );
    assert!(stderr.contains("→ codex/backup"), "{stderr}");
    assert!(stderr.contains("pace 2.80×"), "{stderr}");

    let (stdout, _) = run(&["e", "codex", "--pick", "active", "resume"]);
    assert_eq!(stdout, format!("{}\nresume\n", home("primary").display()));

    // Naming an account never auto-picks, and `--pick` then belongs to the agent.
    let (stdout, _) = run(&["e", "primary", "resume", "--pick", "active"]);
    assert_eq!(
        stdout,
        format!("{}\nresume\n--pick\nactive\n", home("primary").display())
    );

    // A `--fallback` account is never auto-picked as the primary (which would be a duplicate).
    let output = fixture
        .command()
        .env("AAS_SHIM_BIN", &bin)
        .args(["e", "codex", "--fallback", "backup", "resume"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("Duplicate fallback"), "{stderr}");
    assert!(stderr.contains("fallback account: backup"), "{stderr}");

    let (stdout, _) = run(&["candidates", "--provider", "codex", "--json"]);
    let body: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(body["accounts"][0]["name"], "backup");
    assert!((body["accounts"][0]["paceScore"].as_f64().unwrap() - 2.8).abs() < 1e-3);
}

#[test]
fn invalid_fallback_pools_fail_before_launch_or_refresh() {
    let fixture = Fixture::new();
    for (account, message) in [
        ("primary", "Duplicate fallback"),
        ("other", "same provider and endpoint"),
        ("missing", "not found"),
    ] {
        let output = fixture
            .command()
            .args(["exec", "primary", "--fallback", account, "--", "--version"])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(message),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[cfg(unix)]
#[test]
fn update_runs_the_pinned_cli_outside_profiles_and_prunes_superseded_releases() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let fixture = Fixture::new();
    let root = fixture.0.join("standalone");
    let releases = root.join("releases");
    let launcher = fixture.0.join("bin/codex");
    std::fs::create_dir_all(launcher.parent().unwrap()).unwrap();
    let (root_s, launcher_s) = (root.display(), launcher.display());
    let script = format!(
        "#!/bin/sh\ncase \"$1\" in\n--version) echo \"codex-cli $(basename \"$(dirname \"$(dirname \"$(readlink {launcher_s})\")\")\")\";;\nupdate) echo \"CODEX_HOME=${{CODEX_HOME:-unset}}\" > {root_s}/update.log; mkdir -p {root_s}/releases/2.0.0/bin; cp \"$(readlink {launcher_s})\" {root_s}/releases/2.0.0/bin/codex; ln -sfn {root_s}/releases/2.0.0/bin/codex {launcher_s};;\nesac\n"
    );
    for release in ["0.9.0", "1.0.0"] {
        let bin = releases.join(release).join("bin/codex");
        std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
        std::fs::write(&bin, &script).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    symlink(releases.join("1.0.0/bin/codex"), &launcher).unwrap();
    let pin = fixture
        .command()
        .args(["shim", "install", "codex", "--bin"])
        .arg(&launcher)
        .output()
        .unwrap();
    assert!(pin.status.success(), "{pin:?}");

    let dry = fixture
        .command()
        .args(["update", "codex", "--dry-run"])
        .output()
        .unwrap();
    let dry_out =
        String::from_utf8_lossy(&dry.stdout).into_owned() + &String::from_utf8_lossy(&dry.stderr);
    assert!(dry.status.success(), "{dry_out}");
    assert!(dry_out.contains("codex 1.0.0 (standalone)"), "{dry_out}");
    assert!(dry_out.contains("would prune 0.9.0"), "{dry_out}");
    assert!(releases.join("0.9.0").exists() && !root.join("update.log").exists());

    let output = fixture
        .command()
        .env("CODEX_HOME", fixture.0.join("profiles/codex-primary"))
        .args(["update", "codex"])
        .output()
        .unwrap();
    let report = String::from_utf8_lossy(&output.stdout).into_owned()
        + &String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{report}");
    assert!(report.contains("codex 1.0.0 → 2.0.0"), "{report}");
    assert_eq!(
        std::fs::read_to_string(root.join("update.log")).unwrap(),
        "CODEX_HOME=unset\n"
    );
    assert!(releases.join("2.0.0").exists());
    assert!(!releases.join("1.0.0").exists() && !releases.join("0.9.0").exists());
}

#[cfg(unix)]
#[test]
fn state_moves_from_the_old_directory_and_the_link_is_removed_on_finish() {
    let root = std::env::temp_dir().join(format!("aas-migrate-test-{}", uuid::Uuid::new_v4()));
    let base = if cfg!(target_os = "macos") {
        root.join("Library/Application Support")
    } else {
        root.join(".config")
    };
    let legacy = base.join("asx");
    std::fs::create_dir_all(legacy.join("profiles/codex-primary")).unwrap();
    std::fs::write(
        legacy.join("accounts.json"),
        json!({"version":1,"accounts":[{"provider":"codex","name":"primary","addedAt":"2026-01-01","profileType":"isolated"}]}).to_string(),
    )
    .unwrap();
    let aas = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_aas"))
            .env("HOME", &root)
            .env("AAS_NO_KEYCHAIN", "1")
            .env_remove("AAS_CONFIG_DIR")
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("CODEX_HOME")
            .env_remove("CLAUDE_CONFIG_DIR")
            .args(args)
            .output()
            .unwrap()
    };

    let output = aas(&["list"]);
    assert!(output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("moved state"));
    assert!(String::from_utf8_lossy(&output.stdout).contains("primary"));
    assert!(base.join("aas/accounts.json").is_file());
    // Agents started from the old path still reach their profile.
    assert!(std::fs::symlink_metadata(&legacy)
        .unwrap()
        .file_type()
        .is_symlink());
    assert!(legacy.join("profiles/codex-primary").is_dir());

    // A second run neither moves nor announces anything.
    let output = aas(&["list"]);
    assert!(output.status.success(), "{output:?}");
    assert!(!String::from_utf8_lossy(&output.stderr).contains("moved state"));

    let output = aas(&["migrate"]);
    assert!(output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stdout).contains("--finish"));

    let output = aas(&["migrate", "--finish"]);
    assert!(output.status.success(), "{output:?}");
    assert!(std::fs::symlink_metadata(&legacy).is_err());
    assert!(base.join("aas/profiles/codex-primary").is_dir());

    let output = aas(&["list"]);
    assert!(String::from_utf8_lossy(&output.stdout).contains("primary"));
    let _ = std::fs::remove_dir_all(&root);
}
