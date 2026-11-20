use rusqlite::{
    Connection,
    hooks::{AuthAction, AuthContext, Authorization},
};
use sha2::{Digest, Sha256};
use steve_store_sqlite::{
    convert::{self, Outcome},
    digest::Source,
    layout::Layout,
    legacy, migrate, schema,
};
use steve_types::Code;

#[path = "common/legacy_input.rs"]
mod legacy_input;
fn old(conn: &Connection, version: Option<i64>) {
    if let Some(version) = version {
        legacy_input::at(conn, version);
    } else {
        legacy_input::at(conn, 114);
    }
}

fn data(conn: &Connection) {
    conn.execute_batch(&format!(
        "INSERT INTO workspaces(id,name,created) VALUES('w','fixture',0);
        INSERT INTO users(id,workspace_id,subject,name,role,created) VALUES
        ('owner','w','owner','owner','owner',0),
        ('second','w','second','second','owner',1),
        ('member','w','member','member','member',2);
        INSERT INTO workspace_members VALUES('w','member','owner',0);
        INSERT INTO spaces(id,workspace_id,system_id,kind,external_id,name,created,updated)
        VALUES('space','w','fixture','group','fixture','fixture',0,0);
        INSERT INTO {messages}(id,workspace_id,space_id,direction,text,created)
        VALUES('message','w','space','in','fixture 中😀',0);
        INSERT INTO items(id,workspace_id,no,title,status,stage,created,updated)
        VALUES('item','w',1,'fixture 中😀','open','new',0,0);",
        messages = Source::ChanMessage.shape().table
    ))
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
        Source::ChanMessage.shape().table,
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
    assert!(has(conn, "schema_compat"));
    assert!(!has(conn, "schema_version"));
    assert_eq!(
        conn.query_row("SELECT min_reader FROM schema_compat", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        steve_store_sqlite::compat::LEGACY_READER_FENCE
    );
    assert!(matches!(
        steve_store_sqlite::open::inspect(conn).unwrap(),
        steve_store_sqlite::compat::StorageState::Current { .. }
    ));
    for t in schema::CONTRACT_TABLES {
        assert!(has(conn, t));
    }
}

#[test]
fn converts_113_and_114_preserving_data_and_all_owners() {
    for version in [Some(113), None] {
        let mut conn = Connection::open_in_memory().unwrap();
        old(&conn, version);
        data(&conn);
        let mut before = counts(&conn);
        before[1] -= 2;
        before[2] -= 2;
        let from = legacy::version(&conn).unwrap();
        assert_eq!(
            convert::convert(&mut conn).unwrap(),
            Outcome::Converted {
                from_legacy_version: from,
                deleted_tokens: 2,
                deleted_clients: 2,
                deleted_sessions: 2,
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
            assert_eq!(
                conn.query_row(
                    &format!("SELECT count(*) FROM {table} WHERE id IN ('prior','member')"),
                    [],
                    |r| r.get::<_, u32>(0)
                )
                .unwrap(),
                0
            );
        }
        for (table, column) in [
            (Source::ChanMessage.shape().table, "text"),
            ("items", "title"),
        ] {
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
fn retries_after_readonly_legacy_inspection_without_a_log() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("steve.sqlite3");
    let conn = Connection::open(&path).unwrap();
    old(&conn, Some(113));
    data(&conn);
    let mut before = counts(&conn);
    before[1] -= 2;
    before[2] -= 2;
    assert_eq!(legacy::version(&conn).unwrap(), 113);
    let head = legacy::version(&conn).unwrap();
    assert!(head == 113);
    assert!(!has(&conn, "meta"));
    // Read-only inspection creates no restart state; baseline validation follows 114 replay.
    assert_eq!(
        steve_store_sqlite::open::inspect(&conn).unwrap(),
        steve_store_sqlite::compat::StorageState::Legacy {
            schema_version: 113
        }
    );
    drop(conn);
    let mut conn = Connection::open(&path).unwrap();
    assert_eq!(
        convert::convert(&mut conn).unwrap(),
        Outcome::Converted {
            from_legacy_version: head,
            deleted_tokens: 2,
            deleted_clients: 2,
            deleted_sessions: 2,
        }
    );
    assert_current(&conn);
    assert_eq!(counts(&conn), before);
}

#[test]
fn schema_drift_is_diagnostic_and_rejection_does_not_change_file_bytes() {
    for version in [Some(113), None] {
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
fn empty_and_older_prefixes_are_rejected_without_writes() {
    // Keep the former 112 prefix as an explicit refusal, not an accepted input.
    for version in [None, Some(1), Some(108), Some(112)] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("steve.sqlite3");
        let c = Connection::open(&path).unwrap();
        if let Some(v) = version {
            old(&c, Some(v));
        }
        drop(c);
        let before = std::fs::read(&path).unwrap();
        let mut c = Connection::open(&path).unwrap();
        let e = convert::convert(&mut c).unwrap_err();
        assert_eq!(e.code, Code::Incompatible);
        if version.is_some() {
            assert!(e.safe_detail.contains("先升级到上一个发布版"));
        }
        drop(c);
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
}

#[test]
fn current_below_catalog_reader_floor_is_rejected_without_writes() {
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
        convert::convert(&mut conn).unwrap_err().code,
        Code::Incompatible
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
    for version in [Some(113), None] {
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
                deleted_sessions: 2,
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
            deleted_tokens: 2,
            deleted_clients: 2,
            deleted_sessions: 2,
            ..
        }
    ));
}

#[test]
fn quick_check_failure_rolls_back_pending_migrations_before_commit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("steve.sqlite3");
    let conn = Connection::open(&path).unwrap();
    old(&conn, Some(113));
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
    assert_eq!(legacy::version(&conn).unwrap(), 113);
    assert!(!has(&conn, "meta"));
    assert!(conn.is_autocommit());
    assert!(
        conn.query_row("PRAGMA foreign_keys", [], |r| r.get::<_, bool>(0))
            .unwrap()
    );
    drop(conn);
    assert_eq!(Sha256::digest(std::fs::read(&path).unwrap()), before);
}

#[test]
fn independent_owner_client_survives_with_deleted_token_reference_cleared() {
    let mut c = Connection::open_in_memory().unwrap();
    old(&c, Some(113));
    data(&c);
    c.execute("UPDATE clients SET token_id='member' WHERE id='owner'", [])
        .unwrap();
    let mut references = Vec::new();
    let tables = c
        .prepare("SELECT name FROM sqlite_schema WHERE type='table'")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    for table in tables {
        let mut stmt = c
            .prepare(&format!(
                "PRAGMA foreign_key_list(\"{}\")",
                table.replace('"', "\"\"")
            ))
            .unwrap();
        for fk in stmt
            .query_map([], |r| Ok((r.get::<_, String>(2)?, r.get::<_, String>(3)?)))
            .unwrap()
        {
            let (parent, column) = fk.unwrap();
            if ["access_tokens", "clients", "auth_sessions"].contains(&parent.as_str()) {
                references.push((table.clone(), column, parent));
            }
        }
    }
    assert_eq!(
        references,
        [("clients".into(), "token_id".into(), "access_tokens".into())]
    );
    assert!(matches!(
        convert::convert(&mut c).unwrap(),
        Outcome::Converted {
            deleted_tokens: 2,
            deleted_clients: 2,
            deleted_sessions: 2,
            ..
        }
    ));
    assert_eq!(
        c.query_row("SELECT token_id FROM clients WHERE id='owner'", [], |r| {
            r.get::<_, Option<String>>(0)
        })
        .unwrap(),
        None
    );
    assert_eq!(
        c.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r
            .get::<_, u32>(
            0
        ))
        .unwrap(),
        0
    );
    assert_eq!(
        c.query_row("SELECT count(*) FROM users", [], |r| r.get::<_, u32>(0))
            .unwrap(),
        3
    );
}

#[test]
fn final_commit_failure_preserves_legacy_input_credentials_and_references_for_retry() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("steve.sqlite3");
    let c = Connection::open(&path).unwrap();
    old(&c, Some(113));
    data(&c);
    c.execute("UPDATE clients SET token_id='member' WHERE id='owner'", [])
        .unwrap();
    let before_credentials = credentials(&c);
    drop(c);
    let before = std::fs::read(&path).unwrap();
    let mut c = Connection::open(&path).unwrap();
    c.commit_hook(Some(|| true)).unwrap();
    assert!(convert::convert(&mut c).is_err());
    assert_eq!(legacy::version(&c).unwrap(), 113);
    assert_eq!(credentials(&c), before_credentials);
    assert_eq!(
        c.query_row("SELECT token_id FROM clients WHERE id='owner'", [], |r| {
            r.get::<_, String>(0)
        })
        .unwrap(),
        "member"
    );
    assert!(!has(&c, "meta"));
    assert!(c.is_autocommit());
    drop(c);
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let mut c = Connection::open(&path).unwrap();
    assert!(matches!(
        convert::convert(&mut c).unwrap(),
        Outcome::Converted {
            deleted_tokens: 2,
            deleted_clients: 2,
            deleted_sessions: 2,
            ..
        }
    ));
}

#[test]
fn fresh_and_converted_metadata_have_the_same_current_shape() {
    let mut fresh = Connection::open_in_memory().unwrap();
    migrate::initialize(&mut fresh, &Layout::single_file()).unwrap();
    let mut converted = Connection::open_in_memory().unwrap();
    old(&converted, Some(113));
    data(&converted);
    convert::convert(&mut converted).unwrap();
    for table in [
        "schema_version",
        "schema_compat",
        "meta",
        "domain_schema",
        "uow_applied",
        "cursor",
    ] {
        let objects = |c: &Connection| {
            c.prepare(
                "SELECT type,name,sql FROM sqlite_schema WHERE tbl_name=?1 ORDER BY type,name",
            )
            .unwrap()
            .query_map([table], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                ))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
        };
        assert_eq!(objects(&fresh), objects(&converted), "{table}");
    }
    let meta = |c: &Connection| {
        c.prepare("SELECT domain,key,value FROM meta WHERE key NOT IN ('file_id','store_generation') ORDER BY domain,key").unwrap()
        .query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?)))
        .unwrap().collect::<rusqlite::Result<Vec<_>>>().unwrap()
    };
    assert_eq!(meta(&fresh), meta(&converted));
    for c in [&fresh, &converted] {
        assert_current(c);
    }
}
