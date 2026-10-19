//! Credential maintenance regressions. Every CLI and token is a local fixture; no network/login.
#![cfg(unix)]

use serde_json::json;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("aas-maintenance-{}", uuid::Uuid::new_v4()));
        let home = dir.join("profiles/codex-work");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir(dir.join("bin")).unwrap();
        std::fs::create_dir(dir.join("native")).unwrap();
        std::fs::write(dir.join("accounts.json"), json!({"version":1,"accounts":[{"provider":"codex","name":"work","addedAt":"2026-01-01","profileType":"isolated"}]}).to_string()).unwrap();
        let f = Self(dir);
        f.auth("e30.eyJleHAiOjF9.sig"); // exp=1
        f.cli("exit 0\n");
        f
    }
    fn auth(&self, token: &str) {
        std::fs::write(self.0.join("profiles/codex-work/auth.json"), json!({"auth_mode":"chatgpt","tokens":{"access_token":token,"refresh_token":"synthetic"}}).to_string()).unwrap();
    }
    fn cli(&self, body: &str) {
        let p = self.0.join("bin/codex");
        std::fs::write(&p, format!("#!/bin/sh\n{body}")).unwrap();
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    fn command(&self) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_aas"));
        c.env("AAS_CONFIG_DIR", &self.0)
            .env("AAS_NO_KEYCHAIN", "1")
            .env("CODEX_HOME", self.0.join("native"))
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.0.join("bin").display()),
            )
            .env("AAS_TEST_ROOT", &self.0)
            .env_remove("AAS_SHIM_BIN")
            .env_remove("AAS_SHIM");
        c
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn successful_native_command_does_not_report_an_expired_credential_as_refreshed() {
    let f = Fixture::new();
    let r = f
        .command()
        .args(["refresh", "work", "--no-login"])
        .output()
        .unwrap();
    assert!(!r.status.success());
    assert!(String::from_utf8_lossy(&r.stderr).contains("still expired"));
    assert!(!String::from_utf8_lossy(&r.stdout).contains('✓'));
}

#[test]
fn refreshed_token_must_have_a_valid_expiration() {
    let f = Fixture::new();
    let fresh = json!({"auth_mode":"chatgpt","tokens":{"access_token":"e30.eyJleHAiOjQxMDI0NDQ4MDB9.sig","refresh_token":"rotated"}}).to_string();
    std::fs::write(f.0.join("fresh.json"), &fresh).unwrap();
    f.cli("cp \"$AAS_TEST_ROOT/fresh.json\" \"$CODEX_HOME/auth.json\"\n");
    let r = f
        .command()
        .args(["refresh", "work", "--no-login"])
        .output()
        .unwrap();
    assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
    assert!(String::from_utf8_lossy(&r.stdout).contains("refreshed via native codex"));
    assert_eq!(
        std::fs::read_to_string(f.0.join("profiles/codex-work/auth.json")).unwrap(),
        fresh
    );
}

#[test]
fn expired_credential_stops_exec_and_proxy_before_launching_an_agent() {
    let f = Fixture::new();
    f.cli("if [ \"$1\" = doctor ] || [ \"$1\" = login ]; then exit 0; fi\ntouch \"$AAS_TEST_ROOT/agent-launched\"\n");
    for args in [
        vec!["exec", "work", "--", "--version"],
        vec!["proxy", "work", "claude"],
    ] {
        let r = f.command().args(args).output().unwrap();
        assert!(!r.status.success());
        assert!(String::from_utf8_lossy(&r.stderr).contains("still expired"));
        assert!(!f.0.join("agent-launched").exists());
    }
}

#[test]
fn native_command_failure_does_not_start_a_relogin_flow() {
    let f = Fixture::new();
    f.cli("if [ \"$1\" = login ] && [ \"$2\" != status ]; then touch \"$AAS_TEST_ROOT/login-started\"; fi\nexit 1\n");
    let r = f.command().args(["refresh", "work"]).output().unwrap();
    assert!(!r.status.success());
    assert!(String::from_utf8_lossy(&r.stderr).contains("check CLI availability"));
    assert!(!f.0.join("login-started").exists());
}

#[test]
fn usage_never_launches_native_codex_and_reuses_failures_until_credentials_change() {
    let f = Fixture::new();
    f.cli("touch \"$AAS_TEST_ROOT/native-launched\"\nexit 1\n");
    let usage = |fresh: bool| {
        let mut command = f.command();
        command.args(["usage", "work", "--json"]);
        if fresh {
            command.arg("--fresh");
        }
        let r = command.output().unwrap();
        assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
        serde_json::from_slice::<serde_json::Value>(&r.stdout).unwrap()["accounts"][0].clone()
    };
    let first = usage(false);
    assert!(first["error"]
        .as_str()
        .unwrap()
        .contains("access token expired"));
    assert_eq!(first["cached"], false);
    assert_eq!(usage(false)["cached"], true);
    std::thread::sleep(std::time::Duration::from_millis(3));
    assert_eq!(
        usage(true)["cached"],
        false,
        "--fresh must bypass an earlier failure"
    );
    f.auth("e30.eyJleHAiOjJ9.sig"); // Different (also expired) credential; no network needed.
    assert_eq!(
        usage(false)["cached"],
        false,
        "native auth changes must invalidate failure reuse"
    );
    let cache: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(f.0.join("usage-cache.json")).unwrap())
            .unwrap();
    assert!(cache["failures"]["codex/work"]["credentialFingerprint"].is_string());
    assert!(!cache.to_string().contains("synthetic"));
    assert!(!f.0.join("native-launched").exists());
}

#[test]
fn expired_auth_does_not_show_a_fresh_success_snapshot_from_an_earlier_credential() {
    let f = Fixture::new();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis();
    // --fresh discovers the local expiry, invalidating even a still-fresh success observation.
    std::fs::write(f.0.join("usage-cache.json"), json!({"version":1,"entries":{"codex/work":{"fetchedAtMs":now,"usage":{"headline":"old success","meters":[],"notes":[],"error":null}}}}).to_string()).unwrap();
    let r = f
        .command()
        .args(["usage", "work", "--json", "--fresh"])
        .output()
        .unwrap();
    assert!(r.status.success());
    let v: serde_json::Value = serde_json::from_slice(&r.stdout).unwrap();
    assert!(v["accounts"][0]["error"]
        .as_str()
        .unwrap()
        .contains("expired"));
    let cache: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(f.0.join("usage-cache.json")).unwrap())
            .unwrap();
    assert!(cache["entries"]["codex/work"].is_null());
}
