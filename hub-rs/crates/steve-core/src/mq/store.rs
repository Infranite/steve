//! A target's rows as the state machines see them, and back: [`load`] reads a target's live rows into
//! a [`Queue`] and [`Trains`], [`Loaded::save`] writes what changed, in the same transaction. The
//! state is the rows; nothing else is kept between events.

use std::collections::HashMap;

use rusqlite::Connection;
use steve_store::mq::{self, MqBatch, MqEntry, MqTarget, MqTrain};

use super::policy::Policy;
use super::queue::{Batch, BatchState, Entry, EntryState, Queue};
use super::train::{Train, TrainState, Trains};

type Result<T> = std::result::Result<T, steve_store::StoreError>;

/// One target, loaded.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub target: MqTarget,
    pub policy: Policy,
    pub queue: Queue,
    pub trains: Trains,
    /// The rows as loaded (what is not the machines' stays as it was).
    pub entries: HashMap<String, MqEntry>,
    batches: HashMap<i64, MqBatch>,
    train_rows: HashMap<i64, MqTrain>,
    /// Set by the caller before saving: why an entry was returned.
    pub reasons: Vec<(String, String)>,
    /// Set by the caller before saving: entries that landed, with the commit.
    pub landed: Vec<(String, Option<String>)>,
    /// What the release train has to do, gathered while the event is applied.
    pub train_out: Vec<super::train::Action>,
}

fn entry_state(s: &str) -> Option<EntryState> {
    Some(match s {
        "queued" => EntryState::Queued,
        "batched" => EntryState::Batched,
        "landed" => EntryState::Landed,
        "returned" => EntryState::Returned,
        "withdrawn" => EntryState::Withdrawn,
        _ => return None,
    })
}

fn entry_str(s: EntryState) -> &'static str {
    match s {
        EntryState::Queued => "queued",
        EntryState::Batched => "batched",
        EntryState::Landed => "landed",
        EntryState::Returned => "returned",
        EntryState::Withdrawn => "withdrawn",
    }
}

pub fn batch_state(s: &str) -> BatchState {
    match s {
        "running" => BatchState::Running,
        "waiting" => BatchState::Waiting,
        "unknown" => BatchState::Unknown,
        "landed" => BatchState::Landed,
        "split" => BatchState::Split,
        _ => BatchState::Returned,
    }
}

pub fn batch_str(s: BatchState) -> &'static str {
    match s {
        BatchState::Running => "running",
        BatchState::Waiting => "waiting",
        BatchState::Unknown => "unknown",
        BatchState::Landed => "landed",
        BatchState::Split => "split",
        BatchState::Returned => "returned",
    }
}

pub fn train_state(s: &str) -> TrainState {
    match s {
        "open" => TrainState::Open,
        "awaiting" => TrainState::Awaiting,
        "cutting" => TrainState::Cutting,
        "released" => TrainState::Released,
        "failed" => TrainState::Failed,
        "unknown" => TrainState::Unknown,
        "halted" => TrainState::Halted,
        _ => TrainState::Dropped,
    }
}

pub fn train_str(s: TrainState) -> &'static str {
    match s {
        TrainState::Open => "open",
        TrainState::Awaiting => "awaiting",
        TrainState::Cutting => "cutting",
        TrainState::Released => "released",
        TrainState::Failed => "failed",
        TrainState::Unknown => "unknown",
        TrainState::Halted => "halted",
        TrainState::Dropped => "dropped",
    }
}

/// `target`'s live rows; `project`: the project's policy defaults.
pub fn load(
    c: &Connection,
    target: &str,
    project: Option<&serde_json::Value>,
) -> Result<Option<Loaded>> {
    let Some(t) = mq::target_get(c, target)? else {
        return Ok(None);
    };
    let rows = mq::entries_live(c, target)?;
    let batches = mq::batches_live(c, target)?;
    let trains = mq::trains_live(c, target)?;
    let mut queue = Queue {
        entries: vec![],
        batches: vec![],
        next: t.next_batch.max(0) as u64,
        last_arrival: None,
        let_through: t.let_through,
    };
    for e in &rows {
        let Some(state) = entry_state(&e.state) else {
            continue;
        };
        if matches!(state, EntryState::Queued | EntryState::Batched) {
            queue.last_arrival = Some(queue.last_arrival.map_or(e.arrived, |l| l.max(e.arrived)));
        }
        queue.entries.push(Entry {
            id: e.id.clone(),
            key: e.key.clone(),
            arrived: e.arrived,
            after: serde_json::from_str(&e.after).unwrap_or_default(),
            state,
            batch: e.batch_no.map(|b| b as u64),
        });
    }
    for b in &batches {
        queue.batches.push(Batch {
            no: b.no as u64,
            parent: b.parent.map(|p| p as u64),
            root: b.root as u64,
            members: serde_json::from_str(&b.members).unwrap_or_default(),
            state: batch_state(&b.state),
            reruns: b.reruns.max(0) as u32,
        });
    }
    let mut tr = Trains {
        trains: vec![],
        next: t.next_train.max(0) as u64,
        wanted_now: rows
            .iter()
            .filter(|e| e.release_now && matches!(e.state.as_str(), "queued" | "batched"))
            .map(|e| e.id.clone())
            .collect(),
    };
    for r in &trains {
        let mut members: Vec<&MqEntry> = rows.iter().filter(|e| e.train_no == Some(r.no)).collect();
        members.sort_by_key(|e| (e.landed_at.unwrap_or(i64::MAX), e.arrived));
        tr.trains.push(Train {
            no: r.no as u64,
            members: members.iter().map(|e| e.id.clone()).collect(),
            opened: r.opened,
            last: r.last,
            state: train_state(&r.state),
            now: r.now,
        });
    }
    let policy = Policy::of(&t.policy, project);
    Ok(Some(Loaded {
        target: t,
        policy,
        queue,
        trains: tr,
        entries: rows.into_iter().map(|e| (e.id.clone(), e)).collect(),
        batches: batches.into_iter().map(|b| (b.no, b)).collect(),
        train_rows: trains.into_iter().map(|r| (r.no, r)).collect(),
        reasons: vec![],
        landed: vec![],
        train_out: vec![],
    }))
}

impl Loaded {
    /// Write the machines' state back (`at`: now).
    pub fn save(&self, c: &Connection, at: i64) -> Result<()> {
        let id = &self.target.id;
        mq::target_counters_set(
            c,
            id,
            self.queue.next as i64,
            self.trains.next as i64,
            self.queue.let_through,
        )?;
        for b in &self.queue.batches {
            let old = self.batches.get(&(b.no as i64));
            let state = batch_str(b.state).to_string();
            let members = serde_json::to_string(&b.members).unwrap_or_else(|_| "[]".into());
            let row = match old {
                Some(o) => MqBatch {
                    parent: b.parent.map(|p| p as i64),
                    root: b.root as i64,
                    ended: if b.state.live() {
                        None
                    } else {
                        o.ended.or(Some(at))
                    },
                    step: if b.state == BatchState::Running {
                        o.step.clone()
                    } else {
                        None
                    },
                    state,
                    members,
                    reruns: b.reruns as i64,
                    ..o.clone()
                },
                None => MqBatch {
                    target_id: id.clone(),
                    no: b.no as i64,
                    parent: b.parent.map(|p| p as i64),
                    root: b.root as i64,
                    ended: (!b.state.live()).then_some(at),
                    step: None,
                    state,
                    base: None,
                    tip: None,
                    members,
                    checks: None,
                    pr: None,
                    outcome: None,
                    reruns: b.reruns as i64,
                    opened: at,
                },
            };
            if old != Some(&row) {
                mq::batch_put(c, &row)?;
            }
        }
        let mut train_of: HashMap<&str, (i64, TrainState)> = HashMap::new();
        for t in &self.trains.trains {
            for m in &t.members {
                train_of.insert(m.as_str(), (t.no as i64, t.state));
            }
            let old = self.train_rows.get(&(t.no as i64));
            let over = !t.state.live();
            let row = match old {
                Some(o) => MqTrain {
                    state: train_str(t.state).into(),
                    now: t.now,
                    opened: t.opened,
                    last: t.last,
                    ended: if over { o.ended.or(Some(at)) } else { None },
                    ..o.clone()
                },
                None => MqTrain {
                    target_id: id.clone(),
                    no: t.no as i64,
                    state: train_str(t.state).into(),
                    now: t.now,
                    tip: None,
                    version: None,
                    tag: None,
                    decision_id: None,
                    opened: t.opened,
                    last: t.last,
                    closes_at: None,
                    ended: over.then_some(at),
                },
            };
            if old != Some(&row) {
                mq::train_put(c, &row)?;
            }
        }
        for e in &self.queue.entries {
            let Some(old) = self.entries.get(&e.id) else {
                continue;
            };
            let (train, released) = match train_of.get(e.id.as_str()) {
                Some((no, s)) => (Some(*no), *s == TrainState::Released),
                None => (old.train_no, false),
            };
            let state = if released && e.state == EntryState::Landed {
                "released"
            } else {
                entry_str(e.state)
            };
            let batch = e.batch.map(|b| b as i64).or(match e.state {
                EntryState::Landed | EntryState::Returned => old.batch_no,
                _ => None,
            });
            if old.state != state || old.batch_no != batch || old.train_no != train {
                mq::entry_set(c, &e.id, state, batch, train, None)?;
            }
        }
        // what the train has that the queue no longer holds (the store loaded it as landed)
        for (eid, (no, s)) in &train_of {
            if self.queue.entry(eid).is_some() {
                continue;
            }
            if let Some(old) = self.entries.get(*eid) {
                let state = if *s == TrainState::Released {
                    "released"
                } else {
                    old.state.as_str()
                };
                if old.state != state || old.train_no != Some(*no) {
                    mq::entry_set(c, eid, state, old.batch_no, Some(*no), None)?;
                }
            }
        }
        let wanted: Vec<&String> = self.trains.wanted_now.iter().collect();
        for (eid, old) in &self.entries {
            let now = wanted.contains(&eid);
            if old.release_now != now && matches!(old.state.as_str(), "queued" | "batched") {
                mq::entry_release_now(c, eid, now)?;
            }
        }
        for (eid, why) in &self.reasons {
            c.execute(
                "UPDATE mq_entries SET reason = ? WHERE id = ?",
                rusqlite::params![why, eid],
            )?;
        }
        for (eid, commit) in &self.landed {
            mq::entry_landed(c, eid, commit.as_deref(), at)?;
        }
        Ok(())
    }
}

/// The 031 lanes into the queue, each once ([`mq::convert_031`]): run at every boot with the queue on.
/// The items converted are told so on their timeline.
pub fn convert_lanes(store: &steve_store::Store) -> Result<mq::Converted> {
    let note = steve_i18n::stored(|| steve_i18n::t!("core.mq.converted"));
    store.transaction(|tx| mq::convert_031(tx, &note))
}

#[cfg(test)]
mod tests {
    use super::super::queue::{Action, Event, Outcome};
    use super::super::train;
    use super::*;
    use steve_store::{Store, StoreError};

    fn store() -> (tempfile::TempDir, Store) {
        let d = tempfile::tempdir().unwrap();
        let s = Store::open(d.path().join("hub.sqlite3"), steve_store::NoPublisher).unwrap();
        s.ensure_workspace().unwrap();
        (d, s)
    }

    fn add(s: &Store, t: &str, item: &str) -> String {
        s.transaction::<_, StoreError>(|tx| {
            mq::entry_add(
                tx,
                &mq::NewEntry {
                    target_id: t,
                    workspace_id: Some("default"),
                    item_id: Some(item),
                    cycle: Some(1),
                    key: None,
                    after: &[],
                    head: "h",
                    branch: None,
                    consent: None,
                    node_id: None,
                },
            )
        })
        .unwrap()
        .id
    }

    /// One event through the rows: load, apply, save.
    fn step(s: &Store, t: &str, f: impl FnOnce(&mut Loaded) -> Vec<Action>) -> Vec<Action> {
        s.transaction::<_, StoreError>(|tx| {
            let mut l = load(tx, t, None)?.unwrap();
            let out = f(&mut l);
            l.save(tx, 100)?;
            Ok(out)
        })
        .unwrap()
    }

    #[test]
    fn the_state_is_the_rows_across_events() {
        let (_d, s) = store();
        let t = s
            .transaction::<_, StoreError>(|tx| mq::target_for(tx, "github.com/o/r", "main"))
            .unwrap()
            .id;
        let a = add(&s, &t, "ST-1");
        let out = step(&s, &t, |l| {
            let p = l.policy.clone();
            l.queue.apply(
                &p,
                Event::Arrive {
                    id: a.clone(),
                    key: "ST-1".into(),
                    at: 1,
                    after: vec![],
                },
            )
        });
        assert!(matches!(&out[..], [Action::Start { batch: 1, .. }]));
        let b = add(&s, &t, "ST-2");
        let c = add(&s, &t, "ST-3");
        for (id, key) in [(&b, "ST-2"), (&c, "ST-3")] {
            assert!(
                step(&s, &t, |l| {
                    let p = l.policy.clone();
                    l.queue.apply(
                        &p,
                        Event::Arrive {
                            id: id.clone(),
                            key: key.into(),
                            at: 2,
                            after: vec![],
                        },
                    )
                })
                .is_empty()
            );
        }
        // batch 1 lands: the train takes A, batch 2 starts with B and C
        let out = step(&s, &t, |l| {
            let p = l.policy.clone();
            let out = l.queue.apply(
                &p,
                Event::Done {
                    batch: 1,
                    outcome: Outcome::Landed {
                        commit: "c1".into(),
                    },
                },
            );
            l.trains.apply(
                &p,
                train::Event::Landed {
                    id: a.clone(),
                    at: 3,
                },
            );
            l.landed.push((a.clone(), Some("c1".into())));
            out
        });
        assert_eq!(out.len(), 2);
        let row = |id: &str| s.reader(|c| mq::entry_get(c, id)).unwrap().unwrap();
        assert_eq!(
            (row(&a).state.as_str(), row(&a).train_no),
            ("landed", Some(1))
        );
        assert_eq!(row(&a).landed_commit.as_deref(), Some("c1"));
        assert_eq!(
            (row(&b).state.as_str(), row(&b).batch_no),
            ("batched", Some(2))
        );
        // batch 2 fails and is split: the rows say so
        step(&s, &t, |l| {
            let p = l.policy.clone();
            l.queue.apply(
                &p,
                Event::Done {
                    batch: 2,
                    outcome: Outcome::Failed { tail: "x".into() },
                },
            )
        });
        let live = s.reader(|c| mq::batches_live(c, &t)).unwrap();
        let st: Vec<(i64, &str)> = live.iter().map(|b| (b.no, b.state.as_str())).collect();
        assert_eq!(st, [(2, "split"), (3, "running"), (4, "waiting")]);
        // B fails alone: returned with its reason
        step(&s, &t, |l| {
            let p = l.policy.clone();
            let out = l.queue.apply(
                &p,
                Event::Done {
                    batch: 3,
                    outcome: Outcome::Failed { tail: "x".into() },
                },
            );
            l.reasons.push((b.clone(), "checks failed".into()));
            out
        });
        assert_eq!(row(&b).state, "returned");
        assert_eq!(row(&b).reason.as_deref(), Some("checks failed"));
        assert_eq!(row(&c).batch_no, Some(4));
        // the train closes and releases: A is released
        step(&s, &t, |l| {
            let p = l.policy.clone();
            l.trains.apply(
                &p,
                train::Event::Now {
                    at: 5,
                    approved: true,
                },
            );
            l.trains.apply(
                &p,
                train::Event::Cut {
                    train: 1,
                    result: train::CutResult::Released {
                        version: Some("1.0.0".into()),
                    },
                },
            );
            vec![]
        });
        assert_eq!(row(&a).state, "released");
        let tr = s.reader(|c| mq::train_get(c, &t, 1)).unwrap().unwrap();
        assert_eq!(tr.state, "released");
        assert!(tr.ended.is_some());
        // counters persist
        let l = s.reader(|c| load(c, &t, None)).unwrap().unwrap();
        assert_eq!((l.queue.next, l.trains.next), (4, 1));
        assert_eq!(l.queue.busy().map(|b| b.no), Some(4));
    }
}
