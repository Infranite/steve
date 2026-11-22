//! YOLO scope, window and action persistence.

use sea_query::{Condition, Expr, ExprTrait, Func, OnConflict, Order, Query, SelectStatement};
use serde::Serialize;
use serde_json::{Value, json};

use crate::contract::{Q, Row};
use crate::repo::to_json;
use crate::types::json_or;
use crate::{Read, Result, W, uid};

/// `object_type` of the workspace row.
pub const WORKSPACE: &str = "workspace";
pub const PROJECT: &str = "project";
pub const ITEM: &str = "item";
/// `revisions.object_type` of every scope change.
pub const REVISION_TYPE: &str = "yolo_scope";

/// One `yolo_scopes` row.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ScopeRow {
    pub workspace_id: String,
    pub object_type: String,
    pub object_id: String,
    /// `on` | `off` | `inherit`
    pub mode: String,
    /// Breaker state of the workspace row: `{code, reason, at}`.
    pub paused: Option<Value>,
    pub reason: String,
    pub set_by: String,
    pub revision: i64,
    pub updated: i64,
}

impl ScopeRow {
    fn from_row(r: &Row) -> Result<Self> {
        Ok(Self {
            workspace_id: r.get(0)?,
            object_type: r.get(1)?,
            object_id: r.get(2)?,
            mode: r.get(3)?,
            paused: r
                .get::<Option<String>>(4)?
                .map(|s| json_or(Some(s), Value::Null))
                .filter(|v| !matches!(v, Value::Null)),
            reason: r.get(5)?,
            set_by: r.get(6)?,
            revision: r.get(7)?,
            updated: r.get(8)?,
        })
    }

    /// The part a revision records.
    fn snapshot(&self) -> Value {
        json!({ "mode": self.mode, "paused": self.paused, "reason": self.reason })
    }
}

const SCOPE_COLS: &[&str] = &[
    "workspace_id",
    "object_type",
    "object_id",
    "mode",
    "paused",
    "reason",
    "set_by",
    "revision",
    "updated",
];

fn scope_key(ws: &str, object_type: &str, id: &str) -> Condition {
    Condition::all()
        .add(Expr::col("workspace_id").eq(ws))
        .add(Expr::col("object_type").eq(object_type))
        .add(Expr::col("object_id").eq(id))
}

pub fn scope_get(
    c: &mut impl Read,
    ws: &str,
    object_type: &str,
    id: &str,
) -> Result<Option<ScopeRow>> {
    c.one(
        Query::select()
            .columns(SCOPE_COLS.iter().copied())
            .from("yolo_scopes")
            .cond_where(scope_key(ws, object_type, id)),
        ScopeRow::from_row,
    )
}

/// Every row of a workspace: the workspace row first, then projects, then items.
pub fn scopes(c: &mut impl Read, ws: &str) -> Result<Vec<ScopeRow>> {
    c.all(
        Query::select()
            .columns(SCOPE_COLS.iter().copied())
            .from("yolo_scopes")
            .and_where(Expr::col("workspace_id").eq(ws))
            .order_by_expr(
                Expr::cust(
                    "CASE object_type WHEN 'workspace' THEN 0 WHEN 'project' THEN 1 ELSE 2 END",
                ),
                Order::Asc,
            )
            .order_by("object_id", Order::Asc),
        ScopeRow::from_row,
    )
}

/// What [`scope_put`] changes; `None` keeps the stored value (or the default for a new row).
#[derive(Debug, Clone, Default)]
pub struct ScopeChange<'a> {
    pub mode: Option<&'a str>,
    /// `Some(None)` clears the breaker state.
    pub paused: Option<Option<&'a Value>>,
    pub reason: Option<&'a str>,
}

/// Inserts or updates one row, bumps its `revision`, writes the `revisions` row (object id
/// `<type>:<id>`, patch `{before, after}`) and queues `{type:"yolo", ws}`. Returns `(before, after)`.
/// A change that changes nothing writes nothing and returns the row twice.
pub fn scope_put(
    tx: &mut W<'_>,
    ws: &str,
    object_type: &str,
    id: &str,
    ch: &ScopeChange,
    by: &str,
    now: i64,
) -> Result<(Option<ScopeRow>, ScopeRow)> {
    let before = scope_get(tx, ws, object_type, id)?;
    let mode = ch
        .mode
        .map(str::to_string)
        .or_else(|| before.as_ref().map(|b| b.mode.clone()))
        .unwrap_or_else(|| {
            if object_type == WORKSPACE {
                "off"
            } else {
                "inherit"
            }
            .into()
        });
    let paused = match ch.paused {
        Some(p) => p.cloned(),
        None => before.as_ref().and_then(|b| b.paused.clone()),
    };
    let reason = ch
        .reason
        .map(str::to_string)
        .or_else(|| before.as_ref().map(|b| b.reason.clone()))
        .unwrap_or_default();
    if let Some(b) = &before
        && b.mode == mode
        && b.paused == paused
        && b.reason == reason
    {
        return Ok((before.clone(), b.clone()));
    }
    let revision = before.as_ref().map_or(1, |b| b.revision + 1);
    let paused_s = paused.as_ref().map(to_json).transpose()?;

    tx.exec(
        Query::insert()
            .into_table("yolo_scopes")
            .columns(SCOPE_COLS.iter().copied())
            .values_panic([
                ws.into(),
                object_type.into(),
                id.into(),
                mode.into(),
                paused_s.into(),
                reason.into(),
                by.into(),
                revision.into(),
                now.into(),
            ])
            .on_conflict(
                OnConflict::columns(["workspace_id", "object_type", "object_id"])
                    .update_columns(["mode", "paused", "reason", "set_by", "revision", "updated"])
                    .to_owned(),
            ),
    )?;
    let after = crate::repo::must(scope_get(tx, ws, object_type, id)?)?;
    crate::contract::audit::revise(
        tx,
        ws,
        REVISION_TYPE,
        &format!("{object_type}:{id}"),
        revision,
        &before.as_ref().map_or(Value::Null, ScopeRow::snapshot),
        &after.snapshot(),
        by,
    )?;
    tx.emit(steve_types::DomainEvent::from(
        steve_types::AutonomyEvent::Yolo { ws: ws.into() },
    ));
    Ok((before, after))
}

/// One `yolo_windows` row: a stretch during which the workspace switch was on.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WindowRow {
    pub id: String,
    pub workspace_id: String,
    pub started: i64,
    pub ended: Option<i64>,
    /// `off` | `tripped`
    pub ended_why: Option<String>,
    /// The cards open when it started may be handled too.
    pub include_open: bool,
    pub counters: Value,
}

impl WindowRow {
    fn from_row(r: &Row) -> Result<Self> {
        Ok(Self {
            id: r.get(0)?,
            workspace_id: r.get(1)?,
            started: r.get(2)?,
            ended: r.get(3)?,
            ended_why: r.get(4)?,
            include_open: r.get::<i64>(5)? != 0,
            counters: json_or(r.get(6)?, json!({})),
        })
    }
}

const WINDOW_COLS: &[&str] = &[
    "id",
    "workspace_id",
    "started",
    "ended",
    "ended_why",
    "include_open",
    "counters",
];

/// The window still running in a workspace, if any.
fn window_query() -> SelectStatement {
    Query::select()
        .columns(WINDOW_COLS.iter().copied())
        .from("yolo_windows")
        .take()
}

pub fn window_current(c: &mut impl Read, ws: &str) -> Result<Option<WindowRow>> {
    let order = c.db().dialect().insertion_order("yolo_windows");
    c.one(
        window_query()
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("ended").is_null())
            .order_by("started", Order::Desc)
            .order_by_expr(order, Order::Desc)
            .limit(1),
        WindowRow::from_row,
    )
}

pub fn window_get(c: &mut impl Read, id: &str) -> Result<Option<WindowRow>> {
    c.one(
        window_query().and_where(Expr::col("id").eq(id)),
        WindowRow::from_row,
    )
}

/// Windows of a workspace, newest first.
pub fn windows(c: &mut impl Read, ws: &str, limit: u64) -> Result<Vec<WindowRow>> {
    let order = c.db().dialect().insertion_order("yolo_windows");
    c.all(
        window_query()
            .and_where(Expr::col("workspace_id").eq(ws))
            .order_by("started", Order::Desc)
            .order_by_expr(order, Order::Desc)
            .limit(limit),
        WindowRow::from_row,
    )
}

/// Starts a window (`yw_…`).
pub fn window_open(w: &mut W<'_>, ws: &str, include_open: bool, now: i64) -> Result<WindowRow> {
    let id = uid("yw");
    w.exec(
        Query::insert()
            .into_table("yolo_windows")
            .columns(["id", "workspace_id", "started", "include_open"])
            .values_panic([
                id.clone().into(),
                ws.into(),
                now.into(),
                i64::from(include_open).into(),
            ]),
    )?;
    crate::repo::must(window_get(w, &id)?)
}

/// Ends a running window; a window that already ended keeps its first end.
pub fn window_close(w: &mut W<'_>, id: &str, why: &str, now: i64) -> Result<()> {
    w.exec(
        Query::update()
            .table("yolo_windows")
            .values([("ended", now.into()), ("ended_why", why.into())])
            .and_where(Expr::col("id").eq(id))
            .and_where(Expr::col("ended").is_null()),
    )?;
    Ok(())
}

/// A new `yolo_actions` row.
#[derive(Debug, Clone, Default)]
pub struct NewAction {
    pub workspace_id: String,
    pub window_id: Option<String>,
    pub decision_id: Option<String>,
    pub item_id: Option<String>,
    pub session_id: Option<String>,
    pub kind: String,
    pub subject_type: String,
    pub rule: String,
    /// `scheduled` | `decided` | `declined` | `muted` | `cancelled` | `failed`
    pub outcome: String,
    pub choice: Option<String>,
    pub text: Option<String>,
    pub reason: String,
    pub checks: Value,
    pub due_at: Option<i64>,
    pub undo: Option<Value>,
}

/// One `yolo_actions` row.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ActionRow {
    pub id: String,
    pub workspace_id: String,
    pub window_id: Option<String>,
    pub decision_id: Option<String>,
    pub item_id: Option<String>,
    pub session_id: Option<String>,
    pub kind: String,
    pub subject_type: String,
    pub rule: String,
    pub outcome: String,
    pub choice: Option<String>,
    pub text: Option<String>,
    pub reason: String,
    pub checks: Value,
    pub cost: Option<f64>,
    pub due_at: Option<i64>,
    pub done_at: Option<i64>,
    pub undo: Option<Value>,
    pub undone: Option<i64>,
    pub created: i64,
}

impl ActionRow {
    fn from_row(r: &Row) -> Result<Self> {
        Ok(Self {
            id: r.get(0)?,
            workspace_id: r.get(1)?,
            window_id: r.get(2)?,
            decision_id: r.get(3)?,
            item_id: r.get(4)?,
            session_id: r.get(5)?,
            kind: r.get(6)?,
            subject_type: r.get(7)?,
            rule: r.get(8)?,
            outcome: r.get(9)?,
            choice: r.get(10)?,
            text: r.get(11)?,
            reason: r.get(12)?,
            checks: json_or(r.get(13)?, json!({})),
            cost: r.get(14)?,
            due_at: r.get(15)?,
            done_at: r.get(16)?,
            undo: r
                .get::<Option<String>>(17)?
                .map(|s| json_or(Some(s), Value::Null))
                .filter(|v| !matches!(v, Value::Null)),
            undone: r.get(18)?,
            created: r.get(19)?,
        })
    }
}

const ACTION_COLS: &[&str] = &[
    "id",
    "workspace_id",
    "window_id",
    "decision_id",
    "item_id",
    "session_id",
    "kind",
    "subject_type",
    "rule",
    "outcome",
    "choice",
    "text",
    "reason",
    "checks",
    "cost",
    "due_at",
    "done_at",
    "undo",
    "undone",
    "created",
];

fn action_query() -> SelectStatement {
    Query::select()
        .columns(ACTION_COLS.iter().copied())
        .from("yolo_actions")
        .take()
}

fn ordered_actions(c: &mut impl Read, order: Order) -> SelectStatement {
    let insertion = c.db().dialect().insertion_order("yolo_actions");
    action_query()
        .order_by("created", order.clone())
        .order_by_expr(insertion, order)
        .take()
}

pub fn action_get(c: &mut impl Read, id: &str) -> Result<Option<ActionRow>> {
    c.one(
        action_query().and_where(Expr::col("id").eq(id)),
        ActionRow::from_row,
    )
}

/// Records one action (`ya_…`) and queues `{type:"yolo_action", id, decision_id, ws}`.
pub fn action_add(tx: &mut W<'_>, a: &NewAction, now: i64) -> Result<ActionRow> {
    let id = uid("ya");
    let undo = a.undo.as_ref().map(to_json).transpose()?;
    let checks = if matches!(a.checks, Value::Null) {
        "{}".to_string()
    } else {
        to_json(&a.checks)?
    };

    tx.exec(
        Query::insert()
            .into_table("yolo_actions")
            .columns([
                "id",
                "workspace_id",
                "window_id",
                "decision_id",
                "item_id",
                "session_id",
                "kind",
                "subject_type",
                "rule",
                "outcome",
                "choice",
                "text",
                "reason",
                "checks",
                "due_at",
                "undo",
                "created",
            ])
            .values_panic([
                id.clone().into(),
                a.workspace_id.as_str().into(),
                a.window_id.as_deref().into(),
                a.decision_id.as_deref().into(),
                a.item_id.as_deref().into(),
                a.session_id.as_deref().into(),
                a.kind.as_str().into(),
                a.subject_type.as_str().into(),
                a.rule.as_str().into(),
                a.outcome.as_str().into(),
                a.choice.as_deref().into(),
                a.text.as_deref().into(),
                a.reason.as_str().into(),
                checks.into(),
                a.due_at.into(),
                undo.into(),
                now.into(),
            ]),
    )?;
    emit_action(tx, &id, a.decision_id.as_deref(), &a.workspace_id);
    crate::repo::must(action_get(tx, &id)?)
}

/// Actions of one window, oldest first.
pub fn actions_of_window(c: &mut impl Read, window: &str) -> Result<Vec<ActionRow>> {
    let mut query = ordered_actions(c, Order::Asc);
    c.all(
        query.and_where(Expr::col("window_id").eq(window)),
        ActionRow::from_row,
    )
}

/// Number of actions per outcome in one window.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Counts {
    pub decided: i64,
    pub declined: i64,
    pub scheduled: i64,
}

pub fn counts(c: &mut impl Read, window: &str) -> Result<Counts> {
    let rows = c.all(
        Query::select()
            .column("outcome")
            .expr(Func::count(Expr::col(sea_query::Asterisk)))
            .from("yolo_actions")
            .and_where(Expr::col("window_id").eq(window))
            .group_by_col("outcome"),
        |r| Ok((r.get::<String>(0)?, r.get(1)?)),
    )?;
    let mut n = Counts::default();
    for (outcome, k) in rows {
        match outcome.as_str() {
            "decided" => n.decided = k,
            "declined" => n.declined = k,
            "scheduled" => n.scheduled = k,
            _ => {}
        }
    }
    Ok(n)
}

/// Cancels every scheduled action of a window (the switch went off or the breaker tripped); returns how many.
pub fn cancel_scheduled(tx: &mut W<'_>, ws: &str, window: &str, now: i64) -> Result<usize> {
    let ids: Vec<(String, Option<String>)> = tx.all(
        Query::select()
            .columns(["id", "decision_id"])
            .from("yolo_actions")
            .and_where(Expr::col("window_id").eq(window))
            .and_where(Expr::col("outcome").eq("scheduled")),
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    tx.exec(
        Query::update()
            .table("yolo_actions")
            .values([("outcome", "cancelled".into()), ("done_at", now.into())])
            .and_where(Expr::col("window_id").eq(window))
            .and_where(Expr::col("outcome").eq("scheduled")),
    )?;
    for (id, decision_id) in &ids {
        emit_action(tx, id, decision_id.as_deref(), ws);
    }
    Ok(ids.len())
}

/// Every action about one decision, oldest first.
pub fn actions_of_decision(c: &mut impl Read, decision: &str) -> Result<Vec<ActionRow>> {
    let mut query = ordered_actions(c, Order::Asc);
    c.all(
        query.and_where(Expr::col("decision_id").eq(decision)),
        ActionRow::from_row,
    )
}

/// The latest `limit` actions about one item, newest first.
pub fn actions_of_item(c: &mut impl Read, item: &str, limit: u64) -> Result<Vec<ActionRow>> {
    let mut query = ordered_actions(c, Order::Desc);
    c.all(
        query.and_where(Expr::col("item_id").eq(item)).limit(limit),
        ActionRow::from_row,
    )
}

/// Settles an action (`decided` | `cancelled` | `failed`…); `reason` replaces the stored one when given.
/// Only a `scheduled` action changes, so an action is executed once. Returns whether it changed.
pub fn action_finish(
    tx: &mut W<'_>,
    id: &str,
    outcome: &str,
    reason: Option<&str>,
    now: i64,
) -> Result<bool> {
    let mut query = Query::update();
    query
        .table("yolo_actions")
        .values([("outcome", outcome.into()), ("done_at", now.into())])
        .and_where(Expr::col("id").eq(id))
        .and_where(Expr::col("outcome").eq("scheduled"));
    if let Some(reason) = reason {
        query.value("reason", reason);
    }
    let n = tx.exec(&query)?;
    if n == 1
        && let Some(a) = action_get(tx, id)?
    {
        emit_action(tx, &a.id, a.decision_id.as_deref(), &a.workspace_id);
    }
    Ok(n == 1)
}

/// Moves a scheduled action to a later time (its judgement says wait, not no); false when it is no
/// longer scheduled.
pub fn action_postpone(w: &mut W<'_>, id: &str, due_at: i64, reason: &str) -> Result<bool> {
    let n = w.exec(
        Query::update()
            .table("yolo_actions")
            .values([("due_at", due_at.into()), ("reason", reason.into())])
            .and_where(Expr::col("id").eq(id))
            .and_where(Expr::col("outcome").eq("scheduled")),
    )?;
    Ok(n == 1)
}

/// Which actions [`actions_where`] returns; `None` fields do not filter.
#[derive(Debug, Clone, Default)]
pub struct ActionFilter<'a> {
    pub ws: &'a str,
    pub window: Option<&'a str>,
    pub item: Option<&'a str>,
    pub outcome: Option<&'a str>,
    /// Rules, any of them.
    pub rules: &'a [&'a str],
    /// Created at or after (ms).
    pub since: Option<i64>,
    pub limit: i64,
}

/// Actions of a workspace, newest first.
pub fn actions_where(c: &mut impl Read, f: &ActionFilter<'_>) -> Result<Vec<ActionRow>> {
    let mut query = ordered_actions(c, Order::Desc);
    query.and_where(Expr::col("workspace_id").eq(f.ws));
    for (column, value) in [
        ("window_id", f.window),
        ("item_id", f.item),
        ("outcome", f.outcome),
    ] {
        if let Some(value) = value {
            query.and_where(Expr::col(column).eq(value));
        }
    }
    if let Some(since) = f.since {
        query.and_where(Expr::col("created").gte(since));
    }
    if !f.rules.is_empty() {
        query.and_where(Expr::col("rule").is_in(f.rules.iter().copied()));
    }
    query.limit(if f.limit > 0 { f.limit as u64 } else { 1000 });
    c.all(&query, ActionRow::from_row)
}

/// Removes a project or item row (back to “no setting”: a real-directory project counts as off
/// again), with its revision and `{type:"yolo", ws}`. False when there was none.
pub fn scope_clear(
    tx: &mut W<'_>,
    ws: &str,
    object_type: &str,
    id: &str,
    by: &str,
) -> Result<bool> {
    let Some(before) = scope_get(tx, ws, object_type, id)? else {
        return Ok(false);
    };

    tx.exec(
        Query::delete()
            .from_table("yolo_scopes")
            .cond_where(scope_key(ws, object_type, id)),
    )?;
    crate::contract::audit::revise(
        tx,
        ws,
        REVISION_TYPE,
        &format!("{object_type}:{id}"),
        before.revision + 1,
        &before.snapshot(),
        &Value::Null,
        by,
    )?;
    tx.emit(steve_types::DomainEvent::from(
        steve_types::AutonomyEvent::Yolo { ws: ws.into() },
    ));
    Ok(true)
}

/// Stamps the first undo and returns the action that changed.
pub fn stamp_undone(w: &mut W<'_>, id: &str, at: i64) -> Result<Option<ActionRow>> {
    let changed = w.exec(
        Query::update()
            .table("yolo_actions")
            .value("undone", at)
            .and_where(Expr::col("id").eq(id))
            .and_where(Expr::col("undone").is_null()),
    )?;
    if changed == 0 {
        return Ok(None);
    }
    let row = crate::repo::must(action_get(w, id)?)?;
    emit_action(w, &row.id, row.decision_id.as_deref(), &row.workspace_id);
    Ok(Some(row))
}

fn emit_action(w: &mut W<'_>, id: &str, decision_id: Option<&str>, ws: &str) {
    w.emit(steve_types::DomainEvent::from(
        steve_types::AutonomyEvent::YoloAction {
            id: id.into(),
            decision_id: decision_id.map(Into::into),
            ws: ws.into(),
        },
    ));
}

#[derive(Default)]
pub struct Undone {
    pub items: i64,
    pub same_rule_items: i64,
    pub recalls: i64,
}

pub fn undone_since(c: &mut impl Read, ws: &str, since: i64) -> Result<Undone> {
    let selection = Condition::all()
        .add(Expr::col("workspace_id").eq(ws))
        .add(Expr::col("undone").gte(since));
    let distinct_items = Expr::cust("count(DISTINCT coalesce(item_id, id))");
    let counts = Query::select()
        .expr_as(distinct_items.clone(), "n")
        .from("yolo_actions")
        .cond_where(selection.clone())
        .group_by_col("rule")
        .take();
    let most = c
        .one(
            Query::select()
                .expr(Func::max(Expr::col("n")))
                .from_subquery(counts, "counts"),
            |r| r.get::<Option<i64>>(0),
        )?
        .flatten()
        .unwrap_or_default();
    let kind = c.db().dialect().json_field(Expr::col("undo"), "kind");
    let (items, recalls) = c
        .one(
            Query::select()
                .expr(distinct_items)
                .expr(Expr::cust_with_expr(
                    "count(*) FILTER (WHERE ? = 'recall')",
                    kind,
                ))
                .from("yolo_actions")
                .cond_where(selection),
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?
        .unwrap_or_default();
    Ok(Undone {
        items,
        same_rule_items: most,
        recalls,
    })
}

pub fn scheduled_actions(
    c: &mut impl Read,
    claimed: &[String],
    through: Option<i64>,
    only: Option<&str>,
    limit: u64,
) -> Result<Vec<ActionRow>> {
    let dialect = c.db().dialect();
    let claimed = dialect.text_array(claimed)?;
    let mut query = action_query();
    query
        .and_where(Expr::col("outcome").eq("scheduled"))
        .and_where(Expr::col("due_at").is_not_null())
        .and_where(
            Expr::col("decision_id")
                .is_null()
                .or(Expr::col("decision_id").not_in_subquery(claimed)),
        )
        .order_by("due_at", Order::Asc)
        .order_by_expr(dialect.insertion_order("yolo_actions"), Order::Asc)
        .limit(limit);
    if let Some(through) = through {
        query.and_where(Expr::col("due_at").lte(through));
    }
    if let Some(id) = only {
        query.and_where(Expr::col("id").eq(id));
    }
    c.all(&query, ActionRow::from_row)
}
