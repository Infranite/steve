//! Finding the existing item an event belongs to, for the `item.link` action (§3.2):
//! `branch` (a `steve/<id>` branch name in the payload), `ref` (an `ST-123` in the text), `thread`
//! (an item made from an earlier event of the same `thread_key`). Other modules register better
//! finders for a method ([`crate::FlowSetup::item_finder`], e.g. GitHub's PR → branch lookup);
//! they are asked first, the built-in one last.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use regex::Regex;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;

use crate::FlowResult;

/// The lookup methods of `item.link`.
pub const METHODS: [&str; 3] = ["branch", "ref", "thread"];

pub trait ItemFinder: Send + Sync {
    /// The item `event` (the run's event object, [`crate::ingest::event_json`]) belongs to.
    fn find(&self, c: &Connection, ws: &str, event: &Value) -> FlowResult<Option<String>>;
}

/// The finders by method; registered ones first.
#[derive(Clone, Default)]
pub struct ItemFinders(HashMap<String, Vec<Arc<dyn ItemFinder>>>);

impl ItemFinders {
    pub fn add(&mut self, by: &str, f: Arc<dyn ItemFinder>) {
        self.0.entry(by.to_string()).or_default().push(f);
    }

    pub fn find(
        &self,
        c: &Connection,
        ws: &str,
        by: &str,
        event: &Value,
    ) -> FlowResult<Option<String>> {
        for f in self.0.get(by).into_iter().flatten() {
            if let Some(id) = f.find(c, ws, event)? {
                return Ok(Some(id));
            }
        }
        match by {
            "branch" => ByBranch.find(c, ws, event),
            "ref" => ByRef.find(c, ws, event),
            "thread" => ByThread.find(c, ws, event),
            _ => Ok(None),
        }
    }
}

fn item_exists(c: &Connection, ws: &str, id: &str) -> FlowResult<bool> {
    Ok(c.query_row(
        "SELECT EXISTS (SELECT 1 FROM items WHERE id = ? AND workspace_id = ?)",
        params![id, ws],
        |r| r.get(0),
    )?)
}

/// Every string in a JSON value, depth first.
fn strings<'v>(v: &'v Value, out: &mut Vec<&'v str>) {
    match v {
        Value::String(s) => out.push(s),
        Value::Array(a) => a.iter().for_each(|x| strings(x, out)),
        Value::Object(m) => m.values().for_each(|x| strings(x, out)),
        _ => {}
    }
}

fn first_existing(
    c: &Connection,
    ws: &str,
    re: &Regex,
    texts: &[&str],
) -> FlowResult<Option<String>> {
    for t in texts {
        for cap in re.captures_iter(t) {
            let id = cap[1].to_ascii_uppercase();
            if item_exists(c, ws, &id)? {
                return Ok(Some(id));
            }
        }
    }
    Ok(None)
}

/// `steve/<id>` anywhere in the payload (head branch of a check or PR).
pub struct ByBranch;

impl ItemFinder for ByBranch {
    fn find(&self, c: &Connection, ws: &str, event: &Value) -> FlowResult<Option<String>> {
        static RE: OnceLock<Regex> = OnceLock::new();
        let re = RE.get_or_init(|| Regex::new(r"(?i)\bsteve/([a-z]{1,6}-\d{1,9})\b").expect("re"));
        let mut texts = Vec::new();
        strings(&event["payload"], &mut texts);
        first_existing(c, ws, re, &texts)
    }
}

/// `ST-123` in the summary or the payload text.
pub struct ByRef;

impl ItemFinder for ByRef {
    fn find(&self, c: &Connection, ws: &str, event: &Value) -> FlowResult<Option<String>> {
        static RE: OnceLock<Regex> = OnceLock::new();
        let re = RE.get_or_init(|| Regex::new(r"\b([A-Z]{1,6}-\d{1,9})\b").expect("re"));
        let mut texts = vec![event["summary"].as_str().unwrap_or("")];
        strings(&event["payload"], &mut texts);
        first_existing(c, ws, re, &texts)
    }
}

/// The latest item made from (linked `from`) another event of the same thread.
pub struct ByThread;

impl ItemFinder for ByThread {
    fn find(&self, c: &Connection, ws: &str, event: &Value) -> FlowResult<Option<String>> {
        let Some(thread) = event["thread_key"].as_str().filter(|t| !t.is_empty()) else {
            return Ok(None);
        };
        Ok(c.query_row(
            "SELECT l.subject_id FROM links l JOIN ext_events e ON e.id = l.object_id
              WHERE l.workspace_id = ?1 AND l.subject_type = 'item' AND l.predicate = 'from' AND l.object_type = 'event'
                AND e.workspace_id = ?1 AND e.thread_key = ?2 AND e.id != ?3
                AND EXISTS (SELECT 1 FROM items i WHERE i.id = l.subject_id AND i.workspace_id = ?1)
              ORDER BY e.received DESC, l.created DESC LIMIT 1",
            params![ws, thread, event["id"].as_str().unwrap_or("")],
            |r| r.get(0),
        )
        .optional()?)
    }
}
