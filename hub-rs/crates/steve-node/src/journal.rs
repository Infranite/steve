//! The Node's execution journal: what it accepted, what it reported and
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

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Mutex;

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Serialize, de::DeserializeOwned};
use steve_node_api::*;

mod disk_records;

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
CREATE TABLE IF NOT EXISTS command_specs (op TEXT PRIMARY KEY, body TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS harness_setups (harness TEXT PRIMARY KEY, op TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS artifact_grants (
  attempt TEXT PRIMARY KEY, workspace TEXT NOT NULL, body TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS artifact_grants_workspace ON artifact_grants(workspace);
CREATE TABLE IF NOT EXISTS answers (
  op TEXT PRIMARY KEY, permission TEXT NOT NULL, resolution TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS disk_observations (
  seq INTEGER PRIMARY KEY AUTOINCREMENT, op TEXT NOT NULL, phase TEXT NOT NULL,
  node_incarnation INTEGER NOT NULL, triggered INTEGER NOT NULL, completed INTEGER,
  stage TEXT NOT NULL, body TEXT NOT NULL, UNIQUE(op,phase));
CREATE INDEX IF NOT EXISTS disk_observations_stage ON disk_observations(stage,seq);
";

/// The journal's schema version (`PRAGMA user_version`). 3: the journal has its own identity.
const VERSION: i64 = 3;

pub(crate) struct Journal {
    db: Mutex<(Connection, BTreeMap<String, StreamHead>)>,
    /// This journal's identity, fixed when it was made: a new journal in the same home (deleted or
    /// recreated) is another one, so its streams and event ids never repeat what the Hub holds.
    id: String,
}

/// The printed form of a journal id (the random value kept in meta).
fn journal_tag(v: i64) -> String {
    format!("{:016x}", v as u64)
}

/// Free bytes on the filesystem of `dir` for an unprivileged writer; `None` if it cannot be read.
/// Advisory: another process may take the space before the journal writes.
pub(crate) fn free_bytes(dir: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
    let mut st = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: `path` is NUL-terminated and `st` is written by statvfs before it is read.
    if unsafe { libc::statvfs(path.as_ptr(), st.as_mut_ptr()) } != 0 {
        return None;
    }
    let st = unsafe { st.assume_init() };
    #[allow(clippy::unnecessary_cast)] // the field types differ between platforms
    Some((st.f_bavail as u64).saturating_mul(st.f_frsize as u64))
}

/// One transaction on the journal.
pub(crate) struct Tx<'a>(
    Transaction<'a>,
    &'a BTreeMap<String, StreamHead>,
    RefCell<BTreeMap<String, Option<StreamHead>>>,
);

const RETENTION_MS: i64 = 24 * 60 * 60_000;
const PRUNE_BATCH: usize = 128;

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
        if version > VERSION {
            return Err(ApiError::new(
                ErrorCode::Unsupported,
                "journal needs a newer Node",
            ));
        }
        let mut j = Journal {
            db: Mutex::new((c, BTreeMap::new())),
            id: String::new(),
        };
        let (incarnation, id) =
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
                // v3: the journal's identity. Streams made before it keep their epoch ('e1'), so
                // the positions the Hub holds for them stay valid.
                t.c()
                    .execute(
                        "INSERT OR IGNORE INTO meta (key, value) VALUES ('journal_id', random())",
                        [],
                    )
                    .map_err(internal)?;
                let id = t.journal_id()?;
                t.c()
                    .pragma_update(None, "user_version", VERSION)
                    .map_err(internal)?;
                let inc = t.bump("incarnation")?;
                // A restart always resumes a sweep, even if the previous sweep finished recently.
                t.c()
                    .execute("DELETE FROM meta WHERE key='prune_after'", [])
                    .map_err(internal)?;
                t.mark_unknown()?;
                t.optional_disk_update(|| {
                    t.interrupt_disk_records(
                        crate::disk_observation::Unavailable::Restarted,
                        crate::now(),
                    )
                })?;
                Ok((inc, id))
            })?;
        j.id = id;
        let heads = j.read(|t| {
            t.rows(
                "SELECT stream, epoch, produced, acknowledged FROM streams ORDER BY stream",
                [],
                |r| {
                    Ok(StreamHead {
                        stream: conv(StreamId::new(r.get::<_, String>(0)?))?,
                        epoch: conv(Epoch::new(r.get::<_, String>(1)?))?,
                        produced: r.get::<_, i64>(2)? as u64,
                        acknowledged: r.get::<_, i64>(3)? as u64,
                    })
                },
            )
        })?;
        j.db.lock().unwrap_or_else(|e| e.into_inner()).1 = heads
            .into_iter()
            .map(|h| (h.stream.as_str().into(), h))
            .collect();
        Ok((j, incarnation))
    }

    /// Tests: caps the journal's file at `pages` (or its size now, if larger), as a full disk would.
    pub fn cap_pages(&self, pages: u64) -> ApiResult<u64> {
        let c = self.db.lock().unwrap_or_else(|e| e.into_inner());
        let n: i64 =
            c.0.pragma_update_and_check(None, "max_page_count", pages as i64, |r| r.get(0))
                .map_err(internal)?;
        Ok(n as u64)
    }

    /// This journal's identity (part of every epoch and event id it makes).
    pub fn id(&self) -> &str {
        &self.id
    }

    /// One write transaction (IMMEDIATE: it holds the write lock from the start).
    pub fn tx<R>(&self, f: impl FnOnce(&Tx) -> ApiResult<R>) -> ApiResult<R> {
        self.transaction(TransactionBehavior::Immediate, f)
    }

    /// One read transaction: everything read in it is the same moment.
    pub fn read<R>(&self, f: impl FnOnce(&Tx) -> ApiResult<R>) -> ApiResult<R> {
        self.transaction(TransactionBehavior::Deferred, f)
    }

    fn transaction<R>(
        &self,
        behavior: TransactionBehavior,
        f: impl FnOnce(&Tx) -> ApiResult<R>,
    ) -> ApiResult<R> {
        let mut db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        let (c, heads) = &mut *db;
        let (r, changes) = {
            let t = Tx(
                c.transaction_with_behavior(behavior).map_err(internal)?,
                heads,
                RefCell::new(BTreeMap::new()),
            );
            let r = f(&t)?;
            let changes = t.2.take();
            t.0.commit().map_err(internal)?;
            (r, changes)
        };
        // Both closure errors and COMMIT errors drop the private delta. Publish only after commit,
        // while still holding the connection lock; readers cannot see an intermediate cache.
        for (stream, head) in changes {
            match head {
                Some(h) => {
                    heads.insert(stream, h);
                }
                None => {
                    heads.remove(&stream);
                }
            }
        }
        Ok(r)
    }
}

impl Tx<'_> {
    fn c(&self) -> &Connection {
        &self.0
    }

    /// Additive auxiliary state: old Nodes ignore this table. There is no new startup refusal
    /// or journal version bump; grants are independently checked against workspace generation.
    pub fn put_artifact_grant(&self, grant: &crate::artifact::Grant) -> ApiResult<()> {
        self.c()
            .execute(
                "INSERT INTO artifact_grants(attempt,workspace,body) VALUES (?1,?2,?3)",
                params![
                    grant.execution.attempt().as_str(),
                    grant.workspace.id.as_str(),
                    json(grant)
                ],
            )
            .map_err(internal)?;
        Ok(())
    }

    pub fn artifact_grant(&self, attempt: &AttemptId) -> ApiResult<Option<crate::artifact::Grant>> {
        let body: Option<String> = self
            .c()
            .query_row(
                "SELECT body FROM artifact_grants WHERE attempt=?1",
                [attempt.as_str()],
                |r| r.get(0),
            )
            .optional()
            .map_err(internal)?;
        body.map(|s| from(&s).map_err(internal)).transpose()
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

    /// The next session or workspace revision. It names this journal: a journal made again in the
    /// same home never hands out a revision the Hub holds from the old one.
    pub fn revision(&self) -> ApiResult<String> {
        let n = self.bump("revision")?;
        Ok(format!("r{}_{n}", self.journal_id()?))
    }

    fn journal_id(&self) -> ApiResult<String> {
        let v: i64 = self
            .c()
            .query_row("SELECT value FROM meta WHERE key = 'journal_id'", [], |r| {
                r.get(0)
            })
            .map_err(internal)?;
        Ok(journal_tag(v))
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
                reason: steve_node_api::RESTARTED.into(),
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

    pub fn save_spec<T: Serialize>(&self, op: &OperationId, body: &T) -> ApiResult<()> {
        self.c()
            .execute(
                "INSERT INTO command_specs VALUES (?1, ?2)",
                params![op.as_str(), json(body)],
            )
            .map_err(internal)?;
        Ok(())
    }
    pub fn spec(&self, op: &OperationId) -> ApiResult<Option<serde_json::Value>> {
        let s: Option<String> = self
            .c()
            .query_row(
                "SELECT body FROM command_specs WHERE op=?1",
                [op.as_str()],
                |r| r.get(0),
            )
            .optional()
            .map_err(internal)?;
        s.map(|s| serde_json::from_str(&s).map_err(internal))
            .transpose()
    }
    pub fn session_of(&self, op: &OperationId) -> ApiResult<Option<SessionId>> {
        let s: Option<String> = self
            .c()
            .query_row("SELECT session FROM ops WHERE id=?1", [op.as_str()], |r| {
                r.get(0)
            })
            .map_err(internal)?;
        s.map(|s| SessionId::new(s).map_err(internal)).transpose()
    }
    pub fn unreserve_owned(&self, op: &OperationId) -> ApiResult<()> {
        self.c()
            .execute("DELETE FROM reservations WHERE op=?1", [op.as_str()])
            .map_err(internal)?;
        Ok(())
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

    /// The latest explicit setup survives spec/event retention, including failure and an
    /// interrupted installation. An interrupted operation is observed, never replayed on boot.
    pub fn installation(&self, harness: &str) -> ApiResult<Option<Receipt>> {
        let op: Option<String> = self
            .c()
            .query_row(
                "SELECT op FROM harness_setups WHERE harness=?1",
                [harness],
                |r| r.get(0),
            )
            .optional()
            .map_err(internal)?;
        match op {
            Some(op) => Ok(self
                .op(&OperationId::new(op).map_err(internal)?)?
                .map(|o| o.receipt)),
            None => Ok(None),
        }
    }

    pub fn set_installation(&self, harness: &str, op: &OperationId) -> ApiResult<()> {
        self.c().execute("INSERT INTO harness_setups(harness,op) VALUES (?1,?2) ON CONFLICT(harness) DO UPDATE SET op=excluded.op", params![harness, op.as_str()]).map_err(internal)?;
        Ok(())
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

    /// How many commands ([`crate::exec`]) in `w` are not finished (running, or unknown since a
    /// restart).
    pub fn open_execs_in(&self, w: &WorkspaceRef) -> ApiResult<usize> {
        let rows: Vec<Op> = self.rows(
            "SELECT method, digest, receipt, execution, workspace, cancel FROM ops WHERE method = 'exec' AND workspace = ?1",
            [json(w)],
            Self::op_row,
        )?;
        Ok(rows
            .iter()
            .filter(|o| !o.receipt.state.is_finished())
            .count())
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
        let mut head = match self.head(stream.as_str()) {
            Some(h) => h,
            None => {
                let epoch = Epoch::new(format!("j{}", self.journal_id()?)).map_err(internal)?;
                self.c().execute("INSERT INTO streams (stream, epoch, produced, acknowledged) VALUES (?1, ?2, 0, 0)", params![stream.as_str(), epoch.as_str()]).map_err(internal)?;
                StreamHead {
                    stream: stream.clone(),
                    epoch,
                    produced: 0,
                    acknowledged: 0,
                }
            }
        };
        let produced = head.produced;
        let seq = produced + 1;
        let ev = build(Watermark {
            stream: stream.clone(),
            epoch: head.epoch.clone(),
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
        head.produced = seq;
        self.2
            .borrow_mut()
            .insert(stream.as_str().into(), Some(head));
        Ok(ev)
    }

    /// Events not acknowledged yet, in order, at most `limit`, past what is in flight: an event at
    /// or below `sent[stream]` is skipped (its batch is out already). A stream's positions only
    /// grow, so this is what follows them.
    pub fn unacked_past(
        &self,
        sent: &HashMap<String, u64>,
        limit: usize,
    ) -> ApiResult<Vec<NodeEvent>> {
        let mut st = self
            .c()
            .prepare_cached("SELECT stream, seq, event FROM events ORDER BY rowid")
            .map_err(internal)?;
        let mut rows = st.query([]).map_err(internal)?;
        let mut out = vec![];
        while out.len() < limit {
            let Some(r) = rows.next().map_err(internal)? else {
                break;
            };
            let stream: String = r.get(0).map_err(internal)?;
            let seq: i64 = r.get(1).map_err(internal)?;
            if sent.get(&stream).is_some_and(|s| seq as u64 <= *s) {
                continue;
            }
            let event: String = r.get(2).map_err(internal)?;
            out.push(from(&event).map_err(internal)?);
        }
        Ok(out)
    }

    fn head(&self, stream: &str) -> Option<StreamHead> {
        self.2
            .borrow()
            .get(stream)
            .cloned()
            .unwrap_or_else(|| self.1.get(stream).cloned())
    }

    pub fn covered(&self, marks: &[Watermark]) -> bool {
        marks.iter().all(|m| {
            self.head(m.stream.as_str())
                .is_none_or(|h| h.epoch != m.epoch || h.acknowledged >= m.seq)
        })
    }

    pub fn heads(&self) -> ApiResult<Vec<StreamHead>> {
        let changes = self.2.borrow();
        let mut out: Vec<_> = self
            .1
            .iter()
            .filter(|(s, _)| !changes.contains_key(*s))
            .map(|(_, h)| h.clone())
            .chain(changes.values().filter_map(Clone::clone))
            .collect();
        out.sort_unstable_by(|a, b| a.stream.as_str().cmp(b.stream.as_str()));
        Ok(out)
    }

    /// Moves the watermarks and drops what they cover, all or nothing. Returns whether any moved.
    pub fn ack(&self, ack: &EventAck) -> ApiResult<bool> {
        let mut moved = false;
        for w in &ack.durable {
            let Some(mut head) = self.head(w.stream.as_str()) else {
                return Err(ApiError::new(
                    ErrorCode::Invalid,
                    "ack of an unknown stream",
                ));
            };
            let (epoch, produced, acknowledged) = (
                head.epoch.as_str(),
                head.produced as i64,
                head.acknowledged as i64,
            );
            if epoch != w.epoch.as_str() {
                return Err(ApiError::new(ErrorCode::Stale, "ack of another epoch"));
            }
            if w.seq as i64 > produced {
                return Err(ApiError::new(ErrorCode::Invalid, "ack ahead of the stream"));
            }
            if w.seq as i64 > acknowledged {
                moved = true;
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
                head.acknowledged = w.seq;
                self.2
                    .borrow_mut()
                    .insert(w.stream.as_str().into(), Some(head));
            }
        }
        self.compact()?;
        self.prune(crate::now())?;
        Ok(moved)
    }

    /// Drops what finished operations no longer need: the body of one whose final state the Hub
    /// has taken, the answer of one that finished. The final state is on the operation's stream
    /// (`OperationChanged`) or, for an execution, on its session's or job's (`AttemptEnded`); it is
    /// taken once every event of those streams is acknowledged (one that finished without any
    /// stream, like a cancel, has nothing to deliver and is taken at once). An operation that is not finished
    /// (running or unknown) keeps its body for recovery; the ops rows (id, digest, receipt) stay
    /// for idempotency until retention expires.
    fn compact(&self) -> ApiResult<()> {
        let specs: Vec<(String, String, Option<String>)> = self.rows(
            "SELECT o.id, o.receipt, o.execution FROM command_specs s JOIN ops o ON o.id = s.op",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        for (op, receipt, execution) in specs {
            if !self.final_taken(&op, &receipt, execution.as_deref())? {
                continue;
            }
            self.c()
                .execute("DELETE FROM command_specs WHERE op = ?1", [&op])
                .map_err(internal)?;
        }
        let answers: Vec<(String, String)> = self.rows(
            "SELECT o.id, o.receipt FROM answers a JOIN ops o ON o.id = a.op",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        for (op, receipt) in answers {
            let r: Receipt = from(&receipt).map_err(internal)?;
            if r.state.is_finished() {
                self.c()
                    .execute("DELETE FROM answers WHERE op = ?1", [&op])
                    .map_err(internal)?;
            }
        }
        Ok(())
    }

    // Shared by compaction and tombstone retention: missing streams have nothing to deliver.
    fn op_streams(op: &str, execution: Option<&str>) -> ApiResult<Vec<String>> {
        let mut streams = vec![crate::stream_of(&Subject::Operation {
            operation: OperationId::new(op).map_err(internal)?,
        })];
        if let Some(e) = execution {
            streams.push(crate::stream_of(&Subject::Execution(
                from(e).map_err(internal)?,
            )));
        }
        Ok(streams)
    }

    fn final_taken(&self, op: &str, receipt: &str, execution: Option<&str>) -> ApiResult<bool> {
        Ok(from::<Receipt>(receipt)
            .map_err(internal)?
            .state
            .is_finished()
            && Self::op_streams(op, execution)?
                .iter()
                .all(|s| self.head(s).is_none_or(|h| h.produced == h.acknowledged)))
    }

    /// One bounded slice of an hourly sweep; persist the cursor with the deletions, so failure or
    /// restart resumes safely. 24h exceeds the first-party Hub's largest accept window (10min).
    /// Session streams and externally referenced receipts/grants survive retention.
    pub(crate) fn prune(&self, now: i64) -> ApiResult<usize> {
        let next: i64 = self
            .c()
            .query_row(
                "SELECT COALESCE((SELECT value FROM meta WHERE key='prune_after'),0)",
                [],
                |r| r.get(0),
            )
            .map_err(internal)?;
        if now < next {
            return Ok(0);
        }
        let cursor: i64 = self
            .c()
            .query_row(
                "SELECT COALESCE((SELECT value FROM meta WHERE key='prune_cursor'),0)",
                [],
                |r| r.get(0),
            )
            .map_err(internal)?;
        let rows: Vec<(i64, String, String, Option<String>, i64)> = self.rows("SELECT rowid,id,receipt,execution,accepted FROM ops WHERE rowid>?1 ORDER BY rowid LIMIT ?2", params![cursor, PRUNE_BATCH as i64], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)))?;
        for (_, op, receipt, execution, accepted) in &rows {
            if *accepted >= now.saturating_sub(RETENTION_MS)
                || !self.final_taken(op, receipt, execution.as_deref())?
            {
                continue;
            }
            let held: bool = self.c().query_row("SELECT EXISTS(SELECT 1 FROM harness_setups WHERE op=?1 UNION ALL SELECT 1 FROM waits WHERE op=?1 UNION ALL SELECT 1 FROM attempts a JOIN artifact_grants g ON g.attempt=a.attempt WHERE a.op=?1)", [op], |r| r.get(0)).map_err(internal)?;
            if held {
                continue;
            }
            let streams = Self::op_streams(op, execution.as_deref())?;
            let pending: bool = self
                .c()
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM events WHERE stream=?1 OR stream=?2)",
                    params![streams[0], streams.get(1)],
                    |r| r.get(0),
                )
                .map_err(internal)?;
            if pending {
                continue;
            }
            let shared = match execution.as_deref().map(from::<ExecutionRef>).transpose().map_err(internal)? {
                Some(ExecutionRef::Turn { .. }) => true,
                Some(ExecutionRef::Job { job, .. }) => self.c().query_row("SELECT EXISTS(SELECT 1 FROM ops WHERE id!=?1 AND json_extract(execution,'$.job')=?2)", params![op,job.as_str()], |r| r.get(0)).map_err(internal)?,
                None => false,
            };
            for stream in streams.into_iter().take(if shared { 1 } else { 2 }) {
                self.c()
                    .execute(
                        "DELETE FROM streams WHERE stream=?1 AND produced=acknowledged",
                        [&stream],
                    )
                    .map_err(internal)?;
                self.2.borrow_mut().insert(stream, None);
            }
            for table in ["attempts", "command_specs", "reservations", "answers"] {
                self.c()
                    .execute(&format!("DELETE FROM {table} WHERE op=?1"), [op])
                    .map_err(internal)?;
            }
            self.c()
                .execute("DELETE FROM ops WHERE id=?1", [op])
                .map_err(internal)?;
        }
        let done = rows.len() < PRUNE_BATCH;
        self.c().execute("INSERT INTO meta(key,value) VALUES ('prune_cursor',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [if done { 0 } else { rows.last().unwrap().0 }]).map_err(internal)?;
        self.c().execute("INSERT INTO meta(key,value) VALUES ('prune_after',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [if done { now.saturating_add(60*60_000) } else { 0 }]).map_err(internal)?;
        Ok(rows.len())
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

    /// Two workspaces may have been prepared at the same path or one inside another.
    /// Within an overlap the filesystem alone cannot prove item ownership.
    pub fn overlaps_other_workspace(
        &self,
        current: &WorkspaceId,
        base: &std::path::Path,
        file: &std::path::Path,
    ) -> ApiResult<bool> {
        let mut q = self
            .c()
            .prepare("SELECT path FROM workspaces WHERE id != ?")
            .map_err(internal)?;
        let mut rows = q.query([current.as_str()]).map_err(internal)?;
        while let Some(row) = rows.next().map_err(internal)? {
            let path: String = row.get(0).map_err(internal)?;
            let path = std::path::Path::new(&path);
            // A parent workspace cannot read inside this one either (the test is symmetric).
            if file.starts_with(path) && path.starts_with(base) || base.starts_with(path) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub fn put_workspace(&self, w: &WorkspaceRow) -> ApiResult<()> {
        // Re-preparing the same resource creates a new directory generation. Its predecessors'
        // artifact grants must neither survive as authorization nor accumulate in the journal.
        self.c()
            .execute(
                "DELETE FROM artifact_grants WHERE workspace=?1",
                [w.view.workspace.id.as_str()],
            )
            .map_err(internal)?;
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
            .execute(
                "DELETE FROM artifact_grants WHERE workspace = ?1",
                [id.as_str()],
            )
            .map_err(internal)?;
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

    /// Live SQLite pages: includes command history, ids, indexes and payloads. Free pages are
    /// reusable after acknowledgement. WAL/temporary journal files require filesystem headroom.
    pub fn payload_bytes(&self) -> ApiResult<u64> {
        let page: i64 = self
            .c()
            .pragma_query_value(None, "page_size", |r| r.get(0))
            .map_err(internal)?;
        let count: i64 = self
            .c()
            .pragma_query_value(None, "page_count", |r| r.get(0))
            .map_err(internal)?;
        let free: i64 = self
            .c()
            .pragma_query_value(None, "freelist_count", |r| r.get(0))
            .map_err(internal)?;
        Ok(((count - free).max(0) * page) as u64)
    }

    /// The journal file's pages (live and free), in bytes.
    pub fn file_bytes(&self) -> ApiResult<u64> {
        let page: i64 = self
            .c()
            .pragma_query_value(None, "page_size", |r| r.get(0))
            .map_err(internal)?;
        let count: i64 = self
            .c()
            .pragma_query_value(None, "page_count", |r| r.get(0))
            .map_err(internal)?;
        Ok((count.max(0) * page) as u64)
    }

    /// Bytes of events the Hub has not acknowledged yet.
    pub fn unacked_bytes(&self) -> ApiResult<u64> {
        let n: i64 = self
            .c()
            .query_row(
                "SELECT COALESCE(SUM(length(CAST(event AS BLOB))), 0) FROM events",
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

#[cfg(test)]
mod retention_tests {
    use super::*;

    fn add(
        t: &Tx,
        name: &str,
        accepted: i64,
        finished: bool,
        execution: Option<ExecutionRef>,
        acked: bool,
    ) -> ApiResult<Watermark> {
        let operation = OperationId::new(name).unwrap();
        let receipt: Receipt = from(&json(&serde_json::json!({"operation": name, "state": {"state": if finished { "finished" } else { "running" }, "outcome": "succeeded"}}))).unwrap();
        t.c().execute("INSERT INTO ops(id,method,digest,receipt,execution,accepted) VALUES (?1,'test',?2,?3,?4,?5)", params![name,Digest::sha256(name.as_bytes()).as_str(),json(&receipt),execution.as_ref().map(json),accepted]).map_err(internal)?;
        let mut mark = None;
        for subject in std::iter::once(Subject::Operation {
            operation: operation.clone(),
        })
        .chain(execution.map(Subject::Execution))
        {
            let stream = StreamId::new(crate::stream_of(&subject)).unwrap();
            let ev = t.record(&stream, |cursor| serde_json::from_value(serde_json::json!({
                "schema": EVENT_SCHEMA, "id": format!("ev_{name}_{}",cursor.seq), "at": accepted,
                "cursor": cursor, "subject": subject, "body": {"type": "operation_changed", "receipt": receipt}
            })).unwrap())?;
            if acked {
                t.ack(&EventAck {
                    durable: vec![ev.cursor.clone()],
                })?;
            }
            mark = Some(ev.cursor);
        }
        t.save_spec(&operation, &serde_json::json!({}))?;
        Ok(mark.unwrap())
    }

    fn consistent(j: &Journal) {
        j.read(|t| {
            let sql: Vec<String> = t.rows("SELECT json_object('stream',stream,'epoch',epoch,'produced',produced,'acknowledged',acknowledged) FROM streams ORDER BY stream", [], |r| r.get(0))?;
            let sql: Vec<StreamHead> = sql.iter().map(|s| from(s).unwrap()).collect();
            assert_eq!(t.heads()?,sql);
            Ok(())
        }).unwrap();
    }

    fn sweep(j: &Journal, now: i64) {
        j.tx(|t| {
            t.c()
                .execute(
                    "DELETE FROM meta WHERE key IN ('prune_after','prune_cursor')",
                    [],
                )
                .map_err(internal)?;
            Ok(())
        })
        .unwrap();
        while j.tx(|t| t.prune(now)).unwrap() == PRUNE_BATCH {}
    }

    #[test]
    fn pruning_retains_live_unacked_recent_session_and_referenced_state_across_restart() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("j");
        let (j, _) = Journal::open(&path).unwrap();
        let now = crate::now();
        let old = now - RETENTION_MS - 1;
        let job = |job: &str, attempt: &str| {
            Some(ExecutionRef::Job {
                job: JobId::new(job).unwrap(),
                attempt: AttemptId::new(attempt).unwrap(),
            })
        };
        j.tx(|t| {
            t.c()
                .execute("INSERT INTO meta VALUES ('prune_after',?1)", [i64::MAX])
                .map_err(internal)?;
            for (name, age, finished, acked) in [
                ("old", old, true, true),
                ("young", now, true, true),
                ("unacked", old, true, false),
                ("live", old, false, true),
                ("installation", old, true, true),
            ] {
                add(t, name, age, finished, None, acked)?;
            }
            add(
                t,
                "turn",
                old,
                true,
                Some(ExecutionRef::Turn {
                    session: SessionId::new("session").unwrap(),
                    turn: TurnId::new("turn").unwrap(),
                    attempt: AttemptId::new("turn_attempt").unwrap(),
                }),
                true,
            )?;
            add(t, "job_old", old, true, job("shared", "a1"), true)?;
            add(t, "job_live", old, false, job("shared", "a2"), true)?;
            add(t, "job_last", old, true, job("last", "a3"), true)?;
            t.take_attempt(
                &job("last", "a3").unwrap(),
                &OperationId::new("job_last").unwrap(),
            )?;
            t.c()
                .execute("INSERT INTO reservations VALUES ('owned','old')", [])
                .map_err(internal)?;
            t.set_installation("test", &OperationId::new("installation").unwrap())?;
            Ok(())
        })
        .unwrap();
        sweep(&j, now);
        consistent(&j);
        let before = j
            .read(|t| {
                let ids: Vec<_> = t
                    .ops()?
                    .into_iter()
                    .map(|(id, _)| id.as_str().to_owned())
                    .collect();
                assert_eq!(
                    ids,
                    ["installation", "job_live", "live", "unacked", "young"]
                );
                assert_eq!(t.heads()?.len(), 7); // Five retained op streams, the shared job and session.
                assert_eq!(
                    t.c()
                        .query_row("SELECT count(*) FROM attempts", [], |r| r.get::<_, i64>(0))
                        .unwrap(),
                    0
                );
                assert_eq!(
                    t.c()
                        .query_row("SELECT count(*) FROM reservations", [], |r| r
                            .get::<_, i64>(0))
                        .unwrap(),
                    0
                );
                assert!(t.spec(&OperationId::new("old").unwrap())?.is_none());
                Ok(t.heads()?)
            })
            .unwrap();
        drop(j);
        let (j, _) = Journal::open(&path).unwrap();
        consistent(&j);
        j.read(|t| {
            assert_eq!(t.heads()?, before);
            assert!(matches!(
                t.op(&OperationId::new("live").unwrap())?
                    .unwrap()
                    .receipt
                    .state,
                Observed::Unknown { .. }
            ));
            assert!(t.op(&OperationId::new("old").unwrap())?.is_none());
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn record_ack_prune_and_failed_commit_publish_only_committed_heads() {
        let d = tempfile::tempdir().unwrap();
        let (j, _) = Journal::open(&d.path().join("j")).unwrap();
        let now = crate::now();
        let mark = j.tx(|t| add(t, "op", now, true, None, false)).unwrap();
        let ack = EventAck {
            durable: vec![mark.clone()],
        };
        assert!(j.tx(|t| t.ack(&ack)).unwrap());
        assert!(!j.tx(|t| t.ack(&ack)).unwrap());
        consistent(&j);
        let baseline = j.read(|t| t.heads()).unwrap();
        assert!(
            j.tx(|t| {
                add(t, "rolled_back", now, true, None, false)?;
                let mut bad = mark.clone();
                bad.stream = StreamId::new("missing").unwrap();
                t.ack(&EventAck { durable: vec![bad] })
            })
            .is_err()
        );
        assert!(
            j.tx::<()>(|t| {
                t.c()
                    .execute(
                        "DELETE FROM meta WHERE key IN ('prune_after','prune_cursor')",
                        [],
                    )
                    .map_err(internal)?;
                t.prune(now + RETENTION_MS + 1)?;
                assert!(t.heads()?.is_empty());
                Err(internal("rollback after prune"))
            })
            .is_err()
        );
        assert_eq!(j.read(|t| t.heads()).unwrap(), baseline);
        consistent(&j);
        j.db.lock().unwrap().0.execute_batch("PRAGMA foreign_keys=ON; CREATE TABLE parent(id PRIMARY KEY); CREATE TABLE child(id REFERENCES parent(id) DEFERRABLE INITIALLY DEFERRED);").unwrap();
        assert!(
            j.tx(|t| {
                add(t, "commit_failure", now, true, None, true)?;
                t.c()
                    .execute("INSERT INTO child VALUES (1)", [])
                    .map_err(internal)?;
                Ok(())
            })
            .is_err()
        );
        consistent(&j);
        assert_eq!(j.read(|t| t.heads()).unwrap(), baseline);
        for (epoch, seq, code) in [
            ("wrong", 1, ErrorCode::Stale),
            (mark.epoch.as_str(), 2, ErrorCode::Invalid),
        ] {
            let mut bad = mark.clone();
            bad.epoch = Epoch::new(epoch).unwrap();
            bad.seq = seq;
            assert_eq!(
                j.tx(|t| t.ack(&EventAck { durable: vec![bad] }))
                    .unwrap_err()
                    .code,
                code
            );
        }
    }

    #[test]
    fn ack_and_heads_work_without_a_streams_table_to_select() {
        let d = tempfile::tempdir().unwrap();
        let (j, _) = Journal::open(&d.path().join("j")).unwrap();
        let mark = j
            .tx(|t| add(t, "cached", crate::now(), true, None, true))
            .unwrap();
        // Every SELECT from streams would fail, including a per-watermark SELECT. The redundant
        // ack needs no writes, and the entire heads vector must still come from committed memory.
        j.tx(|t| {
            t.c()
                .execute("ALTER TABLE streams RENAME TO hidden_streams", [])
                .map_err(internal)?;
            Ok(())
        })
        .unwrap();
        j.read(|t| {
            assert!(!t.ack(&EventAck {
                durable: vec![mark]
            })?);
            assert_eq!(t.heads()?.len(), 1);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn pruning_is_bounded_and_hourly() {
        let d = tempfile::tempdir().unwrap();
        let (j, _) = Journal::open(&d.path().join("j")).unwrap();
        let now = crate::now();
        j.tx(|t| {
            for i in 0..(PRUNE_BATCH + 1) {
                add(
                    t,
                    &format!("op{i}"),
                    now - RETENTION_MS - 1,
                    true,
                    None,
                    false,
                )?;
            }
            Ok(())
        })
        .unwrap();
        // Unacknowledged operations consume scan slots but survive, so each sweep is bounded.
        assert_eq!(j.tx(|t| t.prune(now)).unwrap(), PRUNE_BATCH);
        assert_eq!(j.tx(|t| t.prune(now)).unwrap(), 1);
        assert_eq!(j.tx(|t| t.prune(now)).unwrap(), 0);
        assert_eq!(j.tx(|t| t.prune(now + 60 * 60_000)).unwrap(), PRUNE_BATCH);
        consistent(&j);
    }
}
