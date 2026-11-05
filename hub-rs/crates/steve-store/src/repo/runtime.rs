//! Hub execution ledger. Every write uses the caller's transaction: propagate errors so the whole
//! plan/apply rolls back. No Node calls, core callbacks, or nested transactions live here.
//! Opaque protocol JSON is retained verbatim after JSON serialization; the coordinator validates
//! protocol types/digests. SQL identity, CAS, leases and replay invariants are enforced here.
use rusqlite::{OptionalExtension, params};
use serde_json::Value;

use super::{all, to_json};
use crate::{Queued, Result, StoreError, Tx};

fn conflict(detail: &str) -> StoreError {
    StoreError::Conflict(detail.into())
}

/// Immutable binding. Changing Node/workspace requires a separate reconciliation operation.
pub struct SessionBinding<'a> {
    pub session: &'a str,
    pub node: &'a str,
    pub workspace: &'a Value,
    pub node_session: &'a str,
    pub config_revision: &'a str,
}

/// The workspace a session is bound to (the protocol's `WorkspaceRef` JSON), if it is bound.
pub fn workspace_ref_of(c: &rusqlite::Connection, session: &str) -> Result<Option<String>> {
    Ok(c.query_row(
        "SELECT workspace_ref FROM runtime_sessions WHERE session_id=?",
        [session],
        |r| r.get(0),
    )
    .optional()?)
}

/// Idempotent insert; a reused session with a different binding is a conflict.
pub fn bind_session_in(tx: &Tx<'_>, b: &SessionBinding<'_>, now: i64) -> Result<bool> {
    let workspace = to_json(b.workspace)?;
    let old: Option<(String,String,String,String)> = tx.query_row(
        "SELECT node_id,workspace_ref,node_session_id,config_revision FROM runtime_sessions WHERE session_id=?",
        [b.session], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
    if let Some(old) = old {
        if old
            != (
                b.node.into(),
                workspace,
                b.node_session.into(),
                b.config_revision.into(),
            )
        {
            return Err(conflict("session binding changed"));
        }
        return Ok(false);
    }
    tx.execute("INSERT INTO runtime_sessions(session_id,node_id,workspace_ref,node_session_id,config_revision,phase,updated) VALUES (?,?,?,?,?,'unopened',?)",
        params![b.session,b.node,workspace,b.node_session,b.config_revision,now])?;
    Ok(true)
}

/// CAS fields of a session projection. Revision is incremented even for phase-preserving updates.
pub struct SessionUpdate<'a> {
    pub phase: &'a str,
    pub active_attempt: Option<&'a str>,
    pub release_pending: bool,
}
pub fn session_cas_in(
    tx: &Tx<'_>,
    session: &str,
    revision: i64,
    u: &SessionUpdate<'_>,
    now: i64,
) -> Result<bool> {
    if ![
        "unopened",
        "opening",
        "ready",
        "active",
        "releasing",
        "released",
        "configuring",
    ]
    .contains(&u.phase)
    {
        return Err(conflict("invalid session phase"));
    }
    if ["ready", "released"].contains(&u.phase) {
        let unresolved: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM runtime_turns WHERE session_id=? AND state<>'finished')",
            [session],
            |r| r.get(0),
        )?;
        if unresolved || u.active_attempt.is_some() {
            return Err(conflict("cannot free a session with unresolved work"));
        }
    }
    if let Some(a) = u.active_attempt {
        let valid: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM runtime_attempts a JOIN runtime_turns t USING(turn_id) WHERE a.attempt_id=? AND t.session_id=? AND a.state<>'finished')",params![a,session],|r|r.get(0))?;
        if !valid {
            return Err(conflict(
                "active attempt belongs to another session or is finished",
            ));
        }
    }
    Ok(tx.execute("UPDATE runtime_sessions SET phase=?,active_attempt=?,release_pending=?,revision=revision+1,updated=? WHERE session_id=? AND revision=?",
        params![u.phase,u.active_attempt,u.release_pending,now,session,revision])? == 1)
}

/// Atomically reserve one logical turn and move its queued rows into Node-owned dispatch receipts.
/// Empty means no queued work. A retry with the same turn reads the same rows; another turn cannot
/// claim while this session has an unfinished turn (including unknown). Caller inserts attempt and
/// command in this same transaction. Holds and permission to dispatch are Hub policy, checked by caller.
pub fn claim_in(tx: &Tx<'_>, session: &str, turn: &str, now: i64) -> Result<Vec<Queued>> {
    let existing: Option<String> = tx
        .query_row(
            "SELECT session_id FROM runtime_turns WHERE turn_id=?",
            [turn],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(s) = existing {
        if s != session {
            return Err(conflict("turn belongs to another session"));
        }
        return all(
            tx,
            "SELECT id,session_id,text,via,created,files FROM session_dispatch WHERE turn_id=? AND backend='node' ORDER BY created,rowid",
            [turn],
            Queued::from_row,
        );
    }
    let ready: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM runtime_sessions WHERE session_id=? AND phase='ready' AND release_pending=0 AND active_attempt IS NULL)", [session], |r|r.get(0))?;
    if !ready {
        return Err(conflict("session is not ready to claim"));
    }
    let xs = all(
        tx,
        "SELECT * FROM queued WHERE session_id=? ORDER BY created,rowid",
        [session],
        Queued::from_row,
    )?;
    if xs.is_empty() {
        return Ok(xs);
    }
    tx.execute("INSERT INTO runtime_turns(turn_id,session_id,state,created,updated) VALUES (?,?,'planned',?,?)",params![turn,session,now,now])?;
    tx.execute("INSERT INTO session_dispatch(id,session_id,text,via,created,files,state,updated,turn_id,backend) SELECT id,session_id,text,via,created,files,'claimed',?,?,'node' FROM queued WHERE session_id=?",params![now,turn,session])?;
    tx.execute("DELETE FROM queued WHERE session_id=?", [session])?;
    tx.execute("UPDATE runtime_sessions SET phase='active',revision=revision+1,updated=? WHERE session_id=?",params![now,session])?;
    Ok(xs)
}

/// Immutable attempt identity and frozen request. A retry is a new attempt, never a rewritten spec.
pub struct NewAttempt<'a> {
    pub id: &'a str,
    pub turn: &'a str,
    pub ordinal: i64,
    pub node: &'a str,
    pub operation: &'a str,
    pub execution: &'a Value,
    pub spec: &'a Value,
    pub digest: &'a str,
}
pub fn insert_attempt_in(tx: &Tx<'_>, a: &NewAttempt<'_>, now: i64) -> Result<bool> {
    let execution = to_json(a.execution)?;
    let spec = to_json(a.spec)?;
    let same: Option<bool> = tx.query_row("SELECT turn_id=? AND ordinal=? AND node_id=? AND operation_id=? AND execution=? AND spec=? AND digest=? FROM runtime_attempts WHERE attempt_id=?",
        params![a.turn,a.ordinal,a.node,a.operation,execution,spec,a.digest,a.id],|r|r.get(0)).optional()?;
    if let Some(same) = same {
        if !same {
            return Err(conflict("attempt identity reused"));
        }
        return Ok(false);
    }
    let valid: bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM runtime_turns t JOIN runtime_sessions s USING(session_id) WHERE t.turn_id=? AND t.state<>'finished' AND s.node_id=? AND s.release_pending=0)",params![a.turn,a.node],|r|r.get(0))?;
    if !valid {
        return Err(conflict("attempt requires an open turn on its bound Node"));
    }
    tx.execute("INSERT INTO runtime_attempts(attempt_id,turn_id,ordinal,node_id,operation_id,execution,spec,digest,state,created,updated) VALUES (?,?,?,?,?,?,?,?,'planned',?,?)",
        params![a.id,a.turn,a.ordinal,a.node,a.operation,execution,spec,a.digest,now,now])?;
    Ok(true)
}

fn valid_execution_state(state: &str) -> bool {
    [
        "planned",
        "accepted",
        "running",
        "waiting",
        "unknown",
        "retry_wait",
        "finished",
    ]
    .contains(&state)
}
/// Caller supplies its observed revision; stale events cannot rewrite a newer projection. Terminal
/// projections are immutable. Unknown can only be closed by explicit reconciliation to finished.
pub fn attempt_cas_in(
    tx: &Tx<'_>,
    id: &str,
    revision: i64,
    state: &str,
    receipt: Option<&Value>,
    completion: Option<&Value>,
    now: i64,
) -> Result<bool> {
    if !valid_execution_state(state) || state == "retry_wait" {
        return Err(conflict("invalid attempt state"));
    }
    Ok(tx.execute("UPDATE runtime_attempts SET state=?,receipt=COALESCE(?,receipt),completion=COALESCE(?,completion),revision=revision+1,updated=? WHERE attempt_id=? AND revision=? AND state<>'finished' AND (state<>'unknown' OR ? IN ('unknown','finished'))",
        params![state,receipt.map(to_json).transpose()?,completion.map(to_json).transpose()?,now,id,revision,state])?==1)
}
/// Persist cancellation before the worker sends it. This never releases the attempt's reservation.
pub fn cancel_attempt_in(tx: &Tx<'_>, id: &str, revision: i64, now: i64) -> Result<bool> {
    Ok(tx.execute("UPDATE runtime_attempts SET cancel_requested=1,revision=revision+1,updated=? WHERE attempt_id=? AND revision=? AND state<>'finished'",params![now,id,revision])?==1)
}
pub struct TurnUpdate<'a> {
    pub state: &'a str,
    pub active_attempt: Option<&'a str>,
    pub retry_ordinal: i64,
    pub applied: bool,
}
pub fn turn_cas_in(
    tx: &Tx<'_>,
    turn: &str,
    revision: i64,
    u: &TurnUpdate<'_>,
    now: i64,
) -> Result<bool> {
    if !valid_execution_state(u.state) || u.retry_ordinal < 0 {
        return Err(conflict("invalid turn state"));
    }
    if let Some(a) = u.active_attempt {
        let valid: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM runtime_attempts WHERE attempt_id=? AND turn_id=?)",
            params![a, turn],
            |r| r.get(0),
        )?;
        if !valid {
            return Err(conflict("attempt belongs to another turn"));
        }
    }
    if u.state == "finished" {
        let live: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM runtime_attempts WHERE turn_id=? AND state<>'finished')",
            [turn],
            |r| r.get(0),
        )?;
        if live {
            return Err(conflict("turn still has an unresolved attempt"));
        }
    }
    Ok(tx.execute("UPDATE runtime_turns SET state=?,active_attempt=?,retry_ordinal=?,applied=?,revision=revision+1,updated=? WHERE turn_id=? AND revision=? AND state<>'finished' AND (state<>'unknown' OR ? IN ('unknown','finished'))",
        params![u.state,u.active_attempt,u.retry_ordinal,u.applied,now,turn,revision,u.state])?==1)
}
/// Node receipts are scoped to the claimed logical turn, never all rows of a session.
pub fn dispatch_receipt_in(tx: &Tx<'_>, turn: &str, state: &str, now: i64) -> Result<usize> {
    if ![
        "submitted",
        "completed",
        "interrupted",
        "cancelled",
        "error",
    ]
    .contains(&state)
    {
        return Err(conflict("invalid dispatch receipt"));
    }
    Ok(tx.execute("UPDATE session_dispatch SET state=?,updated=? WHERE turn_id=? AND backend='node' AND (state='claimed' OR (state='submitted' AND ?<>'submitted'))",params![state,now,turn,state])?)
}

pub struct NewCommand<'a> {
    pub operation: &'a str,
    pub node: &'a str,
    pub method: &'a str,
    pub request: &'a Value,
    pub digest: &'a str,
    pub target_attempt: Option<&'a str>,
    pub dependency: Option<&'a str>,
}
/// Duplicate operation IDs must carry exactly the same immutable envelope, including method/target.
pub fn insert_command_in(tx: &Tx<'_>, c: &NewCommand<'_>, now: i64) -> Result<bool> {
    if c.dependency == Some(c.operation) {
        return Err(conflict("self-dependent command"));
    }
    let request = to_json(c.request)?;
    let same: Option<bool>=tx.query_row("SELECT node_id=? AND method=? AND request=? AND digest=? AND target_attempt IS ? AND dependency_operation IS ? FROM node_commands WHERE operation_id=?",
        params![c.node,c.method,request,c.digest,c.target_attempt,c.dependency,c.operation],|r|r.get(0)).optional()?;
    if let Some(same) = same {
        if !same {
            return Err(conflict("operation identity reused"));
        }
        return Ok(false);
    }
    if let Some(a) = c.target_attempt {
        let valid: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM runtime_attempts WHERE attempt_id=? AND node_id=?)",
            params![a, c.node],
            |r| r.get(0),
        )?;
        if !valid {
            return Err(conflict("command targets another Node's attempt"));
        }
    }
    tx.execute("INSERT INTO node_commands(operation_id,node_id,method,request,digest,target_attempt,dependency_operation,created,updated) VALUES (?,?,?,?,?,?,?,?,?)",
        params![c.operation,c.node,c.method,request,c.digest,c.target_attempt,c.dependency,now,now])?;
    Ok(true)
}

#[derive(Debug, Clone)]
pub struct CommandLease {
    pub operation: String,
    pub node: String,
    pub method: String,
    pub request: Value,
    pub digest: String,
    pub owner: String,
    pub generation: i64,
    pub until: i64,
    /// An earlier send may have reached the Node (`maybe_sent` before this lease set it).
    pub maybe_sent: bool,
}
/// Leases are worker fences, not execution identities. Only pending commands are sent. Accepted or
/// unknown commands require operation lookup/reconciliation, not a fresh execution. A dependency
/// must have a successful final receipt; the caller explicitly sets success via merge_receipt_in.
/// Only `node`'s commands: one coordinator per Node shares the store with the others.
pub fn lease_command_in(
    tx: &Tx<'_>,
    node: &str,
    owner: &str,
    now: i64,
    until: i64,
) -> Result<Option<CommandLease>> {
    if owner.is_empty() || until <= now {
        return Err(conflict("invalid lease"));
    }
    let row: Option<(String,String,String,String,String,i64,bool)>=tx.query_row(
        "SELECT c.operation_id,c.node_id,c.method,c.request,c.digest,c.lease_generation,c.maybe_sent FROM node_commands c WHERE c.node_id=? AND c.state='pending' AND c.retry_at<=? AND (c.lease_until IS NULL OR c.lease_until<=?) AND (c.dependency_operation IS NULL OR EXISTS(SELECT 1 FROM node_commands d WHERE d.operation_id=c.dependency_operation AND d.state='finished' AND json_extract(d.receipt,'$.success')=1)) ORDER BY c.created,c.rowid LIMIT 1",
        params![node,now,now],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?))).optional()?;
    let Some((operation, node, method, request, digest, generation, maybe_sent)) = row else {
        return Ok(None);
    };
    let generation = generation
        .checked_add(1)
        .ok_or_else(|| conflict("lease generation overflow"))?;
    tx.execute("UPDATE node_commands SET lease_owner=?,lease_until=?,lease_generation=?,maybe_sent=1,updated=? WHERE operation_id=?",params![owner,until,generation,now,operation])?;
    Ok(Some(CommandLease {
        operation,
        node,
        method,
        request: serde_json::from_str(&request)?,
        digest,
        owner: owner.into(),
        generation,
        until,
        maybe_sent,
    }))
}
/// Retry only after a transport failure known not to be a final Node rejection. Uncertain sends use
/// the same operation ID/request. A stale worker cannot reschedule a reclaimed command.
/// `uncertain`: this send may have reached the Node (OutcomeUnknown); otherwise it was refused
/// before acceptance. `maybe_sent` stays 1 once any send was uncertain: a later refusal says
/// nothing about the earlier send, which may still arrive. It goes back to 0 only while every send
/// so far was refused.
pub fn retry_command_in(
    tx: &Tx<'_>,
    lease: &CommandLease,
    now: i64,
    retry_at: i64,
    uncertain: bool,
) -> Result<bool> {
    let maybe_sent = lease.maybe_sent || uncertain;
    Ok(tx.execute("UPDATE node_commands SET retry_at=?,lease_owner=NULL,lease_until=NULL,maybe_sent=?,updated=? WHERE operation_id=? AND state='pending' AND lease_owner=? AND lease_generation=? AND lease_until>?",
        params![retry_at,maybe_sent as i64,now,lease.operation,lease.owner,lease.generation,now])?==1)
}

/// Receipt progression deliberately collapses running/waiting to Active: their order is determined
/// by the event stream and projection CAS, not unordered RPC replies. Rejected means *not accepted*.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiptState {
    Accepted,
    Active,
    Unknown,
    Finished,
    Rejected,
}
impl ReceiptState {
    fn name(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Active => "active",
            Self::Unknown => "unknown",
            Self::Finished => "finished",
            Self::Rejected => "rejected",
        }
    }
}
/// `success` is explicit (not inferred from transport success); payload retains the full Node receipt.
/// Terminal contradictions are conflicts. Late earlier receipts are ignored. The same terminal is
/// idempotent; a distinct terminal payload is surfaced for reconciliation, never silently overwritten.
pub fn merge_receipt_in(
    tx: &Tx<'_>,
    operation: &str,
    state: ReceiptState,
    success: bool,
    payload: &Value,
    now: i64,
) -> Result<bool> {
    if success && state != ReceiptState::Finished {
        return Err(conflict("only a finished receipt can succeed"));
    }
    let (old, receipt): (String, Option<String>) = tx.query_row(
        "SELECT state,receipt FROM node_commands WHERE operation_id=?",
        [operation],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let encoded = to_json(&serde_json::json!({"success":success,"payload":payload}))?;
    let next = state.name();
    if ["finished", "rejected"].contains(&old.as_str()) {
        if next == old && receipt.as_deref() == Some(&encoded) {
            return Ok(false);
        }
        if ["finished", "rejected"].contains(&next) {
            return Err(conflict("contradictory terminal receipt"));
        }
        return Ok(false);
    }
    if state == ReceiptState::Rejected && old != "pending" {
        return Err(conflict(
            "accepted operation cannot be rejected as never accepted",
        ));
    }
    let rank = |s: &str| match s {
        "pending" => 0,
        "accepted" => 1,
        "active" => 2,
        "unknown" => 3,
        _ => 4,
    };
    if rank(next) <= rank(&old) {
        return Ok(false);
    }
    tx.execute("UPDATE node_commands SET state=?,receipt=?,revision=revision+1,lease_owner=NULL,lease_until=NULL,updated=? WHERE operation_id=?",params![next,encoded,now,operation])?;
    Ok(true)
}

#[derive(Debug, Clone, Copy)]
pub struct StreamKey<'a> {
    pub node: &'a str,
    pub stream: &'a str,
    pub epoch: &'a str,
}
#[derive(Debug, PartialEq, Eq)]
pub struct Watermarks {
    pub durable: i64,
    pub applied: i64,
}
pub fn watermarks_in(tx: &Tx<'_>, k: StreamKey<'_>) -> Result<Watermarks> {
    Ok(tx.query_row(
        "SELECT durable,applied FROM node_streams WHERE node_id=? AND stream=? AND epoch=?",
        params![k.node, k.stream, k.epoch],
        |r| {
            Ok(Watermarks {
                durable: r.get(0)?,
                applied: r.get(1)?,
            })
        },
    )?)
}
/// What became of one event offered to the inbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Inboxed {
    /// Stored for apply.
    New,
    /// Already held (or applied and pruned since): nothing changed.
    Seen,
    /// It contradicts what the Hub holds. Nothing was stored: the caller sets it aside
    /// ([`quarantine_in`]) or refuses it.
    Conflict(Conflict),
}

/// How an event contradicts the inbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Conflict {
    /// Another event (id or payload) already sits at its stream position.
    Position,
    /// Its id is already held at another stream position.
    EventId,
}

impl Conflict {
    pub fn reason(self) -> &'static str {
        match self {
            Conflict::Position => "different event at the same stream position",
            Conflict::EventId => "event id reused at another stream position",
        }
    }
}

/// Exact JSON text equality is intentional: never silently accept two encodings with differing
/// fields under one source identity. The producer must resend its immutable envelope.
pub fn inbox_insert_in(
    tx: &Tx<'_>,
    k: StreamKey<'_>,
    seq: i64,
    event_id: &str,
    payload: &Value,
    now: i64,
) -> Result<Inboxed> {
    if seq <= 0 {
        return Err(conflict("sequence must be positive"));
    }
    let payload = to_json(payload)?;
    let existing:Option<(String,String)>=tx.query_row("SELECT event_id,payload FROM node_event_inbox WHERE node_id=? AND stream=? AND epoch=? AND seq=?",params![k.node,k.stream,k.epoch,seq],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    if let Some(old) = existing {
        if old != (event_id.into(), payload) {
            return Ok(Inboxed::Conflict(Conflict::Position));
        }
        return Ok(Inboxed::Seen);
    }
    // no row at or below the durable prefix: it was applied and pruned since, and this is a
    // redelivery. It is never stored again (it would be applied twice); its payload can no longer be
    // compared, and a fresh journal never reuses a position (it gets a new epoch).
    let durable: Option<i64> = tx
        .query_row(
            "SELECT durable FROM node_streams WHERE node_id=? AND stream=? AND epoch=?",
            params![k.node, k.stream, k.epoch],
            |r| r.get(0),
        )
        .optional()?;
    if durable.is_some_and(|d| seq <= d) {
        return Ok(Inboxed::Seen);
    }
    let reused: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM node_event_inbox WHERE node_id=? AND event_id=?)",
        params![k.node, event_id],
        |r| r.get(0),
    )?;
    if reused {
        return Ok(Inboxed::Conflict(Conflict::EventId));
    }
    tx.execute(
        "INSERT OR IGNORE INTO node_streams(node_id,stream,epoch) VALUES (?,?,?)",
        params![k.node, k.stream, k.epoch],
    )?;
    tx.execute("INSERT INTO node_event_inbox(node_id,stream,epoch,seq,event_id,payload,received) VALUES (?,?,?,?,?,?,?)",params![k.node,k.stream,k.epoch,seq,event_id,payload,now])?;
    let mut durable = watermarks_in(tx, k)?.durable;
    while let Some(next) = durable.checked_add(1) {
        let exists:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM node_event_inbox WHERE node_id=? AND stream=? AND epoch=? AND seq=?)",params![k.node,k.stream,k.epoch,next],|r|r.get(0))?;
        if !exists {
            break;
        }
        durable = next;
    }
    tx.execute(
        "UPDATE node_streams SET durable=? WHERE node_id=? AND stream=? AND epoch=?",
        params![durable, k.node, k.stream, k.epoch],
    )?;
    Ok(Inboxed::New)
}

/// Why a stream's apply stopped until reconciliation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    Conflict,
    Gap,
    Ahead,
}

impl Fault {
    fn name(self) -> &'static str {
        match self {
            Fault::Conflict => "conflict",
            Fault::Gap => "gap",
            Fault::Ahead => "ahead",
        }
    }
    fn parse(s: &str) -> Option<Fault> {
        Some(match s {
            "conflict" => Fault::Conflict,
            "gap" => Fault::Gap,
            "ahead" => Fault::Ahead,
            _ => return None,
        })
    }
}

/// What [`quarantine_in`] changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quarantined {
    /// The event was not held yet.
    pub new: bool,
    /// Its stream was not faulted before.
    pub faulted: bool,
}

/// Keeps a conflicting event aside (never applied, never pruned: once it is acknowledged the Node
/// drops its copy) and faults its stream, in the caller's transaction.
pub fn quarantine_in(
    tx: &Tx<'_>,
    k: StreamKey<'_>,
    seq: i64,
    event_id: &str,
    payload: &Value,
    reason: Conflict,
    now: i64,
) -> Result<Quarantined> {
    if seq <= 0 {
        return Err(conflict("sequence must be positive"));
    }
    tx.execute(
        "INSERT OR IGNORE INTO node_streams(node_id,stream,epoch) VALUES (?,?,?)",
        params![k.node, k.stream, k.epoch],
    )?;
    let n = tx.execute(
        "INSERT OR IGNORE INTO node_event_quarantine(node_id,stream,epoch,seq,event_id,payload,reason,received) VALUES (?,?,?,?,?,?,?,?)",
        params![k.node, k.stream, k.epoch, seq, event_id, to_json(payload)?, reason.reason(), now],
    )?;
    let faulted = stream_fault_in(tx, k, Fault::Conflict)?;
    Ok(Quarantined {
        new: n > 0,
        faulted,
    })
}

/// Marks a stream faulted: its events stay but are no longer applied ([`next_event_in`]) until
/// reconciliation clears it. The first fault is kept. True: the stream was not faulted before.
pub fn stream_fault_in(tx: &Tx<'_>, k: StreamKey<'_>, fault: Fault) -> Result<bool> {
    tx.execute(
        "INSERT OR IGNORE INTO node_streams(node_id,stream,epoch) VALUES (?,?,?)",
        params![k.node, k.stream, k.epoch],
    )?;
    Ok(tx.execute(
        "UPDATE node_streams SET fault=? WHERE node_id=? AND stream=? AND epoch=? AND fault IS NULL",
        params![fault.name(), k.node, k.stream, k.epoch],
    )? > 0)
}

/// A stream's fault, if any (`None` too for a stream the Hub never heard of).
pub fn stream_fault_of(c: &rusqlite::Connection, k: StreamKey<'_>) -> Result<Option<Fault>> {
    let f: Option<Option<String>> = c
        .query_row(
            "SELECT fault FROM node_streams WHERE node_id=? AND stream=? AND epoch=?",
            params![k.node, k.stream, k.epoch],
            |r| r.get(0),
        )
        .optional()?;
    Ok(f.flatten().as_deref().and_then(Fault::parse))
}

/// What the Hub can acknowledge on a stream: the durable prefix, extended through positions held
/// only in quarantine (and what follows them in the inbox). Acknowledging past `durable` is safe
/// because the quarantined copy is durable; apply still stops at `durable` (and at a fault).
pub fn acked_in(tx: &Tx<'_>, k: StreamKey<'_>) -> Result<i64> {
    let mut acked = watermarks_in(tx, k)?.durable;
    while let Some(next) = acked.checked_add(1) {
        let held: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM node_event_inbox WHERE node_id=?1 AND stream=?2 AND epoch=?3 AND seq=?4)
                 OR EXISTS(SELECT 1 FROM node_event_quarantine WHERE node_id=?1 AND stream=?2 AND epoch=?3 AND seq=?4)",
            params![k.node, k.stream, k.epoch, next],
            |r| r.get(0),
        )?;
        if !held {
            break;
        }
        acked = next;
    }
    Ok(acked)
}

/// Apply the next contiguous event only, in the SAME transaction as its projections/events/stats.
/// Calling this before projections is safe only if errors are propagated and the transaction rolls back.
pub fn mark_applied_in(tx: &Tx<'_>, k: StreamKey<'_>, seq: i64) -> Result<bool> {
    let w = watermarks_in(tx, k)?;
    if seq <= 0 {
        return Err(conflict("sequence must be positive"));
    }
    if seq <= w.applied {
        return Ok(false);
    }
    if w.applied.checked_add(1) != Some(seq) || seq > w.durable {
        return Err(conflict("apply must follow the contiguous durable prefix"));
    }
    tx.execute(
        "UPDATE node_event_inbox SET applied=1 WHERE node_id=? AND stream=? AND epoch=? AND seq=?",
        params![k.node, k.stream, k.epoch, seq],
    )?;
    tx.execute(
        "UPDATE node_streams SET applied=? WHERE node_id=? AND stream=? AND epoch=?",
        params![seq, k.node, k.stream, k.epoch],
    )?;
    Ok(true)
}

/// The next event eligible for ordered apply. Payload is still the original Node envelope.
/// Read it and mark it applied within one transaction together with all projections.
/// A faulted stream has none: its apply waits for reconciliation.
#[derive(Debug)]
pub struct PendingEvent {
    pub seq: i64,
    pub event_id: String,
    pub payload: Value,
}
pub fn next_event_in(tx: &Tx<'_>, k: StreamKey<'_>) -> Result<Option<PendingEvent>> {
    let row: Option<(i64,String,String)> = tx.query_row(
        "SELECT e.seq,e.event_id,e.payload FROM node_event_inbox e JOIN node_streams s USING(node_id,stream,epoch) WHERE e.node_id=? AND e.stream=? AND e.epoch=? AND e.seq=s.applied+1 AND e.seq<=s.durable AND s.fault IS NULL",
        params![k.node,k.stream,k.epoch],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
    row.map(|(seq, event_id, payload)| {
        Ok(PendingEvent {
            seq,
            event_id,
            payload: serde_json::from_str(&payload)?,
        })
    })
    .transpose()
}

/// For reconciliation after a crash or a response lost in transit. A receipt's JSON wrapper has
/// `success` plus the original Node `payload`; it is not a new wire protocol.
#[derive(Debug)]
pub struct CommandRecord {
    pub operation: String,
    pub node: String,
    pub method: String,
    pub request: Value,
    pub digest: String,
    pub state: String,
    pub receipt: Option<Value>,
    pub revision: i64,
}
pub fn command_in(tx: &Tx<'_>, operation: &str) -> Result<Option<CommandRecord>> {
    let row = tx.query_row("SELECT node_id,method,request,digest,state,receipt,revision FROM node_commands WHERE operation_id=?",[operation],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,Option<String>>(5)?,r.get::<_,i64>(6)?))).optional()?;
    row.map(
        |(node, method, request, digest, state, receipt, revision)| {
            Ok(CommandRecord {
                operation: operation.into(),
                node,
                method,
                request: serde_json::from_str(&request)?,
                digest,
                state,
                receipt: receipt.map(|s| serde_json::from_str(&s)).transpose()?,
                revision,
            })
        },
    )
    .transpose()
}

/// Where a session event came from on a Node: its stream position is its identity.
#[derive(Debug, Clone, Copy)]
pub struct EventSource<'a> {
    pub node: &'a str,
    pub stream: &'a str,
    pub epoch: &'a str,
    pub seq: i64,
}

/// [`crate::Events::add`] inside the caller's transaction, for a given `format`: scrubbed, split into
/// payloads, published after the commit. With a source, the same Node event again is a no-op
/// (`None`).
pub fn add_event_in(
    tx: &Tx<'_>,
    session: &str,
    kind: &str,
    mut data: Value,
    format: &str,
    source: Option<EventSource<'_>>,
    now: i64,
) -> Result<Option<i64>> {
    steve_vault::redact::scrub(session, &mut data);
    let (stored, payloads) = crate::event_payload::split(kind, &data)?;
    let stored = to_json(&stored)?;
    let inserted = tx.execute(
        "INSERT OR IGNORE INTO events (session_id, ts, kind, data, format, source_node, source_stream, source_epoch, source_seq) VALUES (?,?,?,?,?,?,?,?,?)",
        params![
            session,
            now,
            kind,
            stored,
            format,
            source.map(|s| s.node),
            source.map(|s| s.stream),
            source.map(|s| s.epoch),
            source.map(|s| s.seq)
        ],
    )?;
    if inserted == 0 {
        return Ok(None);
    }
    let seq = tx.last_insert_rowid();
    crate::event_payload::save(tx, session, seq, &payloads)?;
    tx.publish(crate::ChangeEvent::Event {
        session_id: session.into(),
        seq,
        kind: kind.into(),
        data,
    });
    Ok(Some(seq))
}

/// A Node session's desired harness config (applied at its next idle boundary), as the coordinator
/// wrote it.
pub fn desired_config_in(tx: &Tx<'_>, session: &str) -> Result<Option<Value>> {
    let v: Option<Option<String>> = tx
        .query_row(
            "SELECT desired_config FROM runtime_sessions WHERE session_id=?",
            [session],
            |r| r.get(0),
        )
        .optional()?;
    v.flatten()
        .map(|s| serde_json::from_str(&s).map_err(StoreError::from))
        .transpose()
}

/// Sets or clears the desired config. Leaves the session's revision alone: it is not a phase change.
pub fn set_desired_config_in(
    tx: &Tx<'_>,
    session: &str,
    v: Option<&Value>,
    now: i64,
) -> Result<()> {
    let v = v.map(to_json).transpose()?;
    tx.execute(
        "UPDATE runtime_sessions SET desired_config=?, updated=? WHERE session_id=?",
        params![v, now, session],
    )?;
    Ok(())
}

/// A Node's permission request as the Hub holds it, with its card.
pub struct NewPermission<'a> {
    pub decision: &'a str,
    pub node: &'a str,
    pub session: &'a str,
    pub attempt: &'a str,
    pub permission: &'a str,
    pub request: &'a Value,
    pub expires_at: i64,
}

/// Inserts the request as `waiting`. False: this Node's attempt already has a request of that id.
pub fn insert_permission_in(tx: &Tx<'_>, p: &NewPermission<'_>, now: i64) -> Result<bool> {
    Ok(tx.execute(
        "INSERT INTO runtime_permissions(decision_id,node_id,session_id,attempt_id,permission_id,request,expires_at,state,created,updated)
           VALUES (?,?,?,?,?,?,?,'waiting',?,?) ON CONFLICT(node_id,attempt_id,permission_id) DO NOTHING",
        params![p.decision, p.node, p.session, p.attempt, p.permission, to_json(p.request)?, p.expires_at, now, now],
    )? == 1)
}

#[derive(Debug, Clone)]
pub struct PermissionRow {
    pub decision: String,
    pub node: String,
    pub session: String,
    pub attempt: String,
    pub permission: String,
    pub request: Value,
    pub expires_at: i64,
    pub state: String,
    pub answer_operation: Option<String>,
    pub revision: i64,
}

const PERMISSION: &str = "SELECT decision_id,node_id,session_id,attempt_id,permission_id,request,expires_at,state,answer_operation,revision FROM runtime_permissions";

fn permissions(tx: &Tx<'_>, filter: &str, p: impl rusqlite::Params) -> Result<Vec<PermissionRow>> {
    let mut st = tx.prepare_cached(&format!("{PERMISSION} {filter}"))?;
    let rows = st
        .query_map(p, |r| {
            Ok((
                (
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                ),
                (
                    r.get::<_, String>(5)?,
                    r.get::<_, i64>(6)?,
                    r.get::<_, String>(7)?,
                    r.get::<_, Option<String>>(8)?,
                    r.get::<_, i64>(9)?,
                ),
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter()
        .map(
            |(
                (decision, node, session, attempt, permission),
                (request, expires_at, state, answer_operation, revision),
            )| {
                Ok(PermissionRow {
                    decision,
                    node,
                    session,
                    attempt,
                    permission,
                    request: serde_json::from_str(&request)?,
                    expires_at,
                    state,
                    answer_operation,
                    revision,
                })
            },
        )
        .collect()
}

pub fn permission_in(
    tx: &Tx<'_>,
    node: &str,
    attempt: &str,
    permission: &str,
) -> Result<Option<PermissionRow>> {
    Ok(permissions(
        tx,
        "WHERE node_id=? AND attempt_id=? AND permission_id=?",
        params![node, attempt, permission],
    )?
    .pop())
}

pub fn permission_of_decision_in(tx: &Tx<'_>, decision: &str) -> Result<Option<PermissionRow>> {
    Ok(permissions(tx, "WHERE decision_id=?", [decision])?.pop())
}

pub fn permission_of_answer_in(tx: &Tx<'_>, operation: &str) -> Result<Option<PermissionRow>> {
    Ok(permissions(tx, "WHERE answer_operation=?", [operation])?.pop())
}

/// Moves a request to `state` if it is still at `revision`; `answer_operation` is kept when `None`.
pub fn permission_cas_in(
    tx: &Tx<'_>,
    decision: &str,
    revision: i64,
    state: &str,
    answer_operation: Option<&str>,
    now: i64,
) -> Result<bool> {
    Ok(tx.execute(
        "UPDATE runtime_permissions SET state=?, answer_operation=COALESCE(?,answer_operation), revision=revision+1, updated=? WHERE decision_id=? AND revision=?",
        params![state, answer_operation, now, decision, revision],
    )? == 1)
}

/// `node`'s waiting requests whose expiry has come, oldest first.
pub fn due_permissions_in(
    tx: &Tx<'_>,
    node: &str,
    now: i64,
    limit: i64,
) -> Result<Vec<PermissionRow>> {
    permissions(
        tx,
        "WHERE node_id=? AND state='waiting' AND expires_at<=? ORDER BY expires_at LIMIT ?",
        params![node, now, limit],
    )
}

/// An attempt's requests that are still open (waiting or answering).
pub fn permissions_of_attempt_in(tx: &Tx<'_>, attempt: &str) -> Result<Vec<PermissionRow>> {
    permissions(
        tx,
        "WHERE attempt_id=? AND state IN ('waiting','answering') ORDER BY created, rowid",
        [attempt],
    )
}
