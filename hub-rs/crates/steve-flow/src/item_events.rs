//! Item state changes as events (signals-automation.md §6): steve-core calls [`ingest_item_event`]
//! after an item is finished or dropped; the event (`source=item`, `item.done` / `item.dropped`)
//! goes through the normal pipeline ([`crate::ingest()`]), so it can start a flow whose trigger is
//! `{source:"item", types:[…]}` or wake a `wait(event)` with `match.source = "item"` (the “前置完成”
//! trigger of the domain model: `when: event.payload.item_id == "ST-3"`).
//!
//! Only stored when something could take it (an enabled item trigger, or a waiting run whose match
//! allows item events), so finishing items does not fill the signals page with “没有流程，只记录”.
//! A flow is never started by the end of an item it created itself (no endless chains).
//!
//! `item.done` comes through the outbox ([`ItemTrigger`], consumer `automation.item_trigger` of
//! `work.item.done`): the `ext_events` row, the runs it opens and the receipt commit in one
//! transaction, from the item as it was finished (the event's snapshot), and the event's id keys the
//! dedupe, so a second delivery changes nothing. `item.dropped` still comes from the lifecycle hook
//! ([`ingest_item_event`]).

use std::sync::Arc;

use steve_store::outbox::{Applied, ConsumeError, Consumer, Event, Since};

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use steve_modules::ApiError;

use crate::adapter::{Ingested, RawEvent};
use crate::{FlowCx, FlowResult};

/// `ext_events.source` of item events.
pub const SOURCE: &str = "item";

/// What happened to the item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemEvent {
    /// Finished (`finish`: status `done`).
    Done,
    /// Dropped (`drop`: status `drop`).
    Dropped,
}

impl ItemEvent {
    pub fn type_key(self) -> &'static str {
        match self {
            Self::Done => "item.done",
            Self::Dropped => "item.dropped",
        }
    }

    fn verb(self) -> &'static str {
        match self {
            Self::Done => "完成了",
            Self::Dropped => "放弃了",
        }
    }
}

/// Is there anything an item event of `type_key` could reach in `ws`?
fn anyone_listening(c: &Connection, ws: &str, type_key: &str) -> FlowResult<bool> {
    let trigger: bool = c.query_row(
        "SELECT EXISTS (SELECT 1 FROM flow_triggers t JOIN flows f ON f.id = t.flow_id
           WHERE t.workspace_id = ? AND t.kind = 'item' AND t.enabled = 1 AND f.enabled = 1 AND f.deleted = 0)",
        [ws],
        |r| r.get(0),
    )?;
    if trigger {
        return Ok(true);
    }
    let mut st = c.prepare_cached(
        "SELECT w.match FROM flow_waits w JOIN flow_runs r ON r.id = w.run_id
          WHERE w.workspace_id = ? AND w.mode = 'event' AND r.status = 'waiting'",
    )?;
    let matches = st
        .query_map([ws], |r| r.get::<_, Option<String>>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(matches
        .iter()
        .any(|m| crate::ingest::wait_takes(m.as_deref(), SOURCE, &[type_key])))
}

/// The flows whose runs made `item` (`links(item, 'from', 'flow_run', run)`).
fn creator_flows(c: &Connection, ws: &str, item: &str) -> FlowResult<Vec<String>> {
    let mut st = c.prepare_cached(
        "SELECT DISTINCT r.flow_id FROM links l JOIN flow_runs r ON r.id = l.object_id
          WHERE l.workspace_id = ? AND l.subject_type = 'item' AND l.subject_id = ? AND l.predicate = 'from' AND l.object_type = 'flow_run'",
    )?;
    let rows = st
        .query_map(params![ws, item], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// The event for `item` of `ws`, or `None` when nothing listens.
fn build(
    c: &Connection,
    ws: &str,
    item: &str,
    what: ItemEvent,
    now: i64,
) -> FlowResult<Option<RawEvent>> {
    let row = c
        .query_row(
            "SELECT title, status, bot_id, project_id, source FROM items WHERE id = ? AND workspace_id = ?",
            params![item, ws],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, Option<String>>(4)?,
                ))
            },
        )
        .optional()?;
    let (title, status, bot, project, source) = row.ok_or_else(|| ApiError::not_found("事项"))?;
    let source = source
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .unwrap_or(Value::Null);
    let snap = steve_work_api::ItemDone {
        item_id: item.into(),
        title,
        status,
        bot_id: bot,
        project_id: project,
        source,
        cycle: None,
        revision: None,
        acceptance: None,
    };
    describe(
        c,
        ws,
        &snap,
        what,
        format!("{item}:{}:{now}", what.type_key()),
        now,
    )
}

/// The event for the item snapshot `it`, keyed `external_id`; `None` when nothing listens.
fn describe(
    c: &Connection,
    ws: &str,
    it: &steve_work_api::ItemDone,
    what: ItemEvent,
    external_id: String,
    now: i64,
) -> FlowResult<Option<RawEvent>> {
    if !anyone_listening(c, ws, what.type_key())? {
        return Ok(None);
    }
    let item = it.item_id.as_str();
    let mut ev = RawEvent::new(
        SOURCE,
        what.type_key(),
        external_id,
        now,
        format!("{item}「{}」{}", it.title, what.verb()),
    );
    ev.payload = json!({
        "item_id": item,
        "title": it.title,
        "status": it.status,
        "bot_id": it.bot_id,
        "project_id": it.project_id,
        "source": it.source,
    });
    ev.skip_flows = creator_flows(c, ws, item)?;
    Ok(Some(ev))
}

/// `automation.item_trigger`: `work.item.done` from the outbox becomes the `item.done` event, in the
/// receipt's transaction. Its `external_id` is `<item>:item.done:<outbox event id>`, so a redelivery
/// is a duplicate. An item that is not in the event's workspace is rejected (dead, nothing stored).
pub struct ItemTrigger {
    cx: Arc<FlowCx>,
}

impl ItemTrigger {
    pub const ID: &'static str = "automation.item_trigger";

    pub fn new(cx: Arc<FlowCx>) -> Arc<Self> {
        Arc::new(Self { cx })
    }
}

impl Consumer for ItemTrigger {
    fn id(&self) -> &'static str {
        Self::ID
    }

    fn topic(&self) -> &'static str {
        steve_work_api::ITEM_DONE
    }

    fn since(&self) -> Since {
        Since::Start
    }

    fn apply(&self, tx: &steve_store::Tx, ev: &Event) -> Result<Applied, ConsumeError> {
        if ev.version != steve_work_api::ITEM_DONE_V {
            return Err(ConsumeError::Reject(format!(
                "{} version {}",
                ev.topic, ev.version
            )));
        }
        let it: steve_work_api::ItemDone = serde_json::from_value(ev.payload.clone())
            .map_err(|e| ConsumeError::Reject(format!("{}: {e}", ev.topic)))?;
        let ws = ev.workspace_id.as_str();
        let here: bool = tx.query_row(
            "SELECT EXISTS (SELECT 1 FROM items WHERE id = ? AND workspace_id = ?)",
            params![it.item_id, ws],
            |r| r.get(0),
        )?;
        if !here {
            return Err(ConsumeError::Reject(format!(
                "{} 不在工作空间 {ws}",
                it.item_id
            )));
        }
        let now = self.cx.now();
        let external = format!("{}:{}:{}", it.item_id, ItemEvent::Done.type_key(), ev.id);
        let raw = describe(tx, ws, &it, ItemEvent::Done, external, now).map_err(retry)?;
        let Some(raw) = raw else {
            return Ok(Applied::noted("没有流程在听"));
        };
        crate::ingest::check(&raw).map_err(|e| ConsumeError::Reject(e.to_string()))?;
        let step = crate::ingest::store_in(tx, &self.cx, ws, None, &raw, now).map_err(retry)?;
        // the reliable path never leaves a wake-up to an in-memory callback: an executor that cannot
        // wake a matching wait in this transaction rejects the delivery (rolled back, dead, visible)
        if let crate::ingest::Step::Stored(s) = &step
            && !s.waits.is_empty()
        {
            return Err(ConsumeError::Reject(format!(
                "执行器不能在事务里唤醒等待中的运行（{} 个），这条事件没有处理",
                s.waits.len()
            )));
        }
        let outcome = match &step {
            crate::ingest::Step::Stored(s) => format!("{} · {} 个流程", s.id, s.runs.len()),
            crate::ingest::Step::Done(d) => format!("{d:?}"),
        };
        let (cx, ws) = (self.cx.clone(), ws.to_string());
        Ok(Applied::then(outcome, move || {
            crate::ingest::after_store(&cx, &ws, step);
        }))
    }
}

fn retry(e: crate::FlowError) -> ConsumeError {
    ConsumeError::Retry(e.to_string())
}

/// steve-core, after `finish` (→ [`ItemEvent::Done`]) or `drop` (→ [`ItemEvent::Dropped`]) of an
/// item of `ws`, outside any store transaction. `Ok(None)`: nothing listens, nothing stored.
/// `404 没有这个事项` for an item not in `ws`.
pub async fn ingest_item_event(
    cx: &Arc<FlowCx>,
    ws: &str,
    item_id: &str,
    what: ItemEvent,
) -> FlowResult<Option<Ingested>> {
    let now = cx.now();
    let Some(ev) = cx.store().reader(|c| build(c, ws, item_id, what, now))? else {
        return Ok(None);
    };
    crate::ingest(cx, ws, None, ev).await.map(Some)
}
