//! Observations of work events and marks in Core's contract tests.

use sea_query::{Asterisk, Expr, ExprTrait, Func, Order, Query};
use serde_json::Value;

use crate::Store;
use crate::contract::{Json, Q};

pub struct DoneEvent {
    pub workspace: String,
    pub payload: Value,
}

pub fn done_events(store: &Store, item: &str) -> Vec<DoneEvent> {
    let events = store
        .read(|r| {
            r.all(
                Query::select()
                    .columns(["workspace_id", "payload"])
                    .from("outbox_events")
                    .and_where(Expr::col("topic").eq("work.item.done"))
                    .order_by("seq", Order::Asc),
                |r| {
                    Ok(DoneEvent {
                        workspace: r.get(0)?,
                        payload: r.get::<Json<Value>>(1)?.0,
                    })
                },
            )
        })
        .unwrap();
    events
        .into_iter()
        .filter(|event| event.payload["item_id"] == item)
        .collect()
}

pub fn set_outbox_available(store: &Store, available: bool) {
    let sql = if available {
        "ALTER TABLE outbox_events_gone RENAME TO outbox_events"
    } else {
        "ALTER TABLE outbox_events RENAME TO outbox_events_gone"
    };
    store
        .write_locked(|c| {
            c.execute_batch(sql)?;
            Ok(())
        })
        .unwrap();
}

pub fn outbox_receipt(store: &Store, consumer: &str) -> Option<String> {
    store
        .read(|r| {
            r.one(
                Query::select()
                    .column("outcome")
                    .from("outbox_receipts")
                    .and_where(Expr::col("consumer_id").eq(consumer)),
                |r| r.get(0),
            )
        })
        .unwrap()
        .flatten()
}

pub struct Mark {
    pub kind: String,
    pub from: Option<String>,
    pub to: Option<String>,
    pub reference: Option<String>,
    pub detail: Option<Value>,
}

pub fn item_marks(store: &Store, item: &str, kind: Option<&str>) -> Vec<Mark> {
    let mut query = Query::select();
    query
        .columns(["kind", "from_v", "to_v", "ref", "detail"])
        .from("item_marks")
        .and_where(Expr::col("item_id").eq(item))
        .order_by("id", Order::Asc);
    if let Some(kind) = kind {
        query.and_where(Expr::col("kind").eq(kind));
    }
    store
        .read(|r| {
            r.all(&query, |r| {
                Ok(Mark {
                    kind: r.get(0)?,
                    from: r.get(1)?,
                    to: r.get(2)?,
                    reference: r.get(3)?,
                    detail: r.get::<Option<Json<Value>>>(4)?.map(|detail| detail.0),
                })
            })
        })
        .unwrap()
}

pub fn audit_count(store: &Store, actions: &[&str], target: Option<&str>) -> i64 {
    let mut query = Query::select();
    query
        .expr(Func::count(Expr::col(Asterisk)))
        .from("audit")
        .and_where(Expr::col("action").is_in(actions.iter().copied()));
    if let Some(target) = target {
        query.and_where(Expr::col("target").eq(target));
    }
    store
        .read(|r| r.one(&query, |r| r.get(0)))
        .unwrap()
        .unwrap()
}

pub fn closing_fingerprint(store: &Store, item: &str, fingerprint: Option<&str>) {
    store
        .write_locked(|c| {
            if let Some(fingerprint) = fingerprint {
                c.execute(
                    "UPDATE items SET closing=json_set(closing,'$.fingerprint',?2) WHERE id=?1",
                    [item, fingerprint],
                )?;
            } else {
                c.execute(
                    "UPDATE items SET closing=json_remove(closing,'$.fingerprint') WHERE id=?",
                    [item],
                )?;
            }
            Ok(())
        })
        .unwrap();
}

pub fn remove_decision(store: &Store, id: &str) {
    store
        .write(|w| {
            w.exec(
                Query::delete()
                    .from_table("decisions")
                    .and_where(Expr::col("id").eq(id)),
            )
        })
        .unwrap();
}

pub fn operation_cycle(store: &Store, id: &str, cycle: i64) {
    store
        .write(|w| {
            w.exec(
                Query::update()
                    .table("item_operations")
                    .value("cycle", cycle)
                    .and_where(Expr::col("id").eq(id)),
            )
        })
        .unwrap();
}

pub struct Audit {
    pub actor: String,
    pub target: String,
    pub detail: Value,
}

pub fn audit(store: &Store, action: &str, target: Option<&str>) -> Vec<Audit> {
    let mut query = Query::select();
    query
        .columns(["actor", "target", "detail"])
        .from("audit")
        .and_where(Expr::col("action").eq(action))
        .order_by("id", Order::Asc);
    if let Some(target) = target {
        query.and_where(Expr::col("target").eq(target));
    }
    store
        .read(|r| {
            r.all(&query, |r| {
                Ok(Audit {
                    actor: r.get(0)?,
                    target: r.get(1)?,
                    detail: serde_json::from_str(&r.get::<String>(2)?).unwrap_or(Value::Null),
                })
            })
        })
        .unwrap()
}

pub fn merge_proposal(store: &Store, id: &str) -> (String, String) {
    store
        .read(|r| {
            r.one(
                Query::select()
                    .columns(["decision_id", "status"])
                    .from("merge_proposals")
                    .and_where(Expr::col("id").eq(id)),
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
        })
        .unwrap()
        .unwrap()
}

pub fn party_status(store: &Store, id: &str) -> String {
    store
        .read(|r| {
            r.one(
                Query::select()
                    .column("status")
                    .from("parties")
                    .and_where(Expr::col("id").eq(id)),
                |row| row.get(0),
            )
        })
        .unwrap()
        .unwrap()
}
