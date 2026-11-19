//! Repeat the design probes against Rust's bundled SQLite, not Python's SQLite.
use rusqlite::{
    Connection, ErrorCode, OpenFlags,
    backup::{Backup, StepResult},
};
use std::{path::Path, time::Duration};

fn database(path: &Path, sql: &str) -> Connection {
    let conn = Connection::open(path).unwrap();
    conn.busy_timeout(Duration::ZERO).unwrap();
    conn.execute_batch("PRAGMA journal_mode=WAL;").unwrap();
    conn.execute_batch(sql).unwrap();
    conn
}
fn uri(path: &Path) -> String {
    let encoded: String = path
        .as_os_str()
        .as_encoded_bytes()
        .iter()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"/-_.".contains(b) {
                (*b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect();
    format!("file:{encoded}?mode=ro")
}
#[test]
fn readonly_attach_resolves_unqualified_tables_but_rejects_writes() {
    let temp = tempfile::tempdir().unwrap();
    let main = database(
        &temp.path().join("main.db"),
        "CREATE TABLE items(id INTEGER PRIMARY KEY);",
    );
    let stream_path = temp.path().join("stream.db");
    let stream = database(
        &stream_path,
        "CREATE TABLE events(id INTEGER PRIMARY KEY); INSERT INTO events VALUES(1);",
    );
    main.execute("ATTACH ?1 AS stream", [uri(&stream_path)])
        .unwrap();
    assert_eq!(
        main.query_row("SELECT count(*) FROM events", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1,
        "unqualified names resolve to attached files when absent from main"
    );
    let error = main
        .execute("INSERT INTO events VALUES(2)", [])
        .unwrap_err();
    assert_eq!(
        error.sqlite_error_code(),
        Some(ErrorCode::ReadOnly),
        "a read-only attachment is a real write boundary"
    );
    assert_eq!(
        stream
            .query_row("SELECT count(*) FROM events", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
}
#[test]
fn immediate_locks_readwrite_attachments_but_not_readonly_attachments() {
    let temp = tempfile::tempdir().unwrap();
    let main_path = temp.path().join("main.db");
    let attached = temp.path().join("stream.db");
    let main = database(&main_path, "CREATE TABLE items(id INTEGER PRIMARY KEY);");
    let competitor = database(&attached, "CREATE TABLE events(id INTEGER PRIMARY KEY);");
    main.execute("ATTACH ?1 AS stream", [attached.to_str().unwrap()])
        .unwrap();
    main.execute_batch("BEGIN IMMEDIATE").unwrap();
    let error = competitor.execute_batch("BEGIN IMMEDIATE").unwrap_err();
    assert_eq!(
        error.sqlite_error_code(),
        Some(ErrorCode::DatabaseBusy),
        "BEGIN IMMEDIATE reserves the attached writable file too"
    );
    main.execute_batch("ROLLBACK; DETACH stream;").unwrap();
    main.execute("ATTACH ?1 AS stream", [uri(&attached)])
        .unwrap();
    main.execute_batch("BEGIN IMMEDIATE").unwrap();
    competitor
        .execute_batch("BEGIN IMMEDIATE; INSERT INTO events VALUES(1); COMMIT;")
        .expect("read-only attachment leaves the other file writable");
    main.execute_batch("ROLLBACK").unwrap();
}

#[test]
fn persistent_cross_file_triggers_fail_while_connection_local_revision_triggers_work() {
    let temp = tempfile::tempdir().unwrap();
    let main = database(
        &temp.path().join("main.db"),
        "CREATE TABLE items(id INTEGER PRIMARY KEY, v INT, rev INT DEFAULT 0); INSERT INTO items(id,v) VALUES(1,1);",
    );
    let attached = temp.path().join("stream.db");
    let _stream = database(&attached, "CREATE TABLE events(id INTEGER);");
    main.execute("ATTACH ?1 AS stream", [uri(&attached)])
        .unwrap();
    assert!(main.execute_batch("CREATE TRIGGER explicit_cross AFTER UPDATE ON items BEGIN INSERT INTO stream.events VALUES(NEW.id); END;").is_err(), "qualified attached writes are forbidden in persistent triggers");
    main.execute_batch("CREATE TRIGGER implicit_cross AFTER UPDATE ON items BEGIN INSERT INTO events VALUES(NEW.id); END;").unwrap();
    assert!(
        main.execute("UPDATE items SET v=2", []).is_err(),
        "unqualified trigger targets resolve inside main at preparation/firing, not inside attachments"
    );
    main.execute_batch("DROP TRIGGER implicit_cross; CREATE TEMP TABLE txrev(v INT); INSERT INTO txrev VALUES(42); CREATE TEMP TRIGGER items_rev AFTER UPDATE ON main.items WHEN NEW.rev IS OLD.rev BEGIN UPDATE items SET rev=(SELECT v FROM temp.txrev) WHERE rowid=NEW.rowid; END; UPDATE items SET v=2;").unwrap();
    assert_eq!(
        main.query_row("SELECT v,rev FROM items", [], |r| Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?
        )))
        .unwrap(),
        (2, 42),
        "a TEMP trigger may use connection-local revision state"
    );
}
#[test]
fn backup_on_an_open_read_transaction_uses_the_already_pinned_snapshot() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("source.db");
    let writer = database(
        &path,
        "CREATE TABLE records(id INTEGER PRIMARY KEY); INSERT INTO records VALUES(1);",
    );
    let reader = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    reader
        .execute_batch("BEGIN; SELECT 1 FROM sqlite_schema LIMIT 1;")
        .unwrap();
    writer.execute("INSERT INTO records VALUES(2)", []).unwrap();
    let mut destination = Connection::open_in_memory().unwrap();
    assert_eq!(
        Backup::new(&reader, &mut destination)
            .unwrap()
            .step(-1)
            .unwrap(),
        StepResult::Done
    );
    assert_eq!(
        destination
            .query_row("SELECT count(*) FROM records", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1,
        "backup uses the open read snapshot, not its own later snapshot"
    );
    assert_eq!(
        writer
            .query_row("SELECT count(*) FROM records", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        2
    );
    reader.execute_batch("ROLLBACK").unwrap();
}
#[test]
fn stepwise_backup_never_restarts_when_other_connections_commit_between_steps() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("source.db");
    let writer = database(
        &path,
        "CREATE TABLE records(id INTEGER PRIMARY KEY, body BLOB); WITH RECURSIVE n(v) AS (VALUES(1) UNION ALL SELECT v+1 FROM n WHERE v<2000) INSERT INTO records(body) SELECT zeroblob(4000) FROM n;",
    );
    let reader = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    reader
        .execute_batch("BEGIN; SELECT 1 FROM sqlite_schema LIMIT 1;")
        .unwrap();
    let mut destination = Connection::open_in_memory().unwrap();
    let mut steps = 0;
    let mut copied = 0;
    let mut total = 0;
    {
        let backup = Backup::new(&reader, &mut destination).unwrap();
        loop {
            let result = backup.step(50).unwrap();
            assert!(matches!(result, StepResult::More | StepResult::Done));
            let progress = backup.progress();
            let current = progress.pagecount - progress.remaining;
            assert!(
                current > copied,
                "a pinned snapshot makes forward progress despite intervening commits"
            );
            if steps > 0 {
                assert_eq!(
                    progress.pagecount, total,
                    "snapshot page count does not track live growth"
                );
            }
            copied = current;
            total = progress.pagecount;
            steps += 1;
            writer
                .execute("INSERT INTO records(body) VALUES(zeroblob(4000))", [])
                .unwrap();
            if result == StepResult::Done {
                break;
            }
            assert!(
                steps < 100,
                "backup restarted instead of finishing the snapshot"
            );
        }
    }
    assert_eq!(
        steps,
        (total + 49) / 50,
        "exactly the original snapshot pages were visited once"
    );
    assert!(steps > 1);
    assert_eq!(copied, total);
    assert_eq!(
        destination
            .query_row("SELECT count(*) FROM records", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        2000
    );
    assert_eq!(
        writer
            .query_row("SELECT count(*) FROM records", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        2000 + i64::from(steps)
    );
    reader.execute_batch("ROLLBACK").unwrap();
}
