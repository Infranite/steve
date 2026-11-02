//! “YOLO 替你做了这些决定” (modules/yolo.md §3.3, §7; Y6): the ledger grouped by window and item for
//! `GET /yolo/actions`, with what each row can be undone with right now, and the line `/汇报` gets
//! (a [`steve_modules::DigestNotes`] source).

use std::collections::BTreeMap;

use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use steve_modules::{ApiError, ApiResult};
use steve_store::yolo::{self as repo, ActionRow, WindowRow};

use super::engine::Engine;

/// `GET /yolo/actions` filters.
#[derive(Debug, Clone, Default)]
pub struct Query {
    /// A window id, or `current`; none = the latest windows.
    pub window: Option<String>,
    pub item: Option<String>,
    pub outcome: Option<String>,
}

/// How many windows the digest shows without a filter.
const WINDOWS: i64 = 20;

/// The digest's word for what a rule did.
pub fn category(rule: &str) -> &'static str {
    match rule.split('.').next().unwrap_or("") {
        "accept" => "验收",
        "release" => "发布",
        "ask" => "代答",
        "continue" => "继续",
        "tool" => "工具",
        "action" => "批准",
        "propose" => "开始",
        "chan" | "mail" => "代发",
        _ => "其他",
    }
}

/// `验收 2、发布 1、代答 3` for the decided actions in `acts` (in a fixed order).
pub fn by_category(acts: &[ActionRow]) -> String {
    const ORDER: [&str; 9] = [
        "验收", "发布", "代答", "继续", "工具", "批准", "开始", "代发", "其他",
    ];
    let mut n: BTreeMap<&str, usize> = BTreeMap::new();
    for a in acts.iter().filter(|a| a.outcome == "decided") {
        *n.entry(category(&a.rule)).or_default() += 1;
    }
    ORDER
        .iter()
        .filter_map(|c| n.get(c).map(|k| format!("{c} {k}")))
        .collect::<Vec<_>>()
        .join("、")
}

impl Engine {
    /// `GET /yolo/actions`: windows (newest first) → items → actions (oldest first).
    pub fn digest(&self, ws: &str, q: &Query) -> ApiResult<Value> {
        let store = self.yolo().store();
        let windows: Vec<WindowRow> = match q.window.as_deref() {
            Some("current") => store
                .reader(|c| repo::window_current(c, ws))?
                .into_iter()
                .collect(),
            Some(id) => store
                .reader(|c| repo::window_get(c, id))?
                .filter(|w| w.workspace_id == ws)
                .map(|w| vec![w])
                .ok_or_else(|| ApiError::not_found("窗口"))?,
            None => store.reader(|c| repo::windows(c, ws, WINDOWS))?,
        };
        let waiting: i64 = store.reader(|c| {
            c.prepare_cached(
                "SELECT count(*) FROM decisions WHERE workspace_id = ? AND status = 'open'",
            )?
            .query_row([ws], |r| r.get(0))
            .map_err(ApiError::from)
        })?;
        let mut out = Vec::new();
        for w in windows {
            let mut acts = store.reader(|c| repo::actions_of_window(c, &w.id))?;
            acts.retain(|a| {
                q.item
                    .as_deref()
                    .is_none_or(|i| a.item_id.as_deref() == Some(i))
                    && q.outcome.as_deref().is_none_or(|o| a.outcome == o)
            });
            let mut counts: BTreeMap<String, i64> = BTreeMap::new();
            for a in &acts {
                *counts.entry(a.outcome.clone()).or_default() += 1;
            }
            let mut groups: Vec<(Option<String>, Vec<Value>)> = Vec::new();
            for a in &acts {
                let row = self.action_json(a)?;
                match groups.iter_mut().find(|g| g.0 == a.item_id) {
                    Some(g) => g.1.push(row),
                    None => groups.push((a.item_id.clone(), vec![row])),
                }
            }
            let mut items = Vec::new();
            let mut cost = 0.0;
            let mut has_cost = false;
            for (item, rows) in groups {
                let it = match item.as_deref() {
                    Some(i) => self.core().store().item(i).ok().flatten(),
                    None => None,
                };
                if let Some(i) = item.as_deref()
                    && let Some(c) = self.item_cost(i)?
                {
                    cost += c;
                    has_cost = true;
                }
                items.push(json!({
                    "item_id": item,
                    "title": it.as_ref().map(|i| i.title.clone()),
                    "status": it.as_ref().map(|i| i.status.clone()),
                    "actions": rows,
                }));
            }
            let releases = acts
                .iter()
                .filter(|a| a.outcome == "decided" && a.rule == super::release::RULE)
                .count();
            out.push(json!({
                "id": w.id, "started": w.started, "ended": w.ended, "ended_why": w.ended_why,
                "counts": counts, "summary": by_category(&acts), "releases": releases,
                "cost": has_cost.then_some(cost),
                "waiting": if w.ended.is_none() { Some(waiting) } else { None },
                "items": items,
            }));
        }
        Ok(json!({ "windows": out, "waiting": waiting }))
    }

    /// One ledger row as the digest shows it.
    pub fn action_json(&self, a: &ActionRow) -> ApiResult<Value> {
        let d = match a.decision_id.as_deref() {
            Some(id) => self.core().store().decision(id).ok().flatten(),
            None => None,
        };
        let mut v = serde_json::to_value(a).map_err(ApiError::from)?;
        v["question"] = json!(d.as_ref().map(|d| d.question.clone()));
        v["decision_status"] = json!(d.as_ref().map(|d| d.status.clone()));
        v["can_undo"] = json!(self.undo_kind(a));
        Ok(v)
    }

    /// The model cost of the item's sessions (none without a harness that reports one).
    fn item_cost(&self, item: &str) -> ApiResult<Option<f64>> {
        self.yolo().store().reader(|c| {
            c.prepare_cached(
                "SELECT sum(cost) FROM sessions WHERE item_id = ? AND cost IS NOT NULL",
            )?
            .query_row([item], |r| r.get::<_, Option<f64>>(0))
            .map_err(ApiError::from)
        })
    }
}

/// The `/汇报` line: what YOLO did since the last report (a cursor kept in `settings`, written
/// directly so it is no revision of the user's settings).
pub struct Notes {
    pub store: steve_store::Store,
}

/// The settings key of the report cursor (ms).
pub const REPORTED: &str = "yolo.reported";

impl Notes {
    fn take_inner(&self, ws: &str) -> ApiResult<Option<String>> {
        let now = steve_store::now();
        let since: i64 = self.store.reader(|c| {
            c.prepare_cached("SELECT value FROM settings WHERE workspace_id = ? AND key = ?")?
                .query_row(params![ws, REPORTED], |r| r.get::<_, String>(0))
                .optional()
                .map(|v| v.and_then(|v| v.parse().ok()).unwrap_or(0))
                .map_err(ApiError::from)
        })?;
        let acts = self.store.reader(|c| {
            repo::actions_where(
                c,
                &repo::ActionFilter {
                    ws,
                    since: Some(since),
                    limit: 5000,
                    ..Default::default()
                },
            )
        })?;
        self.store.transaction(|tx| {
            tx.execute(
                "INSERT INTO settings (workspace_id, key, value, updated) VALUES (?, ?, ?, ?)
                 ON CONFLICT(workspace_id, key) DO UPDATE SET value = excluded.value, updated = excluded.updated",
                params![ws, REPORTED, now.to_string(), now],
            )?;
            Ok::<_, ApiError>(())
        })?;
        let decided = acts.iter().filter(|a| a.outcome == "decided").count();
        let declined = acts.iter().filter(|a| a.outcome == "declined").count();
        if decided + declined == 0 {
            return Ok(None);
        }
        let what = by_category(&acts);
        Ok(Some(format!(
            "YOLO 替你做了 {decided} 件{}，留给你 {declined} 件；摘要见 #/yolo",
            if what.is_empty() {
                String::new()
            } else {
                format!("（{what}）")
            }
        )))
    }
}

impl steve_modules::DigestNotes for Notes {
    fn take(&self, ws: &str) -> Vec<String> {
        match self.take_inner(ws) {
            Ok(n) => n.into_iter().collect(),
            Err(e) => {
                tracing::warn!("yolo digest notes: {e}");
                Vec::new()
            }
        }
    }
}
