//! A todo's alarms (steve-flow [`steve_flow::alarms`], family `todo`): its reminder at `remind_at` (kind
//! `todo`) and its wake-up at `snooze_until` (kind `snooze`), only while it is open. A reminder that falls
//! inside the snooze is not rung on its own: the wake-up is the reminder. Planned again whenever one of
//! those fields or the state changes (the triggers of migration 102), so done, dropped, handed, deleted or
//! moved todos lose the alarms they no longer have.

use rusqlite::{Connection, OptionalExtension, params};
use steve_flow::FlowResult;
use steve_flow::alarms::{AlarmLine, AlarmSource, Want};

use crate::todo::Zone;

/// The longest todo text an alarm's line carries.
pub const LINE_MAX: usize = 200;

/// What a todo's alarms are planned from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub owner: String,
    pub state: String,
    pub remind_at: Option<i64>,
    pub snooze_until: Option<i64>,
}

/// The alarms a todo should have (pure).
pub fn wants(p: &Plan) -> Vec<Want> {
    if p.state != "open" {
        return Vec::new();
    }
    let mut out = Vec::new();
    let at = |kind: &'static str, t: i64| Want {
        owner: p.owner.clone(),
        kind,
        planned_for: t,
        fire_at: t,
        stale_at: None,
    };
    if let Some(s) = p.snooze_until {
        out.push(at("snooze", s));
    }
    if let Some(r) = p.remind_at
        && p.snooze_until.is_none_or(|s| r >= s)
    {
        out.push(at("todo", r));
    }
    out
}

/// The `todo` [`AlarmSource`].
pub struct TodoAlarms;

impl AlarmSource for TodoAlarms {
    fn family(&self) -> &'static str {
        "todo"
    }

    fn plan(&self, c: &Connection, ws: &str, id: &str, _now: i64) -> FlowResult<Vec<Want>> {
        let p: Option<Plan> = c
            .query_row(
                "SELECT owner, state, remind_at, snooze_until FROM todos WHERE workspace_id = ? AND id = ?",
                params![ws, id],
                |r| {
                    Ok(Plan {
                        owner: r.get(0)?,
                        state: r.get(1)?,
                        remind_at: r.get(2)?,
                        snooze_until: r.get(3)?,
                    })
                },
            )
            .optional()?;
        Ok(p.map(|p| wants(&p)).unwrap_or_default())
    }

    fn line(
        &self,
        c: &Connection,
        ws: &str,
        id: &str,
        _kind: &str,
    ) -> FlowResult<Option<AlarmLine>> {
        type Row = (
            String,
            String,
            String,
            Option<i64>,
            Option<String>,
            Option<String>,
        );
        let row: Option<Row> = c
            .query_row(
                "SELECT owner, state, text, due, due_date, target_date FROM todos WHERE workspace_id = ? AND id = ?",
                params![ws, id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
            )
            .optional()?;
        let Some((owner, state, text, due, due_date, target)) = row else {
            return Ok(None);
        };
        if state != "open" {
            return Ok(None);
        }
        let zone = Zone::of(c, ws, &owner)?;
        let at = match (due_date, due, target) {
            (Some(d), _, _) => Some(d),
            (None, Some(t), _) => Some(zone.format(t, "%m-%d %H:%M")),
            (None, None, Some(d)) => Some(d),
            _ => None,
        };
        let detail =
            at.map(|at| steve_i18n::stored(|| steve_i18n::t!("digest.alarm.due", at = at)));
        Ok(Some(AlarmLine {
            subject: format!("todo:{id}"),
            text: crate::cut(text.trim(), LINE_MAX),
            detail,
            link: Some(format!("#/todos/{id}")),
            planned_for: None,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(state: &str, remind: Option<i64>, snooze: Option<i64>) -> Vec<(&'static str, i64)> {
        wants(&Plan {
            owner: "me".into(),
            state: state.into(),
            remind_at: remind,
            snooze_until: snooze,
        })
        .into_iter()
        .map(|w| (w.kind, w.planned_for))
        .collect()
    }

    #[test]
    fn an_open_todo_rings_its_reminder_and_its_wake_up() {
        assert_eq!(plan("open", Some(10), None), vec![("todo", 10)]);
        assert_eq!(plan("open", None, Some(20)), vec![("snooze", 20)]);
        // a reminder after the snooze rings too; one inside it is the wake-up
        assert_eq!(
            plan("open", Some(30), Some(20)),
            vec![("snooze", 20), ("todo", 30)]
        );
        assert_eq!(plan("open", Some(10), Some(20)), vec![("snooze", 20)]);
        for s in ["done", "dropped", "handed"] {
            assert!(plan(s, Some(10), Some(20)).is_empty(), "{s}");
        }
        assert!(plan("open", None, None).is_empty());
    }
}
