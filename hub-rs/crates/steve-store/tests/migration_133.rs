use rusqlite::Connection;
use steve_store::{NoPublisher, Store, WS, backup_policy, backup_policy::DailySettings, db};

#[test]
fn installation_backup_policy_carries_forward_valid_settings_only() {
    let before = db::MIGRATIONS
        .iter()
        .position(|sql| sql.contains("CREATE TABLE backup_policy"))
        .expect("backup_policy migration registered") as i64;
    for (legacy, expected) in [
        (
            r#"{"enabled":false,"keep_days":30}"#,
            DailySettings {
                enabled: false,
                keep_days: 30,
            },
        ),
        ("not json", DailySettings::default()),
        (
            r#"{"enabled":false,"keep_days":500}"#,
            DailySettings::default(),
        ),
        ("null", DailySettings::default()),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fixture.sqlite3");
        Store::open(&path, NoPublisher)
            .unwrap()
            .ensure_workspace()
            .unwrap();
        // Keep the pre-backup_policy schema except for its table and version stamps.
        let c = Connection::open(&path).unwrap();
        c.execute_batch(&format!(
            "DROP TABLE backup_policy;
             UPDATE schema_version SET v={before};
             UPDATE schema_compat SET min_reader={before};"
        ))
        .unwrap();
        c.execute(
            "INSERT INTO settings(workspace_id,key,value,updated) VALUES(?,'backup.daily',?,0)",
            [WS, legacy],
        )
        .unwrap();
        drop(c);
        let store = Store::open(&path, NoPublisher).unwrap();
        assert_eq!(store.schema_compat().unwrap().schema, db::SCHEMA_VERSION);
        assert_eq!(store.read(|r| backup_policy::daily(r)).unwrap(), expected);
        assert!(
            store
                .read(|r| steve_store::WorkspaceSettings(r).get(WS, "backup.daily"))
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn backup_policy_is_atomic_with_its_revision() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("fixture.sqlite3"), NoPublisher).unwrap();
    store.ensure_workspace().unwrap();
    let daily = DailySettings {
        enabled: false,
        keep_days: 31,
    };
    assert!(
        store
            .write(|w| {
                backup_policy::set_daily(w, daily, "owner")?;
                assert_eq!(backup_policy::daily(w)?, daily);
                Err::<(), _>(steve_store::StoreError::Conflict("rollback".into()))
            })
            .is_err()
    );
    assert_eq!(
        store.read(|r| backup_policy::daily(r)).unwrap(),
        DailySettings::default()
    );
    store
        .write(|w| backup_policy::set_daily(w, daily, "owner"))
        .unwrap();
    assert_eq!(store.read(|r| backup_policy::daily(r)).unwrap(), daily);
    let db = Connection::open(store.path()).unwrap();
    let revisions: i64 = db
        .query_row(
            "SELECT count(*) FROM revisions WHERE object_type='settings' AND author='owner'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(revisions, 1);
}
