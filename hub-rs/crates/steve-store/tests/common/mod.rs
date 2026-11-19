#![allow(dead_code)]

use std::path::Path;
use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use steve_store::{BusEvent, FnPublisher, Store};
use tempfile::TempDir;

/// Records every published event.
#[derive(Clone, Default)]
pub struct Events(pub Arc<Mutex<Vec<BusEvent>>>);

impl Events {
    /// Returns and clears what was published so far.
    pub fn take(&self) -> Vec<BusEvent> {
        std::mem::take(&mut *self.0.lock().unwrap())
    }
}

pub struct T {
    pub dir: TempDir,
    pub store: Store,
    pub events: Events,
}

/// A store on a fresh database in a temp dir, with `ensure_workspace` done and events cleared.
pub fn fresh() -> T {
    let dir = tempfile::tempdir().unwrap();
    let events = Events::default();
    let sink = events.clone();
    let store = Store::open(
        dir.path().join("steve.sqlite3"),
        FnPublisher(move |e| sink.0.lock().unwrap().push(e)),
    )
    .unwrap();
    store.ensure_workspace().unwrap();
    events.take();
    T { dir, store, events }
}

/// Explicit category for fixtures whose subject is unrelated to classification.
pub fn task_tags(store: &Store, ws: &str) -> Vec<String> {
    vec![
        store
            .tags()
            .kinds(ws)
            .unwrap()
            .into_iter()
            .find(|tag| tag.name == "feat")
            .expect("fixture feat category")
            .id,
    ]
}

/// `sqlite_master` in the format of `sqlite3 db "select type, name, tbl_name, sql from sqlite_master order by type, name"`.
pub fn schema(c: &Connection) -> String {
    let mut st = c
        .prepare("select type, name, tbl_name, sql from sqlite_master order by type, name")
        .unwrap();
    let rows = st
        .query_map([], |r| {
            Ok(format!(
                "{}|{}|{}|{}",
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?.unwrap_or_default()
            ))
        })
        .unwrap();
    let mut s = rows.map(Result::unwrap).collect::<Vec<_>>().join("\n");
    s.push('\n');
    s
}

pub fn schema_of(path: &Path) -> String {
    schema(&Connection::open(path).unwrap())
}

/// Column names of a table in declaration order.
pub fn columns(c: &Connection, table: &str) -> Vec<String> {
    let mut st = c.prepare(&format!("PRAGMA table_info({table})")).unwrap();
    st.query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// Keys of a serialized value, in order.
pub fn keys(v: &impl serde::Serialize) -> Vec<String> {
    match serde_json::to_value(v).unwrap() {
        serde_json::Value::Object(m) => m.keys().cloned().collect(),
        other => panic!("not an object: {other}"),
    }
}

pub fn count(c: &Connection, sql: &str) -> i64 {
    c.query_row(sql, [], |r| r.get(0)).unwrap()
}

/// Drive the production outbox declaration through the unique slice core.
pub fn prune_outbox(store: &Store, now: i64, keep: i64) -> steve_store::Result<usize> {
    use steve_store_sqlite::retention::{self as r, KeepFrom, policies::Outbox};
    let mut policy = Outbox::new();
    policy.definition.keep_from = KeepFrom::Fixed {
        millis: keep.max(0) as u64,
    };
    let mut after = None;
    let mut total = 0;
    loop {
        let report = store.with_conn(|c| {
            Ok(r::run_slice(
                c,
                &policy,
                now,
                r::DEFAULT_BUDGET,
                after.as_ref(),
            )?)
        })?;
        assert_eq!(report.failures, 0);
        total += report.affected as usize;
        after = report.next;
        if after.is_none() {
            return Ok(total);
        }
    }
}

/// Drain a background declaration, releasing the writer between slices.
pub fn retain(store: &Store, kind: steve_store_sqlite::retention::policies::Kind, now: i64) -> u64 {
    let mut after = None;
    let mut total = 0;
    loop {
        let report = store
            .retention_slice(
                kind,
                now,
                steve_store_sqlite::retention::Budget::default(),
                after,
            )
            .unwrap();
        assert_eq!(report.failures, 0);
        total += report.affected;
        after = report.next;
        if after.is_none() {
            return total;
        }
    }
}
