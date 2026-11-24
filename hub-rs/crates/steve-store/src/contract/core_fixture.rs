//! Store-owned fixtures for Core tests that need exact timestamps or states
//! that the normal item and decision commands reject.

pub mod runtime;
pub mod thread;
pub mod usage;
pub mod work;

use sea_query::{Expr, ExprTrait, Query};
use serde_json::Value;

use crate::contract::Q;
use crate::{Result, Store};

pub enum ItemState<'a> {
    Phase(&'a str),
    Closed { outcome: &'a str },
    ClosedAtUpdate { outcome: &'a str },
    Failed,
    Cycle(i64),
    RestartCycle,
    ResumeCycle,
    BumpCycle,
    RunningNextCycle,
    Closing(&'a str),
    Parent(&'a str),
    Workspace(&'a str),
    Source(&'a str),
    Kind(&'a str),
    Title(&'a str),
    Updated(i64),
    ShiftUpdated(i64),
    Archive(Option<i64>),
}

pub fn item(store: &Store, id: &str, state: ItemState<'_>) {
    try_item(store, id, state).unwrap();
}

pub fn try_item(store: &Store, id: &str, state: ItemState<'_>) -> Result<()> {
    use ItemState::*;
    let mut update = Query::update();
    update.table("items").and_where(Expr::col("id").eq(id));
    match state {
        Phase(phase) => update.value("phase", phase),
        Closed { outcome } => update.value("phase", "closed").value("outcome", outcome),
        ClosedAtUpdate { outcome } => update
            .value("phase", "closed")
            .value("outcome", outcome)
            .value("closed_at", Expr::col("updated")),
        Failed => update.value("phase", "closed").value("outcome", "failed"),
        Cycle(cycle) => update.value("cycle", cycle),
        RestartCycle => update.value("phase", "active").value("cycle", 2_i64),
        ResumeCycle => update
            .value("phase", "active")
            .value("outcome", None::<String>)
            .value("cycle", Expr::col("cycle").add(1_i64)),
        BumpCycle => update.value("cycle", Expr::col("cycle").add(1_i64)),
        RunningNextCycle => update
            .value("phase", "active")
            .value("cycle", Expr::col("cycle").add(1_i64)),
        Closing(json) => update.value("closing", json),
        Parent(json) => update.value("parent", json),
        Workspace(id) => update.value("workspace_id", id),
        Source(json) => update.value("source", json),
        Kind(kind) => update.value("kind", kind),
        Title(title) => update.value("title", title),
        Updated(at) => update.value("updated", at),
        ShiftUpdated(offset) => update.value("updated", Expr::col("updated").add(offset)),
        Archive(at) => {
            if let Some(at) = at {
                update.value("updated", at);
            }
            update.value("archived", 1_i64)
        }
    };
    store.write(|w| w.exec(&update).map(|_| ()))
}

pub enum DecisionState<'a> {
    Subject(&'a str),
    Created(i64),
    CreatedAfter(&'a str),
    Resolved(i64),
    CreatedAndResolved { created: i64, resolved: i64 },
    ShiftResolved(i64),
    Expired,
    Workspace(&'a str),
    Result(&'a Value),
    ResolvedResult(&'a Value),
}

pub fn decision(store: &Store, id: &str, state: DecisionState<'_>) {
    use DecisionState::*;
    let mut update = Query::update();
    update.table("decisions").and_where(Expr::col("id").eq(id));
    store
        .write::<_, crate::StoreError>(|w| {
            match state {
                Subject(id) => update.value("subject_id", id),
                Created(at) => update.value("created", at),
                CreatedAfter(other) => {
                    let at = w.one(
                        Query::select()
                            .column("created")
                            .from("decisions")
                            .and_where(Expr::col("id").eq(other)),
                        |r| r.get::<i64>(0),
                    )?;
                    update.value("created", at.map(|at| at + 1))
                }
                Resolved(at) => update.value("resolved", at),
                CreatedAndResolved { created, resolved } => {
                    update.value("created", created).value("resolved", resolved)
                }
                ShiftResolved(offset) => {
                    update.value("resolved", Expr::col("resolved").add(offset))
                }
                Expired => update.value("status", "expired"),
                Workspace(id) => update.value("workspace_id", id),
                Result(value) => update.value("result", serde_json::to_string(value)?),
                ResolvedResult(value) => update
                    .value("status", "resolved")
                    .value("result", serde_json::to_string(value)?),
            };
            w.exec(&update)?;
            Ok(())
        })
        .unwrap();
}

pub enum SessionState<'a> {
    Bot(Option<&'a str>),
    Workdir(&'a str),
    Status(&'a str),
    Workspace(&'a str),
    Source(&'a str),
    Device(&'a str),
    Readonly,
}

pub fn session(store: &Store, id: &str, state: SessionState<'_>) {
    let (column, value) = match state {
        SessionState::Bot(id) => ("bot_id", Expr::val(id)),
        SessionState::Workdir(path) => ("cwd", Expr::val(path)),
        SessionState::Status(status) => ("status", Expr::val(status)),
        SessionState::Workspace(id) => ("workspace_id", Expr::val(id)),
        SessionState::Source(source) => ("source", Expr::val(source)),
        SessionState::Device(id) => ("device_id", Expr::val(id)),
        SessionState::Readonly => ("readonly", Expr::val(1_i64)),
    };
    store
        .write(|w| {
            w.exec(
                Query::update()
                    .table("sessions")
                    .value(column, value)
                    .and_where(Expr::col("id").eq(id)),
            )
        })
        .unwrap();
}

pub fn project_device(store: &Store, id: &str, device: &str) {
    store
        .write(|w| {
            w.exec(
                Query::update()
                    .table("projects")
                    .value("device_id", device)
                    .and_where(Expr::col("id").eq(id)),
            )
        })
        .unwrap();
}

pub fn workspace(store: &Store, id: &str, name: &str, prefix: &str, sort: i64, at: i64) {
    store
        .write(|w| {
            w.exec(
                Query::insert()
                    .into_table("workspaces")
                    .columns(["id", "name", "item_prefix", "sort", "created"])
                    .values_panic([
                        id.into(),
                        name.into(),
                        prefix.into(),
                        sort.into(),
                        at.into(),
                    ]),
            )
        })
        .unwrap();
}

pub fn bot_enabled(store: &Store, id: &str, enabled: bool) {
    store
        .write(|w| {
            w.exec(
                Query::update()
                    .table("bots")
                    .value("enabled", i64::from(enabled))
                    .and_where(Expr::col("id").eq(id)),
            )
        })
        .unwrap();
}

pub fn bot_deleted(store: &Store, id: &str) {
    store
        .write(|w| {
            w.exec(
                Query::update()
                    .table("bots")
                    .value("deleted", 1_i64)
                    .and_where(Expr::col("id").eq(id)),
            )
        })
        .unwrap();
}

pub fn unbind_bot(store: &Store, id: &str) {
    store
        .write(|w| {
            w.exec(
                Query::delete()
                    .from_table("bot_workspaces")
                    .and_where(Expr::col("bot_id").eq(id)),
            )
        })
        .unwrap();
}

pub fn reject_tool_receipts(store: &Store) {
    store.write_locked(|c| {
        c.execute_batch("CREATE TEMP TRIGGER receipt_failure BEFORE INSERT ON events WHEN NEW.kind='steve_tool_receipt' BEGIN SELECT RAISE(FAIL,'receipt write failed'); END")?;
        Ok(())
    }).unwrap();
}

pub fn skip_workflow_goal_version(store: &Store, item: &str) {
    store
        .write(|w| {
            w.exec(
                Query::update()
                    .table("workflows")
                    .value("goal_version", Expr::col("goal_version").sub(1_i64))
                    .and_where(Expr::col("item_id").eq(item)),
            )
        })
        .unwrap();
}

pub fn resume_workflow_deliveries(store: &Store) {
    store
        .write_locked(|c| {
            c.execute_batch("DROP TRIGGER fail_workflow_delivery")?;
            Ok(())
        })
        .unwrap();
}

pub fn completed_hooks(store: &Store, id: &str, item: &str, at: i64) {
    store
        .write(|w| {
            w.exec(
                Query::insert()
                    .into_table("item_operations")
                    .columns([
                        "id", "item_id", "cycle", "revision", "kind", "status", "created",
                        "updated",
                    ])
                    .values_panic([
                        id.into(),
                        item.into(),
                        1_i64.into(),
                        0_i64.into(),
                        "hooks".into(),
                        "succeeded".into(),
                        at.into(),
                        at.into(),
                    ]),
            )
        })
        .unwrap();
}

pub fn fail_operation(store: &Store, id: &str) {
    store
        .write(|w| {
            w.exec(
                Query::update()
                    .table("item_operations")
                    .value("status", "failed")
                    .value("updated", Expr::col("updated").add(1_i64))
                    .and_where(Expr::col("id").eq(id)),
            )
        })
        .unwrap();
}
