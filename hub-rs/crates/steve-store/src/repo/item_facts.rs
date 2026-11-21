//! Shadow facts of an item's work (migration 104, #327): what was delivered (with how far each fact
//! can be trusted), what the work waits on, what a judge concluded, and the operator's overrides of
//! the named work policy. Written only here; read by the core's derived state, which nothing acts on
//! yet. Also the reads of the Node command ledger that state needs (a dispatch whose receipt never
//! came is reconciled under its own `operation_id`, never sent again under a new one).

use rusqlite::params;
use serde::Serialize;
use serde_json::Value;

use super::all;
use crate::{Result, Store, StoreError, Tx, now, uid};

/// Kinds of a delivery fact.
pub const DELIVERY_KINDS: [&str; 8] = [
    "commit", "tree", "pr", "ci", "build", "deploy", "review", "approval",
];
/// How far a fact can be trusted.
pub const PROVENANCES: [&str; 3] = ["verified", "reported", "unknown"];
/// What a wait is on.
pub const WAITS_ON: [&str; 10] = [
    "you",
    "external",
    "item",
    "transient",
    "quota",
    "pr",
    "ci",
    "build",
    "deploy",
    "time",
];
/// What a judge may conclude.
pub const VERDICTS: [&str; 5] = ["done", "not_done", "waiting", "needs_you", "unknown"];

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DeliveryFact {
    pub id: String,
    pub item_id: String,
    pub cycle: i64,
    pub session_id: Option<String>,
    pub turn_id: Option<String>,
    pub kind: String,
    pub head: Option<String>,
    #[serde(rename = "ref")]
    pub ref_: Option<String>,
    pub state: String,
    pub provenance: String,
    pub source: String,
    pub observed_at: i64,
}

#[derive(Debug, Clone, Default)]
pub struct NewDeliveryFact<'a> {
    pub item_id: &'a str,
    pub cycle: i64,
    pub session_id: Option<&'a str>,
    pub turn_id: Option<&'a str>,
    pub kind: &'a str,
    pub head: Option<&'a str>,
    pub ref_: Option<&'a str>,
    pub state: &'a str,
    pub provenance: &'a str,
    pub source: &'a str,
    pub observed_at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ItemWait {
    pub id: String,
    pub item_id: String,
    pub cycle: i64,
    pub session_id: Option<String>,
    pub waits_on: String,
    pub what: String,
    #[serde(rename = "ref")]
    pub ref_: Option<String>,
    pub next_check: i64,
    pub fallback_at: Option<i64>,
    pub wakes: i64,
    pub cap: Option<i64>,
    pub created: i64,
}

#[derive(Debug, Clone, Default)]
pub struct NewWait<'a> {
    pub item_id: &'a str,
    pub cycle: i64,
    pub session_id: Option<&'a str>,
    pub waits_on: &'a str,
    pub what: &'a str,
    pub ref_: Option<&'a str>,
    pub next_check: i64,
    pub fallback_at: Option<i64>,
    pub cap: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Judgement {
    pub id: String,
    pub item_id: String,
    pub cycle: i64,
    pub session_id: String,
    pub turn_id: String,
    pub scope_revision: i64,
    pub verdict: String,
    pub basis: Value,
    pub harness: Option<String>,
    pub model: Option<String>,
    pub created: i64,
    /// The candidate it judged (migration 106); `None` for one written without the judge.
    pub candidate_id: Option<String>,
    /// `facts` | `rule` | `model`
    pub decided_by: Option<String>,
    /// What it would trigger (nothing executes it in shadow).
    pub would: Option<String>,
    /// The model that answered was another than the worker's: `yes` | `no` | `unknown`.
    pub independent: Option<String>,
    pub fingerprint: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct NewJudgement<'a> {
    pub item_id: &'a str,
    pub cycle: i64,
    pub session_id: &'a str,
    pub turn_id: &'a str,
    pub scope_revision: i64,
    pub verdict: &'a str,
    pub basis: Value,
    pub harness: Option<&'a str>,
    pub model: Option<&'a str>,
    pub candidate_id: Option<&'a str>,
    pub decided_by: Option<&'a str>,
    pub would: Option<&'a str>,
    pub independent: Option<&'a str>,
    pub fingerprint: Option<&'a str>,
}

/// States of a judge candidate.
pub const CANDIDATE_STATES: [&str; 5] = ["pending", "judging", "judged", "superseded", "failed"];
/// Readings of the settled gate.
pub const SETTLED: [&str; 3] = ["evaluable", "busy", "unknown"];

/// One judge candidate (migration 106): a terminal turn of an item's current session, to be judged
/// once per (item, cycle, session, logical turn, scope revision).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Candidate {
    pub id: String,
    pub item_id: String,
    pub cycle: i64,
    pub session_id: String,
    pub turn_id: String,
    pub scope_revision: i64,
    pub terminal_attempt: Option<String>,
    pub registered_by: String,
    pub state: String,
    pub settled: Option<String>,
    pub settled_reason: Option<String>,
    #[serde(skip)]
    pub claim: Option<String>,
    pub claimed_at: Option<i64>,
    pub input_seq: Option<i64>,
    pub fingerprint: Option<String>,
    pub failures: i64,
    pub last_error: Option<String>,
    pub next_check: i64,
    pub judgement_id: Option<String>,
    pub created: i64,
    pub updated: i64,
}

#[derive(Debug, Clone, Default)]
pub struct NewCandidate<'a> {
    pub item_id: &'a str,
    pub cycle: i64,
    pub session_id: &'a str,
    pub turn_id: &'a str,
    pub scope_revision: i64,
    pub terminal_attempt: Option<&'a str>,
    /// `turn_end` | `sweep`
    pub registered_by: &'a str,
    pub next_check: i64,
}

/// A terminal logical turn of a session: the newest turn, finished, and its last attempt.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TerminalTurn {
    pub turn_id: String,
    pub attempt: Option<String>,
    pub created: i64,
    pub updated: i64,
}

/// A Node command of a session's turns whose outcome the Hub has not settled (not finished, not
/// rejected): `maybe_sent` says it may have reached the Node, `receipt` whether any answer came.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OpenDispatch {
    pub operation_id: String,
    pub method: String,
    pub state: String,
    pub maybe_sent: bool,
    pub receipt: bool,
    pub retry_at: i64,
    pub turn_id: String,
}

/// The newest logical turn of a session on the Node runtime.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LatestTurn {
    pub turn_id: String,
    pub state: String,
    pub updated: i64,
}

pub struct ItemFacts<'a>(&'a Store);

impl Store {
    pub fn item_facts(&self) -> ItemFacts<'_> {
        ItemFacts(self)
    }
}

fn refuse(what: &str, value: &str) -> StoreError {
    StoreError::Conflict(format!("unknown {what}: {value}"))
}

fn delivery_row(r: &rusqlite::Row) -> rusqlite::Result<DeliveryFact> {
    Ok(DeliveryFact {
        id: r.get("id")?,
        item_id: r.get("item_id")?,
        cycle: r.get("cycle")?,
        session_id: r.get("session_id")?,
        turn_id: r.get("turn_id")?,
        kind: r.get("kind")?,
        head: r.get("head")?,
        ref_: r.get("ref")?,
        state: r.get("state")?,
        provenance: r.get("provenance")?,
        source: r.get("source")?,
        observed_at: r.get("observed_at")?,
    })
}

fn wait_row(r: &rusqlite::Row) -> rusqlite::Result<ItemWait> {
    Ok(ItemWait {
        id: r.get("id")?,
        item_id: r.get("item_id")?,
        cycle: r.get("cycle")?,
        session_id: r.get("session_id")?,
        waits_on: r.get("waits_on")?,
        what: r.get("what")?,
        ref_: r.get("ref")?,
        next_check: r.get("next_check")?,
        fallback_at: r.get("fallback_at")?,
        wakes: r.get("wakes")?,
        cap: r.get("cap")?,
        created: r.get("created")?,
    })
}

fn judgement_row(r: &rusqlite::Row) -> rusqlite::Result<Judgement> {
    Ok(Judgement {
        id: r.get("id")?,
        item_id: r.get("item_id")?,
        cycle: r.get("cycle")?,
        session_id: r.get("session_id")?,
        turn_id: r.get("turn_id")?,
        scope_revision: r.get("scope_revision")?,
        verdict: r.get("verdict")?,
        basis: serde_json::from_str(&r.get::<_, String>("basis")?).unwrap_or(Value::Null),
        harness: r.get("harness")?,
        model: r.get("model")?,
        created: r.get("created")?,
        candidate_id: r.get("candidate_id")?,
        decided_by: r.get("decided_by")?,
        would: r.get("would")?,
        independent: r.get("independent")?,
        fingerprint: r.get("fingerprint")?,
    })
}

fn candidate_row(r: &rusqlite::Row) -> rusqlite::Result<Candidate> {
    Ok(Candidate {
        id: r.get("id")?,
        item_id: r.get("item_id")?,
        cycle: r.get("cycle")?,
        session_id: r.get("session_id")?,
        turn_id: r.get("turn_id")?,
        scope_revision: r.get("scope_revision")?,
        terminal_attempt: r.get("terminal_attempt")?,
        registered_by: r.get("registered_by")?,
        state: r.get("state")?,
        settled: r.get("settled")?,
        settled_reason: r.get("settled_reason")?,
        claim: r.get("claim")?,
        claimed_at: r.get("claimed_at")?,
        input_seq: r.get("input_seq")?,
        fingerprint: r.get("fingerprint")?,
        failures: r.get("failures")?,
        last_error: r.get("last_error")?,
        next_check: r.get("next_check")?,
        judgement_id: r.get("judgement_id")?,
        created: r.get("created")?,
        updated: r.get("updated")?,
    })
}

/// Inserts a judgement (`None`: its key has one already).
fn insert_judgement_in(tx: &Tx<'_>, j: &NewJudgement<'_>) -> Result<Option<String>> {
    if !VERDICTS.contains(&j.verdict) {
        return Err(refuse("verdict", j.verdict));
    }
    let id = uid("jdg");
    let n = tx.execute(
        "INSERT OR IGNORE INTO item_judgements(id,item_id,cycle,session_id,turn_id,scope_revision,verdict,basis,harness,model,created,candidate_id,decided_by,would,independent,fingerprint)
         VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
        params![id, j.item_id, j.cycle, j.session_id, j.turn_id, j.scope_revision,
            j.verdict, j.basis.to_string(), j.harness, j.model, now(), j.candidate_id,
            j.decided_by, j.would, j.independent, j.fingerprint],
    )?;
    Ok((n > 0).then_some(id))
}

/// The events of one attempt in its session ([`ItemFacts::attempt_events`]).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AttemptEvents {
    /// Its first event's seq (its range runs from here to its end).
    pub start: i64,
    /// The seqs of the range confirmed to be its own, oldest first.
    pub own: Vec<i64>,
}

/// What a judge's write finds ([`ItemFacts::candidate_judged`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Judged {
    /// Written: the judgement's id.
    Written(String),
    /// The claim is gone (another pass took the candidate over, or it was superseded).
    Lost,
    /// Its key was judged already: the candidate is superseded.
    Already,
    /// Something it rests on changed since it was claimed (`cycle`, `turn`, `scope`, `inputs` or
    /// `evidence`): nothing was written.
    Stale(&'static str),
}

/// [`ItemFacts::evidence_version`] on `c` (a read, or the write transaction).
fn evidence_version_in(c: &rusqlite::Connection, item: &str, cycle: i64) -> Result<String> {
    let facts: Vec<String> = all(
        c,
        "SELECT id||'|'||kind||'|'||state||'|'||provenance||'|'||COALESCE(head,'')||'|'||COALESCE(ref,'')||'|'||observed_at
         FROM item_delivery_facts WHERE item_id=? AND cycle=? ORDER BY rowid",
        params![item, cycle],
        |r| r.get(0),
    )?;
    let outputs: Vec<String> = all(
        c,
        "SELECT id||'|'||title||'|'||COALESCE(location,'') FROM artifacts WHERE item_id=? ORDER BY rowid",
        [item],
        |r| r.get(0),
    )?;
    /* the cycle's operations, as the derived state reads them for the current result (a merge,
     * a release, the acceptance hooks): each with its status and when it last changed */
    let operations: Vec<String> = all(
        c,
        "SELECT id||'|'||kind||'|'||revision||'|'||required||'|'||status||'|'||updated||'|'||evidence
         FROM item_operations WHERE item_id=? AND cycle=? ORDER BY created,id",
        params![item, cycle],
        |r| r.get(0),
    )?;
    let (revision, closing): (i64, Option<String>) = super::one(
        c,
        "SELECT revision,closing FROM items WHERE id=?",
        [item],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?
    .unwrap_or((0, None));
    Ok(crate::sha(&format!(
        "{}\n--\n{}\n--\n{}\n--\n{revision}\n{}",
        facts.join("\n"),
        outputs.join("\n"),
        operations.join("\n"),
        closing.unwrap_or_default()
    )))
}

/// Why claimed candidate `c` no longer stands, read in the write transaction: its item's cycle
/// moved on (or it closed), the item's current session is another, that session's newest logical
/// turn is another or not finished, the scope revision moved on, the session's input moved past
/// the version it was claimed on, or the evidence version is not `evidence`. `None`: it all still
/// holds.
fn stale_in(tx: &Tx<'_>, c: &Candidate, evidence: &str) -> Result<Option<&'static str>> {
    let cycle: Option<(i64, String)> = super::one(
        tx,
        "SELECT cycle,phase FROM items WHERE id=?",
        [&c.item_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if cycle.is_none_or(|(n, phase)| n != c.cycle || phase == "closed") {
        return Ok(Some("cycle"));
    }
    /* the item's current session: its newest one that has not ended */
    let session: Option<String> = super::one(
        tx,
        "SELECT id FROM sessions WHERE item_id=? AND status<>'ended' ORDER BY no DESC,rowid DESC LIMIT 1",
        [&c.item_id],
        |r| r.get(0),
    )?;
    if session.as_deref() != Some(c.session_id.as_str()) {
        return Ok(Some("session"));
    }
    let turn: Option<(String, String)> = super::one(
        tx,
        "SELECT turn_id,state FROM runtime_turns WHERE session_id=? ORDER BY created DESC,rowid DESC LIMIT 1",
        [&c.session_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if turn.is_none_or(|(t, state)| t != c.turn_id || state != "finished") {
        return Ok(Some("turn"));
    }
    let scope: i64 = tx.query_row(
        "SELECT COALESCE(MAX(version),0) FROM item_goals WHERE item_id=?",
        [&c.item_id],
        |r| r.get(0),
    )?;
    if scope != c.scope_revision {
        return Ok(Some("scope"));
    }
    let input: i64 = tx.query_row(
        "SELECT COALESCE(MAX(seq),0) FROM events WHERE session_id=?",
        [&c.session_id],
        |r| r.get(0),
    )?;
    if c.input_seq != Some(input) {
        return Ok(Some("inputs"));
    }
    if evidence_version_in(tx, &c.item_id, c.cycle)? != evidence {
        return Ok(Some("evidence"));
    }
    Ok(None)
}

impl ItemFacts<'_> {
    /// Records one delivery fact; returns its id.
    pub fn record_delivery(&self, f: &NewDeliveryFact<'_>) -> Result<String> {
        if !DELIVERY_KINDS.contains(&f.kind) {
            return Err(refuse("delivery fact kind", f.kind));
        }
        if !PROVENANCES.contains(&f.provenance) {
            return Err(refuse("provenance", f.provenance));
        }
        let id = uid("dlf");
        self.0.transaction(|tx| {
            tx.execute(
                "INSERT INTO item_delivery_facts(id,item_id,cycle,session_id,turn_id,kind,head,ref,state,provenance,source,observed_at)
                 VALUES (?,?,?,?,?,?,?,?,?,?,?,?)",
                params![id, f.item_id, f.cycle, f.session_id, f.turn_id, f.kind, f.head, f.ref_,
                    f.state, f.provenance, f.source, f.observed_at],
            )?;
            Ok::<_, StoreError>(())
        })?;
        Ok(id)
    }

    /// The delivery facts of `item` in `cycle`, oldest first.
    pub fn deliveries(&self, item: &str, cycle: i64) -> Result<Vec<DeliveryFact>> {
        self.0.read(|c| {
            all(
                c,
                "SELECT * FROM item_delivery_facts WHERE item_id=? AND cycle=? ORDER BY observed_at,rowid",
                params![item, cycle],
                delivery_row,
            )
        })
    }

    /// Registers a wait; returns its id.
    pub fn add_wait(&self, w: &NewWait<'_>) -> Result<String> {
        if !WAITS_ON.contains(&w.waits_on) {
            return Err(refuse("wait", w.waits_on));
        }
        let (id, at) = (uid("wt"), now());
        self.0.transaction(|tx| {
            tx.execute(
                "INSERT INTO item_waits(id,item_id,cycle,session_id,waits_on,what,ref,next_check,fallback_at,cap,created,updated)
                 VALUES (?,?,?,?,?,?,?,?,?,?,?,?)",
                params![id, w.item_id, w.cycle, w.session_id, w.waits_on, w.what, w.ref_,
                    w.next_check, w.fallback_at, w.cap, at, at],
            )?;
            Ok::<_, StoreError>(())
        })?;
        Ok(id)
    }

    /// The open waits of `item` in `cycle`.
    pub fn waits_open(&self, item: &str, cycle: i64) -> Result<Vec<ItemWait>> {
        self.0.read(|c| {
            all(
                c,
                "SELECT * FROM item_waits WHERE item_id=? AND cycle=? AND resolved_at IS NULL ORDER BY next_check,rowid",
                params![item, cycle],
                wait_row,
            )
        })
    }

    /// Ends a wait with why; `false` when it was not open.
    pub fn resolve_wait(&self, id: &str, resolution: &str) -> Result<bool> {
        let at = now();
        self.0.transaction(|tx| {
            Ok::<_, StoreError>(tx.execute(
                "UPDATE item_waits SET resolved_at=?,resolution=?,updated=? WHERE id=? AND resolved_at IS NULL",
                params![at, resolution, at, id],
            )? > 0)
        })
    }

    /// Records a judgement. One per (item, cycle, session, turn, scope revision): a second one for
    /// the same key changes nothing and answers `None`.
    pub fn record_judgement(&self, j: &NewJudgement<'_>) -> Result<Option<String>> {
        self.0.transaction(|tx| insert_judgement_in(tx, j))
    }

    /// The judgements of `item` in `cycle`, newest first.
    pub fn judgements(&self, item: &str, cycle: i64) -> Result<Vec<Judgement>> {
        self.0.read(|c| {
            all(
                c,
                "SELECT * FROM item_judgements WHERE item_id=? AND cycle=? ORDER BY created DESC,rowid DESC",
                params![item, cycle],
                judgement_row,
            )
        })
    }

    /// The operator's overrides of the work policy, by name.
    pub fn policy_overrides(&self) -> Result<Vec<(String, i64)>> {
        self.0.read(|c| {
            all(
                c,
                "SELECT name,value FROM work_policy ORDER BY name",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
        })
    }

    /// Sets an override of the work policy (the caller checks the name).
    pub fn set_policy(&self, name: &str, value: i64) -> Result<()> {
        self.0.transaction(|tx| {
            tx.execute(
                "INSERT INTO work_policy(name,value,updated) VALUES (?,?,?)
                 ON CONFLICT(name) DO UPDATE SET value=excluded.value,updated=excluded.updated",
                params![name, value, now()],
            )?;
            Ok::<_, StoreError>(())
        })
    }

    /// The Node commands that start or carry the turns of `session` and whose outcome is not
    /// settled yet (neither finished nor rejected).
    pub fn open_dispatches(&self, session: &str) -> Result<Vec<OpenDispatch>> {
        self.0.read(|c| {
            all(
                c,
                "SELECT c.operation_id,c.method,c.state,c.maybe_sent,c.receipt IS NOT NULL,c.retry_at,a.turn_id
                 FROM runtime_turns t JOIN runtime_attempts a ON a.turn_id=t.turn_id
                 JOIN node_commands c ON c.operation_id=a.operation_id
                 WHERE t.session_id=? AND c.state NOT IN ('finished','rejected')
                 ORDER BY c.created,c.rowid",
                [session],
                |r| {
                    Ok(OpenDispatch {
                        operation_id: r.get(0)?,
                        method: r.get(1)?,
                        state: r.get(2)?,
                        maybe_sent: r.get::<_, i64>(3)? != 0,
                        receipt: r.get(4)?,
                        retry_at: r.get(5)?,
                        turn_id: r.get(6)?,
                    })
                },
            )
        })
    }

    /// The newest logical turn of `session` on the Node runtime, if it has any.
    pub fn latest_turn(&self, session: &str) -> Result<Option<LatestTurn>> {
        self.0.read(|c| {
            super::one(
                c,
                "SELECT turn_id,state,updated FROM runtime_turns WHERE session_id=? ORDER BY created DESC,rowid DESC LIMIT 1",
                [session],
                |r| {
                    Ok(LatestTurn {
                        turn_id: r.get(0)?,
                        state: r.get(1)?,
                        updated: r.get(2)?,
                    })
                },
            )
        })
    }

    /// When the Node that runs `session` was last seen (`None`: not on a paired Node, or never).
    pub fn node_last_seen(&self, session: &str) -> Result<Option<i64>> {
        self.0.read(|c| {
            Ok(super::one(
                c,
                "SELECT n.last_seen FROM runtime_sessions r JOIN nodes n ON n.node_id=r.node_id WHERE r.session_id=?",
                [session],
                |r| r.get::<_, Option<i64>>(0),
            )?
            .flatten())
        })
    }
    /// Registers a judge candidate for its key; `false` when the key has one already (a turn end
    /// told twice, or the sweep after the turn end did it).
    pub fn register_candidate(&self, c: &NewCandidate<'_>) -> Result<bool> {
        if !["turn_end", "sweep"].contains(&c.registered_by) {
            return Err(refuse("registration", c.registered_by));
        }
        let (id, at) = (uid("jc"), now());
        self.0.transaction(|tx| {
            Ok::<_, StoreError>(tx.execute(
                "INSERT OR IGNORE INTO item_judge_candidates(id,item_id,cycle,session_id,turn_id,scope_revision,terminal_attempt,registered_by,next_check,created,updated)
                 VALUES (?,?,?,?,?,?,?,?,?,?,?)",
                params![id, c.item_id, c.cycle, c.session_id, c.turn_id, c.scope_revision,
                    c.terminal_attempt, c.registered_by, c.next_check, at, at],
            )? > 0)
        })
    }

    /// The candidate of a key, if it was registered.
    pub fn candidate_of(
        &self,
        item: &str,
        cycle: i64,
        session: &str,
        turn: &str,
        scope_revision: i64,
    ) -> Result<Option<Candidate>> {
        self.0.read(|c| {
            super::one(
                c,
                "SELECT * FROM item_judge_candidates WHERE item_id=? AND cycle=? AND session_id=? AND turn_id=? AND scope_revision=?",
                params![item, cycle, session, turn, scope_revision],
                candidate_row,
            )
        })
    }

    pub fn candidate(&self, id: &str) -> Result<Option<Candidate>> {
        self.0.read(|c| {
            super::one(
                c,
                "SELECT * FROM item_judge_candidates WHERE id=?",
                [id],
                candidate_row,
            )
        })
    }

    /// The candidates of `item` in `cycle`, newest first.
    pub fn candidates(&self, item: &str, cycle: i64) -> Result<Vec<Candidate>> {
        self.0.read(|c| {
            all(
                c,
                "SELECT * FROM item_judge_candidates WHERE item_id=? AND cycle=? ORDER BY created DESC,rowid DESC",
                params![item, cycle],
                candidate_row,
            )
        })
    }

    /// Candidates to look at now: pending ones that are due, and ones whose judging pass claimed
    /// them before `stale_before` and never came back (a restart, a lost task). Oldest due first.
    pub fn candidates_due(
        &self,
        at: i64,
        stale_before: i64,
        limit: usize,
    ) -> Result<Vec<Candidate>> {
        self.0.read(|c| {
            all(
                c,
                "SELECT * FROM item_judge_candidates
                 WHERE state IN ('pending','judging') AND next_check<=?
                   AND (state='pending' OR claimed_at IS NULL OR claimed_at<?)
                 ORDER BY next_check,rowid LIMIT ?",
                params![at, stale_before, limit as i64],
                candidate_row,
            )
        })
    }

    /// The gate's reading of a pending candidate that is not judged now, and when it is looked at
    /// again; `false` when it is not pending any more.
    pub fn candidate_settled(
        &self,
        id: &str,
        settled: &str,
        reason: &str,
        next_check: i64,
    ) -> Result<bool> {
        if !SETTLED.contains(&settled) {
            return Err(refuse("settled", settled));
        }
        self.0.transaction(|tx| {
            Ok::<_, StoreError>(tx.execute(
                "UPDATE item_judge_candidates SET settled=?,settled_reason=?,next_check=?,updated=? WHERE id=? AND state='pending'",
                params![settled, reason, next_check, now(), id],
            )? > 0)
        })
    }

    /// A candidate that no longer stands for the current turn (another turn, session, cycle or scope
    /// came): never judged. `false` when it was judged or given up already.
    pub fn candidate_supersede(&self, id: &str, why: &str) -> Result<bool> {
        self.0.transaction(|tx| {
            Ok::<_, StoreError>(tx.execute(
                "UPDATE item_judge_candidates SET state='superseded',settled_reason=?,claim=NULL,updated=? WHERE id=? AND state IN ('pending','judging')",
                params![why, now(), id],
            )? > 0)
        })
    }

    /// Claims a candidate for one judging pass (the only advancer of it): pending, or judging under a
    /// claim older than `stale_before`. Keeps the inputs it is judged on. `false`: somebody else has it.
    pub fn candidate_claim(
        &self,
        id: &str,
        claim: &str,
        input_seq: i64,
        fingerprint: &str,
        stale_before: i64,
    ) -> Result<bool> {
        let at = now();
        self.0.transaction(|tx| {
            Ok::<_, StoreError>(tx.execute(
                "UPDATE item_judge_candidates
                 SET state='judging',settled='evaluable',settled_reason=NULL,claim=?,claimed_at=?,input_seq=?,fingerprint=?,updated=?
                 WHERE id=? AND (state='pending' OR (state='judging' AND (claimed_at IS NULL OR claimed_at<?)))",
                params![claim, at, input_seq, fingerprint, at, id, stale_before],
            )? > 0)
        })
    }

    /// The claimed pass gives the candidate back: pending again at `next_check`, its failures
    /// counted up by `failed` (with why), or given up (`failed` state) when `give_up`. Only the pass
    /// that holds the claim may: `false` otherwise.
    pub fn candidate_release(
        &self,
        id: &str,
        claim: &str,
        failed: Option<&str>,
        give_up: bool,
        next_check: i64,
    ) -> Result<bool> {
        let state = if give_up { "failed" } else { "pending" };
        self.0.transaction(|tx| {
            Ok::<_, StoreError>(tx.execute(
                "UPDATE item_judge_candidates
                 SET state=?,claim=NULL,claimed_at=NULL,failures=failures+?,last_error=COALESCE(?,last_error),next_check=?,updated=?
                 WHERE id=? AND state='judging' AND claim=?",
                params![state, i64::from(failed.is_some()), failed, next_check, now(), id, claim],
            )? > 0)
        })
    }

    /// Writes the verdict of the pass that holds the claim, compare-and-swap in one transaction:
    /// inside it, the claim, the item's cycle, the session's current logical turn (its newest, and
    /// finished), the scope revision, the input version (the session's newest event, as claimed) and
    /// the evidence version ([`ItemFacts::evidence_version`], `evidence` as the pass read it) are
    /// read again. Only when every one is unchanged does the candidate become `judged` and the
    /// judgement get inserted; otherwise nothing is written and the answer says why
    /// ([`Judged::Stale`]), for the caller to supersede the candidate or judge it again.
    pub fn candidate_judged(
        &self,
        id: &str,
        claim: &str,
        evidence: &str,
        j: &NewJudgement<'_>,
    ) -> Result<Judged> {
        self.0.transaction(|tx| {
            let Some(c) = super::one(
                tx,
                "SELECT * FROM item_judge_candidates WHERE id=? AND state='judging' AND claim=?",
                params![id, claim],
                candidate_row,
            )?
            else {
                return Ok::<_, StoreError>(Judged::Lost);
            };
            if let Some(why) = stale_in(tx, &c, evidence)? {
                return Ok(Judged::Stale(why));
            }
            let Some(jid) = insert_judgement_in(tx, j)? else {
                tx.execute(
                    "UPDATE item_judge_candidates SET state='superseded',settled_reason='judged_already',claim=NULL,updated=? WHERE id=?",
                    params![now(), id],
                )?;
                return Ok(Judged::Already);
            };
            tx.execute(
                "UPDATE item_judge_candidates SET state='judged',claim=NULL,judgement_id=?,updated=? WHERE id=?",
                params![jid, now(), id],
            )?;
            Ok(Judged::Written(jid))
        })
    }

    /// The events of attempt `attempt` of `session`, among the session's events up to `until`
    /// (that attempt's end), as the Node stream each came on attributes it: where they begin, and
    /// which are its own. An earlier attempt's event (a retry of the same turn, or another turn) or
    /// a message sent to the session ends the search. Start `until + 1` with none of its own: the
    /// attempt said nothing. `None`: it cannot be told, none of the range's words being its own
    /// while some are nobody's. Events of the range that are not its own (the Hub's own lines,
    /// session-level facts) are left out of [`AttemptEvents::own`]; a reader decides what they
    /// mean.
    pub fn attempt_events(
        &self,
        session: &str,
        attempt: &str,
        until: i64,
    ) -> Result<Option<AttemptEvents>> {
        /// How many of the session's last events are looked at.
        const SCAN: i64 = 5000;
        self.0.read(|c| {
            let rows: Vec<(i64, Option<String>, bool, bool)> = all(
                c,
                "SELECT seq, source_attempt, kind='text',
                        kind IN ('user','user_message_chunk') OR (kind='text' AND json_extract(data,'$.role')='user')
                 FROM events WHERE session_id=? AND seq<=? ORDER BY seq DESC LIMIT ?",
                params![session, until, SCAN],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )?;
            let mut own: Vec<i64> = vec![];
            let mut nobodys_words = false;
            for (seq, of, text, sent) in rows {
                if sent {
                    break;
                }
                match of.as_deref() {
                    Some(a) if a == attempt => own.push(seq),
                    /* another attempt's event: where this one began */
                    Some(_) => break,
                    /* the Hub's own line, or a session-level fact: nobody's attempt */
                    None => nobodys_words |= text,
                }
            }
            own.reverse();
            Ok(match own.first() {
                Some(&start) => Some(AttemptEvents { start, own }),
                None if nobodys_words => None,
                None => Some(AttemptEvents {
                    start: until.saturating_add(1),
                    own,
                }),
            })
        })
    }

    /// The version of what a judgement of `item` in `cycle` rests on besides the session's events:
    /// a digest of its delivery facts, its outputs, its operations (status and when each last
    /// changed), its result revision and its closing (with the checks the executor reported). Any
    /// fact recorded, output added, operation moved or closing changed gives another version.
    pub fn evidence_version(&self, item: &str, cycle: i64) -> Result<String> {
        self.0.read(|c| evidence_version_in(c, item, cycle))
    }

    /// The newest logical turn of `session` when it is finished, with its last attempt.
    pub fn terminal_turn(&self, session: &str) -> Result<Option<TerminalTurn>> {
        self.0.read(|c| {
            super::one(
                c,
                "SELECT t.turn_id,t.updated,t.created,(SELECT a.attempt_id FROM runtime_attempts a WHERE a.turn_id=t.turn_id ORDER BY a.ordinal DESC LIMIT 1)
                 FROM runtime_turns t WHERE t.session_id=? AND t.state='finished'
                   AND t.rowid=(SELECT rowid FROM runtime_turns WHERE session_id=? ORDER BY created DESC,rowid DESC LIMIT 1)",
                [session, session],
                |r| {
                    Ok(TerminalTurn {
                        turn_id: r.get(0)?,
                        updated: r.get(1)?,
                        created: r.get(2)?,
                        attempt: r.get(3)?,
                    })
                },
            )
        })
    }

    /// Open task items (being worked on or closing), not archived: what the judge's sweep looks at.
    pub fn open_task_items(&self) -> Result<Vec<String>> {
        self.0.read(|c| {
            all(
                c,
                "SELECT id FROM items WHERE kind='task' AND phase IN ('active','closing') AND archived=0 ORDER BY rowid",
                [],
                |r| r.get(0),
            )
        })
    }

    /// The logical turn an attempt belongs to.
    pub fn turn_of_attempt(&self, attempt: &str) -> Result<Option<String>> {
        self.0.read(|c| {
            super::one(
                c,
                "SELECT turn_id FROM runtime_attempts WHERE attempt_id=?",
                [attempt],
                |r| r.get(0),
            )
        })
    }

    /// The newest event of `session` (0: none): what the session's input and output have reached.
    pub fn last_event_seq(&self, session: &str) -> Result<i64> {
        self.0.read(|c| {
            Ok(c.query_row(
                "SELECT COALESCE(MAX(seq),0) FROM events WHERE session_id=?",
                [session],
                |r| r.get(0),
            )?)
        })
    }
}
