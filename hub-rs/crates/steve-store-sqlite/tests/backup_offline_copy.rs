use rusqlite::Connection;
use sha2::{Digest, Sha256};
use std::{
    cell::{Cell, RefCell},
    fs,
    path::Path,
    sync::mpsc,
    time::Duration,
};
use steve_store_sqlite::backup::offline::*;

fn fixture(path: &Path, wal: bool) -> Connection {
    let c = Connection::open(path).unwrap();
    if wal {
        c.execute_batch("PRAGMA journal_mode=WAL;").unwrap();
    }
    c.execute_batch("CREATE TABLE records(id INTEGER PRIMARY KEY, body BLOB); WITH RECURSIVE n(v) AS (VALUES(1) UNION ALL SELECT v+1 FROM n WHERE v<300) INSERT INTO records(body) SELECT zeroblob(4000) FROM n;").unwrap();
    c
}
fn count(path: &Path) -> i64 {
    Connection::open(path)
        .unwrap()
        .query_row("SELECT count(*) FROM records", [], |r| r.get(0))
        .unwrap()
}
fn empty(path: &Path) {
    assert!(
        fs::read_dir(path).unwrap().next().is_none(),
        "failed copies leave no published or partial files"
    );
}
fn io_exists(error: BackupError) {
    assert!(
        matches!(error, BackupError::Io(error) if error.kind()==std::io::ErrorKind::AlreadyExists)
    );
}
#[test]
fn concurrent_writes_preserve_snapshot_progress_and_verified_metadata() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source.db");
    let destination = temp.path().join("backup");
    let writer = fixture(&source, true);
    let seen = RefCell::new(Vec::new());
    let (send, commands) = mpsc::channel();
    let (ack, replies) = mpsc::channel();
    std::thread::scope(|scope| {
        let worker = scope.spawn(move || {
            for () in commands {
                writer
                    .execute("INSERT INTO records(body) VALUES(zeroblob(4000))", [])
                    .unwrap();
                ack.send(()).unwrap();
            }
        });
        let result = backup_file(
            &source,
            &destination,
            &CopyOptions {
                pages_per_step: 8,
                deadline: None,
            },
            &|progress| {
                seen.borrow_mut().push(progress);
                send.send(()).unwrap();
                replies.recv().unwrap();
            },
            &CancelFlag::default(),
        );
        drop(send);
        worker.join().unwrap();
        let result = result.unwrap();
        assert_eq!(
            count(&result.path),
            300,
            "all steps copy the snapshot established before the first callback"
        );
        let reports = seen.borrow();
        assert!(reports.len() > 1);
        assert_eq!(count(&source), 300 + reports.len() as i64);
        assert!(
            reports
                .windows(2)
                .all(|pair| pair[0].copied_pages < pair[1].copied_pages
                    && pair[0].total_pages == pair[1].total_pages),
            "concurrent commits must not restart backup progress"
        );
        assert_eq!(reports.last().unwrap().copied_pages, result.pages);
        assert_eq!(reports.last().unwrap().total_pages, result.pages);
        assert_eq!(result.quick_check, "ok");
        let bytes = fs::read(&result.path).unwrap();
        assert_eq!(result.bytes, bytes.len() as u64);
        assert_eq!(
            result.sha256,
            Sha256::digest(bytes)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        );
        assert!(!destination.join("source.db.partial").exists());
    });
}
#[test]
fn read_only_copy_preserves_source_bytes_mtime_and_legacy_schema() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("legacy.db");
    // No current schema, compatibility header, or Engine: even an old schema stamp is opaque.
    let c = Connection::open(&source).unwrap();
    c.execute_batch("CREATE TABLE schema_version(v INT); INSERT INTO schema_version VALUES(50); CREATE TABLE old_record(id TEXT, value TEXT); INSERT INTO old_record VALUES('旧版','😀');").unwrap();
    drop(c);
    let before = fs::read(&source).unwrap();
    let modified = fs::metadata(&source).unwrap().modified().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&source, fs::Permissions::from_mode(0o400)).unwrap();
    }
    let result = backup_file(
        &source,
        &temp.path().join("new/nested"),
        &CopyOptions::default(),
        &|_| {},
        &CancelFlag::default(),
    )
    .unwrap();
    assert_eq!(fs::read(&source).unwrap(), before);
    assert_eq!(fs::metadata(&source).unwrap().modified().unwrap(), modified);
    let target = Connection::open(&result.path).unwrap();
    assert_eq!(
        target
            .query_row("SELECT v FROM schema_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        50
    );
    assert_eq!(
        target
            .query_row("SELECT value FROM old_record", [], |r| r
                .get::<_, String>(0))
            .unwrap(),
        "😀"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&result.path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        for path in [temp.path().join("new"), temp.path().join("new/nested")] {
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
    }
}
#[test]
fn cancellation_during_copy_deletes_partial_and_never_publishes() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source.db");
    drop(fixture(&source, false));
    let dest = temp.path().join("dest");
    let cancel = CancelFlag::default();
    let error = backup_file(
        &source,
        &dest,
        &CopyOptions {
            pages_per_step: 1,
            deadline: None,
        },
        &|progress| {
            assert!(dest.join("source.db.partial").exists());
            assert!(!dest.join("source.db").exists());
            assert!(progress.copied_pages < progress.total_pages);
            cancel.cancel();
        },
        &cancel,
    )
    .unwrap_err();
    assert!(matches!(error, BackupError::Cancelled));
    empty(&dest);
    // A cleaned attempt does not consume the name; retry succeeds without manual cleanup.
    backup_file(
        &source,
        &dest,
        &CopyOptions::default(),
        &|_| {},
        &CancelFlag::default(),
    )
    .unwrap();
}
#[test]
fn pre_cancel_and_zero_deadline_have_no_published_output() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source.db");
    drop(fixture(&source, false));
    let dest = temp.path().join("dest");
    fs::create_dir(&dest).unwrap();
    let cancel = CancelFlag::default();
    cancel.cancel();
    assert!(matches!(
        backup_file(
            &source,
            &dest,
            &CopyOptions::default(),
            &|_| panic!("cancelled before copying"),
            &cancel
        ),
        Err(BackupError::Cancelled)
    ));
    empty(&dest);
    assert!(matches!(
        backup_file(
            &source,
            &dest,
            &CopyOptions {
                deadline: Some(Duration::ZERO),
                ..CopyOptions::default()
            },
            &|_| panic!("expired before copying"),
            &CancelFlag::default()
        ),
        Err(BackupError::DeadlineExceeded)
    ));
    empty(&dest);
}
#[test]
fn existing_partial_final_and_racing_final_are_never_overwritten() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source.db");
    drop(fixture(&source, false));
    let dest = temp.path().join("dest");
    fs::create_dir(&dest).unwrap();
    let partial = dest.join("source.db.partial");
    let final_path = dest.join("source.db");
    fs::write(&partial, b"existing partial").unwrap();
    io_exists(
        backup_file(
            &source,
            &dest,
            &CopyOptions::default(),
            &|_| {},
            &CancelFlag::default(),
        )
        .unwrap_err(),
    );
    assert_eq!(fs::read(&partial).unwrap(), b"existing partial");
    fs::remove_file(&partial).unwrap();
    fs::write(&final_path, b"existing final").unwrap();
    io_exists(
        backup_file(
            &source,
            &dest,
            &CopyOptions::default(),
            &|_| {},
            &CancelFlag::default(),
        )
        .unwrap_err(),
    );
    assert_eq!(fs::read(&final_path).unwrap(), b"existing final");
    fs::remove_file(&final_path).unwrap();
    let raced = Cell::new(false);
    io_exists(
        backup_file(
            &source,
            &dest,
            &CopyOptions::default(),
            &|_| {
                if !raced.replace(true) {
                    fs::write(&final_path, b"racing final").unwrap();
                }
            },
            &CancelFlag::default(),
        )
        .unwrap_err(),
    );
    assert_eq!(fs::read(&final_path).unwrap(), b"racing final");
    assert!(!partial.exists());
}
#[test]
fn corrupted_btree_fails_quick_check_and_is_not_published() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("broken.db");
    let c = fixture(&source, false);
    c.execute_batch("CREATE INDEX records_body ON records(body); PRAGMA writable_schema=ON; UPDATE sqlite_schema SET rootpage=(SELECT rootpage FROM sqlite_schema WHERE name='records') WHERE name='records_body'; PRAGMA writable_schema=OFF;").unwrap();
    drop(c);
    let dest = temp.path().join("dest");
    let error = backup_file(
        &source,
        &dest,
        &CopyOptions::default(),
        &|_| {},
        &CancelFlag::default(),
    )
    .unwrap_err();
    assert!(
        matches!(error, BackupError::QuickCheckFailed(_)),
        "copied physical pages must pass quick_check before publication: {error:?}"
    );
    empty(&dest);
}
#[test]
fn invalid_step_size_missing_source_and_callback_unwind_leave_no_copy() {
    let temp = tempfile::tempdir().unwrap();
    let dest = temp.path().join("dest");
    for pages_per_step in [0, u32::MAX] {
        assert!(matches!(
            backup_file(
                &temp.path().join("missing"),
                &dest,
                &CopyOptions {
                    pages_per_step,
                    deadline: None
                },
                &|_| {},
                &CancelFlag::default()
            ),
            Err(BackupError::InvalidOptions)
        ));
    }
    assert!(
        backup_file(
            &temp.path().join("missing"),
            &dest,
            &CopyOptions::default(),
            &|_| {},
            &CancelFlag::default()
        )
        .is_err()
    );
    assert!(!dest.exists());
    let source = temp.path().join("source.db");
    drop(fixture(&source, false));
    assert!(
        std::panic::catch_unwind(|| backup_file(
            &source,
            &dest,
            &CopyOptions::default(),
            &|_| panic!("callback failure"),
            &CancelFlag::default()
        ))
        .is_err()
    );
    empty(&dest);
}
