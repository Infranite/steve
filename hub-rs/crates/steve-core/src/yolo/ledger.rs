//! The ledger (modules/yolo.md §3.3): every decision YOLO looked at gets a `yolo_actions` row, and
//! what it answered carries `decided_by: "yolo"` in the decision's `result`, next to the action, rule
//! and reason. Also the streaks the table needs (automatic continues without progress).

use serde_json::{Value, json};
use steve_modules::ApiResult;
use steve_modules::outbound::YOLO_ACTOR;
use steve_store::base::audit;
use steve_store::yolo::{self as repo, ActionRow, ITEM, NewAction, ScopeChange};

use super::Yolo;
use super::judge::Verdict;
use crate::repo::Decision;

/// Rules that send a stopped agent back to work; three in a row without progress take the item out.
pub const CONTINUE_RULES: &[&str] = &[
    "continue.stall",
    "continue.error",
    "continue.release",
    "accept.rework",
];

/// Shown when an item leaves YOLO after going round in circles.
pub const STUCK_REASON: &str = "连续 3 次自动继续没有进展";
/// Shown when an item leaves YOLO after its rebase before a release came back three times.
pub const REBASE_REASON: &str = "发布前的 rebase 已经交回 Bot 3 次";

/// What a run of earlier automatic continues says about the item.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Streak {
    /// Automatic continues and reworks in a row whose progress snapshot equals the current one.
    pub no_progress: u32,
    /// Automatic continues after session errors in a row (for the back-off).
    pub errors: u32,
}

/// `actions`: the item's actions, newest first. Only what YOLO actually did counts; an answer of a
/// different kind (an acceptance, an approval) ends the run.
pub fn streak(actions: &[ActionRow], progress: &Value) -> Streak {
    let mut s = Streak::default();
    let (mut same, mut errors) = (true, true);
    for a in actions.iter().filter(|a| a.outcome == "decided") {
        if !CONTINUE_RULES.contains(&a.rule.as_str()) {
            break;
        }
        if same && a.checks.get("progress") == Some(progress) {
            s.no_progress += 1;
        } else {
            same = false;
        }
        if errors && a.rule == "continue.error" {
            s.errors += 1;
        } else {
            errors = false;
        }
        if !same && !errors {
            break;
        }
    }
    s
}

/// The row for a verdict about `d`; `outcome` and `due_at` are the caller's.
pub fn new_action(
    d: &Decision,
    window: Option<&str>,
    v: &Verdict,
    outcome: &str,
    due_at: Option<i64>,
    progress: &Value,
) -> NewAction {
    let mut a = NewAction {
        workspace_id: d.workspace_id.clone(),
        window_id: window.map(String::from),
        decision_id: Some(d.id.clone()),
        item_id: d.item_id.clone(),
        session_id: d.session_id.clone(),
        kind: d.kind.clone(),
        subject_type: d.subject_type.clone(),
        rule: v.rule().to_string(),
        outcome: outcome.into(),
        due_at,
        ..Default::default()
    };
    match v {
        Verdict::Auto {
            choice,
            text,
            reason,
            undo,
            checks,
            ..
        } => {
            a.choice = Some(choice.clone());
            a.text = text.clone();
            a.reason = reason.clone();
            let mut c = if checks.is_object() {
                checks.clone()
            } else {
                json!({})
            };
            if CONTINUE_RULES.contains(&a.rule.as_str()) {
                c["progress"] = progress.clone();
            }
            a.checks = c;
            a.undo = undo.map(|u| json!({ "kind": u }));
        }
        Verdict::Decline { reason, .. } | Verdict::Mute { reason, .. } => {
            a.reason = reason.clone();
        }
        Verdict::Wait { reason, .. } => a.reason = reason.clone(),
        Verdict::Off => {}
    }
    a
}

/// The fields merged into the `result` of a decision YOLO answered.
pub fn result_with_yolo(result: &Value, action: &ActionRow) -> Value {
    let mut r = if result.is_object() {
        result.clone()
    } else {
        json!({})
    };
    r["decided_by"] = json!(YOLO_ACTOR);
    r["yolo_action"] = json!(action.id);
    r["yolo_rule"] = json!(action.rule);
    r["yolo_reason"] = json!(action.reason);
    r
}

impl Yolo {
    /// Writes one action row.
    pub fn record(&self, a: &NewAction) -> ApiResult<ActionRow> {
        let now = self.now();
        self.store()
            .transaction(|tx| repo::action_add(tx, a, now).map_err(Into::into))
    }

    /// Moves a scheduled action to `due_at` (still scheduled); false when it was no longer scheduled.
    pub fn postpone(&self, id: &str, due_at: i64, reason: &str) -> ApiResult<bool> {
        self.store()
            .transaction(|tx| repo::action_postpone(tx, id, due_at, reason).map_err(Into::into))
    }

    /// Settles a scheduled action; false when it was no longer scheduled.
    pub fn finish(&self, id: &str, outcome: &str, reason: Option<&str>) -> ApiResult<bool> {
        let now = self.now();
        self.store()
            .transaction(|tx| repo::action_finish(tx, id, outcome, reason, now).map_err(Into::into))
    }

    /// Takes one item out of YOLO (the item layer becomes `off` with `reason`), with audit, revision
    /// and a notice. Nothing happens when it is already off.
    pub fn item_off(&self, ws: &str, item: &str, title: &str, reason: &str) -> ApiResult<bool> {
        let now = self.now();
        let changed = self.store().transaction(|tx| {
            let (before, after) = repo::scope_put(
                tx,
                ws,
                ITEM,
                item,
                &ScopeChange {
                    mode: Some("off"),
                    reason: Some(reason),
                    ..Default::default()
                },
                YOLO_ACTOR,
                now,
            )?;
            let changed = before.as_ref() != Some(&after);
            if changed {
                audit(
                    tx,
                    ws,
                    YOLO_ACTOR,
                    "yolo.scope",
                    &format!("{ITEM}:{item}"),
                    &json!({ "object_type": ITEM, "object_id": item, "mode": "off", "reason": reason }),
                )?;
            }
            Ok::<_, steve_modules::ApiError>(changed)
        })?;
        if changed {
            let text = format!("YOLO 不再替你处理「{title}」：{reason}");
            if let Err(e) =
                crate::notify::notify(&self.0.db, ws, "system", "info", &text, Some(item))
            {
                tracing::error!("yolo item off notice: {e}");
            }
        }
        Ok(changed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn act(rule: &str, outcome: &str, progress: Value) -> ActionRow {
        ActionRow {
            id: "ya_1".into(),
            workspace_id: "w".into(),
            window_id: None,
            decision_id: None,
            item_id: None,
            session_id: None,
            kind: "fail".into(),
            subject_type: "item".into(),
            rule: rule.into(),
            outcome: outcome.into(),
            choice: None,
            text: None,
            reason: String::new(),
            checks: json!({ "progress": progress }),
            cost: None,
            due_at: None,
            done_at: None,
            undo: None,
            undone: None,
            created: 0,
        }
    }

    #[test]
    fn streaks_count_continues_without_progress() {
        let p = json!({"done": 1, "head": null});
        let q = json!({"done": 2, "head": null});
        let list = vec![
            act("continue.error", "decided", p.clone()),
            act("continue.error", "declined", p.clone()),
            act("continue.stall", "decided", p.clone()),
            act("continue.error", "decided", q.clone()),
            act("continue.error", "decided", p.clone()),
        ];
        assert_eq!(
            streak(&list, &p),
            Streak {
                no_progress: 2,
                errors: 1
            }
        );
        let list = vec![
            act("continue.error", "decided", p.clone()),
            act("accept.checks", "decided", p.clone()),
            act("continue.error", "decided", p.clone()),
        ];
        assert_eq!(
            streak(&list, &p),
            Streak {
                no_progress: 1,
                errors: 1
            }
        );
        assert_eq!(streak(&[], &p), Streak::default());
    }
}
