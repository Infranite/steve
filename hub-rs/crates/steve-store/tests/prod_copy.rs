//! Checks against a backup of the production database. Skipped unless `STEVE_STORE_PROD_COPY` points at one:
//!
//!   sqlite3 ~/.steve/data/steve.sqlite3 ".backup '/tmp/x/steve.sqlite3'"
//!   STEVE_STORE_PROD_COPY=/tmp/x/steve.sqlite3 cargo test -p steve-store --test prod_copy
//!
//! The test copies that file again into its own temp dir, so the given copy is never written.

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use common::*;
use steve_store::*;

fn prod_copy() -> Option<(tempfile::TempDir, PathBuf)> {
    let src = PathBuf::from(std::env::var_os("STEVE_STORE_PROD_COPY")?);
    let home = std::env::var("HOME").unwrap_or_default();
    let real = std::fs::canonicalize(&src).unwrap();
    assert!(
        !real.starts_with(Path::new(&home).join(".steve")),
        "point STEVE_STORE_PROD_COPY at a backup, never at ~/.steve"
    );
    let dir = tempfile::tempdir().unwrap();
    let dst = dir.path().join("steve.sqlite3");
    // `.backup` again rather than a file copy, so a -wal next to the source is included.
    let st = Command::new("sqlite3")
        .arg(&real)
        .arg(format!(".backup '{}'", dst.display()))
        .status()
        .unwrap();
    assert!(st.success());
    Some((dir, dst))
}

fn cli(path: &Path, sql: &str) -> String {
    let out = Command::new("sqlite3").arg(path).arg(sql).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

const TABLES: &[&str] = &[
    "workspaces",
    "users",
    "auth_sessions",
    "counters",
    "bots",
    "items",
    "item_goals",
    "timeline",
    "artifacts",
    "sessions",
    "events",
    "queued",
    "decisions",
    "messages",
    "links",
    "revisions",
    "audit",
    "workspace_members",
    "projects",
    "bot_workspaces",
    "bot_memories",
    "item_deps",
    "tags",
    "item_tags",
    "settings",
    "devices",
    "schema_version",
];

#[test]
fn opens_prod_copy_unchanged_and_reads_everything() {
    let Some((_dir, path)) = prod_copy() else {
        eprintln!("STEVE_STORE_PROD_COPY not set; skipped");
        return;
    };
    let counts_before: Vec<String> = TABLES
        .iter()
        .map(|t| cli(&path, &format!("SELECT count(*) FROM {t}")))
        .collect();
    let schema_before = schema_of(&path);
    let v_before: i64 = cli(&path, "SELECT v FROM schema_version").parse().unwrap();
    assert!(
        (2..=db::SCHEMA_VERSION).contains(&v_before),
        "prod copy at v{v_before}"
    );

    let s = Store::open(&path, NoPublisher).unwrap();
    if v_before == db::SCHEMA_VERSION {
        assert_eq!(
            schema_of(&path),
            schema_before,
            "opening at the current version must not change the schema"
        );
    }
    assert_eq!(
        schema_of(&path),
        schema_of(fresh().store.path()),
        "migrated prod schema equals a fresh schema"
    );
    s.with_conn(|c| {
        assert_eq!(
            c.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))?,
            "ok"
        );
        assert_eq!(
            count(c, "SELECT count(*) FROM pragma_foreign_key_check"),
            0,
            "foreign_key_check is empty after migrating"
        );
        for (t, n) in TABLES.iter().zip(&counts_before) {
            assert_eq!(
                count(c, &format!("SELECT count(*) FROM {t}")).to_string(),
                *n,
                "{t}"
            );
        }
        Ok(())
    })
    .unwrap();
    drop(s);
    assert_eq!(
        cli(&path, "SELECT v FROM schema_version"),
        db::SCHEMA_VERSION.to_string()
    );
    let s = Store::open(&path, NoPublisher).unwrap();

    // Every row decodes through the repositories, and the numbers match the sqlite3 CLI.
    let n = |sql: &str| cli(&path, sql).parse::<usize>().unwrap();
    let wss = s.workspaces().all().unwrap();
    assert_eq!(wss.len(), n("SELECT count(*) FROM workspaces"));
    let (mut items, mut sessions, mut decisions, mut messages, mut tags, mut projects) =
        (0, 0, 0, 0, 0, 0);
    for w in &wss {
        items += s.items().all(&w.id).unwrap().len();
        sessions += s.sessions().list(Some(&w.id)).unwrap().len();
        decisions += s.decisions().open(&w.id).unwrap().len()
            + s.decisions().recent(&w.id, -1).unwrap().len();
        messages += s.messages().list(&w.id, "steve", i64::MAX).unwrap().len();
        tags += s.tags().list(&w.id).unwrap().len();
        projects += s.projects().list(&w.id, false).unwrap().len()
            + s.projects().list(&w.id, true).unwrap().len();
        s.settings().all(&w.id).unwrap();
    }
    assert_eq!(items, n("SELECT count(*) FROM items WHERE kind = 'task'"));
    assert_eq!(sessions, n("SELECT count(*) FROM sessions"));
    assert_eq!(
        decisions,
        n("SELECT count(*) FROM decisions WHERE status = 'open' OR resolved IS NOT NULL")
    );
    assert_eq!(
        messages,
        n("SELECT count(*) FROM messages WHERE space = 'steve'")
    );
    assert_eq!(tags, n("SELECT count(*) FROM tags"));
    assert_eq!(projects, n("SELECT count(*) FROM projects"));
    assert_eq!(
        s.bots().list(None).unwrap().len(),
        n("SELECT count(*) FROM bots WHERE deleted = 0")
    );
    assert_eq!(
        s.devices().list().unwrap().len(),
        n("SELECT count(*) FROM devices")
    );
    assert_eq!(
        s.users().count().unwrap() as usize,
        n("SELECT count(*) FROM users")
    );
    let mut events = 0;
    for ses in s.sessions().list(None).unwrap() {
        events += s.events().after(&ses.id, 0, i64::MAX).unwrap().len();
    }
    assert_eq!(events, n("SELECT count(*) FROM events"));
    assert_eq!(
        s.peek_counter("item:ST").unwrap().to_string(),
        cli(
            &path,
            "SELECT coalesce(max(value), 0) FROM counters WHERE workspace_id = '*' AND name = 'item:ST'"
        )
    );
    // no JSON column fell back to its default where the text is not the default
    assert_eq!(
        n("SELECT count(*) FROM items WHERE json_valid(plan) AND json_valid(source)"),
        n("SELECT count(*) FROM items"),
        "all plan/source JSON valid"
    );
}
