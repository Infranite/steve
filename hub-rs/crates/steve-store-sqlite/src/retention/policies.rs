//! Transitional table declarations (G2-T8), to move into entity owners in wave 4.
use super::*;
use rusqlite::params;
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
};
use steve_storage_api::{Domain, retention::OpName, uow::AnyOp};

pub const DAY: u64 = 86_400_000;
pub const OUTBOX_KEEP_MS: i64 = 7 * DAY as i64;
pub const DIAGNOSTIC_RETENTION_DAYS: i64 = 7;
pub const DIAGNOSTIC_KEEP_MS: i64 = DIAGNOSTIC_RETENTION_DAYS * DAY as i64;
pub const ITEM_BYTES: i64 = 10 * 1024 * 1024;
pub const TOTAL_BYTES: i64 = 100 * 1024 * 1024;
/// Background convergence leaves 10 MiB for new facts before the hard total cap.
pub const BACKGROUND_TOTAL_BYTES: i64 = TOTAL_BYTES - ITEM_BYTES;
pub const DIAGNOSTIC_EVICT_ROWS: u32 = 200;
const KEPT_TURNS: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Outbox,
    Jobs,
    DiagnosticAge,
    DiagnosticCapacity,
    Vitals,
    Turns,
    Tools,
    Feed,
    Attention,
}
pub const ALL: [Kind; 9] = [
    Kind::Outbox,
    Kind::Jobs,
    Kind::DiagnosticAge,
    Kind::DiagnosticCapacity,
    Kind::Vitals,
    Kind::Turns,
    Kind::Tools,
    Kind::Feed,
    Kind::Attention,
];
impl Kind {
    /// The sole dispatch point. Selection, protection and mutation belong to table types.
    pub fn policy(self) -> Box<dyn Policy<Cursor = i64>> {
        match self {
            Self::Outbox => Box::new(Outbox::new()),
            Self::Jobs => Box::new(Jobs::new()),
            Self::DiagnosticAge => Box::new(Diagnostics::age()),
            Self::DiagnosticCapacity => Box::new(Diagnostics::capacity()),
            Self::Vitals => Box::new(Vitals::new()),
            Self::Turns => Box::new(Turns::new()),
            Self::Tools => Box::new(Tools::new()),
            Self::Feed => Box::new(Feed::new()),
            Self::Attention => Box::new(Attention::new()),
        }
    }
}
pub fn integer(row: &Row, field: &str) -> rusqlite::Result<i64> {
    match value(row, field) {
        Some(Cell::Int(n) | Cell::Millis(n)) => Ok(*n),
        _ => Err(invalid("missing integer retention field")),
    }
}
pub fn text<'a>(row: &'a Row, field: &str) -> rusqlite::Result<&'a str> {
    match value(row, field) {
        Some(Cell::Text(v)) => Ok(v),
        _ => Err(invalid("missing text retention field")),
    }
}
fn age_definition(entity: &str, keep: u64) -> RetentionPolicy {
    RetentionPolicy {
        entity: entity.into(),
        select: Select::MaxAge {
            field: "at".into(),
            keep_ms: keep,
        },
        keep_from: KeepFrom::Fixed { millis: keep },
        before: None,
        action: Action::Delete,
        protect: None,
        budget: DEFAULT_BUDGET,
    }
}
fn age_row(row: &rusqlite::Row<'_>, at: i64) -> rusqlite::Result<Row> {
    Ok(Row(vec![
        ("rowid".into(), Cell::Int(row.get(0)?)),
        ("at".into(), Cell::Millis(at)),
    ]))
}
/// All identifiers come from declarations below. Windows inspect primary keys, including
/// fresh/protected rows, so a head full of non-selected rows cannot starve the tail.
fn window(
    c: &Connection,
    table: &str,
    columns: &str,
    after: Option<&i64>,
    limit: u32,
    decode: impl Fn(&rusqlite::Row<'_>) -> rusqlite::Result<Row>,
) -> rusqlite::Result<Window<i64>> {
    let bound = if after.is_some() {
        "rowid > ?1"
    } else {
        "?1 IS NULL"
    };
    let sql = format!("SELECT rowid, {columns} FROM {table} WHERE {bound} ORDER BY rowid LIMIT ?2");
    let rows = c
        .prepare(&sql)?
        .query_map(params![after, limit], |r| Ok((decode(r)?, r.get(0)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let more = rows
        .last()
        .map(|(_, last)| {
            c.query_row(
                &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE rowid > ?1)"),
                [last],
                |r| r.get(0),
            )
        })
        .transpose()?
        .unwrap_or(false);
    Ok(Window { rows, more })
}
fn delete(c: &Connection, table: &str, row: &Row) -> rusqlite::Result<u64> {
    Ok(c.execute(
        &format!("DELETE FROM {table} WHERE rowid=?"),
        [integer(row, "rowid")?],
    )? as u64)
}

pub struct Outbox {
    pub definition: RetentionPolicy,
}
impl Outbox {
    pub fn new() -> Self {
        let mut definition = age_definition("outbox_events", OUTBOX_KEEP_MS as u64);
        definition.protect = Some(OpName("outbox_in_use".into()));
        Self { definition }
    }
}
impl Policy for Outbox {
    type Cursor = i64;
    fn definition(&self) -> &RetentionPolicy {
        &self.definition
    }
    fn window(
        &self,
        c: &Connection,
        after: Option<&i64>,
        limit: u32,
    ) -> rusqlite::Result<Window<i64>> {
        window(c, "outbox_events", "created, topic", after, limit, |r| {
            let mut row = age_row(r, r.get(1)?)?;
            row.0.push(("topic".into(), Cell::Text(r.get(2)?)));
            Ok(row)
        })
    }
    fn protect(&self, c: &Connection, row: &Row) -> rusqlite::Result<bool> {
        const IN_USE: &str = "SELECT
            EXISTS(SELECT 1 FROM outbox_leases WHERE event_seq=?1)
            OR EXISTS(SELECT 1 FROM outbox_consumers k
                WHERE k.topic=?2 AND ?1>k.since_seq AND NOT EXISTS(
                    SELECT 1 FROM outbox_receipts r WHERE r.event_seq=?1 AND r.consumer_id=k.id))";
        c.query_row(
            IN_USE,
            params![integer(row, "rowid")?, text(row, "topic")?],
            |r| r.get(0),
        )
    }
    fn apply(&self, c: &Connection, row: &Row) -> rusqlite::Result<u64> {
        c.execute(
            "DELETE FROM outbox_receipts WHERE event_seq=?",
            [integer(row, "rowid")?],
        )?;
        delete(c, "outbox_events", row)
    }
}

pub struct Jobs {
    pub definition: RetentionPolicy,
}
impl Jobs {
    pub fn new() -> Self {
        let mut definition = age_definition("runtime_jobs", 7 * DAY);
        definition.select = Select::Terminal {
            state: "state".into(),
            done: vec![Cell::Text("finished".into())],
            since: "at".into(),
            keep_ms: 7 * DAY,
        };
        definition.protect = Some(OpName("job_in_use".into()));
        Self { definition }
    }
}
impl Policy for Jobs {
    type Cursor = i64;
    fn definition(&self) -> &RetentionPolicy {
        &self.definition
    }
    fn window(
        &self,
        c: &Connection,
        after: Option<&i64>,
        limit: u32,
    ) -> rusqlite::Result<Window<i64>> {
        window(
            c,
            "runtime_jobs",
            "updated, state, job_id",
            after,
            limit,
            |r| {
                let mut row = age_row(r, r.get(1)?)?;
                row.0.extend([
                    ("state".into(), Cell::Text(r.get(2)?)),
                    ("job_id".into(), Cell::Text(r.get(3)?)),
                ]);
                Ok(row)
            },
        )
    }
    fn protect(&self, c: &Connection, row: &Row) -> rusqlite::Result<bool> {
        const IN_USE: &str = "SELECT
            EXISTS(SELECT 1 FROM runtime_job_attempts WHERE job_id=?1 AND state<>'finished')
            OR EXISTS(SELECT 1 FROM node_commands c
                JOIN runtime_job_attempts a ON c.target_job_attempt=a.attempt_id
                WHERE a.job_id=?1 AND c.state NOT IN ('finished','rejected'))
            OR EXISTS(SELECT 1 FROM node_commands d
                JOIN node_commands c ON d.dependency_operation=c.operation_id
                JOIN runtime_job_attempts a ON c.target_job_attempt=a.attempt_id
                WHERE a.job_id=?1 AND (d.target_job_attempt IS NULL OR d.target_job_attempt NOT IN (
                    SELECT attempt_id FROM runtime_job_attempts WHERE job_id=?1)))";
        c.query_row(IN_USE, [text(row, "job_id")?], |r| r.get(0))
    }
    fn apply(&self, c: &Connection, row: &Row) -> rusqlite::Result<u64> {
        let job = text(row, "job_id")?;
        c.execute(
            "DELETE FROM node_commands WHERE target_job_attempt IN (
            SELECT attempt_id FROM runtime_job_attempts WHERE job_id=?)",
            [job],
        )?;
        c.execute("DELETE FROM runtime_job_attempts WHERE job_id=?", [job])?;
        delete(c, "runtime_jobs", row)
    }
}

struct ItemReserve<'a> {
    workspace: &'a str,
    item: &'a str,
    incoming: i64,
}
pub struct Diagnostics<'a> {
    definition: RetentionPolicy,
    reserve: Option<ItemReserve<'a>>,
    // Reinitialized by each window inside its read/write snapshot, then decremented by deletes.
    bytes: std::cell::Cell<Option<i64>>,
}
impl Diagnostics<'_> {
    pub fn age() -> Self {
        Self {
            definition: age_definition("diagnostic_events", DIAGNOSTIC_KEEP_MS as u64),
            reserve: None,
            bytes: std::cell::Cell::new(None),
        }
    }
    pub fn capacity() -> Self {
        let mut policy = Self::age();
        policy.definition.select = Select::Bytes {
            group: vec![],
            max: BACKGROUND_TOTAL_BYTES as u64,
        };
        policy
    }
    /// Foreground eviction targets this workspace/item's oldest rows using diagnostic_events_item.
    /// It never spends the small writer allowance walking unrelated records.
    pub fn for_insert<'a>(workspace: &'a str, item: &'a str, incoming: i64) -> Diagnostics<'a> {
        let mut policy = Diagnostics::capacity();
        policy.definition.select = Select::Bytes {
            group: vec!["item_id".into()],
            max: ITEM_BYTES as u64,
        };
        policy.definition.budget.rows = DIAGNOSTIC_EVICT_ROWS;
        policy.reserve = Some(ItemReserve {
            workspace,
            item,
            incoming,
        });
        policy
    }
    fn pressured(&self, bytes: i64) -> bool {
        match &self.reserve {
            Some(scope) => bytes.saturating_add(scope.incoming) > ITEM_BYTES,
            None => bytes > BACKGROUND_TOTAL_BYTES,
        }
    }
}
fn diagnostic_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Row> {
    let mut row = age_row(r, r.get(1)?)?;
    row.0.extend([
        ("item_id".into(), Cell::Text(r.get(2)?)),
        ("bytes".into(), Cell::Int(r.get(3)?)),
    ]);
    Ok(row)
}
impl Policy for Diagnostics<'_> {
    type Cursor = i64;
    fn definition(&self) -> &RetentionPolicy {
        &self.definition
    }
    fn window(
        &self,
        c: &Connection,
        after: Option<&i64>,
        limit: u32,
    ) -> rusqlite::Result<Window<i64>> {
        self.bytes.set(None);
        if matches!(self.definition.select, Select::Bytes { .. }) {
            let bytes = match &self.reserve {
                Some(scope) => c.query_row(
                    "SELECT bytes FROM diagnostic_item_stats WHERE item_id=?",
                    [scope.item],
                    |r| r.get(0),
                )?,
                None => c.query_row(
                    "SELECT coalesce(sum(bytes),0) FROM diagnostic_item_stats",
                    [],
                    |r| r.get(0),
                )?,
            };
            self.bytes.set(Some(bytes));
            if !self.pressured(bytes) {
                return Ok(Window {
                    rows: vec![],
                    more: false,
                });
            }
        }
        let Some(scope) = &self.reserve else {
            return window(
                c,
                "diagnostic_events",
                "at, item_id, bytes",
                after,
                limit,
                diagnostic_row,
            );
        };
        let bound = if after.is_some() {
            "seq>?3"
        } else {
            "?3 IS NULL"
        };
        let sql = format!(
            "SELECT seq,at,item_id,bytes FROM diagnostic_events
            WHERE workspace_id=?1 AND item_id=?2 AND {bound} ORDER BY seq LIMIT ?4"
        );
        let rows = c
            .prepare(&sql)?
            .query_map(params![scope.workspace, scope.item, after, limit], |r| {
                Ok((diagnostic_row(r)?, r.get(0)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let more = rows
            .last()
            .map(|(_, last)| {
                c.query_row(
                    "SELECT EXISTS(SELECT 1 FROM diagnostic_events
            WHERE workspace_id=?1 AND item_id=?2 AND seq>?3)",
                    params![scope.workspace, scope.item, last],
                    |r| r.get(0),
                )
            })
            .transpose()?
            .unwrap_or(false);
        Ok(Window { rows, more })
    }
    fn overflow(&self, _: &Connection, _: &Row) -> rusqlite::Result<bool> {
        let bytes = self
            .bytes
            .get()
            .ok_or_else(|| invalid("capacity window must initialize bytes"))?;
        Ok(self.pressured(bytes))
    }
    fn apply(&self, c: &Connection, row: &Row) -> rusqlite::Result<u64> {
        c.execute(
            "UPDATE diagnostic_item_stats
            SET bytes=max(0,bytes-?1),removed_through=max(removed_through,?2)
            WHERE item_id=?3",
            params![
                integer(row, "bytes")?,
                integer(row, "rowid")?,
                text(row, "item_id")?
            ],
        )?;
        let affected = delete(c, "diagnostic_events", row)?;
        if affected > 0
            && let Some(bytes) = self.bytes.get()
        {
            self.bytes
                .set(Some(bytes.saturating_sub(integer(row, "bytes")?)));
        }
        Ok(affected)
    }
}

pub struct Vitals {
    definition: RetentionPolicy,
}
impl Vitals {
    pub fn new() -> Self {
        Self {
            definition: age_definition("node_vitals", DAY),
        }
    }
}
impl Policy for Vitals {
    type Cursor = i64;
    fn definition(&self) -> &RetentionPolicy {
        &self.definition
    }
    fn window(&self, c: &Connection, a: Option<&i64>, n: u32) -> rusqlite::Result<Window<i64>> {
        window(c, "node_vitals", "at", a, n, |r| age_row(r, r.get(1)?))
    }
    fn apply(&self, c: &Connection, r: &Row) -> rusqlite::Result<u64> {
        delete(c, "node_vitals", r)
    }
}

pub struct Turns {
    definition: RetentionPolicy,
    kept: RefCell<BTreeMap<String, BTreeSet<i64>>>,
}
impl Turns {
    pub fn new() -> Self {
        let mut definition = age_definition("session_turns", 0);
        definition.select = Select::MaxRows {
            group: vec!["session_id".into()],
            keep: KEPT_TURNS as u64,
            order: vec![("updated".into(), steve_storage_api::query::Order::Desc)],
        };
        Self {
            definition,
            kept: RefCell::default(),
        }
    }
}
impl Policy for Turns {
    type Cursor = i64;
    fn definition(&self) -> &RetentionPolicy {
        &self.definition
    }
    fn window(&self, c: &Connection, a: Option<&i64>, n: u32) -> rusqlite::Result<Window<i64>> {
        self.kept.borrow_mut().clear();
        window(c, "session_turns", "updated, session_id", a, n, |r| {
            let mut row = age_row(r, r.get(1)?)?;
            row.0.push(("session_id".into(), Cell::Text(r.get(2)?)));
            Ok(row)
        })
    }
    fn overflow(&self, c: &Connection, row: &Row) -> rusqlite::Result<bool> {
        let session = text(row, "session_id")?.to_owned();
        if !self.kept.borrow().contains_key(&session) {
            let mut st = c.prepare(
                "SELECT rowid FROM session_turns WHERE session_id=?
                ORDER BY updated DESC,rowid DESC LIMIT ?",
            )?;
            let keep = st
                .query_map(params![session, KEPT_TURNS as i64], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            self.kept.borrow_mut().insert(session.clone(), keep);
        }
        Ok(!self.kept.borrow()[&session].contains(&integer(row, "rowid")?))
    }
    fn apply(&self, c: &Connection, r: &Row) -> rusqlite::Result<u64> {
        delete(c, "session_turns", r)
    }
}

pub struct Tools {
    definition: RetentionPolicy,
}
impl Tools {
    pub fn new() -> Self {
        Self {
            definition: age_definition("tool_usage", 90 * DAY),
        }
    }
}
impl Policy for Tools {
    type Cursor = i64;
    fn definition(&self) -> &RetentionPolicy {
        &self.definition
    }
    fn window(&self, c: &Connection, a: Option<&i64>, n: u32) -> rusqlite::Result<Window<i64>> {
        window(c, "tool_usage", "day", a, n, |r| {
            let day: i64 = r.get(1)?;
            age_row(
                r,
                day.saturating_add(1)
                    .saturating_mul(DAY as i64)
                    .saturating_sub(1),
            )
        })
    }
    fn apply(&self, c: &Connection, r: &Row) -> rusqlite::Result<u64> {
        delete(c, "tool_usage", r)
    }
}

pub struct Feed {
    definition: RetentionPolicy,
}
impl Feed {
    pub fn new() -> Self {
        Self {
            definition: age_definition("feed_windows", 0),
        }
    }
}
impl Policy for Feed {
    type Cursor = i64;
    fn definition(&self) -> &RetentionPolicy {
        &self.definition
    }
    fn window(&self, c: &Connection, a: Option<&i64>, n: u32) -> rusqlite::Result<Window<i64>> {
        window(c, "feed_windows", "expires", a, n, |r| {
            let expires: i64 = r.get(1)?;
            age_row(r, expires.saturating_sub(1))
        })
    }
    fn apply(&self, c: &Connection, r: &Row) -> rusqlite::Result<u64> {
        delete(c, "feed_windows", r)
    }
}

pub struct Attention {
    definition: RetentionPolicy,
}
impl Attention {
    pub fn new() -> Self {
        Self {
            definition: age_definition("attention_windows", 0),
        }
    }
}
impl Policy for Attention {
    type Cursor = i64;
    fn definition(&self) -> &RetentionPolicy {
        &self.definition
    }
    fn window(&self, c: &Connection, a: Option<&i64>, n: u32) -> rusqlite::Result<Window<i64>> {
        window(c, "attention_windows", "expires", a, n, |r| {
            let expires: i64 = r.get(1)?;
            age_row(r, expires.saturating_sub(1))
        })
    }
    fn apply(&self, c: &Connection, r: &Row) -> rusqlite::Result<u64> {
        delete(c, "attention_windows", r)
    }
}

pub type DomainAction<'a> = dyn Fn(&Connection, &Row) -> rusqlite::Result<u64> + 'a;
/// Only Core calls this bounded, pre-authorized compaction. No keepSess resolution or
/// automatic scheduling here: Core owns those decisions and holds its busy guard.
pub struct SessionEvents<'a> {
    definition: RetentionPolicy,
    session: &'a str,
    upper: i64,
    action: &'a DomainAction<'a>,
}
impl<'a> SessionEvents<'a> {
    pub fn new(session: &'a str, upper: i64, action: &'a DomainAction<'a>) -> Self {
        let mut definition = age_definition("events", 0);
        definition.select = Select::MaxRows {
            group: vec![],
            keep: 0,
            order: vec![],
        };
        definition.before = Some(OpName("compact_session_event".into()));
        definition.action = Action::Op(AnyOp {
            name: "compact_session_event".into(),
            domain: Domain::Stream,
            body: "{}".parse().expect("static JSON"),
        });
        Self {
            definition,
            session,
            upper,
            action,
        }
    }
}
impl Policy for SessionEvents<'_> {
    type Cursor = i64;
    fn definition(&self) -> &RetentionPolicy {
        &self.definition
    }
    fn window(
        &self,
        c: &Connection,
        after: Option<&i64>,
        limit: u32,
    ) -> rusqlite::Result<Window<i64>> {
        let bound = if after.is_some() {
            "seq>?2"
        } else {
            "?2 IS NULL"
        };
        let sql = format!(
            "SELECT seq,ts,session_id,kind,data FROM events
            WHERE session_id=?1 AND {bound} AND seq<=?3 ORDER BY seq LIMIT ?4"
        );
        let rows = c
            .prepare(&sql)?
            .query_map(params![self.session, after, self.upper, limit], |r| {
                let mut row = age_row(r, r.get(1)?)?;
                for (i, field) in [(2, "session_id"), (3, "kind"), (4, "data")] {
                    row.0.push((field.into(), Cell::Text(r.get(i)?)));
                }
                Ok((row, r.get(0)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let more = rows
            .last()
            .map(|(_, last)| {
                c.query_row(
                    "SELECT EXISTS(SELECT 1 FROM events
            WHERE session_id=?1 AND seq>?2 AND seq<=?3)",
                    params![self.session, last, self.upper],
                    |r| r.get(0),
                )
            })
            .transpose()?
            .unwrap_or(false);
        Ok(Window { rows, more })
    }
    fn primary_key(&self, row: &Row) -> rusqlite::Result<String> {
        Ok(integer(row, "rowid")?.to_string())
    }
    fn overflow(&self, _: &Connection, _: &Row) -> rusqlite::Result<bool> {
        Ok(true)
    }
    fn apply(&self, c: &Connection, row: &Row) -> rusqlite::Result<u64> {
        (self.action)(c, row)
    }
}

macro_rules! defaults {
    ($($type:ty),+ $(,)?) => {$ (
        impl Default for $type { fn default() -> Self { Self::new() } }
    )+};
}
defaults!(Outbox, Jobs, Vitals, Turns, Tools, Feed, Attention);
