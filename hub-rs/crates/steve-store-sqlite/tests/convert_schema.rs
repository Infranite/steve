use rusqlite::Connection;
use steve_store_sqlite::schema;
use steve_types::Code;

fn initialized(core: &str) -> Connection {
    let c = Connection::open_in_memory().unwrap();
    for sql in [core, schema::STREAM, schema::COMMS] {
        c.execute_batch(sql).unwrap();
    }
    c
}
fn changed_items(from: &str, to: &str) -> Connection {
    let reference = initialized(schema::CORE);
    let ddl: String = reference
        .query_row(
            "SELECT sql FROM sqlite_schema WHERE name='items'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(ddl.matches(from).count(), 1);
    assert!(schema::CORE.contains(&ddl));
    initialized(&schema::CORE.replacen(&ddl, &ddl.replacen(from, to, 1), 1))
}
fn rejected(c: &Connection, object: &str) {
    let e = schema::verify_baseline(c).unwrap_err();
    assert_eq!(e.code, Code::Incompatible);
    assert!(e.safe_detail.contains(object), "{}", e.safe_detail);
}

#[test]
fn extra_column_is_rejected() {
    let c = initialized(schema::CORE);
    c.execute_batch("ALTER TABLE items ADD COLUMN unwanted TEXT")
        .unwrap();
    rejected(&c, "items");
}
#[test]
fn missing_index_is_rejected() {
    let c = initialized(schema::CORE);
    c.execute_batch("DROP INDEX access_tokens_user").unwrap();
    rejected(&c, "access_tokens_user");
}
#[test]
fn extra_table_is_rejected() {
    let c = initialized(schema::CORE);
    c.execute_batch("CREATE TABLE unwanted(id INTEGER)")
        .unwrap();
    rejected(&c, "unwanted");
}
#[test]
fn changed_column_type_is_rejected() {
    rejected(
        &changed_items("title TEXT NOT NULL", "title BLOB NOT NULL"),
        "items",
    );
}
#[test]
fn changed_default_is_rejected() {
    rejected(&changed_items("DEFAULT 'task'", "DEFAULT 'chat'"), "items");
}
#[test]
fn removed_not_null_is_rejected() {
    rejected(&changed_items("title TEXT NOT NULL", "title TEXT"), "items");
}
#[test]
fn changed_unique_constraint_is_rejected() {
    rejected(
        &changed_items("UNIQUE (workspace_id, no)", "UNIQUE (workspace_id, title)"),
        "items",
    );
}
#[test]
fn arbitrary_column_order_is_accepted() {
    let c = changed_items(
        "id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL",
        "workspace_id TEXT NOT NULL, id TEXT PRIMARY KEY",
    );
    schema::verify_baseline(&c).unwrap();
    assert!(
        differences(
            "CREATE TABLE unrelated(a TEXT,b INTEGER)",
            "CREATE TABLE unrelated(b INTEGER,a TEXT)"
        )
        .is_empty()
    );
}
fn differences(a: &str, b: &str) -> Vec<String> {
    let connections = [a, b].map(|sql| {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(sql).unwrap();
        c
    });
    schema::differences(&connections[0], &connections[1], false).unwrap()
}
#[test]
fn same_named_column_with_one_different_word_is_rejected() {
    let from = "CREATE TABLE t(k TEXT COLLATE BINARY)";
    let to = "CREATE TABLE t(k TEXT COLLATE NOCASE)";
    let diff = differences(from, to);
    assert_eq!(diff, ["table t: definition differs"]); // table_info alone cannot detect it.
}
#[test]
fn column_default_literal_differences_are_preserved_by_table_info() {
    assert!(
        differences(
            "CREATE TABLE t(k TEXT DEFAULT 'two  spaces')",
            "CREATE TABLE t(k TEXT DEFAULT 'two spaces')"
        )
        .contains(&"table_info(t) differs".to_owned())
    );
}
#[test]
fn quoted_names_nested_parentheses_commas_and_comments_do_not_split_columns() {
    let a = r#""a,""b)" TEXT DEFAULT ('it''s, (text)')"#;
    let b = "[check] TEXT /* not a delimiter: , ( ) */ NOT NULL";
    let c = "`c(d` INTEGER DEFAULT (length('comma, (here)'))";
    let constraint = "CONSTRAINT valid CHECK(length([check]) > 0)";
    let table = |columns| format!("CREATE TABLE odd ({columns},{constraint})");
    assert!(
        differences(
            &table(format!("{a},{b},{c}")),
            &table(format!("{c},{a},{b}"))
        )
        .is_empty()
    );
    let line_comment = "a TEXT DEFAULT 'x' -- comma, quote' and parenthesis )\n";
    assert!(
        differences(
            &format!("CREATE TABLE t({line_comment},b INTEGER)"),
            &format!("CREATE TABLE t(b INTEGER,{line_comment})")
        )
        .is_empty()
    );
}
#[test]
fn table_constraints_options_indexes_triggers_views_and_foreign_keys_remain_strict() {
    for (a, b) in [
        (
            "CREATE TABLE t(a INTEGER,b INTEGER,CHECK(a>0),CHECK(b>0))",
            "CREATE TABLE t(a INTEGER,b INTEGER,CHECK(b>0),CHECK(a>0))",
        ),
        (
            "CREATE TABLE t(a INTEGER PRIMARY KEY)",
            "CREATE TABLE t(a INTEGER PRIMARY KEY) WITHOUT ROWID",
        ),
        (
            "CREATE TABLE t(a,b); CREATE INDEX i ON t(a,b)",
            "CREATE TABLE t(a,b); CREATE INDEX i ON t(b,a)",
        ),
        (
            "CREATE TABLE t(a); CREATE TRIGGER tr AFTER INSERT ON t BEGIN SELECT 1; END",
            "CREATE TABLE t(a); CREATE TRIGGER tr AFTER INSERT ON t BEGIN SELECT 2; END",
        ),
        (
            "CREATE TABLE t(a,b); CREATE VIEW v AS SELECT a FROM t",
            "CREATE TABLE t(a,b); CREATE VIEW v AS SELECT b FROM t",
        ),
        (
            "CREATE TABLE p(a PRIMARY KEY); CREATE TABLE t(a REFERENCES p(a) ON DELETE CASCADE)",
            "CREATE TABLE p(a PRIMARY KEY); CREATE TABLE t(a REFERENCES p(a) ON DELETE RESTRICT)",
        ),
    ] {
        assert!(!differences(a, b).is_empty(), "{a} / {b}");
    }
}

#[test]
fn declarations_constraints_header_and_suffix_share_whitespace_normalization() {
    assert!(differences(
        "CREATE TABLE t (a TEXT PRIMARY KEY NOT NULL,b INTEGER,CONSTRAINT pos CHECK (b > 0)) WITHOUT ROWID",
        "CREATE TABLE t (b INTEGER,a TEXT PRIMARY KEY  NOT\nNULL,CONSTRAINT pos CHECK (b  >\n0)) WITHOUT\nROWID"
    ).is_empty());
}

#[test]
fn multiple_inline_references_ignore_only_generated_fk_ids() {
    let parents = "CREATE TABLE p(id INTEGER PRIMARY KEY); CREATE TABLE q(id INTEGER PRIMARY KEY);";
    let a = format!(
        "{parents} CREATE TABLE child(a INTEGER REFERENCES p(id),b INTEGER REFERENCES q(id))"
    );
    let b = format!(
        "{parents} CREATE TABLE child(b INTEGER REFERENCES q(id),a INTEGER REFERENCES p(id))"
    );
    assert!(differences(&a, &b).is_empty());
    let changed = format!(
        "{parents} CREATE TABLE child(b INTEGER REFERENCES p(id),a INTEGER REFERENCES p(id))"
    );
    assert!(!differences(&a, &changed).is_empty());
    let composite_a = "CREATE TABLE p(a,b,PRIMARY KEY(a,b)); CREATE TABLE child(a,b,FOREIGN KEY(a,b) REFERENCES p(a,b))";
    let composite_b = "CREATE TABLE p(a,b,PRIMARY KEY(a,b)); CREATE TABLE child(a,b,FOREIGN KEY(b,a) REFERENCES p(a,b))";
    assert!(!differences(composite_a, composite_b).is_empty());
}
