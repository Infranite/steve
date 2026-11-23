//! “YOLO 替你做了这些决定”: the ledger grouped by window and item for
//! `GET /yolo/actions`, with what each row can be undone with right now, and the line `/汇报` gets
//! (a [`steve_modules::DigestNotes`] source).

use std::collections::BTreeMap;

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
const WINDOWS: u64 = 20;

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

/// [`by_category`] as the digest page shows it, in the request's language (`by_category` is the `/汇报` line's).
fn by_category_shown(acts: &[ActionRow]) -> String {
    let words = by_category(acts);
    if words.is_empty() {
        return words;
    }
    words
        .split('、')
        .map(|part| match part.rsplit_once(' ') {
            Some((c, n)) => steve_i18n::t!(
                "core.yolo.digest.count",
                category = category_shown(c),
                n = n
            ),
            None => part.to_string(),
        })
        .collect::<Vec<_>>()
        .join(&steve_i18n::t!("core.yolo.digest.list_sep"))
}

/// One of [`category`]'s words in the request's language.
fn category_shown(c: &str) -> String {
    match c {
        "验收" => steve_i18n::t!("core.yolo.category.acceptance"),
        "发布" => steve_i18n::t!("core.yolo.category.release"),
        "代答" => steve_i18n::t!("core.yolo.category.answer"),
        "继续" => steve_i18n::t!("core.yolo.category.continue"),
        "工具" => steve_i18n::t!("core.yolo.category.tool"),
        "批准" => steve_i18n::t!("core.yolo.category.approve"),
        "开始" => steve_i18n::t!("core.yolo.category.start"),
        "代发" => steve_i18n::t!("core.yolo.category.send"),
        "其他" => steve_i18n::t!("core.yolo.category.other"),
        _ => c.to_string(),
    }
}

impl Engine {
    /// `GET /yolo/actions`: windows (newest first) → items → actions (oldest first).
    pub fn digest(&self, ws: &str, q: &Query) -> ApiResult<Value> {
        let store = self.yolo().store();
        let windows: Vec<WindowRow> = match q.window.as_deref() {
            Some("current") => store
                .read(|c| repo::window_current(c, ws))?
                .into_iter()
                .collect(),
            Some(id) => store
                .read(|c| repo::window_get(c, id))?
                .filter(|w| w.workspace_id == ws)
                .map(|w| vec![w])
                .ok_or_else(|| ApiError::not_found("窗口"))?,
            None => store.read(|c| repo::windows(c, ws, WINDOWS))?,
        };
        let waiting = store.read(|r| steve_store::decision_repo::open_count(r, ws))?;
        let mut out = Vec::new();
        for w in windows {
            let mut acts = store.read(|c| repo::actions_of_window(c, &w.id))?;
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
                "counts": counts, "summary": by_category_shown(&acts), "releases": releases,
                "cost": has_cost.then_some(cost),
                "waiting": if w.ended.is_none() { Some(waiting) } else { None },
                "items": items,
            }));
        }
        Ok(json!({ "windows": out, "waiting": waiting }))
    }

    /// The latest badge action for a decision in the workspace's open window.
    pub fn current_decision_action(&self, ws: &str, decision: &str) -> ApiResult<Value> {
        let action = self.yolo().store().read(|c| -> steve_store::Result<_> {
            let Some(window) = repo::window_current(c, ws)? else {
                return Ok(None);
            };
            Ok(repo::actions_of_decision(c, decision)?
                .into_iter()
                .rfind(|a| a.workspace_id == ws && a.window_id.as_ref() == Some(&window.id)))
        })?;
        action
            .as_ref()
            .map_or(Ok(Value::Null), |a| self.action_json(a))
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
        v["reason"] = json!(super::judge::reason_shown(&a.reason));
        // what the web acts on, as flags: `reason` is display text in the request's language
        v["user_took_over"] =
            json!(a.outcome == "cancelled" && a.reason == super::undo::USER_CANCEL);
        v["limit_what"] = json!(
            a.reason
                .strip_prefix(super::limits::REASON_PREFIX)
                .map(super::limits::what_shown)
        );
        Ok(v)
    }

    /// The model cost of the item's sessions (none without a harness that reports one).
    fn item_cost(&self, item: &str) -> ApiResult<Option<f64>> {
        Ok(self
            .yolo()
            .store()
            .read(|r| steve_store::session_repo::item_cost(r, item))?)
    }
}

/// The `/汇报` line: what YOLO did since the last report (a cursor kept in `settings`, written
/// directly so it is no revision of the user's settings).
pub struct Notes {
    pub store: steve_store::Store,
}

impl Notes {
    fn take_inner(&self, ws: &str) -> ApiResult<Option<String>> {
        let now = steve_store::now();
        let acts = self.store.write(|w| {
            let since = steve_store::settings::yolo_report_cursor(w, ws)?;
            let acts = repo::actions_where(
                w,
                &repo::ActionFilter {
                    ws,
                    since: Some(since),
                    limit: 5000,
                    ..Default::default()
                },
            )?;
            steve_store::settings::save_yolo_report_cursor(w, ws, now)?;
            Ok::<_, ApiError>(acts)
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
