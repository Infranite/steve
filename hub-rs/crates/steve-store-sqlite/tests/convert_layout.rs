use rusqlite::{
    Connection,
    hooks::{AuthAction, AuthContext, Authorization},
};
use sha2::{Digest, Sha256};
use steve_store_sqlite::{
    convert::{self, Outcome},
    layout::Layout,
    legacy, migrate, schema,
};
use steve_types::Code;

fn old(conn: &Connection, version: Option<i64>) {
    if let Some(version) = version {
        conn.execute_batch("CREATE TABLE schema_version(v INTEGER NOT NULL); CREATE TABLE schema_compat(min_reader INTEGER NOT NULL);").unwrap();
        for n in 1..=version {
            let sql = std::fs::read_to_string(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join(format!("legacy/migrations/{n:03}.sql")),
            )
            .unwrap();
            conn.execute_batch(&sql).unwrap();
        }
        conn.execute("INSERT INTO schema_version VALUES(?1)", [version])
            .unwrap();
        conn.execute("INSERT INTO schema_compat VALUES(?1)", [version])
            .unwrap();
    } else {
        legacy::final_structure(conn).unwrap();
    }
}

fn data(conn: &Connection) {
    conn.execute_batch(
        "INSERT INTO workspaces(id,name,created) VALUES('w','fixture',0);
        INSERT INTO users(id,workspace_id,subject,name,role,created) VALUES
        ('owner','w','owner','owner','owner',0),
        ('second','w','second','second','owner',1),
        ('member','w','member','member','member',2);
        INSERT INTO workspace_members VALUES('w','member','owner',0);
        INSERT INTO spaces(id,workspace_id,system_id,kind,external_id,name,created,updated)
        VALUES('space','w','fixture','group','fixture','fixture',0,0);
        INSERT INTO chan_messages(id,workspace_id,space_id,direction,text,created)
        VALUES('message','w','space','in','fixture 中😀',0);
        INSERT INTO items(id,workspace_id,no,title,status,stage,created,updated)
        VALUES('item','w',1,'fixture 中😀','open','new',0,0);",
    )
    .unwrap();
    for (id, user, revoked) in [
        ("owner", "owner", None),
        ("second", "second", None),
        ("member", "member", None),
        ("prior", "member", Some(7)),
    ] {
        conn.execute("INSERT INTO access_tokens(id,user_id,name,hash,created,revoked) VALUES(?1,?2,?1,?1,0,?3)", rusqlite::params![id,user,revoked]).unwrap();
        conn.execute(
            "INSERT INTO auth_sessions(token_hash,user_id,created,expires) VALUES(?1,?2,0,?3)",
            rusqlite::params![id, user, revoked.unwrap_or(i64::MAX)],
        )
        .unwrap();
        conn.execute("INSERT INTO clients(id,user_id,token_id,secret_hash,kind,name,created,revoked) VALUES(?1,?2,?1,?1,'web',?1,0,?3)", rusqlite::params![id,user,revoked]).unwrap();
    }
    conn.pragma_update(None, "foreign_keys", true).unwrap();
}

fn counts(conn: &Connection) -> Vec<i64> {
    [
        "users",
        "access_tokens",
        "clients",
        "chan_messages",
        "items",
    ]
    .into_iter()
    .map(|t| {
        conn.query_row(&format!("SELECT count(*) FROM {t}"), [], |r| r.get(0))
            .unwrap()
    })
    .collect()
}

fn has(conn: &Connection, table: &str) -> bool {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?1)",
        [table],
        |r| r.get(0),
    )
    .unwrap()
}

fn assert_current(conn: &Connection) {
    migrate::check_versions(conn, true).unwrap();
    Layout::read(conn).unwrap();
    for t in schema::LEGACY_TABLES {
        assert!(!has(conn, t));
    }
    for t in schema::CONTRACT_TABLES {
        assert!(has(conn, t));
    }
}

#[test]
fn converts_released_prefix_and_head_preserving_data_and_all_owners() {
    for version in [Some(112), None] {
        let mut conn = Connection::open_in_memory().unwrap();
        old(&conn, version);
        data(&conn);
        let before = counts(&conn);
        let from = legacy::version(&conn).unwrap();
        assert_eq!(
            convert::convert(&mut conn).unwrap(),
            Outcome::Converted {
                from_legacy_version: from,
                revoked_tokens: 1,
                revoked_clients: 1,
                revoked_sessions: 2,
            }
        );
        assert_current(&conn);
        assert_eq!(counts(&conn), before);
        let sessions = conn
            .prepare("SELECT user_id FROM auth_sessions ORDER BY user_id")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(sessions, ["owner", "second"]);
        for table in ["access_tokens", "clients"] {
            let revoked = |id| {
                conn.query_row(
                    &format!("SELECT revoked FROM {table} WHERE id=?1"),
                    [id],
                    |r| r.get::<_, Option<i64>>(0),
                )
                .unwrap()
            };
            assert_eq!(revoked("owner"), None);
            assert_eq!(revoked("second"), None);
            assert_eq!(revoked("prior"), Some(7));
            assert!(revoked("member").unwrap() > 7);
        }
        for (table, column) in [("chan_messages", "text"), ("items", "title")] {
            let text: String = conn
                .query_row(&format!("SELECT {column} FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(text, "fixture 中😀");
        }
        let layout = Layout::read(&conn).unwrap();
        assert_eq!(
            convert::convert(&mut conn).unwrap(),
            Outcome::AlreadyCurrent
        );
        assert_eq!(Layout::read(&conn).unwrap(), layout);
        assert_eq!(counts(&conn), before);
    }
}

#[test]
fn restarts_after_the_legacy_transaction_without_a_log() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("steve.sqlite3");
    let conn = Connection::open(&path).unwrap();
    old(&conn, Some(112));
    data(&conn);
    let before = counts(&conn);
    assert_eq!(convert::migrate_legacy(&conn).unwrap(), 112);
    let head = legacy::version(&conn).unwrap();
    assert!(head > 112);
    assert!(!has(&conn, "meta"));
    // A read-only verification phase creates no additional durable restart state.
    schema::verify_baseline(&conn).unwrap();
    drop(conn);
    let mut conn = Connection::open(&path).unwrap();
    assert_eq!(
        convert::convert(&mut conn).unwrap(),
        Outcome::Converted {
            from_legacy_version: head,
            revoked_tokens: 1,
            revoked_clients: 1,
            revoked_sessions: 2,
        }
    );
    assert_current(&conn);
    assert_eq!(counts(&conn), before);
}

#[test]
fn schema_drift_is_diagnostic_and_rejection_does_not_change_file_bytes() {
    for version in [Some(112), None] {
        for (sql, object) in [
            ("ALTER TABLE users ADD COLUMN unexpected TEXT", "users"),
            ("DROP INDEX access_tokens_user", "access_tokens_user"),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("steve.sqlite3");
            let conn = Connection::open(&path).unwrap();
            old(&conn, version);
            data(&conn);
            conn.execute_batch(sql).unwrap();
            drop(conn);
            let before = Sha256::digest(std::fs::read(&path).unwrap());
            let mut conn = Connection::open(&path).unwrap();
            let error = convert::convert(&mut conn).unwrap_err();
            assert_eq!(error.code, Code::Incompatible);
            assert!(error.to_string().contains(object));
            assert!(!error.to_string().contains("Invalid parameter name"));
            assert!(!has(&conn, "meta"));
            drop(conn);
            assert_eq!(Sha256::digest(std::fs::read(&path).unwrap()), before);
        }
    }
}

#[test]
fn rejects_unknown_partial_future_and_transactional_inputs_without_stamping() {
    for sql in [
        "CREATE TABLE meta(domain TEXT,key TEXT,value TEXT); INSERT INTO meta VALUES('core','layout','2');",
        "CREATE TABLE cursor(domain TEXT);",
        "CREATE TABLE mystery(x);",
    ] {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(sql).unwrap();
        let before = schema::normalized_structure(&conn, false).unwrap();
        assert_eq!(
            convert::convert(&mut conn).unwrap_err().code,
            Code::Incompatible
        );
        assert_eq!(schema::normalized_structure(&conn, false).unwrap(), before);
    }
    for sql in [
        "UPDATE meta SET value='99' WHERE key='layout';",
        "UPDATE meta SET value='99' WHERE key='min_reader';",
        "UPDATE domain_schema SET version=99 WHERE domain='core';",
        "CREATE TABLE schema_version(v);",
    ] {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate::initialize(&mut conn, &Layout::single_file()).unwrap();
        conn.execute_batch(sql).unwrap();
        assert_eq!(
            convert::convert(&mut conn).unwrap_err().code,
            Code::Incompatible
        );
    }
    let mut conn = Connection::open_in_memory().unwrap();
    old(&conn, None);
    let head = legacy::version(&conn).unwrap();
    conn.execute("UPDATE schema_compat SET min_reader=?1", [head + 1])
        .unwrap();
    assert_eq!(
        convert::convert(&mut conn).unwrap_err().code,
        Code::Incompatible
    );
    assert!(!has(&conn, "meta"));
    conn.execute_batch("BEGIN").unwrap();
    assert_eq!(convert::convert(&mut conn).unwrap_err().code, Code::Invalid);
    conn.execute_batch("ROLLBACK").unwrap();
}

#[test]
fn a_failure_after_contract_creation_rolls_back_and_can_be_retried() {
    let mut conn = Connection::open_in_memory().unwrap();
    old(&conn, None);
    data(&conn);
    let before = counts(&conn);
    conn.authorizer(Some(|ctx: AuthContext<'_>| match ctx.action {
        AuthAction::DropTable {
            table_name: "schema_version",
        } => Authorization::Deny,
        _ => Authorization::Allow,
    }))
    .unwrap();
    assert!(convert::convert(&mut conn).is_err());
    assert!(conn.is_autocommit());
    assert!(has(&conn, "schema_version"));
    for t in schema::CONTRACT_TABLES {
        assert!(!has(&conn, t));
    }
    assert_eq!(counts(&conn), before);
    conn.authorizer(None::<fn(AuthContext<'_>) -> Authorization>)
        .unwrap();
    assert!(matches!(
        convert::convert(&mut conn).unwrap(),
        Outcome::Converted { .. }
    ));
    assert_current(&conn);
}

#[test]
fn initial_legacy_metadata_rolls_back_with_a_failed_migration() {
    let mut conn = Connection::open_in_memory().unwrap();
    conn.pragma_update(None, "foreign_keys", true).unwrap();
    conn.authorizer(Some(|ctx: AuthContext<'_>| match ctx.action {
        AuthAction::CreateTable {
            table_name: "access_tokens",
        } => Authorization::Deny,
        _ => Authorization::Allow,
    }))
    .unwrap();
    assert!(convert::convert(&mut conn).is_err());
    assert!(conn.is_autocommit());
    assert!(
        schema::normalized_structure(&conn, false)
            .unwrap()
            .is_empty()
    );
    assert!(
        conn.query_row("PRAGMA foreign_keys", [], |r| r.get::<_, bool>(0))
            .unwrap()
    );
    conn.authorizer(None::<fn(AuthContext<'_>) -> Authorization>)
        .unwrap();
    // Empty conversions now refuse to install a layout without an instance owner.
    // New installations will use initialize when the caller is wired in W2a.6a.
    assert_eq!(
        convert::convert(&mut conn).unwrap_err().code,
        Code::Incompatible
    );
    assert!(!has(&conn, "meta"));
    data(&conn);
    assert!(matches!(
        convert::convert(&mut conn).unwrap(),
        Outcome::Converted { .. }
    ));
    assert_current(&conn);
}

#[test]
fn already_current_accepts_older_domain_versions_without_writes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("steve.sqlite3");
    let mut conn = Connection::open(&path).unwrap();
    migrate::initialize(&mut conn, &Layout::single_file()).unwrap();
    conn.execute("UPDATE domain_schema SET version=0 WHERE domain='core'", [])
        .unwrap();
    assert!(migrate::check_versions(&conn, true).is_err());
    drop(conn);
    let before = Sha256::digest(std::fs::read(&path).unwrap());
    let mut conn = Connection::open(&path).unwrap();
    assert_eq!(
        convert::convert(&mut conn).unwrap(),
        Outcome::AlreadyCurrent
    );
    assert_eq!(
        conn.query_row(
            "SELECT version FROM domain_schema WHERE domain='core'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    drop(conn);
    assert_eq!(Sha256::digest(std::fs::read(&path).unwrap()), before);
}

fn credentials(conn: &Connection) -> Vec<Vec<(String, Option<i64>)>> {
    [
        ("access_tokens", "id", "revoked"),
        ("clients", "id", "revoked"),
        ("auth_sessions", "token_hash", "expires"),
    ]
    .into_iter()
    .map(|(table, id, status)| {
        conn.prepare(&format!("SELECT {id},{status} FROM {table} ORDER BY {id}"))
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    })
    .collect()
}

#[test]
fn missing_instance_owner_preserves_all_credentials_and_legacy_layout() {
    for version in [Some(112), None] {
        let mut conn = Connection::open_in_memory().unwrap();
        old(&conn, version);
        data(&conn);
        // A workspace owner does not satisfy instance ownership.
        conn.execute("UPDATE users SET role='member'", []).unwrap();
        let before = credentials(&conn);
        let error = convert::convert(&mut conn).unwrap_err();
        assert_eq!(error.code, Code::Incompatible);
        assert!(error.to_string().contains("no instance owner"));
        assert_eq!(credentials(&conn), before);
        assert!(!has(&conn, "meta"));
        assert!(has(&conn, "schema_version"));
        assert!(conn.is_autocommit());
        // Repairing ownership allows a retry without an operation log.
        conn.execute(
            "UPDATE users SET role='owner' WHERE id IN ('owner','second')",
            [],
        )
        .unwrap();
        assert!(matches!(
            convert::convert(&mut conn).unwrap(),
            Outcome::Converted {
                revoked_sessions: 2,
                ..
            }
        ));
    }
}

#[test]
fn session_invalidation_failure_rolls_back_tokens_clients_and_layout() {
    let mut conn = Connection::open_in_memory().unwrap();
    old(&conn, None);
    data(&conn);
    let before = credentials(&conn);
    conn.authorizer(Some(|ctx: AuthContext<'_>| match ctx.action {
        AuthAction::Delete {
            table_name: "auth_sessions",
        } => Authorization::Deny,
        _ => Authorization::Allow,
    }))
    .unwrap();
    assert!(convert::convert(&mut conn).is_err());
    assert_eq!(credentials(&conn), before);
    assert!(!has(&conn, "meta"));
    assert!(has(&conn, "schema_version"));
    assert!(conn.is_autocommit());
    conn.authorizer(None::<fn(AuthContext<'_>) -> Authorization>)
        .unwrap();
    assert!(matches!(
        convert::convert(&mut conn).unwrap(),
        Outcome::Converted {
            revoked_tokens: 1,
            revoked_clients: 1,
            revoked_sessions: 2,
            ..
        }
    ));
}

#[test]
fn quick_check_failure_rolls_back_pending_migrations_before_commit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("steve.sqlite3");
    let conn = Connection::open(&path).unwrap();
    old(&conn, Some(112));
    data(&conn);
    // Create an actual CHECK violation without changing schema or relying on sleeps.
    conn.execute_batch("PRAGMA ignore_check_constraints=ON; INSERT INTO attention_meta(id,epoch) VALUES(2,'fixture'); PRAGMA ignore_check_constraints=OFF;").unwrap();
    assert_ne!(
        conn.query_row("PRAGMA quick_check", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    drop(conn);
    let before = Sha256::digest(std::fs::read(&path).unwrap());
    let mut conn = Connection::open(&path).unwrap();
    let error = convert::convert(&mut conn).unwrap_err();
    assert_eq!(error.code, Code::Incompatible);
    assert_eq!(error.safe_detail, "legacy storage quick_check failed");
    assert_eq!(legacy::version(&conn).unwrap(), 112);
    assert!(!has(&conn, "meta"));
    assert!(conn.is_autocommit());
    assert!(
        conn.query_row("PRAGMA foreign_keys", [], |r| r.get::<_, bool>(0))
            .unwrap()
    );
    drop(conn);
    assert_eq!(Sha256::digest(std::fs::read(&path).unwrap()), before);
}
