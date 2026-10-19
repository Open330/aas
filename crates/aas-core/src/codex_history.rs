//! Preserve Codex's profile-local rollout index when its home moves.

use fs2::FileExt;
use rusqlite::{Connection, OpenFlags};
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

fn error(error: impl std::fmt::Display) -> io::Error {
    io::Error::other(format!("could not update Codex history paths: {error}"))
}

/// Probe Codex's native locks, retaining them during rename where the OS permits it. Native
/// Codex and older AAS launches participate too; no process scanner or external tool is needed.
pub(crate) fn lock_idle_profile(home: &Path) -> io::Result<Vec<File>> {
    let mut paths = vec![home.join(".sqlite-maintenance.lock")];
    match std::fs::read_dir(home.join("thread-writer-locks")) {
        Ok(entries) => {
            for entry in entries {
                let path = entry?.path();
                if path
                    .extension()
                    .is_some_and(|extension| extension == "lock")
                {
                    paths.push(path);
                }
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let mut locks = Vec::new();
    for path in paths {
        let file = match File::open(&path) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        file.try_lock_exclusive().map_err(|e| {
            io::Error::new(
                e.kind(),
                format!(
                    "cannot rename Codex profile {}: it is in use or its lock is unavailable; close its Codex sessions and daemon, then retry ({e})",
                    home.display()
                ),
            )
        })?;
        locks.push(file);
    }
    // Windows byte-range locks prevent renaming the containing directory even when handles
    // allow delete sharing. Release our probe handles first; Windows still rejects the move
    // if an agent reopens the files in the meantime. Unix can retain these locks across rename.
    #[cfg(windows)]
    locks.clear();
    Ok(locks)
}

struct DatabaseChange {
    name: PathBuf,
    rows: Vec<(String, String, String)>,
}

#[derive(Default)]
pub(crate) struct HistoryRewrite {
    changes: Vec<DatabaseChange>,
}

impl HistoryRewrite {
    /// Each database update is atomic. Keep the exact previous values for rollback if another
    /// database, the directory move, or the account/active-marker transaction fails later.
    pub(crate) fn apply(&mut self, home: &Path, from: &Path, to: &Path) -> io::Result<()> {
        let prefix = format!("{}{sep}", from.display(), sep = std::path::MAIN_SEPARATOR);
        let replacement = format!("{}{sep}", to.display(), sep = std::path::MAIN_SEPARATOR);
        for entry in std::fs::read_dir(home)? {
            let entry = entry?;
            let name = entry.file_name();
            let name_text = name.to_string_lossy();
            if !name_text.starts_with("state_") || !name_text.ends_with(".sqlite") {
                continue;
            }
            if entry.file_type()?.is_symlink() {
                return Err(error(
                    "refusing to rewrite a shared/symlinked state database",
                ));
            }
            let mut db = open(&entry.path())?;
            let has_paths: bool = db.query_row(
                "SELECT count(*) = 2 FROM pragma_table_info('threads') WHERE name IN ('id', 'rollout_path')",
                [], |row| row.get(0),
            ).map_err(error)?;
            if !has_paths {
                continue;
            }
            let tx = db.transaction().map_err(error)?;
            let rows = {
                let mut stmt = tx.prepare(
                    "SELECT id, rollout_path, ?2 || substr(rollout_path, length(?1) + 1) FROM threads WHERE instr(rollout_path, ?1) = 1",
                ).map_err(error)?;
                let mapped = stmt
                    .query_map([&prefix, &replacement], |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, String>(2)?,
                        ))
                    })
                    .map_err(error)?;
                mapped.collect::<Result<Vec<_>, _>>().map_err(error)?
            };
            for (id, before, after) in &rows {
                tx.execute(
                    "UPDATE threads SET rollout_path = ?1 WHERE id = ?2 AND rollout_path = ?3",
                    [after, id, before],
                )
                .map_err(error)?;
            }
            tx.commit().map_err(error)?;
            self.changes.push(DatabaseChange {
                name: name.into(),
                rows,
            });
        }
        Ok(())
    }

    pub(crate) fn rollback(&self, home: &Path) -> io::Result<()> {
        for change in self.changes.iter().rev() {
            let mut db = open(&home.join(&change.name))?;
            let tx = db.transaction().map_err(error)?;
            for (id, before, after) in &change.rows {
                tx.execute(
                    "UPDATE threads SET rollout_path = ?1 WHERE id = ?2 AND rollout_path = ?3",
                    [before, id, after],
                )
                .map_err(error)?;
            }
            tx.commit().map_err(error)?;
        }
        Ok(())
    }
}

fn open(path: &Path) -> io::Result<Connection> {
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE).map_err(error)?;
    // Never wait indefinitely on a running agent or silently create a missing state database.
    db.busy_timeout(std::time::Duration::from_secs(1))
        .map_err(error)?;
    Ok(db)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{model::AccountRecord, naming::profile_home, store::AccountStore};
    use std::ffi::OsString;
    use std::sync::{
        atomic::{AtomicU64, Ordering},
        MutexGuard,
    };

    static SEQ: AtomicU64 = AtomicU64::new(0);
    struct Fixture {
        dir: PathBuf,
        previous: Option<OsString>,
        _guard: MutexGuard<'static, ()>,
    }
    impl Fixture {
        fn new() -> Self {
            let guard = crate::ENV_LOCK.lock().unwrap();
            let dir = std::env::temp_dir().join(format!(
                "aas-history-한글_%_{}-{}",
                std::process::id(),
                SEQ.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&dir).unwrap();
            let previous = std::env::var_os("AAS_CONFIG_DIR");
            std::env::set_var("AAS_CONFIG_DIR", &dir);
            let fixture = Self {
                dir,
                previous,
                _guard: guard,
            };
            fixture
                .store()
                .add(AccountRecord::new("codex", "history-old"))
                .unwrap();
            let home = fixture.home("history-old");
            std::fs::create_dir_all(&home).unwrap();
            std::fs::write(home.join("auth.json"), b"credential-preserved").unwrap();
            fixture
        }
        fn store(&self) -> AccountStore {
            AccountStore::at(&self.dir)
        }
        fn home(&self, name: &str) -> PathBuf {
            profile_home("codex", name)
        }
        fn database(&self) -> Connection {
            let db = Connection::open(self.home("history-old").join("state_5.sqlite")).unwrap();
            db.execute_batch(
                "CREATE TABLE threads(id TEXT PRIMARY KEY, rollout_path TEXT, title TEXT);",
            )
            .unwrap();
            for (id, path) in [
                (
                    "session",
                    self.home("history-old").join("sessions/rollout.jsonl"),
                ),
                (
                    "archived",
                    self.home("history-old")
                        .join("archived_sessions/rollout.jsonl"),
                ),
                (
                    "unrelated",
                    self.home("history-old")
                        .with_file_name("codex-old-other")
                        .join("sessions/rollout.jsonl"),
                ),
                (
                    "already-new",
                    self.home("history-new").join("sessions/other.jsonl"),
                ),
            ] {
                db.execute(
                    "INSERT INTO threads VALUES(?1, ?2, ?3)",
                    [
                        id,
                        &path.to_string_lossy(),
                        &self.home("history-old").to_string_lossy(),
                    ],
                )
                .unwrap();
            }
            db
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => std::env::set_var("AAS_CONFIG_DIR", value),
                None => std::env::remove_var("AAS_CONFIG_DIR"),
            }
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
    fn paths(db: &Connection) -> Vec<(String, String, String)> {
        db.prepare("SELECT id, rollout_path, title FROM threads ORDER BY id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    #[test]
    fn rename_preserves_credentials_and_rewrites_only_own_history_paths() {
        let f = Fixture::new();
        let before = paths(&f.database());
        f.store().set_active("codex", "history-old").unwrap();
        f.store().rename("history-old", "history-new").unwrap();
        let db = Connection::open(f.home("history-new").join("state_5.sqlite")).unwrap();
        let after = paths(&db);
        for ((id, old_path, title), (new_id, new_path, new_title)) in before.iter().zip(&after) {
            assert_eq!(id, new_id);
            assert_eq!(title, new_title);
            let expected = if ["session", "archived"].contains(&id.as_str()) {
                f.home("history-new")
                    .join(
                        Path::new(old_path)
                            .strip_prefix(f.home("history-old"))
                            .unwrap(),
                    )
                    .to_string_lossy()
                    .into_owned()
            } else {
                old_path.clone()
            };
            assert_eq!(new_path, &expected);
        }
        assert_eq!(
            std::fs::read(f.home("history-new").join("auth.json")).unwrap(),
            b"credential-preserved"
        );
        assert!(!f.home("history-old").exists());
        assert_eq!(
            f.store().get_active("codex").unwrap().as_deref(),
            Some("history-new")
        );
    }

    #[test]
    fn metadata_failure_rolls_back_history_and_profile_without_touching_unrelated_rows() {
        let f = Fixture::new();
        let before = paths(&f.database());
        f.store().set_active("codex", "history-old").unwrap();
        std::fs::create_dir(f.dir.join(".active.json.bak")).unwrap();
        assert!(f.store().rename("history-old", "history-new").is_err());
        assert!(!f.home("history-new").exists());
        let db = Connection::open(f.home("history-old").join("state_5.sqlite")).unwrap();
        assert_eq!(paths(&db), before);
        assert!(f.store().get("codex", "history-old").unwrap().is_some());
        assert_eq!(
            f.store().get_active("codex").unwrap().as_deref(),
            Some("history-old")
        );
    }

    #[test]
    fn corrupt_database_aborts_rename_and_restores_original_profile() {
        let f = Fixture::new();
        std::fs::write(
            f.home("history-old").join("state_5.sqlite"),
            b"not a database",
        )
        .unwrap();
        assert!(f.store().rename("history-old", "history-new").is_err());
        assert!(f.home("history-old").is_dir());
        assert!(!f.home("history-new").exists());
        assert_eq!(
            std::fs::read(f.home("history-old").join("state_5.sqlite")).unwrap(),
            b"not a database"
        );
        assert!(f.store().get("codex", "history-old").unwrap().is_some());
    }

    #[test]
    fn native_codex_locks_prevent_renaming_live_profiles() {
        let f = Fixture::new();
        for relative in [
            ".sqlite-maintenance.lock",
            "thread-writer-locks/session.lock",
        ] {
            let path = f.home("history-old").join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let lock = File::create(path).unwrap();
            FileExt::lock_shared(&lock).unwrap();
            let err = f.store().rename("history-old", "history-new").unwrap_err();
            assert!(
                err.to_string().contains("close its Codex sessions"),
                "{err}"
            );
            assert!(f.home("history-old").exists());
            assert!(!f.home("history-new").exists());
            assert!(f.store().get("codex", "history-old").unwrap().is_some());
            drop(lock);
        }
        f.store().rename("history-old", "history-new").unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn shared_state_database_is_not_modified_and_shared_sessions_link_survives() {
        use std::os::unix::fs::symlink;
        let f = Fixture::new();
        let external = f.dir.join("shared-state.sqlite");
        let db = f.database();
        let before = paths(&db);
        drop(db);
        std::fs::rename(f.home("history-old").join("state_5.sqlite"), &external).unwrap();
        symlink(&external, f.home("history-old").join("state_5.sqlite")).unwrap();
        assert!(f.store().rename("history-old", "history-new").is_err());
        assert_eq!(paths(&Connection::open(external).unwrap()), before);
        std::fs::remove_file(f.home("history-old").join("state_5.sqlite")).unwrap();
        let shared = f.dir.join("shared-sessions");
        std::fs::create_dir(&shared).unwrap();
        symlink(&shared, f.home("history-old").join("sessions")).unwrap();
        f.store().rename("history-old", "history-new").unwrap();
        assert_eq!(
            std::fs::read_link(f.home("history-new").join("sessions")).unwrap(),
            shared
        );
    }
}
