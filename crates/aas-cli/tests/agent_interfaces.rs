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
