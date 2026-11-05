//! The Node's execution journal (docs/node-protocol.md §4): what it accepted, what it reported and
//! where its streams stand, on disk. No business truth: the Hub keeps items, queues and decisions.
//!
//! On every start the Node is a new incarnation. What was accepted and had not finished is then
//! `unknown`: the process that ran it is gone with the old incarnation, what it did up to there is
//! in its events, and only the Hub decides what happens next. An empty memory never means "never ran".

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Serialize, de::DeserializeOwned};
use steve_node_api::*;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS ops (
  id TEXT PRIMARY KEY, method TEXT NOT NULL, digest TEXT NOT NULL, receipt TEXT NOT NULL,
  execution TEXT, workspace TEXT, cancel INTEGER NOT NULL DEFAULT 0, accepted INTEGER NOT NULL);
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
";

pub(crate) struct Journal {
    db: Mutex<Connection>,
}

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

pub(crate) struct SessionRow {
    pub open: OpenSession,
    pub view: SessionView,
    pub incarnation: u64,
    pub acp_id: Option<String>,
}

fn json<T: Serialize>(v: &T) -> String {
    serde_json::to_string(v).expect("the contract's types serialize")
}

fn from<T: DeserializeOwned>(s: &str) -> rusqlite::Result<T> {
    serde_json::from_str(s).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })
}

fn internal(e: impl std::fmt::Display) -> ApiError {
    ApiError::new(ErrorCode::Internal, format!("journal: {e}"))
}

impl Journal {
    /// Opens (or creates) the journal and begins a new incarnation: what was live is now unknown.
    pub fn open(path: &Path) -> ApiResult<(Journal, u64)> {
        let c = Connection::open(path).map_err(internal)?;
        c.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;")
            .map_err(internal)?;
        c.execute_batch(SCHEMA).map_err(internal)?;
        let j = Journal { db: Mutex::new(c) };
        let incarnation = j.bump("incarnation")?;
        j.mark_unknown()?;
        Ok((j, incarnation))
    }

    fn db(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.db.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A counter that never goes back (incarnations, revisions).
    pub fn bump(&self, key: &str) -> ApiResult<u64> {
        let c = self.db();
        c.execute(
            "INSERT INTO meta (key, value) VALUES (?1, 1) ON CONFLICT(key) DO UPDATE SET value = value + 1",
            [key],
        )
        .map_err(internal)?;
        let v: i64 = c
            .query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
            .map_err(internal)?;
        Ok(v as u64)
    }

    fn mark_unknown(&self) -> ApiResult<()> {
        let c = self.db();
        let mut st = c.prepare("SELECT id, receipt FROM ops").map_err(internal)?;
        let rows: Vec<(String, String)> = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(internal)?
            .collect::<Result<_, _>>()
            .map_err(internal)?;
        drop(st);
        for (id, receipt) in rows {
            let mut r: Receipt = from(&receipt).map_err(internal)?;
            if r.state.is_finished() || matches!(r.state, Observed::Unknown { .. }) {
                continue;
            }
            r.state = Observed::Unknown {
                reason: "the node restarted while it ran".into(),
            };
            c.execute(
                "UPDATE ops SET receipt = ?2 WHERE id = ?1",
                params![id, json(&r)],
            )
            .map_err(internal)?;
        }
        // harness processes are gone with the old incarnation
        let mut st = c
            .prepare("SELECT id, view FROM sessions")
            .map_err(internal)?;
        let rows: Vec<(String, String)> = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(internal)?
            .collect::<Result<_, _>>()
            .map_err(internal)?;
        drop(st);
        for (id, view) in rows {
            let mut v: SessionView = from(&view).map_err(internal)?;
            if v.phase != SessionPhase::Released {
                v.phase = SessionPhase::Released;
                c.execute(
                    "UPDATE sessions SET view = ?2 WHERE id = ?1",
                    params![id, json(&v)],
                )
                .map_err(internal)?;
            }
        }
        Ok(())
    }

    // ── operations ──

    pub fn op(&self, id: &OperationId) -> ApiResult<Option<Op>> {
        let c = self.db();
        c.query_row(
            "SELECT method, digest, receipt, execution, workspace, cancel FROM ops WHERE id = ?1",
            [id.as_str()],
            |r| {
                let digest: String = r.get(1)?;
                let execution: Option<String> = r.get(3)?;
                let workspace: Option<String> = r.get(4)?;
                Ok(Op {
                    method: r.get(0)?,
                    digest: Digest::new(digest).map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            1,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?,
                    receipt: from(&r.get::<_, String>(2)?)?,
                    execution: execution.map(|e| from(&e)).transpose()?,
                    workspace: workspace.map(|w| from(&w)).transpose()?,
                    cancel: r.get::<_, i64>(5)? != 0,
                })
            },
        )
        .optional()
        .map_err(internal)
    }

    pub fn ops(&self) -> ApiResult<Vec<(OperationId, Op)>> {
        let ids: Vec<String> = {
            let c = self.db();
            let mut st = c
                .prepare("SELECT id FROM ops ORDER BY id")
                .map_err(internal)?;
            st.query_map([], |r| r.get(0))
                .map_err(internal)?
                .collect::<Result<_, _>>()
                .map_err(internal)?
        };
        let mut out = vec![];
        for id in ids {
            let op = OperationId::new(id).map_err(internal)?;
            if let Some(o) = self.op(&op)? {
                out.push((op, o));
            }
        }
        Ok(out)
    }

    pub fn accept(&self, id: &OperationId, op: &Op, now: i64) -> ApiResult<()> {
        self.db()
            .execute(
                "INSERT INTO ops (id, method, digest, receipt, execution, workspace, cancel, accepted)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, ?7)",
                params![
                    id.as_str(),
                    op.method,
                    op.digest.as_str(),
                    json(&op.receipt),
                    op.execution.as_ref().map(json),
                    op.workspace.as_ref().map(json),
                    now
                ],
            )
            .map_err(internal)?;
        Ok(())
    }

    /// Sets the state, unless it is final already (a final state never changes).
    pub fn set_state(&self, id: &OperationId, state: Observed) -> ApiResult<bool> {
        let Some(mut op) = self.op(id)? else {
            return Ok(false);
        };
        if op.receipt.state.is_finished() {
            return Ok(false);
        }
        op.receipt.state = state;
        self.db()
            .execute(
                "UPDATE ops SET receipt = ?2 WHERE id = ?1",
                params![id.as_str(), json(&op.receipt)],
            )
            .map_err(internal)?;
        Ok(true)
    }

    pub fn set_cancel(&self, id: &OperationId) -> ApiResult<()> {
        self.db()
            .execute("UPDATE ops SET cancel = 1 WHERE id = ?1", [id.as_str()])
            .map_err(internal)?;
        Ok(())
    }

    pub fn attempt(&self, a: &AttemptId) -> ApiResult<Option<(ExecutionRef, OperationId)>> {
        let c = self.db();
        c.query_row(
            "SELECT execution, op FROM attempts WHERE attempt = ?1",
            [a.as_str()],
            |r| {
                let e: String = r.get(0)?;
                let op: String = r.get(1)?;
                Ok((e, op))
            },
        )
        .optional()
        .map_err(internal)?
        .map(|(e, op)| {
            Ok((
                from(&e).map_err(internal)?,
                OperationId::new(op).map_err(internal)?,
            ))
        })
        .transpose()
    }

    pub fn take_attempt(&self, exec: &ExecutionRef, op: &OperationId) -> ApiResult<()> {
        self.db()
            .execute(
                "INSERT INTO attempts (attempt, execution, op) VALUES (?1, ?2, ?3)",
                params![exec.attempt().as_str(), json(exec), op.as_str()],
            )
            .map_err(internal)?;
        Ok(())
    }

    // ── events ──

    /// Numbers `ev` in its stream and keeps it until acknowledged.
    pub fn record(
        &self,
        stream: &StreamId,
        build: impl FnOnce(Watermark) -> NodeEvent,
    ) -> ApiResult<NodeEvent> {
        let c = self.db();
        let row: Option<(String, i64)> = c
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
                c.execute(
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
        c.execute(
            "INSERT INTO events (stream, seq, event) VALUES (?1, ?2, ?3)",
            params![stream.as_str(), seq as i64, json(&ev)],
        )
        .map_err(internal)?;
        c.execute(
            "UPDATE streams SET produced = ?2 WHERE stream = ?1",
            params![stream.as_str(), seq as i64],
        )
        .map_err(internal)?;
        Ok(ev)
    }

    /// Events not acknowledged yet, in order, at most `limit`.
    pub fn unacked(&self, limit: usize) -> ApiResult<Vec<NodeEvent>> {
        let c = self.db();
        let mut st = c
            .prepare("SELECT event FROM events ORDER BY rowid LIMIT ?1")
            .map_err(internal)?;
        let rows: Vec<String> = st
            .query_map([limit as i64], |r| r.get(0))
            .map_err(internal)?
            .collect::<Result<_, _>>()
            .map_err(internal)?;
        rows.iter().map(|s| from(s).map_err(internal)).collect()
    }

    pub fn heads(&self) -> ApiResult<Vec<StreamHead>> {
        let c = self.db();
        let mut st = c
            .prepare("SELECT stream, epoch, produced, acknowledged FROM streams ORDER BY stream")
            .map_err(internal)?;
        let rows: Vec<(String, String, i64, i64)> = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .map_err(internal)?
            .collect::<Result<_, _>>()
            .map_err(internal)?;
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

    pub fn ack(&self, ack: &EventAck) -> ApiResult<()> {
        let c = self.db();
        for w in &ack.durable {
            let row: Option<(String, i64, i64)> = c
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
                c.execute(
                    "UPDATE streams SET acknowledged = ?2 WHERE stream = ?1",
                    params![w.stream.as_str(), w.seq as i64],
                )
                .map_err(internal)?;
                c.execute(
                    "DELETE FROM events WHERE stream = ?1 AND seq <= ?2",
                    params![w.stream.as_str(), w.seq as i64],
                )
                .map_err(internal)?;
            }
        }
        Ok(())
    }

    pub fn replay(&self, from_mark: &Watermark, max_bytes: u32) -> ApiResult<EventBatch> {
        let c = self.db();
        let row: Option<(String, i64, i64)> = c
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
        let mut st = c
            .prepare("SELECT event FROM events WHERE stream = ?1 AND seq > ?2 ORDER BY seq")
            .map_err(internal)?;
        let rows: Vec<String> = st
            .query_map(
                params![from_mark.stream.as_str(), from_mark.seq as i64],
                |r| r.get(0),
            )
            .map_err(internal)?
            .collect::<Result<_, _>>()
            .map_err(internal)?;
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
        let c = self.db();
        c.query_row(
            "SELECT resource, spec, view, path, incarnation, kind, repo FROM workspaces WHERE id = ?1",
            [id.as_str()],
            |r| {
                let spec: String = r.get(1)?;
                Ok(WorkspaceRow {
                    resource: r.get(0)?,
                    spec: Digest::new(spec).map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            1,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?,
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
        self.db()
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
        self.db()
            .execute("DELETE FROM workspaces WHERE id = ?1", [id.as_str()])
            .map_err(internal)?;
        Ok(())
    }

    pub fn session(&self, id: &SessionId) -> ApiResult<Option<SessionRow>> {
        let c = self.db();
        c.query_row(
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
        let c = self.db();
        let mut st = c
            .prepare("SELECT id, open FROM sessions")
            .map_err(internal)?;
        let rows: Vec<(String, String)> = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(internal)?
            .collect::<Result<_, _>>()
            .map_err(internal)?;
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
        self.db()
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
}
