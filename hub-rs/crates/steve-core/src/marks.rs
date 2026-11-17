//! Statistics hooks: the structured points of an item's life that the
//! statistics read instead of timeline text. Status changes are recorded by the store itself; the
//! orchestrator only marks deliveries, fail cards, release outcomes and pull requests, one call each.
//!
//! A mark that cannot be written is logged and dropped: statistics never stop the work they describe.

use serde_json::{Map, Value, json};
use steve_store::{NewMark, fail_reason};

use crate::Tx;
use crate::release::NO_RESULT;

/// The `release` mark's value for an outcome written on the approved release decision; `None` for
/// `releasing` (not an outcome yet) and anything unknown. A failure that went back to the previous
/// version is `rolled_back`; one nobody reported (`NO_RESULT`) is `unknown`.
pub(crate) fn release_value(outcome: &str, result: &Map<String, Value>) -> Option<&'static str> {
    let text = |k: &str| result.get(k).and_then(Value::as_str).unwrap_or_default();
    match outcome {
        "released" => Some("released"),
        "not_released" => Some("not_released"),
        "failed" if text("reason") == NO_RESULT => Some("unknown"),
        "failed" if text("note").starts_with("已回到") => Some("rolled_back"),
        "failed" => Some("failed"),
        _ => None,
    }
}

/// Why Steve refused a delivery: the reason code of its `reject` mark.
pub mod reject {
    /// A work unit still has open steps.
    pub const UNIT_UNFINISHED: &str = "unit_unfinished";
    /// A work unit's report did not meet the graph's evidence rules (summary, outputs, unit checks,
    /// an operated check for an integration).
    pub const UNIT_EVIDENCE: &str = "unit_evidence";
    /// A work unit delivered while its work graph was paused.
    pub const UNIT_PAUSED: &str = "unit_paused";
    /// A work unit delivered with input still queued for its session.
    pub const UNIT_QUEUED: &str = "unit_queued";
    /// A work unit delivered on an attempt that no longer counts: it was replaced, or the item's goal
    /// or cycle changed, or the item ended.
    pub const UNIT_STALE: &str = "unit_stale";
    /// The item's work graph has units that still block it.
    pub const WORKFLOW_OPEN: &str = "workflow_open";
    /// The design confirmation needs a design file that opens and was received.
    pub const DESIGN_FILE: &str = "design_file";
    /// A check names a scope that does not exist.
    pub const SCOPE: &str = "scope";
}

impl Tx<'_> {
    fn stats_mark(&self, ws: &str, item_id: &str, m: NewMark) {
        let m = NewMark {
            workspace_id: ws.into(),
            item_id: item_id.into(),
            ts: crate::util::now(),
            ..m
        };
        if let Err(e) = self.db().store().stats().mark_add(&m) {
            tracing::warn!("stats mark {} {item_id}: {e}", m.kind);
        }
    }

    /// `deliver`: `auto` when the delivery was accepted by itself, else `manual` with the accept card.
    pub(crate) fn mark_deliver(
        &self,
        ws: &str,
        item_id: &str,
        actor: &str,
        decision: Option<&str>,
    ) {
        self.stats_mark(
            ws,
            item_id,
            NewMark {
                kind: "deliver".into(),
                to_v: Some(if decision.is_some() { "manual" } else { "auto" }.into()),
                actor: actor.into(),
                r#ref: decision.map(Into::into),
                ..Default::default()
            },
        );
    }

    /// `fail`: the reason code from the card's text, the text itself in `detail.why`.
    pub(crate) fn mark_fail(&self, ws: &str, item_id: &str, why: &str, decision: &str) {
        self.stats_mark(
            ws,
            item_id,
            NewMark {
                kind: "fail".into(),
                to_v: Some(fail_reason(why).into()),
                actor: "steve".into(),
                r#ref: Some(decision.into()),
                detail: json!({ "why": why }),
                ..Default::default()
            },
        );
    }

    /// `pr` ([`crate::pull::MARK_PR`]): a pull request left to you to merge (`awaiting`) or merged
    /// (`merged`), with the review its change had in `detail.review` (`graph`, `you`, `none`).
    pub(crate) fn mark_pr(&self, ws: &str, item_id: &str, to: &str, detail: Value) {
        self.stats_mark(
            ws,
            item_id,
            NewMark {
                kind: crate::pull::MARK_PR.into(),
                to_v: Some(to.into()),
                actor: "steve".into(),
                detail,
                ..Default::default()
            },
        );
    }

    /// `invalidate`: a closing was void; the reason code ([`crate::cycle::void`]), its text in `detail.why`.
    pub(crate) fn mark_invalidate(&self, ws: &str, item_id: &str, code: &str, why: &str) {
        self.stats_mark(
            ws,
            item_id,
            NewMark {
                kind: "invalidate".into(),
                to_v: Some(code.into()),
                actor: "steve".into(),
                detail: json!({ "why": why }),
                ..Default::default()
            },
        );
    }

    /// `reject`: Steve refused a delivery; the reason code ([`reject`]), the refusal in `detail.why`.
    pub(crate) fn mark_reject(&self, ws: &str, item_id: &str, actor: &str, code: &str, why: &str) {
        self.stats_mark(
            ws,
            item_id,
            NewMark {
                kind: "reject".into(),
                to_v: Some(code.into()),
                actor: actor.into(),
                detail: json!({ "why": why }),
                ..Default::default()
            },
        );
    }

    /// `merge`: the item's pull request was seen merged (its address in `ref`).
    pub(crate) fn mark_merge(&self, ws: &str, item_id: &str, url: &str, number: u64) {
        self.stats_mark(
            ws,
            item_id,
            NewMark {
                kind: "merge".into(),
                to_v: Some("merged".into()),
                actor: "steve".into(),
                r#ref: Some(url.into()),
                detail: json!({ "pr": number }),
                ..Default::default()
            },
        );
    }

    /// `wake`: Steve woke a work graph's coordinator (`workflow.progress`) about `unit`, which ended
    /// `status` on `attempt`; on the graph's item, the coordinator's session in `ref`.
    pub(crate) fn mark_wake(
        &self,
        ws: &str,
        parent: &str,
        session: &str,
        unit: &str,
        status: &str,
        attempt: i64,
    ) {
        self.stats_mark(
            ws,
            parent,
            NewMark {
                kind: "wake".into(),
                to_v: Some(status.into()),
                actor: "steve".into(),
                r#ref: Some(session.into()),
                detail: json!({ "unit_id": unit, "status": status, "attempt": attempt }),
                ..Default::default()
            },
        );
    }

    /// `release`: what became of an approved release, once it is known.
    pub(crate) fn mark_release(
        &self,
        item_id: &str,
        decision: &str,
        outcome: &str,
        result: &Map<String, Value>,
    ) {
        let Some(v) = release_value(outcome, result) else {
            return;
        };
        let Ok(Some(item)) = self.db().item(item_id) else {
            return;
        };
        let keep = |k: &str| result.get(k).cloned().unwrap_or(Value::Null);
        self.stats_mark(
            &item.workspace_id,
            item_id,
            NewMark {
                kind: "release".into(),
                to_v: Some(v.into()),
                actor: "steve".into(),
                r#ref: Some(decision.into()),
                detail: json!({ "version": keep("version"), "kind": keep("kind"), "reason": keep("reason") }),
                ..Default::default()
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obj(v: Value) -> Map<String, Value> {
        v.as_object().cloned().unwrap_or_default()
    }

    #[test]
    fn release_values() {
        let none = Map::new();
        assert_eq!(release_value("released", &none), Some("released"));
        assert_eq!(release_value("not_released", &none), Some("not_released"));
        assert_eq!(release_value("releasing", &none), None);
        assert_eq!(
            release_value("failed", &obj(json!({"reason": "x", "note": "已回到 v1"}))),
            Some("rolled_back")
        );
        assert_eq!(
            release_value("failed", &obj(json!({"reason": NO_RESULT, "note": ""}))),
            Some("unknown")
        );
        assert_eq!(
            release_value(
                "failed",
                &obj(json!({"note": "Hub 已是新版本，前端仍是 v1"}))
            ),
            Some("failed")
        );
    }
}
