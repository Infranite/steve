//! The Node's execution journal (docs/node-protocol.md §4): what it accepted, what it reported and
//! where its streams stand, on disk. No business truth: the Hub keeps items, queues and decisions.
//!
//! Every change is one SQLite transaction ([`Journal::tx`]): a command with its state and the events
//! it causes, an attempt's end with its closing events and the session it frees. Numbering an event
//! is part of the transaction that records it. Reads that must agree (a snapshot and its stream
//! heads) run in one read transaction.
//!
//! On every start the Node is a new incarnation. What was accepted and had not finished is then
//! `unknown`: the process that ran it is gone with the old incarnation (or still runs, which the Node
//! checks before letting anything go), what it did up to there is in its events, and only the Hub
//! decides what happens next. An empty memory never means "never ran".

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Serialize, de::DeserializeOwned};
use steve_node_api::*;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS ops (
  id TEXT PRIMARY KEY, method TEXT NOT NULL, digest TEXT NOT NULL, receipt TEXT NOT NULL,
  execution TEXT, workspace TEXT, session TEXT, cancel INTEGER NOT NULL DEFAULT 0,
  accepted INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS ops_session ON ops(session);
CREATE TABLE IF NOT EXISTS attempts (attempt TEXT PRIMARY KEY, execution TEXT NOT NULL, op TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS streams (
  stream TEXT PRIMARY KEY, epoch TEXT NOT NULL, produced INTEGER NOT NULL, acknowledged INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS events (
  stream TEXT NOT NULL, seq INTEGER NOT NULL, event TEXT NOT NULL, PRIMARY KEY (stream, seq));
CREATE TABLE IF NOT EXISTS workspaces (
  id TEXT PRIMARY KEY, resource TEXT NOT NULL, spec TEXT NOT NULL, kind TEXT NOT NULL,
  view TEXT NOT NULL, path TEXT NOT NULL, repo TEXT, incarnation INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS sessions (
  id TEXT PRIMARY KEY, open TEXT NOT NULL, view TEXT NOT NULL, incarnation INTEGER NOT NULL,
  acp_id TEXT);
CREATE TABLE IF NOT EXISTS waits (
  id TEXT PRIMARY KEY, op TEXT NOT NULL, session TEXT NOT NULL, request TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS procs (
  session TEXT PRIMARY KEY, pgid INTEGER NOT NULL, generation INTEGER NOT NULL,
  incarnation INTEGER NOT NULL, meter TEXT NOT NULL, reports INTEGER NOT NULL DEFAULT 0,
  started INTEGER);
CREATE TABLE IF NOT EXISTS reservations (key TEXT PRIMARY KEY, op TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS answers (
  op TEXT PRIMARY KEY, permission TEXT NOT NULL, resolution TEXT NOT NULL);
";

pub(crate) struct Journal {
    db: Mutex<Connection>,
}

/// One transaction on the journal.
pub(crate) struct Tx<'a>(Transaction<'a>);

#[derive(Clone)]
pub(crate) struct Op {
    pub method: String,
    pub digest: Digest,
    pub receipt: Receipt,
    pub execution: Option<ExecutionRef>,
    pub workspace: Option<WorkspaceRef>,
    pub cancel: bool,
}

pub(crate) struct WorkspaceRow {
    pub resource: String,
    pub spec: Digest,
    pub kind: PrepareKind,
    /// The repository of a worktree.
    pub repo: Option<String>,
    pub view: WorkspaceView,
    pub path: String,
    pub incarnation: u64,
}

#[derive(Clone)]
pub(crate) struct SessionRow {
    pub open: OpenSession,
    pub view: SessionView,
    pub incarnation: u64,
    pub acp_id: Option<String>,
}

/// A harness process group this Node started (to check it is gone before anything is freed).
pub(crate) struct Proc {
    pub session: SessionId,
    pub pgid: i64,
    pub incarnation: u64,
    /// The leader's start time when it was started (to tell a reused pid).
    pub started: Option<u64>,
}

fn json<T: Serialize>(v: &T) -> String {
    serde_json::to_string(v).expect("the contract's types serialize")
}

fn from<T: DeserializeOwned>(s: &str) -> rusqlite::Result<T> {
    serde_json::from_str(s).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })
}

fn conv<T>(r: Result<T, BadId>) -> rusqlite::Result<T> {
    r.map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })
}

pub(crate) fn internal(e: impl std::fmt::Display) -> ApiError {
    ApiError::new(ErrorCode::Internal, format!("journal: {e}"))
}

impl Journal {
    /// Opens (or creates) the journal and begins a new incarnation: what was live is now unknown,
    /// sessions are released (their processes belong to the old incarnation).
    pub fn open(path: &Path) -> ApiResult<(Journal, u64)> {
        let c = Connection::open(path).map_err(internal)?;
        c.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA busy_timeout=5000;",
        )
        .map_err(internal)?;
        let version: i64 = c
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .map_err(internal)?;
        if version > 1 {
            return Err(ApiError::new(
                ErrorCode::Unsupported,
                "journal needs a newer Node",
            ));
        }
        let j = Journal { db: Mutex::new(c) };
        let incarnation =
            j.tx(|t| {
                // Schema and recovery are one commit. A failed migration must not consume an
                // incarnation or change any receipt. Version zero includes the original journal.
                t.c().execute_batch(SCHEMA).map_err(internal)?;
                let has_started: bool = t.c().query_row(
                "SELECT EXISTS(SELECT 1 FROM pragma_table_info('procs') WHERE name='started')",
                [], |r| r.get(0)).map_err(internal)?;
                if !has_started {
                    t.c()
                        .execute_batch("ALTER TABLE procs ADD COLUMN started INTEGER")
                        .map_err(internal)?;
                }
                t.c()
                    .pragma_update(None, "user_version", 1)
                    .map_err(internal)?;
                let inc = t.bump("incarnation")?;
                t.mark_unknown()?;
                Ok(inc)
            })?;
        Ok((j, incarnation))
    }

    /// One write transaction (IMMEDIATE: it holds the write lock from the start).
    pub fn tx<R>(&self, f: impl FnOnce(&Tx) -> ApiResult<R>) -> ApiResult<R> {
        let mut c = self.db.lock().unwrap_or_else(|e| e.into_inner());
        let t = c
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(internal)?;
        let t = Tx(t);
        let r = f(&t)?;
        t.0.commit().map_err(internal)?;
        Ok(r)
    }

    /// One read transaction: everything read in it is the same moment.
    pub fn read<R>(&self, f: impl FnOnce(&Tx) -> ApiResult<R>) -> ApiResult<R> {
        let mut c = self.db.lock().unwrap_or_else(|e| e.into_inner());
        let t = c
            .transaction_with_behavior(TransactionBehavior::Deferred)
            .map_err(internal)?;
        let t = Tx(t);
        let r = f(&t)?;
        t.0.commit().map_err(internal)?;
        Ok(r)
    }
}

impl Tx<'_> {
    fn c(&self) -> &Connection {
        &self.0
    }

    /// A counter that never goes back (incarnations, revisions, event ids).
    pub fn bump(&self, key: &str) -> ApiResult<u64> {
        self.c()
            .execute(
                "INSERT INTO meta (key, value) VALUES (?1, 1) ON CONFLICT(key) DO UPDATE SET value = value + 1",
                [key],
            )
            .map_err(internal)?;
        let v: i64 = self
            .c()
            .query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
            .map_err(internal)?;
        Ok(v as u64)
    }

    pub(crate) fn mark_unknown(&self) -> ApiResult<()> {
        let rows: Vec<(String, String)> = self.rows("SELECT id, receipt FROM ops", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
        for (id, receipt) in rows {
            let mut r: Receipt = from(&receipt).map_err(internal)?;
            if r.state.is_finished() || matches!(r.state, Observed::Unknown { .. }) {
                continue;
            }
            r.state = Observed::Unknown {
                reason: "the node restarted while it ran".into(),
            };
            self.c()
                .execute(
                    "UPDATE ops SET receipt = ?2 WHERE id = ?1",
                    params![id, json(&r)],
                )
                .map_err(internal)?;
        }
        // nobody waits for an answer of the old processes
        self.c()
            .execute("DELETE FROM waits", [])
            .map_err(internal)?;
        self.c()
            .execute("DELETE FROM answers", [])
            .map_err(internal)?;
        let rows: Vec<(String, String)> = self.rows("SELECT id, view FROM sessions", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
        for (id, view) in rows {
            let mut v: SessionView = from(&view).map_err(internal)?;
            if v.phase != SessionPhase::Released {
                v.phase = SessionPhase::Released;
                self.c()
                    .execute(
                        "UPDATE sessions SET view = ?2 WHERE id = ?1",
                        params![id, json(&v)],
                    )
                    .map_err(internal)?;
            }
        }
        Ok(())
    }

    fn rows<T, P: rusqlite::Params>(
        &self,
        sql: &str,
        p: P,
        f: impl FnMut(&rusqlite::Row) -> rusqlite::Result<T>,
    ) -> ApiResult<Vec<T>> {
        let mut st = self.c().prepare(sql).map_err(internal)?;
        let v = st
            .query_map(p, f)
            .map_err(internal)?
            .collect::<Result<_, _>>()
            .map_err(internal)?;
        Ok(v)
    }

    // ── operations ──

    fn op_row(r: &rusqlite::Row) -> rusqlite::Result<Op> {
        let execution: Option<String> = r.get(3)?;
        let workspace: Option<String> = r.get(4)?;
        Ok(Op {
            method: r.get(0)?,
            digest: conv(Digest::new(r.get::<_, String>(1)?))?,
            receipt: from(&r.get::<_, String>(2)?)?,
            execution: execution.map(|e| from(&e)).transpose()?,
            workspace: workspace.map(|w| from(&w)).transpose()?,
            cancel: r.get::<_, i64>(5)? != 0,
        })
    }

    pub fn op(&self, id: &OperationId) -> ApiResult<Option<Op>> {
        self.c()
            .query_row(
                "SELECT method, digest, receipt, execution, workspace, cancel FROM ops WHERE id = ?1",
                [id.as_str()],
                Self::op_row,
            )
            .optional()
            .map_err(internal)
    }

    pub fn ops(&self) -> ApiResult<Vec<(OperationId, Op)>> {
        let rows: Vec<(String, Op)> = self.rows(
            "SELECT method, digest, receipt, execution, workspace, cancel, id FROM ops ORDER BY id",
            [],
            |r| Ok((r.get(6)?, Self::op_row(r)?)),
        )?;
        rows.into_iter()
            .map(|(id, o)| Ok((OperationId::new(id).map_err(internal)?, o)))
            .collect()
    }

    /// Every unfinished session operation, including open/configure/release and unknown.
    pub fn open_ops_of(&self, session: &SessionId) -> ApiResult<Vec<(OperationId, Op)>> {
        let rows: Vec<(String, Op)> = self.rows(
            "SELECT method, digest, receipt, execution, workspace, cancel, id FROM ops WHERE session = ?1",
            [session.as_str()],
            |r| Ok((r.get(6)?, Self::op_row(r)?)),
        )?;
        rows.into_iter()
            .filter(|(_, o)| !o.receipt.state.is_finished())
            .map(|(id, o)| Ok((OperationId::new(id).map_err(internal)?, o)))
            .collect()
    }

    pub fn accept(
        &self,
        id: &OperationId,
        op: &Op,
        session: Option<&SessionId>,
        now: i64,
    ) -> ApiResult<()> {
        self.c()
            .execute(
                "INSERT INTO ops (id, method, digest, receipt, execution, workspace, session, cancel, accepted)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, ?8)",
                params![
                    id.as_str(),
                    op.method,
                    op.digest.as_str(),
                    json(&op.receipt),
                    op.execution.as_ref().map(json),
                    op.workspace.as_ref().map(json),
                    session.map(|s| s.as_str().to_string()),
                    now
                ],
            )
            .map_err(internal)?;
        Ok(())
    }

    /// Sets the state, unless it is final already (a final state never changes). False: unchanged.
    pub fn set_state(&self, id: &OperationId, state: &Observed) -> ApiResult<bool> {
        let Some(mut op) = self.op(id)? else {
            return Ok(false);
        };
        if op.receipt.state.is_finished() {
            return Ok(false);
        }
        op.receipt.state = state.clone();
        self.c()
            .execute(
                "UPDATE ops SET receipt = ?2 WHERE id = ?1",
                params![id.as_str(), json(&op.receipt)],
            )
            .map_err(internal)?;
        Ok(true)
    }

    pub fn set_cancel(&self, id: &OperationId) -> ApiResult<()> {
        self.c()
            .execute("UPDATE ops SET cancel = 1 WHERE id = ?1", [id.as_str()])
            .map_err(internal)?;
        Ok(())
    }

    pub fn attempt(&self, a: &AttemptId) -> ApiResult<Option<(ExecutionRef, OperationId)>> {
        let row: Option<(String, String)> = self
            .c()
            .query_row(
                "SELECT execution, op FROM attempts WHERE attempt = ?1",
                [a.as_str()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(internal)?;
        row.map(|(e, op)| {
            Ok((
                from(&e).map_err(internal)?,
                OperationId::new(op).map_err(internal)?,
            ))
        })
        .transpose()
    }

    pub fn take_attempt(&self, exec: &ExecutionRef, op: &OperationId) -> ApiResult<()> {
        self.c()
            .execute(
                "INSERT INTO attempts (attempt, execution, op) VALUES (?1, ?2, ?3)",
                params![exec.attempt().as_str(), json(exec), op.as_str()],
            )
            .map_err(internal)?;
        Ok(())
    }

    // ── events ──

    /// Numbers an event in its stream and keeps it until acknowledged (in this transaction).
    pub fn record(
        &self,
        stream: &StreamId,
        build: impl FnOnce(Watermark) -> NodeEvent,
    ) -> ApiResult<NodeEvent> {
        let row: Option<(String, i64)> = self
            .c()
            .query_row(
                "SELECT epoch, produced FROM streams WHERE stream = ?1",
                [stream.as_str()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(internal)?;
        let (epoch, produced) = match row {
            Some((e, p)) => (e, p as u64),
            None => {
                self.c()
                    .execute(
                        "INSERT INTO streams (stream, epoch, produced, acknowledged) VALUES (?1, 'e1', 0, 0)",
                        [stream.as_str()],
                    )
                    .map_err(internal)?;
                ("e1".into(), 0)
            }
        };
        let seq = produced + 1;
        let ev = build(Watermark {
            stream: stream.clone(),
            epoch: Epoch::new(epoch).map_err(internal)?,
            seq,
        });
        self.c()
            .execute(
                "INSERT INTO events (stream, seq, event) VALUES (?1, ?2, ?3)",
                params![stream.as_str(), seq as i64, json(&ev)],
            )
            .map_err(internal)?;
        self.c()
            .execute(
                "UPDATE streams SET produced = ?2 WHERE stream = ?1",
                params![stream.as_str(), seq as i64],
            )
            .map_err(internal)?;
        Ok(ev)
    }

    /// Events not acknowledged yet, in order, at most `limit`.
    pub fn unacked(&self, limit: usize) -> ApiResult<Vec<NodeEvent>> {
        let rows: Vec<String> = self.rows(
            "SELECT event FROM events ORDER BY rowid LIMIT ?1",
            [limit as i64],
            |r| r.get(0),
        )?;
        rows.iter().map(|s| from(s).map_err(internal)).collect()
    }

    pub fn heads(&self) -> ApiResult<Vec<StreamHead>> {
        let rows: Vec<(String, String, i64, i64)> = self.rows(
            "SELECT stream, epoch, produced, acknowledged FROM streams ORDER BY stream",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
        rows.into_iter()
            .map(|(s, e, p, a)| {
                Ok(StreamHead {
                    stream: StreamId::new(s).map_err(internal)?,
                    epoch: Epoch::new(e).map_err(internal)?,
                    produced: p as u64,
                    acknowledged: a as u64,
                })
            })
            .collect()
    }

    /// Moves the watermarks and drops what they cover, all or nothing.
    pub fn ack(&self, ack: &EventAck) -> ApiResult<()> {
        for w in &ack.durable {
            let row: Option<(String, i64, i64)> = self
                .c()
                .query_row(
                    "SELECT epoch, produced, acknowledged FROM streams WHERE stream = ?1",
                    [w.stream.as_str()],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()
                .map_err(internal)?;
            let Some((epoch, produced, acknowledged)) = row else {
                return Err(ApiError::new(
                    ErrorCode::Invalid,
                    "ack of an unknown stream",
                ));
            };
            if epoch != w.epoch.as_str() {
                return Err(ApiError::new(ErrorCode::Stale, "ack of another epoch"));
            }
            if w.seq as i64 > produced {
                return Err(ApiError::new(ErrorCode::Invalid, "ack ahead of the stream"));
            }
            if w.seq as i64 > acknowledged {
                self.c()
                    .execute(
                        "UPDATE streams SET acknowledged = ?2 WHERE stream = ?1",
                        params![w.stream.as_str(), w.seq as i64],
                    )
                    .map_err(internal)?;
                self.c()
                    .execute(
                        "DELETE FROM events WHERE stream = ?1 AND seq <= ?2",
                        params![w.stream.as_str(), w.seq as i64],
                    )
                    .map_err(internal)?;
            }
        }
        Ok(())
    }

    pub fn replay(&self, from_mark: &Watermark, max_bytes: u32) -> ApiResult<EventBatch> {
        let row: Option<(String, i64, i64)> = self
            .c()
            .query_row(
                "SELECT epoch, produced, acknowledged FROM streams WHERE stream = ?1",
                [from_mark.stream.as_str()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(internal)?;
        let Some((epoch, produced, acknowledged)) = row else {
            return Err(ApiError::new(ErrorCode::Invalid, "no such stream"));
        };
        if epoch != from_mark.epoch.as_str() {
            return Err(ApiError::new(ErrorCode::Gap, "another stream epoch"));
        }
        if (from_mark.seq as i64) < acknowledged {
            return Err(ApiError::new(
                ErrorCode::Gap,
                "already acknowledged and dropped",
            ));
        }
        if from_mark.seq as i64 > produced {
            return Err(ApiError::new(ErrorCode::Invalid, "ahead of the stream"));
        }
        let rows: Vec<String> = self.rows(
            "SELECT event FROM events WHERE stream = ?1 AND seq > ?2 ORDER BY seq",
            params![from_mark.stream.as_str(), from_mark.seq as i64],
            |r| r.get(0),
        )?;
        let mut events = vec![];
        let mut used = 0usize;
        for s in rows {
            if used + s.len() > max_bytes as usize {
                if events.is_empty() {
                    return Err(ApiError::new(
                        ErrorCode::Exhausted,
                        "the next event does not fit in max_bytes",
                    ));
                }
                break;
            }
            used += s.len();
            events.push(from(&s).map_err(internal)?);
        }
        Ok(EventBatch { events })
    }

    // ── workspaces and sessions ──

    pub fn workspace(&self, id: &WorkspaceId) -> ApiResult<Option<WorkspaceRow>> {
        self.c()
            .query_row(
                "SELECT resource, spec, view, path, incarnation, kind, repo FROM workspaces WHERE id = ?1",
                [id.as_str()],
                |r| {
                    Ok(WorkspaceRow {
                        resource: r.get(0)?,
                        spec: conv(Digest::new(r.get::<_, String>(1)?))?,
                        view: from(&r.get::<_, String>(2)?)?,
                        path: r.get(3)?,
                        incarnation: r.get::<_, i64>(4)? as u64,
                        kind: from(&r.get::<_, String>(5)?)?,
                        repo: r.get(6)?,
                    })
                },
            )
            .optional()
            .map_err(internal)
    }

    pub fn put_workspace(&self, w: &WorkspaceRow) -> ApiResult<()> {
        self.c()
            .execute(
                "INSERT OR REPLACE INTO workspaces (id, resource, spec, kind, view, path, repo, incarnation)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    w.view.workspace.id.as_str(),
                    w.resource,
                    w.spec.as_str(),
                    json(&w.kind),
                    json(&w.view),
                    w.path,
                    w.repo,
                    w.incarnation as i64
                ],
            )
            .map_err(internal)?;
        Ok(())
    }

    pub fn drop_workspace(&self, id: &WorkspaceId) -> ApiResult<()> {
        self.c()
            .execute("DELETE FROM workspaces WHERE id = ?1", [id.as_str()])
            .map_err(internal)?;
        Ok(())
    }

    pub fn session(&self, id: &SessionId) -> ApiResult<Option<SessionRow>> {
        self.c()
            .query_row(
                "SELECT open, view, incarnation, acp_id FROM sessions WHERE id = ?1",
                [id.as_str()],
                |r| {
                    Ok(SessionRow {
                        open: from(&r.get::<_, String>(0)?)?,
                        view: from(&r.get::<_, String>(1)?)?,
                        incarnation: r.get::<_, i64>(2)? as u64,
                        acp_id: r.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(internal)
    }

    pub fn sessions_in(&self, w: &WorkspaceRef) -> ApiResult<Vec<SessionId>> {
        let rows: Vec<(String, String)> = self.rows("SELECT id, open FROM sessions", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
        let mut out = vec![];
        for (id, open) in rows {
            let o: OpenSession = from(&open).map_err(internal)?;
            if o.workspace == *w {
                out.push(SessionId::new(id).map_err(internal)?);
            }
        }
        Ok(out)
    }

    pub fn put_session(&self, s: &SessionRow) -> ApiResult<()> {
        self.c()
            .execute(
                "INSERT INTO sessions (id, open, view, incarnation, acp_id) VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(id) DO UPDATE SET open = excluded.open, view = excluded.view,
                   incarnation = excluded.incarnation, acp_id = excluded.acp_id",
                params![
                    s.view.id.as_str(),
                    json(&s.open),
                    json(&s.view),
                    s.incarnation as i64,
                    s.acp_id
                ],
            )
            .map_err(internal)?;
        Ok(())
    }

    // ── permission waits ──

    pub fn put_wait(
        &self,
        op: &OperationId,
        session: &SessionId,
        req: &PermissionRequest,
    ) -> ApiResult<()> {
        self.c()
            .execute(
                "INSERT INTO waits (id, op, session, request) VALUES (?1, ?2, ?3, ?4)",
                params![req.id.as_str(), op.as_str(), session.as_str(), json(req)],
            )
            .map_err(internal)?;
        Ok(())
    }

    pub fn wait(
        &self,
        id: &PermissionId,
    ) -> ApiResult<Option<(OperationId, SessionId, PermissionRequest)>> {
        let row: Option<(String, String, String)> = self
            .c()
            .query_row(
                "SELECT op, session, request FROM waits WHERE id = ?1",
                [id.as_str()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(internal)?;
        row.map(|(op, s, req)| {
            Ok((
                OperationId::new(op).map_err(internal)?,
                SessionId::new(s).map_err(internal)?,
                from(&req).map_err(internal)?,
            ))
        })
        .transpose()
    }

    pub fn waits_of(&self, op: &OperationId) -> ApiResult<Vec<PermissionRequest>> {
        let rows: Vec<String> = self.rows(
            "SELECT request FROM waits WHERE op = ?1 ORDER BY id",
            [op.as_str()],
            |r| r.get(0),
        )?;
        rows.iter().map(|s| from(s).map_err(internal)).collect()
    }

    pub fn drop_wait(&self, id: &PermissionId) -> ApiResult<bool> {
        let n = self
            .c()
            .execute("DELETE FROM waits WHERE id = ?1", [id.as_str()])
            .map_err(internal)?;
        Ok(n > 0)
    }

    // ── harness process groups ──

    /// Records a new process group of the session. Refused while another one of it is recorded (it
    /// was not confirmed gone): never overwritten.
    pub fn put_proc(
        &self,
        session: &SessionId,
        pgid: i64,
        generation: u64,
        incarnation: u64,
        meter: &str,
        started: Option<u64>,
    ) -> ApiResult<()> {
        if self.proc_of(session)?.is_some() {
            return Err(ApiError::new(
                ErrorCode::Busy,
                "a harness of this session was not confirmed gone",
            ));
        }
        self.c()
            .execute(
                "INSERT INTO procs (session, pgid, generation, incarnation, meter, reports, started)
                 VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6)",
                params![
                    session.as_str(),
                    pgid,
                    generation as i64,
                    incarnation as i64,
                    meter,
                    started.map(|s| s as i64)
                ],
            )
            .map_err(internal)?;
        Ok(())
    }

    // ── reservations (a workspace being prepared or tidied, an execution being abandoned) ──

    pub fn reservation(&self, key: &str) -> ApiResult<Option<OperationId>> {
        let row: Option<String> = self
            .c()
            .query_row("SELECT op FROM reservations WHERE key = ?1", [key], |r| {
                r.get(0)
            })
            .optional()
            .map_err(internal)?;
        row.map(|o| OperationId::new(o).map_err(internal))
            .transpose()
    }

    pub fn reserve(&self, key: &str, op: &OperationId) -> ApiResult<()> {
        self.c()
            .execute(
                "INSERT INTO reservations (key, op) VALUES (?1, ?2)",
                params![key, op.as_str()],
            )
            .map_err(internal)?;
        Ok(())
    }

    pub fn unreserve(&self, key: &str) -> ApiResult<()> {
        self.c()
            .execute("DELETE FROM reservations WHERE key = ?1", [key])
            .map_err(internal)?;
        Ok(())
    }

    /// Bytes of events the Hub has not acknowledged yet.
    pub fn unacked_bytes(&self) -> ApiResult<u64> {
        let n: i64 = self
            .c()
            .query_row(
                "SELECT COALESCE(SUM(length(event)), 0) FROM events",
                [],
                |r| r.get(0),
            )
            .map_err(internal)?;
        Ok(n as u64)
    }

    // ── answers being applied (a resolution accepted, not yet given to the harness) ──

    pub fn put_answer(&self, op: &OperationId, res: &PermissionResolution) -> ApiResult<()> {
        self.c()
            .execute(
                "INSERT INTO answers (op, permission, resolution) VALUES (?1, ?2, ?3)",
                params![op.as_str(), res.id.as_str(), json(res)],
            )
            .map_err(internal)?;
        Ok(())
    }

    pub fn answer_of(&self, op: &OperationId) -> ApiResult<Option<PermissionResolution>> {
        let row: Option<String> = self
            .c()
            .query_row(
                "SELECT resolution FROM answers WHERE op = ?1",
                [op.as_str()],
                |r| r.get(0),
            )
            .optional()
            .map_err(internal)?;
        row.map(|s| from(&s).map_err(internal)).transpose()
    }

    pub fn answer_pending_for(&self, permission: &PermissionId) -> ApiResult<bool> {
        Ok(self
            .c()
            .query_row(
                "SELECT 1 FROM answers WHERE permission = ?1",
                [permission.as_str()],
                |_| Ok(()),
            )
            .optional()
            .map_err(internal)?
            .is_some())
    }

    /// Last durable authorization check, in the transaction reserving the send. A queued Cancel
    /// message cannot be used as evidence: the cancel flag may already have committed.
    pub fn answer_for_send(&self, op: &OperationId, now: i64) -> ApiResult<()> {
        let stale = || ApiError::new(ErrorCode::Stale, "permission no longer applicable");
        let res = self.answer_of(op)?.ok_or_else(stale)?;
        let (attempt, _, req) = self.wait(&res.id)?.ok_or_else(stale)?;
        let run = self.op(&attempt)?.ok_or_else(stale)?;
        if run.cancel
            || run.receipt.state.is_finished()
            || matches!(run.receipt.state, Observed::Unknown { .. })
            || req.expires_at.0 <= now
            || req.execution != res.execution
            || req.digest != res.action_digest
        {
            return Err(stale());
        }
        Ok(())
    }

    pub fn drop_answer(&self, op: &OperationId) -> ApiResult<()> {
        self.c()
            .execute("DELETE FROM answers WHERE op = ?1", [op.as_str()])
            .map_err(internal)?;
        Ok(())
    }

    pub fn drop_proc(&self, session: &SessionId, pgid: i64) -> ApiResult<()> {
        self.c()
            .execute(
                "DELETE FROM procs WHERE session = ?1 AND pgid = ?2",
                params![session.as_str(), pgid],
            )
            .map_err(internal)?;
        Ok(())
    }

    pub fn procs(&self) -> ApiResult<Vec<Proc>> {
        let rows: Vec<(String, i64, i64, Option<i64>)> = self.rows(
            "SELECT session, pgid, incarnation, started FROM procs",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
        rows.into_iter()
            .map(|(s, p, i, st)| {
                Ok(Proc {
                    session: SessionId::new(s).map_err(internal)?,
                    pgid: p,
                    incarnation: i as u64,
                    started: st.map(|v| v as u64),
                })
            })
            .collect()
    }

    pub fn proc_of(&self, session: &SessionId) -> ApiResult<Option<Proc>> {
        Ok(self.procs()?.into_iter().find(|p| p.session == *session))
    }

    /// The process's meter and the next report number on it.
    pub fn next_report(&self, session: &SessionId) -> ApiResult<Option<(String, u64)>> {
        self.c()
            .execute(
                "UPDATE procs SET reports = reports + 1 WHERE session = ?1",
                [session.as_str()],
            )
            .map_err(internal)?;
        self.c()
            .query_row(
                "SELECT meter, reports FROM procs WHERE session = ?1",
                [session.as_str()],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u64)),
            )
            .optional()
            .map_err(internal)
    }
}

#[cfg(test)]
mod migration_tests {
    use super::*;
    fn old(path: &Path) -> Connection {
        let c = Connection::open(path).unwrap();
        c.execute_batch(&SCHEMA.replace(
            "reports INTEGER NOT NULL DEFAULT 0,\n  started INTEGER",
            "reports INTEGER NOT NULL DEFAULT 0",
        ))
        .unwrap();
        c.execute("INSERT INTO meta VALUES ('incarnation',7)", [])
            .unwrap();
        c.execute(
            "INSERT INTO procs(session,pgid,generation,incarnation,meter) VALUES ('s',42,1,7,'m')",
            [],
        )
        .unwrap();
        c
    }
    #[test]
    fn old_journal_is_upgraded_without_inventing_process_identity() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("node.db");
        drop(old(&p));
        let (j, inc) = Journal::open(&p).unwrap();
        assert_eq!(inc, 8);
        j.read(|t| {
            let ps = t.procs()?;
            assert_eq!(ps.len(), 1);
            assert_eq!(ps[0].started, None);
            Ok(())
        })
        .unwrap();
        drop(j);
        assert_eq!(Journal::open(&p).unwrap().1, 9);
    }
    #[test]
    fn failed_recovery_rolls_back_schema_and_incarnation() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("node.db");
        let c = old(&p);
        c.execute("INSERT INTO ops(id,method,digest,receipt,accepted) VALUES ('bad','x','x','not-json',1)", []).unwrap();
        assert!(Journal::open(&p).is_err());
        assert_eq!(
            c.query_row("SELECT value FROM meta WHERE key='incarnation'", [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap(),
            7
        );
        assert_eq!(
            c.query_row(
                "SELECT count(*) FROM pragma_table_info('procs') WHERE name='started'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        assert_eq!(
            c.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}

#[cfg(test)]
mod answer_fence_tests {
    use super::*;
    use steve_node_api::testkit::build::{answer, permission};
    #[test]
    fn durable_cancel_wins_even_before_actor_sees_its_message() {
        let d = tempfile::tempdir().unwrap();
        let (j, _) = Journal::open(&d.path().join("j")).unwrap();
        let op = OperationId::new("run").unwrap();
        let resolve = OperationId::new("resolve").unwrap();
        let session = SessionId::new("s").unwrap();
        let exec = ExecutionRef::Turn {
            session: session.clone(),
            turn: TurnId::new("t").unwrap(),
            attempt: AttemptId::new("a").unwrap(),
        };
        let req = permission("p", exec.clone(), "echo hello");
        j.tx(|t| {
            t.accept(
                &op,
                &Op {
                    method: "start_turn".into(),
                    digest: Digest::sha256(b"run"),
                    receipt: Receipt {
                        operation: op.clone(),
                        state: Observed::Running,
                    },
                    execution: Some(exec),
                    workspace: None,
                    cancel: false,
                },
                Some(&session),
                1,
            )?;
            t.put_wait(&op, &session, &req)?;
            t.put_answer(&resolve, &answer(&req, PermissionDecision::AllowOnce))?;
            t.answer_for_send(&resolve, 1)
        })
        .unwrap();
        j.tx(|t| t.set_cancel(&op)).unwrap(); // no actor Cancel message has been consumed
        assert_eq!(
            j.tx(|t| t.answer_for_send(&resolve, 1)).unwrap_err().code,
            ErrorCode::Stale
        );
        j.read(|t| {
            assert!(t.wait(&req.id)?.is_some());
            assert!(t.answer_of(&resolve)?.is_some());
            Ok(())
        })
        .unwrap();
    }
}
