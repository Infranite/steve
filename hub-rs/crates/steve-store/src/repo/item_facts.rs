//! Shadow facts of an item's work (migration 100, #327): what was delivered (with how far each fact
//! can be trusted), what the work waits on, what a judge concluded, and the operator's overrides of
//! the named work policy. Written only here; read by the core's derived state, which nothing acts on
//! yet. Also the reads of the Node command ledger that state needs (a dispatch whose receipt never
//! came is reconciled under its own `operation_id`, never sent again under a new one).

use rusqlite::params;
use serde::Serialize;
use serde_json::Value;

use super::all;
use crate::{Result, Store, StoreError, now, uid};

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
    })
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
        if !VERDICTS.contains(&j.verdict) {
            return Err(refuse("verdict", j.verdict));
        }
        let id = uid("jdg");
        self.0.transaction(|tx| {
            let n = tx.execute(
                "INSERT OR IGNORE INTO item_judgements(id,item_id,cycle,session_id,turn_id,scope_revision,verdict,basis,harness,model,created)
                 VALUES (?,?,?,?,?,?,?,?,?,?,?)",
                params![id, j.item_id, j.cycle, j.session_id, j.turn_id, j.scope_revision,
                    j.verdict, j.basis.to_string(), j.harness, j.model, now()],
            )?;
            Ok::<_, StoreError>((n > 0).then_some(id.clone()))
        })
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
}
