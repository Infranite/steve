//! The merge queue of one target (an upstream repository and a branch), as a pure state machine. No
//! git, no store, no clock: the caller loads the target's rows into a [`Queue`], feeds an [`Event`]
//! with the target's [`Policy`], carries out the [`Action`]s it gets back and writes the rows back,
//! in one transaction. The rules are what the tests below say:
//!
//! - **idle starts at once**: with nothing under way, up to `max_batch` ready entries from the head
//!   of the queue form a batch and start; there is no collecting window (`settle_s` only debounces a
//!   burst, from the latest arrival, and is 0 by default);
//! - **busy batches itself**: what arrives while a batch is under way waits and starts together as
//!   the next batch, so batches grow with the load;
//! - **one under way** (`speculate` 1);
//! - **a definite failure is bisected, the passing half lands first**: a failed batch of one returns
//!   that entry to its agent; a failed batch of more is split into a first half and a second half,
//!   which run before anything else, first half first. A passing half lands, and the next half runs
//!   on the new tip; a failing half is split again. When only the combination fails, the half that
//!   runs after the landed one fails again, down to one entry, which is returned saying with which
//!   landed changes it fails. A batch never runs more than `2·⌈log2 n⌉+1` rounds of checks (`n` its
//!   original size): past that a card asks;
//! - **dependencies keep their order**: an entry runs only after the entries it comes after (by
//!   item) landed or in the same batch before it; one whose predecessor was returned waits in the
//!   queue for it;
//! - **no result is never retried by itself**: checks that gave no result (killed, timed out, the
//!   Node gone) or a push whose outcome reconciliation could not tell hold the queue with a card;
//!   only an answer (run again, land without checks, return the batch) moves it on.

use serde::{Deserialize, Serialize};

use super::policy::Policy;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryState {
    Queued,
    /// In a batch under way or waiting its turn.
    Batched,
    Landed,
    /// Sent back to its agent (kept while an entry waits for it).
    Returned,
    Withdrawn,
}

/// One item's request to merge (one delivered head).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    /// What dependencies name: the item (an entry of the same item supersedes a returned one).
    pub key: String,
    pub arrived: i64,
    /// Keys of the entries this one comes after.
    #[serde(default)]
    pub after: Vec<String>,
    pub state: EntryState,
    pub batch: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BatchState {
    /// Integrating, checking or pushing (the caller's own steps).
    Running,
    /// A half of a failed batch, waiting its turn.
    Waiting,
    /// No result: a card asks.
    Unknown,
    Landed,
    /// Failed and split into halves.
    Split,
    /// Its members were returned (or it emptied).
    Returned,
}

impl BatchState {
    /// Still to finish: the queue keeps it.
    pub fn live(self) -> bool {
        matches!(self, Self::Running | Self::Waiting | Self::Unknown)
    }
}

/// A batch `MQ-<no>`; the halves of a split one name it as `parent` and share its `root`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Batch {
    pub no: u64,
    pub parent: Option<u64>,
    pub root: u64,
    pub members: Vec<String>,
    pub state: BatchState,
    /// Times its checks were run again after giving no result.
    #[serde(default)]
    pub reruns: u32,
}

/// One target's queue: its live entries and batches (and, for a batch being bisected, the ended
/// batches of the same root: they count its rounds and say what landed before).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Queue {
    pub entries: Vec<Entry>,
    pub batches: Vec<Batch>,
    /// The number the next batch takes.
    pub next: u64,
    /// When the latest entry arrived (for `settle_s`).
    pub last_arrival: Option<i64>,
    /// One batch may start while the queue is paused.
    #[serde(default)]
    pub let_through: bool,
}

/// What a batch under way came to (the caller's integrate, check and push).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Integrated, checked (or with nothing to check) and pushed: the target is at `commit`.
    Landed { commit: String },
    /// The checks failed, definitely: their output's tail.
    Failed { tail: String },
    /// No result (checks killed or past their deadline, the Node gone, a push reconciliation could
    /// not settle, the target kept moving).
    Unknown { why: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    /// Run the batch's checks again.
    Rerun,
    /// Land it without its checks (only a person answers this).
    Skip,
    /// Send all its members back.
    Return,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// An entry asks to merge.
    Arrive {
        id: String,
        key: String,
        at: i64,
        after: Vec<String>,
    },
    /// An entry whose change is on the target already: landed without a batch.
    Present {
        id: String,
        key: String,
        at: i64,
    },
    /// An entry leaves the queue (its item withdrew). Not one in a batch under way.
    Withdraw {
        id: String,
    },
    /// A member left `batch` while it integrated: a conflict, a clashing numbered file, a commit
    /// not to be found. It goes back to its agent; the batch goes on without it.
    Left {
        batch: u64,
        id: String,
        why: String,
    },
    Done {
        batch: u64,
        outcome: Outcome,
    },
    /// Your answer on a batch's card.
    Answer {
        batch: u64,
        answer: Answer,
    },
    /// Time passed (a debounce may be over).
    Tick {
        at: i64,
    },
    /// Pause the queue, or go on.
    Pause(bool),
    /// While paused, let one batch through.
    Once {
        at: i64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Why {
    /// Its checks failed with it alone on top of what landed; `with`: the changes of the same
    /// original batch that landed before it (it fails together with them).
    Failed { tail: String, with: Vec<String> },
    /// It left while integrating (the caller's reason).
    Left(String),
    /// You returned its batch.
    Returned,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CardKind {
    /// The checks or the push gave no result.
    NoResult(String),
    /// Bisecting this batch would pass the limit of rounds.
    OverLimit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Integrate `members` in order on the target's tip, check, push. `parent`: the batch this is
    /// a half of. `checks`: false when you said to land it without them.
    Start {
        batch: u64,
        parent: Option<u64>,
        members: Vec<String>,
        checks: bool,
    },
    /// `members` landed (`batch` `None`: found on the target already).
    Land {
        batch: Option<u64>,
        members: Vec<String>,
        commit: Option<String>,
    },
    /// Send `id` back to its agent.
    Return { id: String, why: Why },
    /// `id` went back to the queue: what it comes after was returned.
    Requeue { id: String },
    /// Ask about `batch`.
    Card { batch: u64, kind: CardKind },
    /// `batch` failed and was split into `halves`.
    Split { batch: u64, halves: [u64; 2] },
}

/// The most rounds of checks a batch of `n` gets: `2·⌈log2 n⌉+1`.
pub fn round_limit(n: usize) -> u32 {
    let mut log = 0;
    while (1usize << log) < n.max(1) {
        log += 1;
    }
    2 * log + 1
}

impl Queue {
    pub fn entry(&self, id: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.id == id)
    }

    fn entry_mut(&mut self, id: &str) -> Option<&mut Entry> {
        self.entries.iter_mut().find(|e| e.id == id)
    }

    pub fn batch(&self, no: u64) -> Option<&Batch> {
        self.batches.iter().find(|b| b.no == no)
    }

    fn batch_mut(&mut self, no: u64) -> Option<&mut Batch> {
        self.batches.iter_mut().find(|b| b.no == no)
    }

    /// The batch under way or held by a card.
    pub fn busy(&self) -> Option<&Batch> {
        self.batches
            .iter()
            .find(|b| matches!(b.state, BatchState::Running | BatchState::Unknown))
    }

    fn depth(&self, b: &Batch) -> usize {
        let mut d = 0;
        let mut p = b.parent;
        while let Some(no) = p {
            d += 1;
            p = self.batch(no).and_then(|b| b.parent);
            if d > 64 {
                break;
            }
        }
        d
    }

    /// The waiting half that runs next: the deepest, then the oldest (depth first: a half that
    /// failed is split again before the halves waiting behind it run).
    fn next_waiting(&self) -> Option<u64> {
        self.batches
            .iter()
            .filter(|b| b.state == BatchState::Waiting)
            .max_by(|a, b| self.depth(a).cmp(&self.depth(b)).then(b.no.cmp(&a.no)))
            .map(|b| b.no)
    }

    /// Rounds of checks run for `root` so far (batches that started).
    fn rounds(&self, root: u64) -> u32 {
        self.batches
            .iter()
            .filter(|b| b.root == root && b.state != BatchState::Waiting)
            .count() as u32
    }

    fn root_size(&self, root: u64) -> usize {
        self.batch(root).map_or(1, |b| b.members.len())
    }

    /// What `root` landed so far, in order.
    fn landed_of(&self, root: u64) -> Vec<String> {
        let mut v: Vec<&Batch> = self
            .batches
            .iter()
            .filter(|b| b.root == root && b.state == BatchState::Landed)
            .collect();
        v.sort_by_key(|b| b.no);
        v.into_iter().flat_map(|b| b.members.clone()).collect()
    }

    /// `e` may run now, with `before` running ahead of it in the same batch: everything it comes
    /// after landed, or is in `before`.
    fn ready(&self, e: &Entry, before: &[String]) -> bool {
        e.after.iter().all(|k| {
            self.entries.iter().filter(|o| &o.key == k).all(|o| {
                matches!(o.state, EntryState::Landed | EntryState::Withdrawn)
                    || before.contains(&o.id)
            })
        })
    }

    /// Apply `ev`; what the caller has to do, in order.
    pub fn apply(&mut self, policy: &Policy, ev: Event) -> Vec<Action> {
        let mut out = vec![];
        let mut now = None;
        match ev {
            Event::Arrive { id, key, at, after } => {
                // already there (the caller wrote its row first): only what may start now
                if self.entry(&id).is_some() {
                    self.start(policy, Some(at), &mut out);
                    return out;
                }
                // a newer request of the same item supersedes one that was returned
                self.entries
                    .retain(|e| !(e.key == key && e.state == EntryState::Returned));
                self.entries.push(Entry {
                    id,
                    key,
                    arrived: at,
                    after,
                    state: EntryState::Queued,
                    batch: None,
                });
                self.last_arrival = Some(self.last_arrival.map_or(at, |l| l.max(at)));
                now = Some(at);
            }
            Event::Present { id, key, at } => {
                self.entries
                    .retain(|e| !(e.key == key && e.state == EntryState::Returned));
                match self.entry_mut(&id) {
                    Some(e) if e.state == EntryState::Queued => e.state = EntryState::Landed,
                    Some(_) => return out,
                    None => self.entries.push(Entry {
                        id: id.clone(),
                        key,
                        arrived: at,
                        after: vec![],
                        state: EntryState::Landed,
                        batch: None,
                    }),
                }
                out.push(Action::Land {
                    batch: None,
                    members: vec![id],
                    commit: None,
                });
                now = Some(at);
            }
            Event::Withdraw { id } => {
                let Some(e) = self.entry(&id).cloned() else {
                    return out;
                };
                match (
                    e.state,
                    e.batch.and_then(|b| self.batch(b)).map(|b| b.state),
                ) {
                    (EntryState::Queued, _) => {}
                    (EntryState::Batched, Some(BatchState::Waiting)) => {
                        let no = e.batch.unwrap_or_default();
                        if let Some(b) = self.batch_mut(no) {
                            b.members.retain(|m| *m != id);
                            if b.members.is_empty() {
                                b.state = BatchState::Returned;
                            }
                        }
                    }
                    _ => return out,
                }
                if let Some(e) = self.entry_mut(&id) {
                    e.state = EntryState::Withdrawn;
                    e.batch = None;
                }
            }
            Event::Left { batch, id, why } => {
                let Some(b) = self
                    .batch_mut(batch)
                    .filter(|b| b.state == BatchState::Running)
                else {
                    return out;
                };
                if !b.members.contains(&id) {
                    return out;
                }
                b.members.retain(|m| *m != id);
                let emptied = b.members.is_empty();
                if emptied {
                    b.state = BatchState::Returned;
                }
                self.returned(&id);
                out.push(Action::Return {
                    id,
                    why: Why::Left(why),
                });
            }
            Event::Done { batch, outcome } => {
                let Some(b) = self
                    .batch(batch)
                    .filter(|b| b.state == BatchState::Running)
                    .cloned()
                else {
                    return out;
                };
                match outcome {
                    Outcome::Landed { commit } => {
                        self.set(batch, BatchState::Landed);
                        for m in &b.members {
                            if let Some(e) = self.entry_mut(m) {
                                e.state = EntryState::Landed;
                            }
                        }
                        out.push(Action::Land {
                            batch: Some(batch),
                            members: b.members.clone(),
                            commit: Some(commit),
                        });
                    }
                    Outcome::Failed { tail } if b.members.len() <= 1 => {
                        let with = self.landed_of(b.root);
                        self.set(batch, BatchState::Returned);
                        for m in &b.members {
                            self.returned(m);
                            out.push(Action::Return {
                                id: m.clone(),
                                why: Why::Failed {
                                    tail: tail.clone(),
                                    with: with.clone(),
                                },
                            });
                        }
                    }
                    Outcome::Failed { .. } => {
                        if self.rounds(b.root) + 1 > round_limit(self.root_size(b.root)) {
                            self.set(batch, BatchState::Unknown);
                            out.push(Action::Card {
                                batch,
                                kind: CardKind::OverLimit,
                            });
                        } else {
                            self.set(batch, BatchState::Split);
                            let half = b.members.len().div_ceil(2);
                            let mut halves = [0; 2];
                            for (i, part) in [&b.members[..half], &b.members[half..]]
                                .into_iter()
                                .enumerate()
                            {
                                let no = self.number();
                                halves[i] = no;
                                for m in part {
                                    if let Some(e) = self.entry_mut(m) {
                                        e.batch = Some(no);
                                    }
                                }
                                self.batches.push(Batch {
                                    no,
                                    parent: Some(batch),
                                    root: b.root,
                                    members: part.to_vec(),
                                    state: BatchState::Waiting,
                                    reruns: 0,
                                });
                            }
                            out.push(Action::Split { batch, halves });
                        }
                    }
                    Outcome::Unknown { why } => {
                        self.set(batch, BatchState::Unknown);
                        out.push(Action::Card {
                            batch,
                            kind: CardKind::NoResult(why),
                        });
                    }
                }
            }
            Event::Answer { batch, answer } => {
                let Some(b) = self
                    .batch(batch)
                    .filter(|b| b.state == BatchState::Unknown)
                    .cloned()
                else {
                    return out;
                };
                match answer {
                    Answer::Rerun | Answer::Skip => {
                        if let Some(x) = self.batch_mut(batch) {
                            x.state = BatchState::Running;
                            if answer == Answer::Rerun {
                                x.reruns += 1;
                            }
                        }
                        out.push(Action::Start {
                            batch,
                            parent: b.parent,
                            members: b.members.clone(),
                            checks: answer == Answer::Rerun,
                        });
                    }
                    Answer::Return => {
                        self.set(batch, BatchState::Returned);
                        for m in &b.members {
                            self.returned(m);
                            out.push(Action::Return {
                                id: m.clone(),
                                why: Why::Returned,
                            });
                        }
                    }
                }
            }
            Event::Tick { at } => now = Some(at),
            Event::Pause(p) => {
                if !p {
                    self.let_through = false;
                }
                return out;
            }
            Event::Once { at } => {
                self.let_through = true;
                now = Some(at);
            }
        }
        self.start(policy, now, &mut out);
        out
    }

    fn number(&mut self) -> u64 {
        let top = self.batches.iter().map(|b| b.no).max().unwrap_or(0);
        self.next = self.next.max(top) + 1;
        self.next
    }

    fn set(&mut self, no: u64, s: BatchState) {
        if let Some(b) = self.batch_mut(no) {
            b.state = s;
        }
    }

    fn returned(&mut self, id: &str) {
        if let Some(e) = self.entry_mut(id) {
            e.state = EntryState::Returned;
            e.batch = None;
        }
    }

    /// Start what may start: the next waiting half, else (unless paused or settling) a new batch
    /// from the head of the queue. `now`: the time, when the event said it.
    fn start(&mut self, policy: &Policy, now: Option<i64>, out: &mut Vec<Action>) {
        while self.busy().is_none() {
            if let Some(no) = self.next_waiting() {
                let b = self.batch(no).cloned().expect("waiting");
                // a member whose predecessor was returned meanwhile waits in the queue for it
                let mut keep: Vec<String> = vec![];
                for m in &b.members {
                    match self.entry(m).cloned() {
                        Some(e) if self.ready(&e, &keep) => keep.push(m.clone()),
                        Some(_) => {
                            if let Some(e) = self.entry_mut(m) {
                                e.state = EntryState::Queued;
                                e.batch = None;
                            }
                            out.push(Action::Requeue { id: m.clone() });
                        }
                        None => {}
                    }
                }
                let x = self.batch_mut(no).expect("waiting");
                x.members = keep.clone();
                if keep.is_empty() {
                    x.state = BatchState::Returned;
                    continue;
                }
                x.state = BatchState::Running;
                out.push(Action::Start {
                    batch: no,
                    parent: b.parent,
                    members: keep,
                    checks: true,
                });
                return;
            }
            if policy.paused && !self.let_through {
                return;
            }
            let settled = match (now, self.last_arrival) {
                _ if policy.settle_s == 0 => true,
                (_, None) => true,
                (Some(t), Some(l)) => t - l >= policy.settle_s as i64 * 1000,
                (None, Some(_)) => false,
            };
            if !settled {
                return;
            }
            let mut queued: Vec<Entry> = self
                .entries
                .iter()
                .filter(|e| e.state == EntryState::Queued)
                .cloned()
                .collect();
            queued.sort_by_key(|e| e.arrived);
            // in arrival order, each once what it comes after is landed or taken before it
            let mut take: Vec<String> = vec![];
            loop {
                let before = take.len();
                for e in &queued {
                    if take.len() >= policy.batch_limit() {
                        break;
                    }
                    if !take.contains(&e.id) && self.ready(e, &take) {
                        take.push(e.id.clone());
                    }
                }
                if take.len() == before || take.len() >= policy.batch_limit() {
                    break;
                }
            }
            if take.is_empty() {
                return;
            }
            let no = self.number();
            for m in &take {
                if let Some(e) = self.entry_mut(m) {
                    e.state = EntryState::Batched;
                    e.batch = Some(no);
                }
            }
            self.batches.push(Batch {
                no,
                parent: None,
                root: no,
                members: take.clone(),
                state: BatchState::Running,
                reruns: 0,
            });
            self.let_through = false;
            out.push(Action::Start {
                batch: no,
                parent: None,
                members: take,
                checks: true,
            });
            return;
        }
    }

    /// Drop what no longer matters: ended batches of roots with nothing live, entries that ended
    /// and nothing waits for. What the store need not load again.
    pub fn compact(&mut self) {
        let live: Vec<u64> = self
            .batches
            .iter()
            .filter(|b| b.state.live())
            .map(|b| b.root)
            .collect();
        self.batches.retain(|b| live.contains(&b.root));
        let wanted: Vec<String> = self
            .entries
            .iter()
            .filter(|e| matches!(e.state, EntryState::Queued | EntryState::Batched))
            .flat_map(|e| e.after.clone())
            .collect();
        self.entries.retain(|e| match e.state {
            EntryState::Queued | EntryState::Batched => true,
            EntryState::Returned => wanted.contains(&e.key),
            EntryState::Landed | EntryState::Withdrawn => false,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p() -> Policy {
        Policy::default()
    }

    fn arrive(q: &mut Queue, id: &str, at: i64) -> Vec<Action> {
        q.apply(
            &p(),
            Event::Arrive {
                id: id.into(),
                key: id.into(),
                at,
                after: vec![],
            },
        )
    }

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn start(batch: u64, parent: Option<u64>, m: &[&str]) -> Action {
        Action::Start {
            batch,
            parent,
            members: ids(m),
            checks: true,
        }
    }

    fn landed(q: &mut Queue, batch: u64) -> Vec<Action> {
        q.apply(
            &p(),
            Event::Done {
                batch,
                outcome: Outcome::Landed {
                    commit: format!("c{batch}"),
                },
            },
        )
    }

    fn failed(q: &mut Queue, batch: u64) -> Vec<Action> {
        q.apply(
            &p(),
            Event::Done {
                batch,
                outcome: Outcome::Failed {
                    tail: "boom".into(),
                },
            },
        )
    }

    fn land(batch: u64, m: &[&str]) -> Action {
        Action::Land {
            batch: Some(batch),
            members: ids(m),
            commit: Some(format!("c{batch}")),
        }
    }

    fn back(id: &str, with: &[&str]) -> Action {
        Action::Return {
            id: id.into(),
            why: Why::Failed {
                tail: "boom".into(),
                with: ids(with),
            },
        }
    }

    #[test]
    fn idle_starts_at_once_and_busy_batches_what_arrives() {
        let mut q = Queue::default();
        assert_eq!(arrive(&mut q, "A", 0), [start(1, None, &["A"])]);
        // under way: B and C wait, no window
        assert!(arrive(&mut q, "B", 1).is_empty());
        assert!(arrive(&mut q, "C", 2).is_empty());
        assert_eq!(
            landed(&mut q, 1),
            [land(1, &["A"]), start(2, None, &["B", "C"])],
            "the next batch is everything that waited"
        );
        assert_eq!(landed(&mut q, 2), [land(2, &["B", "C"])]);
        assert!(q.busy().is_none());
    }

    #[test]
    fn a_batch_takes_at_most_max_batch_from_the_head() {
        let pol = Policy {
            max_batch: 2,
            ..p()
        };
        let mut q = Queue::default();
        for (i, id) in ["A", "B", "C", "D"].iter().enumerate() {
            q.apply(
                &pol,
                Event::Arrive {
                    id: id.to_string(),
                    key: id.to_string(),
                    at: i as i64,
                    after: vec![],
                },
            );
        }
        let out = q.apply(
            &pol,
            Event::Done {
                batch: 1,
                outcome: Outcome::Landed { commit: "x".into() },
            },
        );
        assert_eq!(out[1], start(2, None, &["B", "C"]));
    }

    #[test]
    fn settle_debounces_from_the_latest_arrival() {
        let pol = Policy {
            settle_s: 30,
            ..p()
        };
        let mut q = Queue::default();
        let a = |q: &mut Queue, id: &str, at| {
            q.apply(
                &pol,
                Event::Arrive {
                    id: id.into(),
                    key: id.into(),
                    at,
                    after: vec![],
                },
            )
        };
        assert!(a(&mut q, "A", 0).is_empty());
        assert!(a(&mut q, "B", 10_000).is_empty());
        assert!(q.apply(&pol, Event::Tick { at: 30_000 }).is_empty());
        assert_eq!(
            q.apply(&pol, Event::Tick { at: 40_000 }),
            [start(1, None, &["A", "B"])]
        );
    }

    #[test]
    fn a_failed_batch_of_one_returns_it() {
        let mut q = Queue::default();
        arrive(&mut q, "A", 0);
        assert_eq!(failed(&mut q, 1), [back("A", &[])]);
        assert_eq!(q.entry("A").unwrap().state, EntryState::Returned);
    }

    #[test]
    fn a_failed_pair_tests_the_first_lands_it_and_returns_the_culprit() {
        let mut q = Queue::default();
        arrive(&mut q, "X", 0);
        arrive(&mut q, "A", 1);
        arrive(&mut q, "B", 2);
        landed(&mut q, 1);
        // [A, B] fails: [A] runs on the tip, then [B] on the new tip
        assert_eq!(
            failed(&mut q, 2),
            [
                Action::Split {
                    batch: 2,
                    halves: [3, 4]
                },
                start(3, Some(2), &["A"])
            ]
        );
        assert_eq!(
            landed(&mut q, 3),
            [land(3, &["A"]), start(4, Some(2), &["B"])]
        );
        assert_eq!(failed(&mut q, 4), [back("B", &["A"])]);
        assert!(q.busy().is_none());
    }

    #[test]
    fn a_failing_first_half_is_returned_and_the_second_runs_after() {
        let mut q = Queue::default();
        arrive(&mut q, "X", 0);
        arrive(&mut q, "A", 1);
        arrive(&mut q, "B", 2);
        landed(&mut q, 1);
        failed(&mut q, 2);
        assert_eq!(
            failed(&mut q, 3),
            [back("A", &[]), start(4, Some(2), &["B"])]
        );
        assert_eq!(landed(&mut q, 4), [land(4, &["B"])]);
    }

    #[test]
    fn bisection_lands_the_innocent_and_finds_the_culprit_within_the_limit() {
        // eight in one batch; F breaks the build
        let mut q = Queue::default();
        arrive(&mut q, "X", 0);
        let m = ["A", "B", "C", "D", "E", "F", "G", "H"];
        for (i, id) in m.iter().enumerate() {
            arrive(&mut q, id, 1 + i as i64);
        }
        landed(&mut q, 1);
        let bad = |members: &[String]| members.iter().any(|x| x == "F");
        let mut ev = failed(&mut q, 2);
        let (mut landed_ids, mut returned) = (vec![], vec![]);
        let mut rounds = 1;
        loop {
            let mut next = None;
            for a in &ev {
                match a {
                    Action::Start { batch, members, .. } => next = Some((*batch, members.clone())),
                    Action::Land { members, .. } => landed_ids.extend(members.clone()),
                    Action::Return { id, .. } => returned.push(id.clone()),
                    _ => {}
                }
            }
            let Some((b, members)) = next else { break };
            rounds += 1;
            ev = if bad(&members) {
                failed(&mut q, b)
            } else {
                landed(&mut q, b)
            };
        }
        assert_eq!(returned, ["F"]);
        let mut l = landed_ids.clone();
        l.sort();
        assert_eq!(l, ["A", "B", "C", "D", "E", "G", "H"]);
        assert!(rounds <= round_limit(8), "{rounds}");
        // the first half went first
        assert_eq!(&landed_ids[..4], ["A", "B", "C", "D"]);
    }

    #[test]
    fn when_only_the_combination_fails_the_last_one_is_returned_naming_the_others() {
        // A and B pass alone, fail together
        let mut q = Queue::default();
        arrive(&mut q, "X", 0);
        arrive(&mut q, "A", 1);
        arrive(&mut q, "B", 2);
        landed(&mut q, 1);
        failed(&mut q, 2); // [A, B]
        assert_eq!(
            landed(&mut q, 3),
            [land(3, &["A"]), start(4, Some(2), &["B"])]
        );
        // B on a tip with A: fails, and says so
        assert_eq!(failed(&mut q, 4), [back("B", &["A"])]);
    }

    #[test]
    fn past_the_round_limit_a_card_asks() {
        assert_eq!(round_limit(1), 1);
        assert_eq!(round_limit(2), 3);
        assert_eq!(round_limit(3), 5);
        assert_eq!(round_limit(8), 7);
        // a batch of more past its limit
        let mut q = Queue::default();
        arrive(&mut q, "A", 0);
        arrive(&mut q, "B", 1);
        q.batches[0].members = ids(&["A"]);
        q.batches.push(Batch {
            no: 7,
            parent: Some(1),
            root: 1,
            members: ids(&["A", "B"]),
            state: BatchState::Running,
            reruns: 0,
        });
        q.batches[0].state = BatchState::Split;
        assert_eq!(
            failed(&mut q, 7),
            [Action::Card {
                batch: 7,
                kind: CardKind::OverLimit
            }]
        );
    }

    #[test]
    fn no_result_holds_the_queue_until_an_answer() {
        let mut q = Queue::default();
        arrive(&mut q, "A", 0);
        arrive(&mut q, "B", 1);
        assert_eq!(
            q.apply(
                &p(),
                Event::Done {
                    batch: 1,
                    outcome: Outcome::Unknown {
                        why: "killed".into()
                    }
                }
            ),
            [Action::Card {
                batch: 1,
                kind: CardKind::NoResult("killed".into())
            }]
        );
        assert!(q.apply(&p(), Event::Tick { at: 99 }).is_empty(), "held");
        assert_eq!(
            q.apply(
                &p(),
                Event::Answer {
                    batch: 1,
                    answer: Answer::Rerun
                }
            ),
            [start(1, None, &["A"])]
        );
        assert_eq!(q.batch(1).unwrap().reruns, 1);
        q.apply(
            &p(),
            Event::Done {
                batch: 1,
                outcome: Outcome::Unknown {
                    why: "again".into(),
                },
            },
        );
        assert_eq!(
            q.apply(
                &p(),
                Event::Answer {
                    batch: 1,
                    answer: Answer::Skip
                }
            ),
            [Action::Start {
                batch: 1,
                parent: None,
                members: ids(&["A"]),
                checks: false
            }]
        );
        q.apply(
            &p(),
            Event::Done {
                batch: 1,
                outcome: Outcome::Unknown { why: "push".into() },
            },
        );
        assert_eq!(
            q.apply(
                &p(),
                Event::Answer {
                    batch: 1,
                    answer: Answer::Return
                }
            ),
            [
                Action::Return {
                    id: "A".into(),
                    why: Why::Returned
                },
                start(2, None, &["B"])
            ]
        );
    }

    #[test]
    fn a_member_that_conflicts_leaves_and_the_batch_goes_on() {
        let mut q = Queue::default();
        arrive(&mut q, "X", 0);
        arrive(&mut q, "A", 1);
        arrive(&mut q, "B", 2);
        landed(&mut q, 1);
        assert_eq!(
            q.apply(
                &p(),
                Event::Left {
                    batch: 2,
                    id: "A".into(),
                    why: "conflict: a.txt".into()
                }
            ),
            [Action::Return {
                id: "A".into(),
                why: Why::Left("conflict: a.txt".into())
            }]
        );
        assert_eq!(landed(&mut q, 2), [land(2, &["B"])]);
        // a batch emptied by leavers ends: the next starts
        arrive(&mut q, "C", 3);
        arrive(&mut q, "D", 4);
        let out = q.apply(
            &p(),
            Event::Left {
                batch: 3,
                id: "C".into(),
                why: "x".into(),
            },
        );
        assert_eq!(out[1], start(4, None, &["D"]));
    }

    #[test]
    fn dependencies_go_in_order_and_wait_for_a_returned_predecessor() {
        let mut q = Queue::default();
        arrive(&mut q, "X", 0);
        arrive(&mut q, "A", 1);
        q.apply(
            &p(),
            Event::Arrive {
                id: "B".into(),
                key: "B".into(),
                at: 2,
                after: ids(&["A"]),
            },
        );
        // B comes after A: both in the next batch, A first
        assert_eq!(landed(&mut q, 1)[1], start(2, None, &["A", "B"]));
        // the batch fails, A alone fails: B does not run without it, it waits in the queue
        failed(&mut q, 2);
        assert_eq!(
            failed(&mut q, 3),
            [back("A", &[]), Action::Requeue { id: "B".into() }]
        );
        assert!(q.busy().is_none());
        assert!(
            q.apply(&p(), Event::Tick { at: 9 }).is_empty(),
            "B waits for A"
        );
        // A comes back (rebased): it and B, in that order, in one batch
        assert_eq!(
            q.apply(
                &p(),
                Event::Arrive {
                    id: "A2".into(),
                    key: "A".into(),
                    at: 10,
                    after: vec![]
                }
            ),
            [start(5, None, &["A2", "B"])]
        );
    }

    #[test]
    fn paused_starts_nothing_but_lets_one_through() {
        let pol = Policy {
            paused: true,
            ..p()
        };
        let mut q = Queue::default();
        let out = q.apply(
            &pol,
            Event::Arrive {
                id: "A".into(),
                key: "A".into(),
                at: 0,
                after: vec![],
            },
        );
        assert!(out.is_empty());
        assert_eq!(
            q.apply(&pol, Event::Once { at: 1 }),
            [start(1, None, &["A"])]
        );
        assert!(!q.let_through);
    }

    #[test]
    fn a_change_on_the_target_already_lands_without_a_batch() {
        let mut q = Queue::default();
        assert_eq!(
            q.apply(
                &p(),
                Event::Present {
                    id: "A".into(),
                    key: "A".into(),
                    at: 0
                }
            ),
            [Action::Land {
                batch: None,
                members: ids(&["A"]),
                commit: None
            }]
        );
        assert!(q.busy().is_none());
    }

    #[test]
    fn withdrawn_entries_leave_the_queue_and_waiting_halves() {
        let mut q = Queue::default();
        arrive(&mut q, "X", 0);
        arrive(&mut q, "A", 1);
        q.apply(&p(), Event::Withdraw { id: "A".into() });
        assert_eq!(q.entry("A").unwrap().state, EntryState::Withdrawn);
        assert_eq!(landed(&mut q, 1), [land(1, &["X"])]);
        // under way: not withdrawn
        arrive(&mut q, "B", 2);
        q.apply(&p(), Event::Withdraw { id: "B".into() });
        assert_eq!(q.entry("B").unwrap().state, EntryState::Batched);
    }

    #[test]
    fn compact_keeps_what_is_live_and_what_is_waited_for() {
        let mut q = Queue::default();
        arrive(&mut q, "X", 0);
        arrive(&mut q, "A", 1);
        q.apply(
            &p(),
            Event::Arrive {
                id: "B".into(),
                key: "B".into(),
                at: 2,
                after: ids(&["A"]),
            },
        );
        landed(&mut q, 1);
        failed(&mut q, 2);
        failed(&mut q, 3);
        q.compact();
        let mut kept: Vec<&str> = q.entries.iter().map(|e| e.id.as_str()).collect();
        kept.sort();
        assert_eq!(kept, ["A", "B"], "X landed and goes; A is waited for");
        assert!(q.batches.is_empty(), "nothing live");
        assert_eq!(q.next, 4, "numbers go on");
    }
}
