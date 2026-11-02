//! Statistics hooks (modules/stats.md §2.5, slice ST3): the structured points of an item's life that the
//! statistics read instead of timeline text. Status changes are recorded by the store itself; the
//! orchestrator only marks deliveries, fail cards and release outcomes, one call each.
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
