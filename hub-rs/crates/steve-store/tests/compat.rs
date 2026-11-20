//! Current data, constraints and schema survive reopen without historical replay.
use rusqlite::Connection;
#[path = "common/current.rs"]
mod current;
fn fresh() -> Connection {
    let mut c = Connection::open_in_memory().unwrap();
    current::migrate(&mut c).unwrap();
    c
}
fn one(c: &Connection, sql: &str) -> i64 {
    c.query_row(sql, [], |r| r.get(0)).unwrap()
}
#[test]
fn delivery_tables_and_existing_device_revision_survive_reopen() {
    let mut c = fresh();
    c.execute_batch(
        "INSERT INTO workspaces(id,name,created) VALUES('default','Existing workspace',1);
         INSERT INTO devices(id,name,roles,max_sessions,revision,seen,created)
             VALUES('delivery-upgrade-device','Existing device','[]',64,17,1,1);",
    )
    .unwrap();
    current::migrate(&mut c).unwrap();
    assert_eq!(
        steve_store_sqlite::compat::CurrentStorage::inspect(&c)
            .unwrap()
            .domains
            .len(),
        3
    );
    assert_eq!(
        one(
            &c,
            "SELECT revision FROM devices WHERE id='delivery-upgrade-device'"
        ),
        17
    );
    assert_eq!(
        one(
            &c,
            "SELECT max_sessions FROM devices WHERE id='delivery-upgrade-device'"
        ),
        64
    );
    let name: String = c
        .query_row("SELECT name FROM workspaces WHERE id='default'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(name, "Existing workspace");
    for table in ["deliveries", "delivery_shares"] {
        let exists: bool = c
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?)",
                [table],
                |r| r.get(0),
            )
            .unwrap();
        assert!(exists, "{table} created by the delivery migration");
    }
    // Reopening the same version must not rebuild or overwrite the existing device.
    current::migrate(&mut c).unwrap();
    assert_eq!(
        one(
            &c,
            "SELECT revision FROM devices WHERE id='delivery-upgrade-device'"
        ),
        17
    );
}

#[test]
fn connector_references_have_foreign_keys_and_indexes() {
    let mut c = fresh();
    assert_eq!(
        one(
            &c,
            "SELECT count(*) FROM sqlite_master WHERE name='connector_refs' AND type='table'"
        ),
        1
    );
    assert_eq!(
        one(
            &c,
            "SELECT count(*) FROM pragma_foreign_key_list('connector_refs')"
        ),
        3
    );
    assert_eq!(
        one(
            &c,
            "SELECT count(*) FROM sqlite_master WHERE name IN ('connector_refs_connection','connector_refs_expiry')"
        ),
        2
    );
    current::migrate(&mut c).unwrap();
    assert_eq!(
        steve_store_sqlite::compat::CurrentStorage::inspect(&c)
            .unwrap()
            .domains
            .len(),
        3
    );
    assert_eq!(one(&c, "SELECT count(*) FROM pragma_foreign_key_check"), 0);
}

#[test]
fn notification_foreign_keys_triggers_and_schema_survive_reopen() {
    let mut c = fresh();
    assert_eq!(
        one(
            &c,
            "SELECT count(*) FROM sqlite_master WHERE name IN ('notification_policies','personal_notifications') AND type='table'"
        ),
        2
    );
    assert_eq!(
        one(
            &c,
            "SELECT count(*) FROM sqlite_master WHERE name IN ('notification_delivery_deleted','notification_delivery_purged','notification_message_purged') AND type='trigger'"
        ),
        3
    );
    assert_eq!(
        one(
            &c,
            "SELECT count(*) FROM pragma_foreign_key_list('personal_notifications') WHERE \"table\"='output_deliveries' AND on_delete='SET NULL'"
        ),
        1
    );
    let schema: String = c
        .query_row(
            "SELECT group_concat(sql, ';') FROM (SELECT sql FROM sqlite_master ORDER BY name)",
            [],
            |r| r.get(0),
        )
        .unwrap();
    current::migrate(&mut c).unwrap();
    let reopened: String = c
        .query_row(
            "SELECT group_concat(sql, ';') FROM (SELECT sql FROM sqlite_master ORDER BY name)",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(schema, reopened);
    assert_eq!(
        steve_store_sqlite::compat::CurrentStorage::inspect(&c)
            .unwrap()
            .domains
            .len(),
        3
    );
    assert_eq!(one(&c, "SELECT count(*) FROM pragma_foreign_key_check"), 0);
}

#[test]
fn facade_refuses_noncanonical_names_before_creating_files() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("other.sqlite3");
    assert!(steve_store::Store::open(&path, steve_store::NoPublisher).is_err());
    assert!(!path.exists());
    assert!(!d.path().join("steve.sqlite3").exists());
}

#[test]
fn facade_refuses_legacy_without_an_instance_owner_and_preserves_bytes() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("steve.sqlite3");
    let c = Connection::open(&path).unwrap();
    c.execute_batch(include_str!(
        "../../steve-store-sqlite/legacy/113_fixture.sql"
    ))
    .unwrap();
    drop(c);
    let before = std::fs::read(&path).unwrap();
    assert!(steve_store::Store::open(&path, steve_store::NoPublisher).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before);
}
