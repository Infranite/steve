use std::{
    fs, io,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    time::Duration,
};
use steve_store_sqlite::backup::*;
fn db(root: &Path) -> PathBuf {
    let p = root.join("source.sqlite3");
    rusqlite::Connection::open(&p).unwrap().execute_batch("CREATE TABLE schema_version(v INT); INSERT INTO schema_version VALUES(50); CREATE TABLE old_record(v TEXT); INSERT INTO old_record VALUES('旧库😀');").unwrap();
    p
}
fn plan(root: &Path, reason: Reason) -> BackupPlan {
    BackupPlan::new(root.join("backups"), reason)
}
fn failed_files(_: &Path, _: &Path, _: &[&str]) -> io::Result<Option<PathBuf>> {
    Err(io::Error::other("attachment disappeared"))
}
fn file(_: &Path, out: &Path, _: &[&str]) -> io::Result<Option<PathBuf>> {
    use std::io::Write;
    let p = out.join("files.tar");
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&p)?;
    f.write_all(b"archive")?;
    f.sync_all()?;
    Ok(Some(p))
}
#[test]
fn each_purpose_publishes_verified_private_manifest_with_snapshot_schema() {
    let root = tempfile::tempdir().unwrap();
    let source = db(root.path());
    for reason in [
        Reason::Manual,
        Reason::Daily,
        Reason::PreUpgrade {
            from: "old".into(),
            to: "new".into(),
        },
        Reason::PreDestructive {
            what: "purge".into(),
        },
        Reason::Diagnostic,
    ] {
        let mut plan = plan(root.path(), reason.clone());
        plan.with_files = Some(FileSet {
            home: root.path().to_owned(),
            parts: &[],
            output: FileOutput::Archive(file),
        });
        let m = backup(&source, &plan, &|_| {}, &CancelFlag::default()).unwrap();
        assert!(m.completed);
        assert_eq!(m.reason, reason);
        assert_eq!(m.schema_version, Some(50));
        assert!(m.files.complete);
        assert_eq!(m.files.consistency, "best_effort");
        assert_eq!(m.database.quick_check.as_deref(), Some("ok"));
        let actual = steve_store_sqlite::backup::plan::artifact(&m.database_path()).unwrap();
        assert_eq!(actual.sha256, m.database.sha256);
        assert_eq!(actual.bytes, m.database.bytes);
        let saved: Manifest =
            serde_json::from_slice(&fs::read(m.directory.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(saved.database.sha256, m.database.sha256);
        assert_eq!(saved.source, source);
        for (path, mode) in [
            (m.directory.clone(), 0o700),
            (m.directory.join("manifest.json"), 0o600),
            (m.database_path(), 0o600),
            (m.directory.join("files.tar"), 0o600),
        ] {
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                mode
            );
        }
    }
    assert_eq!(list(&root.path().join("backups")).unwrap().len(), 5);
}
#[test]
fn attachment_failure_is_explicit_but_does_not_discard_the_database() {
    let root = tempfile::tempdir().unwrap();
    let source = db(root.path());
    let mut plan = plan(root.path(), Reason::Diagnostic);
    plan.with_files = Some(FileSet {
        home: root.path().to_owned(),
        parts: &[],
        output: FileOutput::Archive(failed_files),
    });
    let m = backup(&source, &plan, &|_| {}, &CancelFlag::default()).unwrap();
    assert!(m.completed && m.database_path().is_file());
    assert!(!m.files.complete);
    assert!(m.files.error.as_ref().unwrap().contains("disappeared"));
    assert!(m.files.artifacts.is_empty());
}
#[test]
fn retention_is_scoped_to_completed_owned_directories_and_the_correct_purpose() {
    let root = tempfile::tempdir().unwrap();
    let source = db(root.path());
    let daily = backup(
        &source,
        &plan(root.path(), Reason::Daily),
        &|_| {},
        &CancelFlag::default(),
    )
    .unwrap();
    let diagnostic = backup(
        &source,
        &plan(root.path(), Reason::Diagnostic),
        &|_| {},
        &CancelFlag::default(),
    )
    .unwrap();
    for n in 0..10 {
        backup(
            &source,
            &plan(
                root.path(),
                Reason::PreUpgrade {
                    from: n.to_string(),
                    to: (n + 1).to_string(),
                },
            ),
            &|_| {},
            &CancelFlag::default(),
        )
        .unwrap();
    }
    let out = root.path().join("backups");
    fs::write(out.join("daily-notes.txt"), "foreign").unwrap();
    fs::create_dir(out.join("daily-interrupted.partial")).unwrap();
    fs::create_dir(out.join("daily-foreign")).unwrap();
    let mut newest_daily = backup(
        &source,
        &plan(root.path(), Reason::Daily),
        &|_| {},
        &CancelFlag::default(),
    )
    .unwrap();
    newest_daily.created_ms = daily.created_ms + 1;
    fs::write(
        newest_daily.directory.join("manifest.json"),
        serde_json::to_vec(&newest_daily).unwrap(),
    )
    .unwrap();
    assert_eq!(
        retain_at(
            &out,
            daily.created_ms + DEFAULT_DAILY_KEEP_DAYS * 86400000,
            steve_store_sqlite::backup::DEFAULT_DAILY_KEEP_DAYS
        )
        .unwrap()
        .removed,
        2 // upgrade count pressure; daily age is exactly at the boundary
    );
    assert!(daily.database_path().exists());
    assert_eq!(
        retain_at(
            &out,
            daily.created_ms + DEFAULT_DAILY_KEEP_DAYS * 86400000 + 1,
            steve_store_sqlite::backup::DEFAULT_DAILY_KEEP_DAYS
        )
        .unwrap()
        .removed,
        1
    );
    assert!(!daily.database_path().exists());
    assert!(newest_daily.database_path().exists());
    assert!(diagnostic.database_path().exists());
    assert_eq!(usage(&out).unwrap()["pre-upgrade"].count, 8);
    assert!(
        out.join("daily-notes.txt").exists()
            && out.join("daily-interrupted.partial").exists()
            && out.join("daily-foreign").exists()
    );
    fs::write(diagnostic.directory.join("operator-note"), "keep").unwrap();
    assert_eq!(
        retain_at(
            &out,
            u64::MAX,
            steve_store_sqlite::backup::DEFAULT_DAILY_KEEP_DAYS
        )
        .unwrap()
        .removed,
        7
    );
    assert!(diagnostic.directory.exists());
}
#[test]
fn database_failure_cancel_and_deadline_do_not_publish_directories() {
    let root = tempfile::tempdir().unwrap();
    let source = db(root.path());
    let mut p = plan(root.path(), Reason::Diagnostic);
    p.deadline = Some(Duration::ZERO);
    assert!(backup(&source, &p, &|_| {}, &CancelFlag::default()).is_err());
    p.deadline = None;
    let cancel = CancelFlag::default();
    assert!(backup(&source, &p, &|_| cancel.cancel(), &cancel).is_err());
    assert_eq!(fs::read_dir(&p.dest_root).unwrap().count(), 0);
    assert!(
        backup(
            &root.path().join("missing"),
            &p,
            &|_| {},
            &CancelFlag::default()
        )
        .is_err()
    );
    assert_eq!(fs::read_dir(&p.dest_root).unwrap().count(), 0);
}

#[test]
fn backup_publication_never_runs_retention_even_after_a_clock_correction() {
    let root = tempfile::tempdir().unwrap();
    let source = db(root.path());
    let reason = Reason::PreUpgrade {
        from: "old".into(),
        to: "new".into(),
    };
    for _ in 0..10 {
        let mut m = backup(
            &source,
            &plan(root.path(), reason.clone()),
            &|_| {},
            &CancelFlag::default(),
        )
        .unwrap();
        m.created_ms += 86400000;
        fs::write(
            m.directory.join("manifest.json"),
            serde_json::to_vec(&m).unwrap(),
        )
        .unwrap();
    }
    let p = BackupPlan::new(root.path().join("backups"), reason);
    let current = backup(&source, &p, &|_| {}, &CancelFlag::default()).unwrap();
    assert!(current.database_path().exists());
    assert_eq!(list(&p.dest_root).unwrap().len(), 11);
    assert_eq!(
        retain_at(
            &p.dest_root,
            u64::MAX,
            steve_store_sqlite::backup::DEFAULT_DAILY_KEEP_DAYS
        )
        .unwrap()
        .removed,
        10
    );
    assert!(current.database_path().exists());
}

#[test]
fn unfamiliar_application_schema_metadata_does_not_prevent_a_verified_copy() {
    let root = tempfile::tempdir().unwrap();
    let db = root.path().join("legacy.sqlite3");
    let c = rusqlite::Connection::open(&db).unwrap();
    c.execute_batch("CREATE TABLE schema_version(version TEXT); INSERT INTO schema_version VALUES('ancient'); CREATE TABLE evidence(v); INSERT INTO evidence VALUES('keep');").unwrap();
    drop(c);
    let m = backup(
        &db,
        &plan(root.path(), Reason::Diagnostic),
        &|_| {},
        &CancelFlag::default(),
    )
    .unwrap();
    assert!(m.completed);
    assert_eq!(m.schema_version, None);
    assert_eq!(
        rusqlite::Connection::open(m.database_path())
            .unwrap()
            .query_row("SELECT v FROM evidence", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "keep"
    );
}

#[test]
fn manual_reason_is_the_only_permanent_retention_policy() {
    let root = tempfile::tempdir().unwrap();
    let source = db(root.path());
    let manual = plan(root.path(), Reason::Manual);
    let mut first = backup(&source, &manual, &|_| {}, &CancelFlag::default()).unwrap();
    first.created_ms = 0;
    fs::write(
        first.directory.join("manifest.json"),
        serde_json::to_vec(&first).unwrap(),
    )
    .unwrap();
    let latest = backup(
        &source,
        &plan(root.path(), Reason::Manual),
        &|_| {},
        &CancelFlag::default(),
    )
    .unwrap();
    assert!(
        list(&manual.dest_root)
            .unwrap()
            .iter()
            .find(|m| m.id == first.id)
            .unwrap()
            .reason
            .rule()
            .limits
            .is_none()
    );
    assert_eq!(
        retain_at(
            &manual.dest_root,
            u64::MAX,
            steve_store_sqlite::backup::DEFAULT_DAILY_KEEP_DAYS
        )
        .unwrap()
        .removed,
        0
    );
    assert!(first.database_path().exists() && latest.database_path().exists());
}

#[test]
fn wal_snapshot_metadata_is_immutable_and_leaves_no_sidecars_to_block_retention() {
    let root = tempfile::tempdir().unwrap();
    let special = root.path().join("space #?% 中文");
    fs::create_dir(&special).unwrap();
    let source = db(&special);
    let writer = rusqlite::Connection::open(&source).unwrap();
    writer
        .execute_batch("PRAGMA journal_mode=WAL; INSERT INTO old_record VALUES('wal');")
        .unwrap();
    let plan = plan(&special, Reason::Daily);
    let first = backup(&source, &plan, &|_| {}, &CancelFlag::default()).unwrap();
    assert_eq!(first.schema_version, Some(50));
    let mut names: Vec<_> = fs::read_dir(&first.directory)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    let mut expected = vec!["manifest.json".to_owned(), first.database.name.clone()];
    expected.sort();
    assert_eq!(names, expected);
    let actual = steve_store_sqlite::backup::plan::artifact(&first.database_path()).unwrap();
    assert_eq!(actual.sha256, first.database.sha256);
    let latest = backup(&source, &plan, &|_| {}, &CancelFlag::default()).unwrap();
    let report = retain_at(&plan.dest_root, u64::MAX, DEFAULT_DAILY_KEEP_DAYS).unwrap();
    assert_eq!(report.failures, 0);
    assert_eq!(report.removed, 1);
    assert!(!first.directory.exists() && latest.database_path().exists());
    assert_eq!(
        writer
            .query_row("SELECT count(*) FROM old_record", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        2
    );
}
