use rusqlite::Connection;
use steve_storage_api::Domain;
use steve_store_sqlite::{
    layout::{self, CORE_FILE, Layout, META_DOMAIN},
    open, pragmas,
};
use steve_types::Code;

fn two_files(dir: &std::path::Path) -> Layout {
    let engine = open::open_runtime(dir).unwrap();
    let mut layout = engine.layout().clone();
    drop(engine);
    // Exercise URI encoding, rather than accidentally treating ?/#/% as URI controls.
    let file = "stream ?#%中文.sqlite3";
    layout.domains.insert(Domain::Stream, file.into());
    layout
        .files
        .insert(file.into(), uuid::Uuid::new_v4().to_string());
    let peer = Connection::open(dir.join(file)).unwrap();
    peer.execute_batch(steve_store_sqlite::schema::CONTRACT)
        .unwrap();
    peer.execute_batch(
        "CREATE TABLE engine_peer_only(id INTEGER); INSERT INTO engine_peer_only VALUES(7);",
    )
    .unwrap();
    layout.write_file_meta(&peer, file).unwrap();
    let core = Connection::open(dir.join(CORE_FILE)).unwrap();
    layout.write_file_meta(&core, CORE_FILE).unwrap();
    layout
}

#[test]
fn readonly_attachments_resolve_unqualified_names_in_writer_and_all_pool_connections() {
    let dir = tempfile::tempdir().unwrap();
    let layout = two_files(dir.path());
    layout.verify_files(dir.path()).unwrap();
    let engine = open::open_admin(dir.path()).unwrap();
    let mut connections = Vec::new();
    for _ in 0..engine.readers().max_size() {
        let c = engine.read_conn().unwrap();
        assert_eq!(
            c.query_row("SELECT id FROM engine_peer_only", [], |r| r
                .get::<_, u32>(0))
                .unwrap(),
            7
        );
        assert!(c.is_readonly("main").unwrap());
        assert!(c.is_readonly("db_0").unwrap());
        assert!(
            c.execute("INSERT INTO engine_peer_only VALUES(8)", [])
                .is_err()
        );
        connections.push(c);
    }
    let writer = Connection::open_with_flags(
        engine.path(),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .unwrap();
    pragmas::attach_readonly(&writer, dir.path(), &layout, CORE_FILE).unwrap();
    assert_eq!(
        writer
            .query_row("SELECT id FROM engine_peer_only", [], |r| r
                .get::<_, u32>(0))
            .unwrap(),
        7
    );
    assert_eq!(
        writer
            .execute("INSERT INTO db_0.engine_peer_only VALUES(8)", [])
            .unwrap_err()
            .sqlite_error_code(),
        Some(rusqlite::ErrorCode::ReadOnly)
    );
    // A read-only ATTACH does not take the peer's writer reservation on BEGIN IMMEDIATE.
    writer.execute_batch("BEGIN IMMEDIATE").unwrap();
    let peer = Connection::open(dir.path().join(&layout.domains[&Domain::Stream])).unwrap();
    peer.busy_timeout(std::time::Duration::ZERO).unwrap();
    peer.execute_batch("BEGIN IMMEDIATE; ROLLBACK;").unwrap();
    writer.execute_batch("ROLLBACK").unwrap();
    drop(writer);
    drop(connections);
    drop(peer);
    let peer_engine = engine.open_peer(&layout.domains[&Domain::Stream]).unwrap();
    assert_eq!(
        engine
            .open_peer(&layout.domains[&Domain::Stream])
            .err()
            .unwrap()
            .code,
        Code::Busy
    );
    assert_eq!(
        peer_engine.path(),
        engine.data_dir().join(&layout.domains[&Domain::Stream])
    );
    // Peer readers still use core as main and can read across both files.
    assert_eq!(
        peer_engine
            .read_conn()
            .unwrap()
            .query_row("SELECT count(*) FROM users", [], |r| r.get::<_, u32>(0))
            .unwrap(),
        0
    );
    drop(engine);
    assert_eq!(
        open::open_runtime(dir.path()).err().unwrap().code,
        Code::Busy
    );
    drop(peer_engine);
    drop(open::open_admin(dir.path()).unwrap());
}

#[test]
fn missing_mismatched_and_future_peer_files_are_refused_before_core_wal_selection() {
    let dir = tempfile::tempdir().unwrap();
    let layout = two_files(dir.path());
    let core = Connection::open(dir.path().join(CORE_FILE)).unwrap();
    core.execute_batch("PRAGMA journal_mode=DELETE").unwrap();
    drop(core);
    let file = &layout.domains[&Domain::Stream];
    let peer = Connection::open(dir.path().join(file)).unwrap();
    layout::put_meta(&peer, META_DOMAIN, "min_reader", "3").unwrap();
    drop(peer);
    let original = std::fs::read(dir.path().join(CORE_FILE)).unwrap();
    assert_eq!(
        open::open_runtime(dir.path()).err().unwrap().code,
        Code::Incompatible
    );
    assert_eq!(std::fs::read(dir.path().join(CORE_FILE)).unwrap(), original);
    let peer = Connection::open(dir.path().join(file)).unwrap();
    layout::put_meta(&peer, META_DOMAIN, "min_reader", "2").unwrap();
    layout::put_meta(&peer, META_DOMAIN, "file_id", "wrong-identity").unwrap();
    drop(peer);
    assert_eq!(
        open::open_runtime(dir.path()).err().unwrap().code,
        Code::Incompatible
    );
    assert_eq!(std::fs::read(dir.path().join(CORE_FILE)).unwrap(), original);
    std::fs::rename(dir.path().join(file), dir.path().join("held-peer.sqlite3")).unwrap();
    assert!(open::open_runtime(dir.path()).is_err());
    assert!(!dir.path().join(file).exists());
    assert_eq!(std::fs::read(dir.path().join(CORE_FILE)).unwrap(), original);
}

#[test]
fn layout_rejects_path_traversal_and_conflicting_file_ids() {
    let mut layout = Layout::single_file();
    layout
        .domains
        .insert(Domain::Stream, "../outside.sqlite3".into());
    layout
        .files
        .insert("../outside.sqlite3".into(), "peer".into());
    assert_eq!(layout.validate().unwrap_err().code, Code::Incompatible);
    layout = Layout::single_file();
    layout.domains.insert(Domain::Stream, "peer.sqlite3".into());
    layout
        .files
        .insert("peer.sqlite3".into(), layout.files[CORE_FILE].clone());
    assert!(layout.validate().is_err());
}
