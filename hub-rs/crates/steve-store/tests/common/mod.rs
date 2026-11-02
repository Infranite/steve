#![allow(dead_code)]

use std::path::Path;
use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use steve_store::{ChangeEvent, FnPublisher, Store};
use tempfile::TempDir;

/// Records every published event.
#[derive(Clone, Default)]
pub struct Events(pub Arc<Mutex<Vec<ChangeEvent>>>);

impl Events {
    /// Returns and clears what was published so far.
    pub fn take(&self) -> Vec<ChangeEvent> {
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
