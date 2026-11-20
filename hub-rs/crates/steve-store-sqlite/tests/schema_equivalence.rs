mod common;

use common::*;
use rusqlite::{Connection, types::Value};
use std::collections::BTreeMap;

fn structure(c: &Connection, exclude_contract: bool) -> Vec<Vec<Value>> {
    // rootpage, rowid and sqlite_stat* belong to the physical/optimizer layout.
    let mut schema = rows(
        c,
        "SELECT type,name,tbl_name,sql FROM sqlite_schema WHERE name NOT LIKE 'sqlite_stat%' ORDER BY type,name",
    );
    if exclude_contract {
        schema.retain(
            |row| !matches!(&row[2], Value::Text(t) if CONTRACT_TABLES.contains(&t.as_str()) || LEGACY_TABLES.contains(&t.as_str())),
        );
    }
    for row in &mut schema {
        if let Value::Text(sql) = &mut row[3] {
            *sql = sql.split_whitespace().collect::<Vec<_>>().join(" ");
        }
    }
    schema
}

fn seed_rows(c: &Connection, table: &str) -> Vec<String> {
    if table == "attention_meta" {
        let rows = rows(c, "SELECT id,epoch FROM attention_meta");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0][0], Value::Integer(1));
        let Value::Text(epoch) = &rows[0][1] else {
            panic!("epoch is not text")
        };
        assert_eq!(epoch.len(), 32);
        assert!(
            epoch
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        );
        return vec!["one lowercase 128-bit hex epoch".into()];
    }
    let mut values: Vec<_> = rows(c, &format!("SELECT * FROM {}", ident(table)))
        .into_iter()
        .map(|r| format!("{r:?}"))
        .collect();
    values.sort();
    values
}

fn equivalent(a: &Connection, b: &Connection, exclude_contract: bool) {
    assert_eq!(
        structure(a, exclude_contract),
        structure(b, exclude_contract),
        "sqlite_schema differs"
    );
    let tables = strings(
        a,
        "SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_stat%' ORDER BY name",
    );
    for t in tables {
        if exclude_contract && LEGACY_TABLES.contains(&t.as_str()) {
            continue;
        }
        for pragma in ["table_info", "index_list", "foreign_key_list"] {
            let sql = format!("PRAGMA {pragma}({})", ident(&t));
            let normalized = |c| {
                let mut r = rows(c, &sql);
                if pragma == "index_list" {
                    // seq is SQLite's enumeration order, not index identity or semantics.
                    for row in &mut r {
                        row.remove(0);
                    }
                }
                r.sort_by_key(|r| format!("{r:?}"));
                r
            };
            assert_eq!(normalized(a), normalized(b), "{pragma}({t}) differs");
        }
        if !shadows(a).contains(&t) {
            assert_eq!(
                seed_rows(a, &t),
                seed_rows(b, &t),
                "seed rows in {t} differ"
            );
        }
    }
}

#[test]
fn three_baselines_and_contract_equal_legacy() {
    let old = legacy();
    let new = baseline();
    for t in LEGACY_TABLES {
        assert!(
            rows(
                &new,
                &format!("SELECT name FROM sqlite_schema WHERE name='{t}'")
            )
            .is_empty()
        );
    }
    equivalent(&old, &new, true);
    for c in [&old, &new] {
        assert!(rows(c, "PRAGMA foreign_key_check").is_empty());
        assert_eq!(strings(c, "PRAGMA quick_check"), vec!["ok"]);
    }
    let tables = strings(
        &new,
        "SELECT name FROM sqlite_schema WHERE type='table' ORDER BY name",
    );
    for t in CONTRACT_TABLES {
        assert!(tables.contains(&t.to_string()));
        assert!(rows(&new, &format!("SELECT * FROM {}", ident(t))).is_empty());
    }
    // AUTOINCREMENT remembers a deleted tail even when the table is entirely empty.
    for c in [&old, &new] {
        c.execute_batch("PRAGMA foreign_keys=OFF;").unwrap();
        for (table, columns, values) in [
            ("gateway_result_ready", "call_id", "'sequence-test'"),
            (
                "feed_entries",
                "id,workspace_id,audience,recipient_id,kind,source_type,source_key,source_id,published_at",
                "'entry','w','user','u','completion','notification','key','source',0",
            ),
            (
                "digest_pending",
                "workspace_id,conv,message_id,at",
                "'w','conv','message',0",
            ),
        ] {
            let key = if table == "gateway_result_ready" {
                "ticket"
            } else if table == "feed_entries" {
                "seq"
            } else {
                "position"
            };
            c.execute_batch(&format!(
                "INSERT INTO {table}({key},{columns}) VALUES(10000,{values});
                DELETE FROM {table}; INSERT INTO {table}({columns}) VALUES({values});"
            ))
            .unwrap();
            let next: i64 = c
                .query_row(&format!("SELECT {key} FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(next, 10001, "{table} reused a deleted tail");
        }
    }
}

#[test]
fn route_log_belongs_to_core_in_both_layouts() {
    assert_eq!(domains()["route_log"], "core");
    for split in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        for domain in DOMAINS {
            let file = if split { domain } else { "single" };
            let c = Connection::open(directory.path().join(format!("{file}.sqlite3"))).unwrap();
            c.execute_batch(
                &std::fs::read_to_string(root().join(domain).join("0001_baseline.sql")).unwrap(),
            )
            .unwrap();
        }
        for domain in DOMAINS {
            let file = if split { domain } else { "single" };
            let c = Connection::open(directory.path().join(format!("{file}.sqlite3"))).unwrap();
            let owned = strings(
                &c,
                "SELECT name FROM sqlite_schema WHERE tbl_name='route_log' AND sql IS NOT NULL ORDER BY name",
            );
            assert!(
                rows(
                    &c,
                    "SELECT t.name FROM sqlite_schema t JOIN pragma_foreign_key_list(t.name) f
                     WHERE t.type='table' AND f.\"table\"='route_log'",
                )
                .is_empty()
            );
            if split && domain != "core" {
                assert!(owned.is_empty(), "route_log leaked into {domain}");
                continue;
            }
            assert_eq!(owned, ["route_log", "route_log_msg", "route_log_ws"]);
            assert_eq!(
                strings(&c, "SELECT name FROM pragma_index_info('route_log_msg')"),
                ["message_id"]
            );
            assert_eq!(
                strings(&c, "SELECT name FROM pragma_index_info('route_log_ws')"),
                ["workspace_id", "created"]
            );
            assert!(rows(&c, "PRAGMA foreign_key_list(route_log)").is_empty());
            c.pragma_update(None, "foreign_keys", true).unwrap();
            c.execute("DELETE FROM route_log", []).unwrap();
            c.execute_batch(
                "INSERT INTO route_log(id,workspace_id,message_id,reply_id,model_kind,kind,ms,created)
                 VALUES('route','workspace','message','reply','test','fixed',0,1)",
            )
            .unwrap();
            assert_eq!(
                strings(
                    &c,
                    "SELECT reply_id FROM route_log WHERE message_id='message'"
                ),
                ["reply"]
            );
            let error = c
                .execute_batch(
                    "INSERT INTO route_log(id,workspace_id,message_id,model_kind,kind,ms,created)
                 VALUES('duplicate','workspace','message','test','fixed',0,2)",
                )
                .unwrap_err();
            assert_eq!(
                error.sqlite_error_code(),
                Some(rusqlite::ErrorCode::ConstraintViolation)
            );
        }
    }
}

#[test]
fn legacy_flow_matches_old_store_open() {
    // Temporary dev dependency; this is its sole use. Remove when the old store runner is retired.
    let directory =
        std::env::temp_dir().join(format!("steve-schema-equivalence-{}", std::process::id()));
    std::fs::create_dir(&directory).unwrap();
    let path = directory.join("legacy.sqlite3");
    let store = steve_store::Store::open(&path, steve_store::NoPublisher).unwrap();
    let c = Connection::open(&path).unwrap();
    equivalent(&legacy(), &c, false);
    drop(c);
    drop(store);
    std::fs::remove_dir_all(directory).unwrap();
}

fn archived_at(version: i64) -> Connection {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch("CREATE TABLE schema_version (v INTEGER NOT NULL); CREATE TABLE schema_compat (min_reader INTEGER NOT NULL);").unwrap();
    for n in 1..=version {
        let sql = std::fs::read_to_string(
            root()
                .parent()
                .unwrap()
                .join(format!("legacy/migrations/{n:03}.sql")),
        )
        .unwrap();
        c.execute_batch(&sql).unwrap();
    }
    c.execute("INSERT INTO schema_version VALUES(?)", [version])
        .unwrap();
    c.execute("INSERT INTO schema_compat VALUES(?)", [version])
        .unwrap();
    c
}

#[test]
fn legacy_migration_replays_without_a_support_window() {
    let fresh = Connection::open_in_memory().unwrap();
    steve_store_sqlite::legacy::migrate_to_head(&fresh).unwrap();
    equivalent(&legacy(), &fresh, false);
    let head: i64 = fresh
        .query_row("SELECT v FROM schema_version", [], |r| r.get(0))
        .unwrap();
    for version in [1, 27, 107, 108, 109, 110, 111, 112, 113, head] {
        let c = archived_at(version);
        c.pragma_update(None, "foreign_keys", true).unwrap();
        steve_store_sqlite::legacy::migrate_to_head(&c).unwrap();
        equivalent(&legacy(), &c, false);
        steve_store_sqlite::legacy::migrate_to_head(&c).unwrap();
        assert_eq!(
            rows(&c, "PRAGMA foreign_keys"),
            vec![vec![Value::Integer(1)]]
        );
    }
    for (version, reader) in [(-1, -1), (head + 1, head + 1), (head, head + 1)] {
        let c = legacy();
        c.execute("UPDATE schema_version SET v=?", [version])
            .unwrap();
        c.execute("UPDATE schema_compat SET min_reader=?", [reader])
            .unwrap();
        let before = structure(&c, false);
        assert!(steve_store_sqlite::legacy::migrate_to_head(&c).is_err());
        assert_eq!(before, structure(&c, false));
        assert_eq!(
            seed_rows(&c, "schema_version"),
            vec![format!("[Integer({version})]")]
        );
    }
}

#[test]
fn contract_keys_and_retention_index() {
    let c = baseline();
    c.execute_batch("INSERT INTO meta VALUES('core','schema','1'); INSERT INTO meta VALUES('comms','schema','1');
        INSERT INTO uow_applied VALUES('core','same',1,x'01',1,x'02');
        INSERT INTO uow_applied VALUES('comms','same',1,x'01',1,x'02');
        INSERT INTO cursor VALUES('stream','consumer','0',1);
        INSERT INTO domain_schema VALUES('core',1);").unwrap();
    assert!(
        c.execute_batch("INSERT INTO uow_applied VALUES('core','same',2,x'01',2,x'02')")
            .is_err()
    );
    let plan = rows(
        &c,
        "EXPLAIN QUERY PLAN DELETE FROM uow_applied WHERE issued_at < 2",
    );
    assert!(format!("{plan:?}").contains("uow_applied_issued_at"));
    let counts: BTreeMap<_, _> = CONTRACT_TABLES
        .into_iter()
        .map(|t| {
            (
                t,
                rows(&c, &format!("PRAGMA table_info({})", ident(t))).len(),
            )
        })
        .collect();
    assert_eq!(counts["uow_applied"], 6);
}

#[test]
fn failed_pending_migration_keeps_version_and_restores_foreign_keys() {
    let head: i64 = legacy()
        .query_row("SELECT v FROM schema_version", [], |r| r.get(0))
        .unwrap();
    let c = archived_at(head - 1);
    c.pragma_update(None, "foreign_keys", false).unwrap();
    c.execute_batch(
        "INSERT INTO diagnostic_captures(item_id,workspace_id,actor,started_at,expires_at)
        VALUES('missing-parent','w','test',0,1);",
    )
    .unwrap();
    c.pragma_update(None, "foreign_keys", true).unwrap();
    let before = structure(&c, false);
    let error = steve_store_sqlite::legacy::migrate_to_head(&c).unwrap_err();
    assert!(error.to_string().contains("foreign keys broken"));
    assert!(c.is_autocommit());
    assert_eq!(
        rows(&c, "PRAGMA foreign_keys"),
        vec![vec![Value::Integer(1)]]
    );
    assert_eq!(structure(&c, false), before);
    let version: i64 = c
        .query_row("SELECT v FROM schema_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, head - 1);
}
