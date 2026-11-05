//! Hub-owned Node jobs: a think call or a research run, and its attempts. One attempt runs one
//! model on the job's Node; a fallback is the next attempt, never a rewritten one. Every write uses
//! the caller's transaction, as in [`super::runtime`]: the job, its attempt and the `start_job`
//! command commit together, and errors roll all of them back.
//!
//! The spec, request, receipt and completion JSON are kept verbatim; the coordinator validates
//! the protocol types and digests. Identity, the one open attempt per job, the ordinal sequence and
//! monotonic progress are enforced here. Only written when the Node job path is on.
use rusqlite::{OptionalExtension, params};
use serde_json::Value;

use super::to_json;
use crate::{Result, StoreError, Tx};

fn conflict(detail: &str) -> StoreError {
    StoreError::Conflict(detail.into())
}

/// The method of the command that starts an attempt.
pub const START_JOB: &str = "start_job";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobKind {
    Think,
    Research,
}
impl JobKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Think => "think",
            Self::Research => "research",
        }
    }
    fn parse(s: &str) -> Result<Self> {
        match s {
            "think" => Ok(Self::Think),
            "research" => Ok(Self::Research),
            _ => Err(conflict("unknown job kind")),
        }
    }
}

/// Immutable job identity. `purpose` is the caller's tag (the call tag or `research`).
pub struct NewJob<'a> {
    pub id: &'a str,
    pub kind: JobKind,
    pub purpose: &'a str,
    pub workspace: &'a str,
    pub node: &'a str,
    pub thread_run: Option<&'a str>,
}

/// Immutable attempt identity and frozen spec. The attempt runs on its job's Node.
pub struct NewJobAttempt<'a> {
    pub id: &'a str,
    pub ordinal: i64,
    pub model: &'a str,
    pub operation: &'a str,
    pub execution: &'a Value,
    pub spec: &'a Value,
    pub digest: &'a str,
}

/// The `start_job` envelope sent under the attempt's operation id.
pub struct StartJob<'a> {
    pub request: &'a Value,
    pub digest: &'a str,
}

/// Creates the job with its first attempt (ordinal 0) and that attempt's `start_job` command.
/// The same job, attempt and command again is a no-op (false); a different identity under a used id
/// is a conflict.
pub fn create_job_in(
    tx: &Tx<'_>,
    job: &NewJob<'_>,
    attempt: &NewJobAttempt<'_>,
    start: &StartJob<'_>,
    now: i64,
) -> Result<bool> {
    let old: Option<bool> = tx
        .query_row(
            "SELECT kind=? AND purpose=? AND workspace_id=? AND node_id=? AND thread_run_id IS ? FROM runtime_jobs WHERE job_id=?",
            params![job.kind.name(), job.purpose, job.workspace, job.node, job.thread_run, job.id],
            |r| r.get(0),
        )
        .optional()?;
    let created = match old {
        Some(false) => return Err(conflict("job identity reused")),
        Some(true) => false,
        None => {
            tx.execute(
                "INSERT INTO runtime_jobs(job_id,kind,purpose,workspace_id,node_id,thread_run_id,created,updated) VALUES (?,?,?,?,?,?,?,?)",
                params![job.id, job.kind.name(), job.purpose, job.workspace, job.node, job.thread_run, now, now],
            )?;
            true
        }
    };
    if attempt.ordinal != 0 {
        return Err(conflict("a job starts with attempt 0"));
    }
    Ok(insert_attempt(tx, job.id, attempt, start, now)? || created)
}

/// Adds the next attempt of an open job (the Hub's fallback to another model) with its
/// `start_job` command. The previous attempt must be finished and the ordinal must follow it.
pub fn next_attempt_in(
    tx: &Tx<'_>,
    job: &str,
    attempt: &NewJobAttempt<'_>,
    start: &StartJob<'_>,
    now: i64,
) -> Result<bool> {
    insert_attempt(tx, job, attempt, start, now)
}

fn insert_attempt(
    tx: &Tx<'_>,
    job: &str,
    a: &NewJobAttempt<'_>,
    start: &StartJob<'_>,
    now: i64,
) -> Result<bool> {
    let execution = to_json(a.execution)?;
    let spec = to_json(a.spec)?;
    let (node, state): (String, String) = tx
        .query_row(
            "SELECT node_id,state FROM runtime_jobs WHERE job_id=?",
            [job],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
        .ok_or_else(|| conflict("attempt of an unknown job"))?;
    let same: Option<bool> = tx
        .query_row(
            "SELECT job_id=? AND ordinal=? AND model=? AND node_id=? AND operation_id=? AND execution=? AND spec=? AND digest=? FROM runtime_job_attempts WHERE attempt_id=?",
            params![job, a.ordinal, a.model, node, a.operation, execution, spec, a.digest, a.id],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(same) = same {
        if !same {
            return Err(conflict("job attempt identity reused"));
        }
        // the command went in with the attempt; a resend must carry the same envelope
        insert_job_command_in(tx, &job_command(a, start), now)?;
        return Ok(false);
    }
    if state != "open" {
        return Err(conflict("job is finished"));
    }
    let (open, next): (bool, i64) = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM runtime_job_attempts WHERE job_id=?1 AND state<>'finished'),
                COALESCE((SELECT MAX(ordinal)+1 FROM runtime_job_attempts WHERE job_id=?1),0)",
        [job],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if open {
        return Err(conflict("job already has an open attempt"));
    }
    if a.ordinal != next {
        return Err(conflict("job attempt ordinal out of sequence"));
    }
    tx.execute(
        "INSERT INTO runtime_job_attempts(attempt_id,job_id,ordinal,model,node_id,operation_id,execution,spec,digest,created,updated) VALUES (?,?,?,?,?,?,?,?,?,?,?)",
        params![a.id, job, a.ordinal, a.model, node, a.operation, execution, spec, a.digest, now, now],
    )?;
    tx.execute(
        "UPDATE runtime_jobs SET updated=? WHERE job_id=?",
        params![now, job],
    )?;
    insert_job_command_in(tx, &job_command(a, start), now)?;
    Ok(true)
}

fn job_command<'a>(a: &NewJobAttempt<'a>, start: &StartJob<'a>) -> NewJobCommand<'a> {
    NewJobCommand {
        operation: a.operation,
        attempt: a.id,
        method: START_JOB,
        request: start.request,
        digest: start.digest,
    }
}

/// A command about a job attempt (its `start_job`, or a cancel by operation id), sent to the
/// attempt's Node.
pub struct NewJobCommand<'a> {
    pub operation: &'a str,
    pub attempt: &'a str,
    pub method: &'a str,
    pub request: &'a Value,
    pub digest: &'a str,
}

/// Queues the command as `pending`. A duplicate operation id must carry exactly the same envelope.
pub fn insert_job_command_in(tx: &Tx<'_>, c: &NewJobCommand<'_>, now: i64) -> Result<bool> {
    let request = to_json(c.request)?;
    let node: String = tx
        .query_row(
            "SELECT node_id FROM runtime_job_attempts WHERE attempt_id=?",
            [c.attempt],
            |r| r.get(0),
        )
        .optional()?
        .ok_or_else(|| conflict("command targets an unknown job attempt"))?;
    let same: Option<bool> = tx
        .query_row(
            "SELECT node_id=? AND method=? AND request=? AND digest=? AND target_job_attempt IS ? AND target_attempt IS NULL AND dependency_operation IS NULL FROM node_commands WHERE operation_id=?",
            params![node, c.method, request, c.digest, c.attempt, c.operation],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(same) = same {
        if !same {
            return Err(conflict("operation identity reused"));
        }
        return Ok(false);
    }
    tx.execute(
        "INSERT INTO node_commands(operation_id,node_id,method,request,digest,target_job_attempt,created,updated) VALUES (?,?,?,?,?,?,?,?)",
        params![c.operation, node, c.method, request, c.digest, c.attempt, now, now],
    )?;
    Ok(true)
}

/// Where an attempt stands. Progress only moves forward: a late fact never undoes a newer one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AttemptState {
    Planned,
    Accepted,
    Running,
    /// The Node cannot say what became of it (a restart); only a terminal fact closes it.
    Unknown,
    Finished,
}
impl AttemptState {
    pub fn name(self) -> &'static str {
        match self {
            Self::Planned => "planned",
            Self::Accepted => "accepted",
            Self::Running => "running",
            Self::Unknown => "unknown",
            Self::Finished => "finished",
        }
    }
    fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "planned" => Self::Planned,
            "accepted" => Self::Accepted,
            "running" => Self::Running,
            "unknown" => Self::Unknown,
            "finished" => Self::Finished,
            _ => return Err(conflict("unknown job attempt state")),
        })
    }
}

/// Applies a receipt or an event's fact to an attempt. An earlier state than the stored one is
/// ignored (its receipt is still kept if none was recorded). A completion comes only with
/// `Finished`; the same completion again is a no-op and a different one is a conflict. True when
/// the row changed.
pub fn apply_attempt_in(
    tx: &Tx<'_>,
    attempt: &str,
    state: AttemptState,
    receipt: Option<&Value>,
    completion: Option<&Value>,
    now: i64,
) -> Result<bool> {
    if completion.is_some() && state != AttemptState::Finished {
        return Err(conflict("only a finished attempt has a completion"));
    }
    let (old, old_receipt, old_completion): (String, Option<String>, Option<String>) = tx
        .query_row(
            "SELECT state,receipt,completion FROM runtime_job_attempts WHERE attempt_id=?",
            [attempt],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?
        .ok_or_else(|| conflict("unknown job attempt"))?;
    let old = AttemptState::parse(&old)?;
    let receipt = receipt.map(to_json).transpose()?;
    let completion = completion.map(to_json).transpose()?;
    if old == AttemptState::Finished {
        if state == AttemptState::Finished
            && completion.is_some()
            && old_completion.is_some()
            && completion != old_completion
        {
            return Err(conflict("contradictory job completion"));
        }
        if state != AttemptState::Finished || completion.is_none() || old_completion.is_some() {
            return Ok(false);
        }
    } else if state <= old {
        if receipt.is_none() || old_receipt.is_some() {
            return Ok(false);
        }
        tx.execute(
            "UPDATE runtime_job_attempts SET receipt=?,revision=revision+1,updated=? WHERE attempt_id=?",
            params![receipt, now, attempt],
        )?;
        return Ok(true);
    }
    tx.execute(
        "UPDATE runtime_job_attempts SET state=?,receipt=COALESCE(receipt,?),completion=COALESCE(completion,?),revision=revision+1,updated=? WHERE attempt_id=?",
        params![state.name(), receipt, completion, now, attempt],
    )?;
    Ok(true)
}

/// Persists the intent to cancel an open attempt before the cancel is sent. False when it is
/// already finished or already asked to cancel.
pub fn cancel_attempt_in(tx: &Tx<'_>, attempt: &str, now: i64) -> Result<bool> {
    Ok(tx.execute(
        "UPDATE runtime_job_attempts SET cancel_requested=1,revision=revision+1,updated=? WHERE attempt_id=? AND state<>'finished' AND cancel_requested=0",
        params![now, attempt],
    )? == 1)
}

/// Closes a job once none of its attempts is open. False when it was already finished.
pub fn finish_job_in(tx: &Tx<'_>, job: &str, now: i64) -> Result<bool> {
    let open: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM runtime_job_attempts WHERE job_id=? AND state<>'finished')",
        [job],
        |r| r.get(0),
    )?;
    if open {
        return Err(conflict("job still has an open attempt"));
    }
    Ok(tx.execute(
        "UPDATE runtime_jobs SET state='finished',updated=? WHERE job_id=? AND state='open'",
        params![now, job],
    )? == 1)
}

#[derive(Debug, Clone, PartialEq)]
pub struct JobRow {
    pub id: String,
    pub kind: JobKind,
    pub purpose: String,
    pub workspace: String,
    pub node: String,
    pub thread_run: Option<String>,
    pub finished: bool,
    pub created: i64,
    pub updated: i64,
}

pub fn job_in(tx: &Tx<'_>, job: &str) -> Result<Option<JobRow>> {
    let row = tx
        .query_row(
            "SELECT kind,purpose,workspace_id,node_id,thread_run_id,state,created,updated FROM runtime_jobs WHERE job_id=?",
            [job],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, i64>(6)?,
                    r.get::<_, i64>(7)?,
                ))
            },
        )
        .optional()?;
    row.map(
        |(kind, purpose, workspace, node, thread_run, state, created, updated)| {
            Ok(JobRow {
                id: job.into(),
                kind: JobKind::parse(&kind)?,
                purpose,
                workspace,
                node,
                thread_run,
                finished: state == "finished",
                created,
                updated,
            })
        },
    )
    .transpose()
}

#[derive(Debug, Clone, PartialEq)]
pub struct JobAttemptRow {
    pub id: String,
    pub job: String,
    pub ordinal: i64,
    pub model: String,
    pub node: String,
    pub operation: String,
    pub execution: Value,
    pub spec: Value,
    pub digest: String,
    pub state: AttemptState,
    pub receipt: Option<Value>,
    pub completion: Option<Value>,
    pub cancel_requested: bool,
    pub revision: i64,
}

const ATTEMPT: &str = "SELECT attempt_id,job_id,ordinal,model,node_id,operation_id,execution,spec,digest,state,receipt,completion,cancel_requested,revision FROM runtime_job_attempts";

fn attempts(tx: &Tx<'_>, filter: &str, p: impl rusqlite::Params) -> Result<Vec<JobAttemptRow>> {
    type Raw = (
        (String, String, i64, String, String, String, String),
        (
            String,
            String,
            String,
            Option<String>,
            Option<String>,
            bool,
            i64,
        ),
    );
    let mut st = tx.prepare_cached(&format!("{ATTEMPT} {filter}"))?;
    let rows: Vec<Raw> = st
        .query_map(p, |r| {
            Ok((
                (
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                ),
                (
                    r.get(7)?,
                    r.get(8)?,
                    r.get(9)?,
                    r.get(10)?,
                    r.get(11)?,
                    r.get(12)?,
                    r.get(13)?,
                ),
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let json = |s: Option<String>| -> Result<Option<Value>> {
        Ok(s.map(|s| serde_json::from_str(&s)).transpose()?)
    };
    rows.into_iter()
        .map(
            |(
                (id, job, ordinal, model, node, operation, execution),
                (spec, digest, state, receipt, completion, cancel_requested, revision),
            )| {
                Ok(JobAttemptRow {
                    id,
                    job,
                    ordinal,
                    model,
                    node,
                    operation,
                    execution: serde_json::from_str(&execution)?,
                    spec: serde_json::from_str(&spec)?,
                    digest,
                    state: AttemptState::parse(&state)?,
                    receipt: json(receipt)?,
                    completion: json(completion)?,
                    cancel_requested,
                    revision,
                })
            },
        )
        .collect()
}

pub fn attempt_in(tx: &Tx<'_>, attempt: &str) -> Result<Option<JobAttemptRow>> {
    Ok(attempts(tx, "WHERE attempt_id=?", [attempt])?.pop())
}

/// A job's attempts in ordinal order.
pub fn attempts_of_job_in(tx: &Tx<'_>, job: &str) -> Result<Vec<JobAttemptRow>> {
    attempts(tx, "WHERE job_id=? ORDER BY ordinal", [job])
}

/// `node`'s attempts that are not finished, oldest first. At boot their callers are gone: each is
/// cancelled or abandoned, never run again.
pub fn open_attempts_in(tx: &Tx<'_>, node: &str) -> Result<Vec<JobAttemptRow>> {
    attempts(
        tx,
        "WHERE node_id=? AND state<>'finished' ORDER BY created, rowid",
        [node],
    )
}

/// Deletes up to `limit` finished jobs last touched before `before`, with their attempts and
/// commands. A job stays while any of its commands may still reach its Node (not finished or
/// rejected) or another command depends on one of them. Returns how many jobs went.
pub fn prune_jobs_in(tx: &Tx<'_>, before: i64, limit: i64) -> Result<usize> {
    let jobs: Vec<String> = {
        let mut st = tx.prepare_cached(
            "SELECT j.job_id FROM runtime_jobs j WHERE j.state='finished' AND j.updated<?1
               AND NOT EXISTS(SELECT 1 FROM runtime_job_attempts a WHERE a.job_id=j.job_id AND a.state<>'finished')
               AND NOT EXISTS(SELECT 1 FROM node_commands c JOIN runtime_job_attempts a ON c.target_job_attempt=a.attempt_id
                              WHERE a.job_id=j.job_id AND c.state NOT IN ('finished','rejected'))
               AND NOT EXISTS(SELECT 1 FROM node_commands d JOIN node_commands c ON d.dependency_operation=c.operation_id
                              JOIN runtime_job_attempts a ON c.target_job_attempt=a.attempt_id
                              WHERE a.job_id=j.job_id AND (d.target_job_attempt IS NULL
                                OR d.target_job_attempt NOT IN (SELECT attempt_id FROM runtime_job_attempts WHERE job_id=j.job_id)))
             ORDER BY j.updated LIMIT ?2",
        )?;
        st.query_map(params![before, limit], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?
    };
    for job in &jobs {
        tx.execute(
            "DELETE FROM node_commands WHERE target_job_attempt IN (SELECT attempt_id FROM runtime_job_attempts WHERE job_id=?)",
            [job],
        )?;
        tx.execute("DELETE FROM runtime_job_attempts WHERE job_id=?", [job])?;
        tx.execute("DELETE FROM runtime_jobs WHERE job_id=?", [job])?;
    }
    Ok(jobs.len())
}
