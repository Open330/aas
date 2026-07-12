//! Cross-process keyed operation locks.
//!
//! AAS is commonly invoked by a terminal, BarShelf, and editor integrations at the same time.
//! A stable hash gives each provider/account operation its own lock without putting user supplied
//! account names in a filesystem path. Closing the returned file releases the advisory lock.

use fs2::FileExt;
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

/// Acquire an exclusive process lock for `(scope, key)`.
pub fn acquire(scope: &str, key: &str) -> io::Result<File> {
    let dir = crate::platform::asx_config_dir();
    acquire_in(&dir, scope, key)
}

fn acquire_in(dir: &Path, scope: &str, key: &str) -> io::Result<File> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }

    let mut hasher = Sha256::new();
    hasher.update(scope.as_bytes());
    hasher.update([0]);
    hasher.update(key.as_bytes());
    let digest = hasher.finalize();
    let hash = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let path = dir.join(format!(".operation-{}.lock", &hash[..24]));

    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    file.lock_exclusive()?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn same_key_serializes_process_style_callers() {
        let dir = std::env::temp_dir().join(format!(
            "aas-keyed-lock-{}-{}",
            std::process::id(),
            crate::model::now_iso().replace([':', '.'], "-")
        ));
        let first = acquire_in(&dir, "usage", "claude/a").unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let (acquired_tx, acquired_rx) = mpsc::channel();
        let thread_dir = dir.clone();
        let waiter = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            let second = acquire_in(&thread_dir, "usage", "claude/a").unwrap();
            acquired_tx.send(()).unwrap();
            drop(second);
        });

        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(acquired_rx.recv_timeout(Duration::from_millis(50)).is_err());
        drop(first);
        acquired_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        waiter.join().unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }
}
