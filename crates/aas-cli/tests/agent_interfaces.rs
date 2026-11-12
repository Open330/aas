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
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_aas"));
        cmd.env("AAS_CONFIG_DIR", &self.0)
            .env("AAS_NO_KEYCHAIN", "1");
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
