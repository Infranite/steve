//! Taking back what YOLO did, and the cool-down buttons:
//! `POST /yolo/actions/:id/undo` by kind — `decision` (the core's or the module's `undo_decision`,
//! 24 hours), `rollback` (the last release only, `release.sh --rollback <prev>`), `recall` (the channel
//! message), `reanswer` (a correction sent to the session), `pause` (the item stops and leaves YOLO) —
//! each one stamped, audited as `yolo.undo` and counted for breaker B3; `now` runs a scheduled answer
//! at once, `cancel` leaves the card to the user for the rest of the window.

use std::sync::Arc;

use rusqlite::OptionalExtension;
use serde_json::{Value, json};
use steve_modules::{ApiError, ApiResult};
use steve_store::base::audit;
use steve_store::yolo::{self as repo, ActionRow};

use super::engine::Engine;
use crate::ItemAction;

/// What the API says when there is nothing to undo.
pub const CANNOT: &str = "这个决定不能撤销";
/// The reason a scheduled answer the user took over keeps: the card stays theirs.
pub const USER_CANCEL: &str = "你选择自己来定";
const DAY_MS: i64 = 24 * 60 * 60_000;

/// Recalls a sent channel message (steve-channels' outbox, wired by the Hub).
pub trait Recall: Send + Sync {
    fn recall(&self, ws: &str, actor: &str, message_id: &str) -> ApiResult<()>;
}

fn ce(e: crate::CoreError) -> ApiError {
    match e {
        crate::CoreError::Module(e) => e,
        crate::CoreError::Msg(m) => ApiError::rule(m),
        e => ApiError::Internal(e.to_string()),
    }
}

impl Engine {
    /// Wires the channel recall in (once).
    pub fn set_recall(&self, r: Arc<dyn Recall>) {
        let _ = self.recall_slot().set(r);
    }

    /// The kind of undo `a` offers right now, if any.
    pub fn undo_kind(&self, a: &ActionRow) -> Option<String> {
        if a.outcome != "decided" || a.undone.is_some() {
            return None;
        }
        let kind = a.undo.as_ref()?.get("kind")?.as_str()?.to_string();
        let d = a
            .decision_id
            .as_deref()
            .and_then(|id| self.core().store().decision(id).ok().flatten());
        let ok = match kind.as_str() {
            "decision" => d.as_ref().is_some_and(|d| {
                d.status == "resolved"
                    && d.resolved.is_some_and(|r| self.yolo().now() - r <= DAY_MS)
            }),
            // only a rollback the database allows: release.sh would refuse the rest
            "rollback" => d.as_ref().is_some_and(|d| {
                self.last_release(d)
                    && self
                        .core()
                        .rollback_refusal(d.result["prev"].as_str().unwrap_or_default())
                        .is_none()
            }),
            "recall" => self.recall_slot().get().is_some() && d.is_some(),
            "reanswer" => a.item_id.is_some() || a.session_id.is_some(),
            "pause" => a
                .item_id
                .as_deref()
                .and_then(|i| self.core().store().item(i).ok().flatten())
                .is_some_and(|i| !matches!(i.status.as_str(), "done" | "drop" | "pause")),
            _ => false,
        };
        ok.then_some(kind)
    }

    /// The release of `d` went live, it is the newest one that did, and the version before it is known.
    fn last_release(&self, d: &crate::repo::Decision) -> bool {
        if d.result.get("outcome").and_then(Value::as_str) != Some("released")
            || d.result
                .get("prev")
                .and_then(Value::as_str)
                .is_none_or(|p| p.is_empty())
        {
            return false;
        }
        let newest: Option<String> = self
            .yolo()
            .store()
            .reader(|c| {
                c.prepare_cached(
                    "SELECT id FROM decisions WHERE workspace_id = ? AND kind = 'approve'
                       AND subject_type IN ('release', 'delivery_batch') AND status = 'resolved'
                       AND json_extract(result, '$.approved') = 1
                     ORDER BY resolved DESC, rowid DESC LIMIT 1",
                )?
                .query_row([&d.workspace_id], |r| r.get(0))
                .optional()
                .map_err(ApiError::from)
            })
            .unwrap_or(None);
        newest.as_deref() == Some(d.id.as_str())
    }

    fn own_action(&self, ws: &str, id: &str) -> ApiResult<ActionRow> {
        self.yolo()
            .store()
            .reader(|c| repo::action_get(c, id))?
            .filter(|a| a.workspace_id == ws)
            .ok_or_else(|| ApiError::not_found("记录"))
    }

    /// `POST /yolo/actions/:id/undo` (`text`: the new answer of a `reanswer`). Returns the action.
    pub fn undo(&self, ws: &str, me: &str, id: &str, text: Option<&str>) -> ApiResult<ActionRow> {
        let a = self.own_action(ws, id)?;
        let Some(kind) = self.undo_kind(&a) else {
            return Err(ApiError::rule(CANNOT));
        };
        let core = self.core();
        let decision = a.decision_id.clone().unwrap_or_default();
        let mut detail = json!({ "kind": kind, "decision": a.decision_id, "rule": a.rule });
        match kind.as_str() {
            "decision" => {
                core.undo_decision(&decision, me).map_err(ce)?;
            }
            "rollback" => {
                let d = core
                    .store()
                    .decision(&decision)
                    .map_err(ce)?
                    .ok_or_else(|| ApiError::rule(CANNOT))?;
                let prev = d.result["prev"].as_str().unwrap_or_default().to_string();
                let item = d.item_id.clone().ok_or_else(|| ApiError::rule(CANNOT))?;
                let log = core.rollback_release(&item, &prev, &a.id).map_err(ce)?;
                detail["prev"] = json!(prev);
                detail["log"] = json!(log);
            }
            "recall" => {
                let d = core
                    .store()
                    .decision(&decision)
                    .map_err(ce)?
                    .ok_or_else(|| ApiError::rule(CANNOT))?;
                let r = self
                    .recall_slot()
                    .get()
                    .ok_or_else(|| ApiError::rule(CANNOT))?;
                r.recall(ws, me, &d.subject_id)?;
            }
            "reanswer" => {
                let t = text
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                    .ok_or_else(|| ApiError::invalid("text"))?;
                let old = a.choice.clone().unwrap_or_default();
                let msg = format!(
                    "（更正）用户改了答案：{t}。之前 Steve 替用户回答的是：{old}。按新的答案继续；已经按旧答案做了的，改过来。"
                );
                core.send_to_session(a.item_id.as_deref(), a.session_id.as_deref(), &msg)
                    .map_err(ce)?;
                if let Some(i) = a.item_id.as_deref() {
                    core.store()
                        .timeline_add(i, me, "me", &format!("改了 YOLO 的回答：{t}"), None)
                        .map_err(ce)?;
                }
                detail["text"] = json!(t);
            }
            "pause" => {
                let item = a.item_id.clone().ok_or_else(|| ApiError::rule(CANNOT))?;
                let it = core.item_action(&item, ItemAction::Pause, me).map_err(ce)?;
                self.yolo()
                    .item_off(ws, &item, &it.title, "你在 YOLO 摘要里暂停了这件")?;
            }
            _ => return Err(ApiError::rule(CANNOT)),
        }
        self.yolo().store().transaction(|tx| {
            audit(
                tx,
                ws,
                me,
                "yolo.undo",
                &format!("yolo_action:{}", a.id),
                &detail,
            )?;
            Ok::<_, ApiError>(())
        })?;
        // a rollback is undone once it went through (the target Hub marks it from releases/rollback.json)
        if kind != "rollback" {
            self.yolo().undone(ws, &a.id)?;
        }
        self.yolo()
            .store()
            .reader(|c| repo::action_get(c, &a.id))?
            .ok_or_else(|| ApiError::not_found("记录"))
    }

    /// `POST /yolo/actions/:id/now`: a scheduled answer runs at once (checked again first).
    pub fn run_now(&self, ws: &str, id: &str) -> ApiResult<ActionRow> {
        let a = self.own_action(ws, id)?;
        if a.outcome != "scheduled" {
            return Err(ApiError::conflict("这个代办已经不在冷静期了"));
        }
        let now = self.yolo().now();
        self.yolo().postpone(&a.id, now, &a.reason)?;
        let a = self.own_action(ws, id)?;
        match self.run_due_row(a)? {
            Some(a) => Ok(a),
            None => self.own_action(ws, id),
        }
    }

    /// `POST /yolo/actions/:id/cancel`: “我来定” — the card stays with the user for this window.
    pub fn cancel(&self, ws: &str, id: &str) -> ApiResult<ActionRow> {
        let a = self.own_action(ws, id)?;
        if !self.yolo().finish(&a.id, "cancelled", Some(USER_CANCEL))? {
            return Err(ApiError::conflict("这个代办已经不在冷静期了"));
        }
        self.own_action(ws, id)
    }
}
