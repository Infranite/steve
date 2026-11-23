//! Persisted progress and source links for items opened from a personal channel trigger.

use std::collections::{BTreeMap, HashMap};

use sea_query::{Expr, ExprTrait, OnConflict, Query};
use serde::Serialize;
use serde_json::Value;

use crate::{Read, Result, W};

#[derive(Debug, Clone, PartialEq)]
pub struct OpenItem {
    pub ws: String,
    pub item: String,
    pub message: String,
    pub now: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct History {
    pub diag_seq: i64,
    pub error: String,
    pub error_at: i64,
    pub now: String,
    pub at: i64,
}

impl History {
    fn from_json(v: &Value) -> Self {
        Self {
            diag_seq: v["diag_seq"].as_i64().unwrap_or(0),
            error: v["error"].as_str().unwrap_or_default().to_owned(),
            error_at: v["error_at"].as_i64().unwrap_or(0),
            now: v["now"].as_str().unwrap_or_default().to_owned(),
            at: v["at"].as_i64().unwrap_or(0),
        }
    }
}

fn key(item: &str) -> String {
    format!("trigger.report.{item}")
}

pub fn open_items(r: &mut impl Read) -> Result<Vec<OpenItem>> {
    let mut by_workspace = BTreeMap::<_, HashMap<_, _>>::new();
    for (ws, item, now) in super::items::progress_of_open_items(r)? {
        by_workspace.entry(ws).or_default().insert(item, now);
    }
    let mut out = Vec::new();
    for (ws, items) in by_workspace {
        let names: Vec<_> = items.keys().map(String::as_str).collect();
        let links = super::links::channel_sources(r, &ws, &names)?;
        let messages: Vec<_> = links.iter().map(|(_, id)| id.as_str()).collect();
        let self_triggers = super::thread_triggers::personal_trigger_sources(r, &ws, &messages)?;
        for (item, message) in links {
            if self_triggers.contains(&message)
                && let Some(now) = items.get(&item)
            {
                out.push(OpenItem {
                    ws: ws.clone(),
                    item,
                    message,
                    now: now.clone(),
                });
            }
        }
    }
    Ok(out)
}

pub fn history(r: &mut impl Read, ws: &str, item: &str) -> Result<Option<History>> {
    let raw: Option<String> = r.one(
        Query::select()
            .column("value")
            .from("settings")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("key").eq(key(item))),
        |row| row.get(0),
    )?;
    Ok(raw
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .as_ref()
        .map(History::from_json))
}

pub fn keep(w: &mut W<'_>, ws: &str, item: &str, history: &History, now: i64) -> Result<()> {
    w.exec(
        Query::insert()
            .into_table("settings")
            .columns(["workspace_id", "key", "value", "updated"])
            .values_panic([
                ws.into(),
                key(item).into(),
                serde_json::to_string(history)?.into(),
                now.into(),
            ])
            .on_conflict(
                OnConflict::columns(["workspace_id", "key"])
                    .update_columns(["value", "updated"])
                    .to_owned(),
            ),
    )?;
    Ok(())
}
