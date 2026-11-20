mod common;

use common::*;
use rusqlite::{Connection, types::Value};
use std::collections::BTreeMap;

fn structure(
    c: &Connection,
    exclude_contract: bool,
) -> Vec<steve_store_sqlite::schema::SchemaObject> {
    steve_store_sqlite::schema::normalized_structure(c, exclude_contract).unwrap()
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
    let differences = steve_store_sqlite::schema::differences(a, b, exclude_contract).unwrap();
    assert!(differences.is_empty(), "{}", differences.join("\n"));
    let tables = strings(
        a,
        "SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_stat%' ORDER BY name",
    );
    for t in tables {
        if exclude_contract && LEGACY_TABLES.contains(&t.as_str()) {
            continue;
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
    let head = steve_store_sqlite::compat::LEGACY_MAX;
    let mut c = rusqlite::Connection::open_in_memory().unwrap();
    legacy_input::at(&c, head - 1);
    c.execute_batch("INSERT INTO workspaces(id,name,created) VALUES('w','fixture',0); INSERT INTO users(id,workspace_id,subject,name,role,created) VALUES('owner','w','fixture','fixture','owner',0);").unwrap();
    c.pragma_update(None, "foreign_keys", false).unwrap();
    c.execute_batch(
        "INSERT INTO diagnostic_captures(item_id,workspace_id,actor,started_at,expires_at)
        VALUES('missing-parent','w','test',0,1);",
    )
    .unwrap();
    c.pragma_update(None, "foreign_keys", true).unwrap();
    let before = structure(&c, false);
    let error = steve_store_sqlite::convert::convert(&mut c).unwrap_err();
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
