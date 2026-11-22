//! Finding the existing item an event belongs to, for the `item.link` action:
//! `branch` (a `steve/<id>` branch name in the payload), `ref` (an `ST-123` in the text), `thread`
//! (an item made from an earlier event of the same `thread_key`). Other modules register better
//! finders for a method ([`crate::FlowSetup::item_finder`], e.g. GitHub's PR → branch lookup);
//! they are asked first, the built-in one last.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use regex::Regex;
use serde_json::Value;
use steve_store::base::links::Links;
use steve_store::{ItemReads, SignalRows, Store};

use crate::FlowResult;

/// The lookup methods of `item.link`.
pub const METHODS: [&str; 3] = ["branch", "ref", "thread"];

pub trait ItemFinder: Send + Sync {
    /// The item `event` (the run's event object, [`crate::ingest::event_json`]) belongs to.
    fn find(&self, store: &Store, ws: &str, event: &Value) -> FlowResult<Option<String>>;
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
        store: &Store,
        ws: &str,
        by: &str,
        event: &Value,
    ) -> FlowResult<Option<String>> {
        store.read(|_| {
            Ok((|| -> FlowResult<_> {
                for finder in self.0.get(by).into_iter().flatten() {
                    if let Some(id) = finder.find(store, ws, event)? {
                        return Ok(Some(id));
                    }
                }
                match by {
                    "branch" => ByBranch.find(store, ws, event),
                    "ref" => ByRef.find(store, ws, event),
                    "thread" => ByThread.find(store, ws, event),
                    _ => Ok(None),
                }
            })())
        })?
    }
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
    store: &Store,
    ws: &str,
    re: &Regex,
    texts: &[&str],
) -> FlowResult<Option<String>> {
    let candidates: Vec<_> = texts
        .iter()
        .flat_map(|text| re.captures_iter(text))
        .map(|capture| capture[1].to_ascii_uppercase())
        .collect();
    let existing =
        store.read(|r| ItemReads(r).existing_ids(ws, candidates.iter().map(String::as_str)))?;
    Ok(candidates.into_iter().find(|id| existing.contains(id)))
}

/// `steve/<id>` anywhere in the payload (head branch of a check or PR).
pub struct ByBranch;

impl ItemFinder for ByBranch {
    fn find(&self, store: &Store, ws: &str, event: &Value) -> FlowResult<Option<String>> {
        static RE: OnceLock<Regex> = OnceLock::new();
        let re = RE.get_or_init(|| Regex::new(r"(?i)\bsteve/([a-z]{1,6}-\d{1,9})\b").expect("re"));
        let mut texts = Vec::new();
        strings(&event["payload"], &mut texts);
        first_existing(store, ws, re, &texts)
    }
}

/// `ST-123` in the summary or the payload text.
pub struct ByRef;

impl ItemFinder for ByRef {
    fn find(&self, store: &Store, ws: &str, event: &Value) -> FlowResult<Option<String>> {
        static RE: OnceLock<Regex> = OnceLock::new();
        let re = RE.get_or_init(|| Regex::new(r"\b([A-Z]{1,6}-\d{1,9})\b").expect("re"));
        let mut texts = vec![event["summary"].as_str().unwrap_or("")];
        strings(&event["payload"], &mut texts);
        first_existing(store, ws, re, &texts)
    }
}

/// The latest item made from (linked `from`) another event of the same thread.
pub struct ByThread;

impl ItemFinder for ByThread {
    fn find(&self, store: &Store, ws: &str, event: &Value) -> FlowResult<Option<String>> {
        let Some(thread) = event["thread_key"].as_str().filter(|t| !t.is_empty()) else {
            return Ok(None);
        };
        Ok(store.read(|r| {
            let events =
                SignalRows(r).thread_events(ws, thread, event["id"].as_str().unwrap_or(""))?;
            let at: HashMap<_, _> = events
                .iter()
                .map(|(id, received)| (id.as_str(), *received))
                .collect();
            let mut candidates = Vec::new();
            for batch in events.chunks(500) {
                candidates.extend(Links(r).sources(
                    ws,
                    "item",
                    "from",
                    "event",
                    batch.iter().map(|(id, _)| id.as_str()),
                )?);
            }
            let items =
                ItemReads(r).existing_ids(ws, candidates.iter().map(|(_, id, _)| id.as_str()))?;
            Ok(candidates
                .into_iter()
                .filter(|(_, item, _)| items.contains(item))
                .max_by_key(|(from, _, created)| (at[from.as_str()], *created))
                .map(|(_, item, _)| item))
        })?)
    }
}
