use rusqlite::Connection;
use serde_json::Value;
use std::{fs, path::PathBuf};
use steve_store_sqlite::{compat::StorageState, open};
use steve_types::Code;

// The engine, native installer, Python gate and packaged gate use the same inputs.
fn fixture(name: &str) -> (tempfile::TempDir, PathBuf) {
    let matrix: Value = serde_json::from_str(include_str!(
        "../../steve-install/tests/storage_matrix.json"
    ))
    .unwrap();
    let case = matrix["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["name"] == name)
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("steve.sqlite3");
    let conn = Connection::open(&path).unwrap();
    let base = format!("{}_sql", case["base"].as_str().unwrap());
    conn.execute_batch(&format!(
        "{}{}",
        matrix[&base].as_str().unwrap_or(""),
        case["sql"].as_str().unwrap()
    ))
    .unwrap();
    drop(conn);
    (dir, path)
}

fn inspect_unchanged(path: &std::path::Path) -> Result<StorageState, steve_types::Error> {
    let before = fs::read(path).unwrap();
    let conn = open::open_readonly(path).unwrap();
    assert!(conn.is_readonly("main").unwrap());
    let state = open::inspect(&conn);
    drop(conn);
    assert_eq!(fs::read(path).unwrap(), before);
    state
}

fn rejects(name: &str) {
    let (_dir, path) = fixture(name);
    assert_eq!(
        inspect_unchanged(&path).unwrap_err().code,
        Code::Incompatible
    );
}

#[test]
fn legacy_duplicate_reader_is_incompatible() {
    rejects("legacy-duplicate-reader");
}
#[test]
fn current_unknown_meta_domain_is_incompatible() {
    rejects("current-unknown-meta-domain");
}
#[test]
fn legacy_negative_reader_is_incompatible() {
    rejects("legacy-negative-reader");
}
#[test]
fn legacy_reader_view_is_incompatible() {
    rejects("legacy-reader-view");
}
#[test]
fn current_duplicate_meta_is_incompatible() {
    rejects("current-duplicate-meta");
}
#[test]
fn unknown_sqlite_like_prefix_is_incompatible() {
    rejects("unknown-sqlite-like-prefix");
}

#[test]
fn valid_headers_keep_their_classification() {
    for name in [
        "empty",
        "legacy-113",
        "legacy-114",
        "legacy-113-reader114",
        "current-old-shell-ignored",
    ] {
        let (_dir, path) = fixture(name);
        match (name, inspect_unchanged(&path).unwrap()) {
            ("empty", StorageState::Empty)
            | (
                "legacy-113" | "legacy-113-reader114",
                StorageState::Legacy {
                    schema_version: 113,
                },
            )
            | (
                "legacy-114",
                StorageState::Legacy {
                    schema_version: 114,
                },
            )
            | ("current-old-shell-ignored", StorageState::Current { .. }) => {}
            other => panic!("unexpected classification: {other:?}"),
        }
    }
}

#[test]
fn similar_user_prefixes_are_never_empty() {
    for name in ["sqlitex", "sqlitex_notes", "SQLiteX", "sqliteZ"] {
        let (_dir, path) = fixture("empty");
        Connection::open(&path)
            .unwrap()
            .execute_batch(&format!("CREATE TABLE {name}(value INTEGER)"))
            .unwrap();
        assert_eq!(
            inspect_unchanged(&path).unwrap_err().code,
            Code::Incompatible
        );
    }
}

#[test]
fn all_meta_fields_must_be_text() {
    for mutation in [
        "UPDATE meta SET domain=NULL WHERE key='layout'",
        "UPDATE meta SET key=NULL WHERE key='layout'",
        "UPDATE meta SET value=NULL WHERE key='layout'",
        "UPDATE meta SET value=2 WHERE key='layout'",
    ] {
        let (_dir, path) = fixture("current-old-shell-ignored");
        Connection::open(&path)
            .unwrap()
            .execute_batch(&format!(
                "ALTER TABLE meta RENAME TO old_meta; CREATE TABLE meta(domain,key,value);
             INSERT INTO meta SELECT * FROM old_meta; DROP TABLE old_meta; {mutation}"
            ))
            .unwrap();
        assert_eq!(
            inspect_unchanged(&path).unwrap_err().code,
            Code::Incompatible
        );
    }
}

#[test]
fn duplicate_domain_rows_cannot_be_hidden_by_collection() {
    let (_dir, path) = fixture("current-old-shell-ignored");
    Connection::open(&path)
        .unwrap()
        .execute_batch(
            "ALTER TABLE domain_schema RENAME TO old_domains;
         CREATE TABLE domain_schema(domain TEXT,version INTEGER);
         INSERT INTO domain_schema SELECT * FROM old_domains;
         INSERT INTO domain_schema VALUES('core',1); DROP TABLE old_domains;",
        )
        .unwrap();
    assert_eq!(
        inspect_unchanged(&path).unwrap_err().code,
        Code::Incompatible
    );
}
