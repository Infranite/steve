//! Transitional table declarations. Entity owners take these over with their domain adapters.
use super::*;
use rusqlite::{OptionalExtension, params};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
};
use steve_storage_api::{
    Domain,
    retention::{OpName, SettingScope},
    uow::AnyOp,
};
pub const DAY: u64 = 86_400_000;
pub const ITEM_BYTES: i64 = 10 * 1024 * 1024;
pub const TOTAL_BYTES: i64 = 100 * 1024 * 1024;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Outbox,
    Jobs,
    DiagnosticAge,
    DiagnosticCapacity,
    Vitals,
    Turns,
    Payloads,
    Tools,
    Feed,
    Attention,
}
pub const ALL: [Kind; 10] = [
    Kind::Outbox,
    Kind::Jobs,
    Kind::DiagnosticAge,
    Kind::DiagnosticCapacity,
    Kind::Vitals,
    Kind::Turns,
    Kind::Payloads,
    Kind::Tools,
    Kind::Feed,
    Kind::Attention,
];
pub type DomainAction<'a> = dyn Fn(&Connection, &Row) -> rusqlite::Result<u64> + 'a;
pub struct TablePolicy<'a> {
    pub kind: Kind,
    pub definition: RetentionPolicy,
    pub scope: Option<String>,
    pub upper: Option<i64>,
    pub reserve: Option<(String, i64)>,
    pub domain_action: Option<&'a DomainAction<'a>>,
    pub manual: bool,
    turns: RefCell<BTreeMap<String, BTreeSet<i64>>>,
}
pub fn integer(row: &Row, f: &str) -> rusqlite::Result<i64> {
    match value(row, f) {
        Some(Cell::Int(n) | Cell::Millis(n)) => Ok(*n),
        _ => Err(invalid("missing integer retention field")),
    }
}
pub fn text<'a>(row: &'a Row, f: &str) -> rusqlite::Result<&'a str> {
    match value(row, f) {
        Some(Cell::Text(v)) => Ok(v),
        _ => Err(invalid("missing text retention field")),
    }
}
impl TablePolicy<'_> {
    pub fn new(kind: Kind) -> Self {
        let (entity, field, keep) = match kind {
            Kind::Outbox => ("outbox_events", "at", 7 * DAY),
            Kind::Jobs => ("runtime_jobs", "at", 7 * DAY),
            Kind::DiagnosticAge | Kind::DiagnosticCapacity => ("diagnostic_events", "at", 7 * DAY),
            Kind::Vitals => ("node_vitals", "at", DAY),
            Kind::Turns => ("session_turns", "at", 0),
            Kind::Payloads => ("events", "item_updated", u64::MAX),
            Kind::Tools => ("tool_usage", "at", 90 * DAY),
            Kind::Feed => ("feed_windows", "at", 0),
            Kind::Attention => ("attention_windows", "at", 0),
        };
        let mut definition = RetentionPolicy {
            entity: entity.into(),
            select: Select::MaxAge {
                field: field.into(),
                keep_ms: keep,
            },
            keep_from: KeepFrom::Fixed { millis: keep },
            before: None,
            action: Action::Delete,
            protect: None,
            budget: Budget {
                rows: 500,
                target_ms: 20,
            },
        };
        match kind {
            Kind::Jobs => {
                definition.select = Select::Terminal {
                    state: "state".into(),
                    done: vec![Cell::Text("finished".into())],
                    since: "at".into(),
                    keep_ms: keep,
                }
            }
            Kind::Turns => {
                definition.select = Select::MaxRows {
                    group: vec!["session_id".into()],
                    keep: 64,
                    order: vec![("updated".into(), steve_storage_api::query::Order::Desc)],
                }
            }
            Kind::DiagnosticCapacity => {
                // The grouped cap is declarative; overflow also enforces the store-wide cap.
                definition.select = Select::Bytes {
                    group: vec!["item_id".into()],
                    max: ITEM_BYTES as u64,
                }
            }
            Kind::Payloads => {
                definition.select = Select::Terminal {
                    state: "state".into(),
                    done: vec![Cell::Text("closed".into())],
                    since: field.into(),
                    keep_ms: keep,
                };
                definition.keep_from = KeepFrom::Setting {
                    key: "keepSess".into(),
                    per: SettingScope::Workspace,
                    default_ms: u64::MAX,
                };
                definition.budget.rows = 32;
                definition.before = Some(OpName("compact_session_event".into()));
                definition.action = Action::Op(AnyOp {
                    name: "compact_session_event".into(),
                    domain: Domain::Stream,
                    body: "{}".parse().expect("static JSON"),
                });
            }
            _ => {}
        }
        if matches!(kind, Kind::Outbox | Kind::Jobs | Kind::Payloads) {
            definition.protect = Some(OpName("in_use".into()));
        }
        Self {
            kind,
            definition,
            scope: None,
            upper: None,
            reserve: None,
            domain_action: None,
            manual: false,
            turns: RefCell::default(),
        }
    }
    fn source(&self) -> &'static str {
        match self.kind {
            Kind::Outbox => "outbox_events e",
            Kind::Jobs => "runtime_jobs e",
            Kind::DiagnosticAge | Kind::DiagnosticCapacity => "diagnostic_events e",
            Kind::Vitals => "node_vitals e",
            Kind::Turns => "session_turns e",
            Kind::Tools => "tool_usage e",
            Kind::Feed => "feed_windows e",
            Kind::Attention => "attention_windows e",
            Kind::Payloads => {
                "events e LEFT JOIN sessions s ON s.id=e.session_id LEFT JOIN items i ON i.id=s.item_id LEFT JOIN session_compactions z ON z.session_id=s.id"
            }
        }
    }
}
impl Policy for TablePolicy<'_> {
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
        self.turns.borrow_mut().clear();
        let columns = match self.kind {
            Kind::Outbox => "e.created,e.topic",
            Kind::Jobs => "e.updated,e.state,e.job_id",
            Kind::DiagnosticAge | Kind::DiagnosticCapacity => "e.at,e.item_id,e.bytes",
            Kind::Vitals => "e.at",
            Kind::Turns => "e.updated,e.session_id",
            Kind::Tools => "e.day",
            Kind::Feed | Kind::Attention => "e.expires",
            Kind::Payloads => {
                "e.ts,e.session_id,e.kind,e.data,coalesce(i.workspace_id,''),coalesce(i.updated,0),CASE WHEN i.archived<>0 AND i.phase='closed' THEN 'closed' ELSE 'protected' END,coalesce(s.status,''),coalesce(z.upto,0)"
            }
        };
        let filter = if self.kind == Kind::Payloads {
            format!(
                " AND {} AND {}",
                if self.scope.is_some() {
                    "e.session_id=?3"
                } else {
                    "?3 IS NULL"
                },
                if self.upper.is_some() {
                    "e.seq<=?4"
                } else {
                    "?4 IS NULL"
                }
            )
        } else {
            " AND (?3 IS NULL) AND (?4 IS NULL)".into()
        };
        let bound = if after.is_some() {
            "e.rowid>?1"
        } else {
            "?1 IS NULL"
        };
        let sql = format!(
            "SELECT e.rowid,{columns} FROM {} WHERE {bound}{filter} ORDER BY e.rowid LIMIT ?2",
            self.source()
        );
        let mut st = c.prepare(&sql)?;
        let rows = st
            .query_map(params![after, limit, self.scope, self.upper], |r| {
                let id: i64 = r.get(0)?;
                let mut row = vec![("rowid".into(), Cell::Int(id))];
                let at: i64 = r.get(1)?;
                row.push((
                    "at".into(),
                    Cell::Millis(if self.kind == Kind::Tools {
                        at.saturating_add(1)
                            .saturating_mul(DAY as i64)
                            .saturating_sub(1)
                    } else if matches!(self.kind, Kind::Feed | Kind::Attention) {
                        at.saturating_sub(1)
                    } else {
                        at
                    }),
                ));
                match self.kind {
                    Kind::Outbox => row.push(("topic".into(), Cell::Text(r.get(2)?))),
                    Kind::Jobs => {
                        row.push(("state".into(), Cell::Text(r.get(2)?)));
                        row.push(("job_id".into(), Cell::Text(r.get(3)?)));
                    }
                    Kind::DiagnosticAge | Kind::DiagnosticCapacity => {
                        row.push(("item_id".into(), Cell::Text(r.get(2)?)));
                        row.push(("bytes".into(), Cell::Int(r.get(3)?)));
                    }
                    Kind::Turns => row.push(("session_id".into(), Cell::Text(r.get(2)?))),
                    Kind::Payloads => {
                        for (i, name) in [
                            (2, "session_id"),
                            (3, "kind"),
                            (4, "data"),
                            (5, "workspace_id"),
                            (7, "state"),
                            (8, "status"),
                        ] {
                            row.push((name.into(), Cell::Text(r.get(i)?)));
                        }
                        row.push(("item_updated".into(), Cell::Millis(r.get(6)?)));
                        row.push(("compacted".into(), Cell::Int(r.get(9)?)));
                    }
                    _ => {}
                }
                Ok((Row(row), id))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let more = if let Some((_, last)) = rows.last() {
            c.query_row(
                &format!(
                    "SELECT EXISTS(SELECT 1 FROM {} WHERE e.rowid>?1{filter})",
                    self.source()
                ),
                params![last, limit, self.scope, self.upper],
                |r| r.get(0),
            )?
        } else {
            false
        };
        Ok(Window { rows, more })
    }
    fn keep_ms(&self, c: &Connection, row: &Row) -> rusqlite::Result<Option<u64>> {
        if let KeepFrom::Fixed { millis } = self.definition.keep_from {
            return Ok(Some(millis));
        }
        let ws = text(row, "workspace_id")?;
        let setting: Option<String> = c
            .query_row(
                "SELECT value FROM settings WHERE workspace_id=? AND key='keepSess'",
                [ws],
                |r| r.get(0),
            )
            .optional()?;
        let setting = setting.map(|v| serde_json::from_str::<String>(&v).unwrap_or(v));
        Ok(match setting.as_deref().map(str::trim) {
            Some("90 天") => Some(90 * DAY),
            Some("1 年") => Some(365 * DAY),
            _ => None,
        })
    }
    fn overflow(&self, c: &Connection, row: &Row) -> rusqlite::Result<bool> {
        if self.kind == Kind::Payloads && self.manual {
            return Ok(true);
        }
        if self.kind == Kind::Turns {
            let session = text(row, "session_id")?.to_owned();
            if !self.turns.borrow().contains_key(&session) {
                let mut st=c.prepare("SELECT rowid FROM session_turns WHERE session_id=? ORDER BY updated DESC,rowid DESC LIMIT 64")?;
                let keep = st
                    .query_map([&session], |r| r.get(0))?
                    .collect::<rusqlite::Result<_>>()?;
                self.turns.borrow_mut().insert(session.clone(), keep);
            }
            return Ok(!self.turns.borrow()[&session].contains(&integer(row, "rowid")?));
        }
        let item = text(row, "item_id")?;
        let bytes: i64 = c.query_row(
            "SELECT bytes FROM diagnostic_item_stats WHERE item_id=?",
            [item],
            |r| r.get(0),
        )?;
        let total: i64 = c.query_row(
            "SELECT coalesce(sum(bytes),0) FROM diagnostic_item_stats",
            [],
            |r| r.get(0),
        )?;
        let add = self.reserve.as_ref().map_or(0, |(_, bytes)| *bytes);
        let item_add = self
            .reserve
            .as_ref()
            .filter(|(id, _)| id == item)
            .map_or(0, |(_, bytes)| *bytes);
        Ok(bytes.saturating_add(item_add) > ITEM_BYTES || total.saturating_add(add) > TOTAL_BYTES)
    }
    fn protect(&self, c: &Connection, row: &Row) -> rusqlite::Result<bool> {
        match self.kind{
            Kind::Outbox=>c.query_row("SELECT EXISTS(SELECT 1 FROM outbox_leases WHERE event_seq=?1) OR EXISTS(SELECT 1 FROM outbox_consumers k WHERE k.topic=?2 AND ?1>k.since_seq AND NOT EXISTS(SELECT 1 FROM outbox_receipts r WHERE r.event_seq=?1 AND r.consumer_id=k.id))",params![integer(row,"rowid")?,text(row,"topic")?],|r|r.get(0)),
            Kind::Jobs=>c.query_row("SELECT EXISTS(SELECT 1 FROM runtime_job_attempts WHERE job_id=?1 AND state<>'finished') OR EXISTS(SELECT 1 FROM node_commands c JOIN runtime_job_attempts a ON c.target_job_attempt=a.attempt_id WHERE a.job_id=?1 AND c.state NOT IN ('finished','rejected')) OR EXISTS(SELECT 1 FROM node_commands d JOIN node_commands c ON d.dependency_operation=c.operation_id JOIN runtime_job_attempts a ON c.target_job_attempt=a.attempt_id WHERE a.job_id=?1 AND (d.target_job_attempt IS NULL OR d.target_job_attempt NOT IN (SELECT attempt_id FROM runtime_job_attempts WHERE job_id=?1)))",[text(row,"job_id")?],|r|r.get(0)),
            Kind::Payloads if !self.manual=>{
                if integer(row,"rowid")?<=integer(row,"compacted")?||text(row,"status")?!="ended"{return Ok(true);}
                c.query_row("SELECT EXISTS(SELECT 1 FROM runtime_turns WHERE session_id=? AND state<>'finished')",[text(row,"session_id")?],|r|r.get(0))
            },_=>Ok(false)
        }
    }
    fn apply(&self, c: &Connection, row: &Row) -> rusqlite::Result<u64> {
        let id = integer(row, "rowid")?;
        match self.kind {
            Kind::Jobs => {
                let job = text(row, "job_id")?;
                c.execute("DELETE FROM node_commands WHERE target_job_attempt IN (SELECT attempt_id FROM runtime_job_attempts WHERE job_id=?)",[job])?;
                c.execute("DELETE FROM runtime_job_attempts WHERE job_id=?", [job])?;
            }
            Kind::Outbox => {
                c.execute("DELETE FROM outbox_receipts WHERE event_seq=?", [id])?;
            }
            Kind::DiagnosticAge | Kind::DiagnosticCapacity => {
                c.execute("UPDATE diagnostic_item_stats SET bytes=max(0,bytes-?1),removed_through=max(removed_through,?2) WHERE item_id=?3",params![integer(row,"bytes")?,id,text(row,"item_id")?])?;
            }
            Kind::Payloads => {
                return self
                    .domain_action
                    .ok_or_else(|| invalid("session compaction handler required"))?(
                    c, row
                );
            }
            _ => {}
        }
        Ok(c.execute(
            &format!(
                "DELETE FROM {} WHERE rowid=?",
                self.source().split_ascii_whitespace().next().unwrap()
            ),
            [id],
        )? as u64)
    }
}
