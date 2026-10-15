//! Codex 0.157's control socket lives below CODEX_HOME. Our isolated profile paths
//! can exceed sockaddr_un::sun_path, especially on macOS. Keep profile identity
//! intact and use Codex's in-process mode when the installed CLI supports it.

use std::collections::HashMap;
use std::path::Path;

#[cfg(unix)]
fn socket_path_too_long(home: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let socket = home.join("app-server-control/app-server-control.sock");
    // sockaddr_un contains only integer fields, so all-zero is a valid value.
    let addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    socket.as_os_str().as_bytes().len() >= addr.sun_path.len()
}

#[cfg(not(unix))]
fn socket_path_too_long(_home: &Path) -> bool {
    false
}

pub(super) async fn avoid_long_socket_path(
    program: &Path,
    env: &HashMap<String, String>,
    args: &mut Vec<String>,
) -> bool {
    // Stop at the prompt delimiter: text after it must never be treated as flags.
    if args
        .iter()
        .take_while(|arg| arg.as_str() != "--")
        .any(|arg| arg == "--no-daemon" || arg == "--remote" || arg.starts_with("--remote="))
    {
        return false;
    }
    let home = env
        .get("CODEX_HOME")
        .filter(|home| !home.is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| aas_core::platform::home_dir().join(".codex"));
    // Match the absolute path Codex uses, including relative AAS_CONFIG_DIR values.
    let home = if home.is_absolute() {
        home
    } else {
        match std::env::current_dir() {
            Ok(cwd) => cwd.join(home),
            Err(_) => return false,
        }
    };
    if !socket_path_too_long(&home) {
        return false;
    }

    // Older Codex versions have no daemon and reject --no-daemon. Probe the exact
    // executable and environment used for the launch (including shim hand-off).
    let mut command = tokio::process::Command::new(program);
    command
        .env_clear()
        .envs(env)
        .arg("--help")
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    let output = tokio::time::timeout(std::time::Duration::from_secs(3), command.output()).await;
    let Ok(Ok(output)) = output else {
        return false;
    };
    if !output.status.success()
        || !String::from_utf8_lossy(&output.stdout)
            .split_whitespace()
            .any(|word| word == "--no-daemon")
    {
        return false;
    }
    // Root option works for fresh sessions, resume and fork without rewriting
    // subcommand arguments or moving a user's -- prompt delimiter.
    args.insert(0, "--no-daemon".into());
    true
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn checks_native_socket_limit_in_bytes_including_nul() {
        let addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
        let suffix_len = "/app-server-control/app-server-control.sock".len();
        let home_len = addr.sun_path.len() - suffix_len;
        assert!(!socket_path_too_long(Path::new(&"x".repeat(home_len - 1))));
        assert!(socket_path_too_long(Path::new(&"x".repeat(home_len))));
        assert!(socket_path_too_long(Path::new(&"한".repeat(home_len))));
        assert!(socket_path_too_long(Path::new(
            "/Users/june/Library/Application Support/asx/profiles/codex-e-ed_codex"
        )));
    }

    #[tokio::test]
    async fn probes_support_and_preserves_resume_fork_and_prompt_arguments() {
        let dir = std::env::temp_dir().join(format!("aas-daemon-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let program = dir.join("codex");
        std::fs::write(&program, "#!/bin/sh\n[ \"$1\" = --help ] || exit 1\nprintf '  --no-daemon  Run without server\\n'\n").unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut env = HashMap::from([("CODEX_HOME".into(), format!("/{}", "x".repeat(120)))]);
        for original in [
            vec![],
            vec!["resume", "--last"],
            vec!["fork", "session-id"],
            vec!["--", "--no-daemon"],
        ] {
            let mut args: Vec<String> = original.iter().map(|s| s.to_string()).collect();
            assert!(avoid_long_socket_path(&program, &env, &mut args).await);
            assert_eq!(args[0], "--no-daemon");
            assert_eq!(&args[1..], original);
        }
        for original in [
            vec!["--no-daemon"],
            vec!["--remote", "ws://localhost:1234"],
            vec!["--remote=unix:///tmp/server.sock"],
        ] {
            let mut args: Vec<String> = original.iter().map(|s| s.to_string()).collect();
            assert!(!avoid_long_socket_path(&program, &env, &mut args).await);
            assert_eq!(args, original);
        }
        std::fs::write(&program, "#!/bin/sh\nprintf 'Old Codex help\\n'\n").unwrap();
        assert!(!avoid_long_socket_path(&program, &env, &mut vec![]).await);
        std::fs::write(&program, "#!/bin/sh\nprintf '  --no-daemon\\n'\nexit 1\n").unwrap();
        assert!(!avoid_long_socket_path(&program, &env, &mut vec![]).await);
        env.insert("CODEX_HOME".into(), "/tmp/short".into());
        assert!(!avoid_long_socket_path(&program, &env, &mut vec![]).await);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
