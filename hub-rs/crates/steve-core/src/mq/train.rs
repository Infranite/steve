//! The release train of one target, as a pure state machine (the same contract as
//! [`super::queue`]: rows in, events, actions out, rows back). Merging is continuous; releasing is
//! what is worth batching (a tag, a restart, a version number spent), so the window lives here:
//!
//! - what lands joins the open train; the train closes at the earliest of: `release_window_s`
//!   after its first change landed, `release_max` changes, `release_idle_s` without a landing (off
//!   by default), or when you say "release now" (on the train, or "merge and release now" on an
//!   item: once that item lands);
//! - a closed train asks once (one card per train). What lands while that card waits joins the same
//!   train and the card is updated in place; once you approve, the train's changes are fixed and
//!   what lands later goes to the next train;
//! - "release now" from you closes and approves at once (no second question);
//! - one train releases at a time; an open train that came due meanwhile closes when it is over;
//! - a failed release keeps its changes until you retry or give up (they then go with the next
//!   train); a release whose outcome reconciliation could not tell asks, and is never cut again by
//!   itself;
//! - the target's branch failing before a release halts its train (a card offers to revert a
//!   batch, wait for a fix, or drop the train); what lands meanwhile, a fix included, joins it.

use serde::{Deserialize, Serialize};

use super::policy::Policy;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrainState {
    /// Collecting what lands.
    Open,
    /// Closed, its card waiting for you.
    Awaiting,
    /// Approved: being released.
    Cutting,
    Released,
    /// The release failed: a card asks.
    Failed,
    /// The release's outcome is not known: a card asks.
    Unknown,
    /// The target's branch failed before the release: a card asks.
    Halted,
    /// Given up (its changes went to the next train).
    Dropped,
}

impl TrainState {
    /// Closed and not over: no other train releases meanwhile.
    pub fn in_flight(self) -> bool {
        matches!(
            self,
            Self::Awaiting | Self::Cutting | Self::Failed | Self::Unknown | Self::Halted
        )
    }

    pub fn live(self) -> bool {
        self == Self::Open || self.in_flight()
    }
}

/// A train `RT-<no>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Train {
    pub no: u64,
    pub members: Vec<String>,
    /// When its first change landed (or it took changes over from a train given up).
    pub opened: i64,
    /// When its latest change landed.
    pub last: i64,
    pub state: TrainState,
    /// You asked to release it as soon as it may.
    #[serde(default)]
    pub now: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Trains {
    pub trains: Vec<Train>,
    pub next: u64,
    /// Entries asked to "merge and release now" that have not landed yet.
    #[serde(default)]
    pub wanted_now: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CutResult {
    /// Released (the version, for a release that names one).
    Released {
        version: Option<String>,
    },
    Failed {
        why: String,
    },
    Unknown {
        why: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Landed {
        id: String,
        at: i64,
    },
    /// "Release now": the open train closes; `approved`, it releases without asking again (you
    /// said so).
    Now {
        at: i64,
        approved: bool,
    },
    /// "Merge and release now" on one entry: its train closes as soon as it has landed.
    NowFor {
        id: String,
        at: i64,
    },
    Tick {
        at: i64,
    },
    /// Your answer on a train's card: release it, or not this time (its changes go with the next).
    Answer {
        train: u64,
        release: bool,
        at: i64,
    },
    /// What the release came to.
    Cut {
        train: u64,
        result: CutResult,
    },
    /// On a failed or unknown release: try again.
    Retry {
        train: u64,
    },
    /// On a failed, unknown or halted release: give up; its changes go with the next train.
    GiveUp {
        train: u64,
        at: i64,
    },
    /// The target's branch failed before its release.
    Red {
        why: String,
    },
    /// The target's branch passes again.
    Green {
        at: i64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CardKind {
    Failed(String),
    Unknown(String),
    Red(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Ask whether to release `train` (one card per train).
    Ask {
        train: u64,
        members: Vec<String>,
    },
    /// The card of `train` now lists `members` (updated in place).
    Update {
        train: u64,
        members: Vec<String>,
    },
    /// Release `train` (tag or the repository's release step) on the Node.
    Cut {
        train: u64,
        members: Vec<String>,
    },
    Released {
        train: u64,
        members: Vec<String>,
        version: Option<String>,
    },
    Card {
        train: u64,
        kind: CardKind,
    },
    /// `members` of `from` (given up) go with `into`.
    Carry {
        from: u64,
        into: u64,
        members: Vec<String>,
    },
}

impl Trains {
    pub fn train(&self, no: u64) -> Option<&Train> {
        self.trains.iter().find(|t| t.no == no)
    }

    fn train_mut(&mut self, no: u64) -> Option<&mut Train> {
        self.trains.iter_mut().find(|t| t.no == no)
    }

    pub fn open(&self) -> Option<&Train> {
        self.trains.iter().find(|t| t.state == TrainState::Open)
    }

    pub fn flight(&self) -> Option<&Train> {
        self.trains.iter().find(|t| t.state.in_flight())
    }

    fn number(&mut self) -> u64 {
        let top = self.trains.iter().map(|t| t.no).max().unwrap_or(0);
        self.next = self.next.max(top) + 1;
        self.next
    }

    /// The open train (made at `at` when there is none).
    fn open_at(&mut self, at: i64) -> &mut Train {
        if self.open().is_none() {
            let no = self.number();
            self.trains.push(Train {
                no,
                members: vec![],
                opened: at,
                last: at,
                state: TrainState::Open,
                now: false,
            });
        }
        self.trains
            .iter_mut()
            .find(|t| t.state == TrainState::Open)
            .expect("open")
    }

    pub fn apply(&mut self, policy: &Policy, ev: Event) -> Vec<Action> {
        let mut out = vec![];
        let at = match ev {
            Event::Landed { id, at } => {
                if self.trains.iter().any(|t| t.members.contains(&id)) {
                    return out;
                }
                let now = self.wanted_now.contains(&id);
                self.wanted_now.retain(|w| *w != id);
                // a card still waiting (or a halted train) takes it, and says so
                if let Some(t) = self
                    .trains
                    .iter_mut()
                    .find(|t| matches!(t.state, TrainState::Awaiting | TrainState::Halted))
                {
                    t.members.push(id);
                    t.last = at;
                    t.now |= now;
                    if t.state == TrainState::Awaiting {
                        out.push(Action::Update {
                            train: t.no,
                            members: t.members.clone(),
                        });
                    }
                    return out;
                }
                let t = self.open_at(at);
                if t.members.is_empty() {
                    t.opened = at;
                }
                t.members.push(id);
                t.last = at;
                t.now |= now;
                at
            }
            Event::Now { at, approved } => {
                if let Some(t) = self.train_mut_where(|t| t.state == TrainState::Awaiting)
                    && approved
                {
                    t.state = TrainState::Cutting;
                    out.push(Action::Cut {
                        train: t.no,
                        members: t.members.clone(),
                    });
                    return out;
                }
                match self.trains.iter_mut().find(|t| t.state == TrainState::Open) {
                    Some(t) if !t.members.is_empty() => t.now = true,
                    _ => return out,
                }
                let before = out.len();
                self.close_due(policy, at, &mut out);
                if approved {
                    // you said release now: the card that just opened is answered
                    if let Some(Action::Ask { train, members }) = out.get(before).cloned() {
                        out.truncate(before);
                        if let Some(t) = self.train_mut(train) {
                            t.state = TrainState::Cutting;
                        }
                        out.push(Action::Cut { train, members });
                    }
                }
                return out;
            }
            Event::NowFor { id, at } => {
                match self.trains.iter_mut().find(|t| t.members.contains(&id)) {
                    Some(t) if t.state == TrainState::Open => t.now = true,
                    Some(_) => return out,
                    None => {
                        if !self.wanted_now.contains(&id) {
                            self.wanted_now.push(id);
                        }
                        return out;
                    }
                }
                at
            }
            Event::Tick { at } => at,
            Event::Answer { train, release, at } => {
                let Some(t) =
                    self.train_mut_where(|t| t.no == train && t.state == TrainState::Awaiting)
                else {
                    return out;
                };
                if release {
                    t.state = TrainState::Cutting;
                    out.push(Action::Cut {
                        train,
                        members: t.members.clone(),
                    });
                    return out;
                }
                self.carry(train, at, &mut out);
                at
            }
            Event::Cut { train, result } => {
                let Some(t) = self.train_mut_where(|t| {
                    t.no == train && matches!(t.state, TrainState::Cutting | TrainState::Unknown)
                }) else {
                    return out;
                };
                match result {
                    CutResult::Released { version } => {
                        t.state = TrainState::Released;
                        out.push(Action::Released {
                            train,
                            members: t.members.clone(),
                            version,
                        });
                        let at = t.last;
                        // the next may be due already
                        self.close_due(policy, at, &mut out);
                    }
                    CutResult::Failed { why } => {
                        t.state = TrainState::Failed;
                        out.push(Action::Card {
                            train,
                            kind: CardKind::Failed(why),
                        });
                    }
                    CutResult::Unknown { why } => {
                        t.state = TrainState::Unknown;
                        out.push(Action::Card {
                            train,
                            kind: CardKind::Unknown(why),
                        });
                    }
                }
                return out;
            }
            Event::Retry { train } => {
                if let Some(t) = self.train_mut_where(|t| {
                    t.no == train && matches!(t.state, TrainState::Failed | TrainState::Unknown)
                }) {
                    t.state = TrainState::Cutting;
                    out.push(Action::Cut {
                        train,
                        members: t.members.clone(),
                    });
                }
                return out;
            }
            Event::GiveUp { train, at } => {
                if self.train(train).is_none_or(|t| {
                    !matches!(
                        t.state,
                        TrainState::Failed | TrainState::Unknown | TrainState::Halted
                    )
                }) {
                    return out;
                }
                self.carry(train, at, &mut out);
                at
            }
            Event::Red { why } => {
                let Some(t) = self.train_mut_where(|t| {
                    matches!(t.state, TrainState::Open | TrainState::Awaiting)
                        && !t.members.is_empty()
                }) else {
                    return out;
                };
                t.state = TrainState::Halted;
                out.push(Action::Card {
                    train: t.no,
                    kind: CardKind::Red(why),
                });
                return out;
            }
            Event::Green { at } => {
                if let Some(t) = self.train_mut_where(|t| t.state == TrainState::Halted) {
                    // open again: it closes (and asks) by its own rules
                    t.state = TrainState::Open;
                }
                at
            }
        };
        self.close_due(policy, at, &mut out);
        out
    }

    fn train_mut_where(&mut self, f: impl Fn(&Train) -> bool) -> Option<&mut Train> {
        self.trains.iter_mut().find(|t| f(t))
    }

    /// `train` is given up: its changes go with the open train (made now when there is none).
    fn carry(&mut self, train: u64, at: i64, out: &mut Vec<Action>) {
        let Some(t) = self.train_mut(train) else {
            return;
        };
        t.state = TrainState::Dropped;
        let members = t.members.clone();
        if members.is_empty() {
            return;
        }
        let into = self.open_at(at);
        if into.members.is_empty() {
            into.opened = at;
            into.last = at;
        }
        let mut all = members.clone();
        all.append(&mut into.members);
        into.members = all;
        let into = into.no;
        out.push(Action::Carry {
            from: train,
            into,
            members,
        });
    }

    /// Close the open train when it is due and nothing else is releasing.
    fn close_due(&mut self, policy: &Policy, at: i64, out: &mut Vec<Action>) {
        if self.flight().is_some() {
            return;
        }
        let Some(t) = self
            .trains
            .iter_mut()
            .find(|t| t.state == TrainState::Open && !t.members.is_empty())
        else {
            return;
        };
        let due = t.now
            || t.members.len() >= policy.train_limit()
            || at - t.opened >= policy.window_ms()
            || policy
                .release_idle_s
                .is_some_and(|i| at - t.last >= i as i64 * 1000);
        if due {
            t.state = TrainState::Awaiting;
            out.push(Action::Ask {
                train: t.no,
                members: t.members.clone(),
            });
        }
    }

    /// When the open train closes by time, if nothing else closes it first (for a countdown).
    pub fn closes_at(&self, policy: &Policy) -> Option<i64> {
        let t = self.open().filter(|t| !t.members.is_empty())?;
        let mut at = t.opened + policy.window_ms();
        if let Some(i) = policy.release_idle_s {
            at = at.min(t.last + i as i64 * 1000);
        }
        Some(at)
    }

    /// Drop the trains that are over.
    pub fn compact(&mut self) {
        self.trains.retain(|t| t.state.live());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: i64 = 3_600_000;

    fn p() -> Policy {
        Policy::default()
    }

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn land(t: &mut Trains, id: &str, at: i64) -> Vec<Action> {
        t.apply(&p(), Event::Landed { id: id.into(), at })
    }

    #[test]
    fn a_train_closes_after_its_window_from_the_first_landing() {
        let mut t = Trains::default();
        assert!(land(&mut t, "A", 0).is_empty());
        assert!(land(&mut t, "B", H).is_empty());
        assert_eq!(t.closes_at(&p()), Some(2 * H));
        assert!(t.apply(&p(), Event::Tick { at: 2 * H - 1 }).is_empty());
        assert_eq!(
            t.apply(&p(), Event::Tick { at: 2 * H }),
            [Action::Ask {
                train: 1,
                members: ids(&["A", "B"])
            }]
        );
    }

    #[test]
    fn a_full_train_closes_at_once() {
        let pol = Policy {
            release_max: 3,
            ..p()
        };
        let mut t = Trains::default();
        for (i, id) in ["A", "B"].iter().enumerate() {
            assert!(
                t.apply(
                    &pol,
                    Event::Landed {
                        id: id.to_string(),
                        at: i as i64
                    }
                )
                .is_empty()
            );
        }
        assert_eq!(
            t.apply(
                &pol,
                Event::Landed {
                    id: "C".into(),
                    at: 3
                }
            ),
            [Action::Ask {
                train: 1,
                members: ids(&["A", "B", "C"])
            }]
        );
    }

    #[test]
    fn idle_closing_is_off_by_default_and_closes_when_set() {
        let mut t = Trains::default();
        land(&mut t, "A", 0);
        assert!(t.apply(&p(), Event::Tick { at: H }).is_empty());
        let pol = Policy {
            release_idle_s: Some(1800),
            ..p()
        };
        assert_eq!(
            t.apply(&pol, Event::Tick { at: H }),
            [Action::Ask {
                train: 1,
                members: ids(&["A"])
            }]
        );
    }

    #[test]
    fn what_lands_while_the_card_waits_joins_it_and_after_approval_goes_next() {
        let mut t = Trains::default();
        land(&mut t, "A", 0);
        t.apply(&p(), Event::Tick { at: 2 * H });
        assert_eq!(
            land(&mut t, "B", 2 * H + 1),
            [Action::Update {
                train: 1,
                members: ids(&["A", "B"])
            }]
        );
        assert_eq!(
            t.apply(
                &p(),
                Event::Answer {
                    train: 1,
                    release: true,
                    at: 2 * H + 2
                }
            ),
            [Action::Cut {
                train: 1,
                members: ids(&["A", "B"])
            }]
        );
        // fixed now: C waits for the next train, even when it is due
        assert!(land(&mut t, "C", 2 * H + 3).is_empty());
        assert!(t.apply(&p(), Event::Tick { at: 5 * H }).is_empty());
        assert_eq!(
            t.apply(
                &p(),
                Event::Cut {
                    train: 1,
                    result: CutResult::Released {
                        version: Some("1.2.3".into())
                    }
                }
            )[0],
            Action::Released {
                train: 1,
                members: ids(&["A", "B"]),
                version: Some("1.2.3".into())
            }
        );
        // the next was due meanwhile: it closes once the release is over
        assert_eq!(
            t.apply(&p(), Event::Tick { at: 5 * H }),
            [Action::Ask {
                train: 2,
                members: ids(&["C"])
            }]
        );
    }

    #[test]
    fn release_now_closes_and_releases_without_a_second_question() {
        let mut t = Trains::default();
        land(&mut t, "A", 0);
        land(&mut t, "B", 1);
        assert_eq!(
            t.apply(
                &p(),
                Event::Now {
                    at: 2,
                    approved: true
                }
            ),
            [Action::Cut {
                train: 1,
                members: ids(&["A", "B"])
            }]
        );
        // closing now without approval asks
        let mut t = Trains::default();
        land(&mut t, "A", 0);
        assert_eq!(
            t.apply(
                &p(),
                Event::Now {
                    at: 1,
                    approved: false
                }
            ),
            [Action::Ask {
                train: 1,
                members: ids(&["A"])
            }]
        );
        assert!(
            Trains::default()
                .apply(
                    &p(),
                    Event::Now {
                        at: 0,
                        approved: true
                    }
                )
                .is_empty(),
            "nothing to release"
        );
    }

    #[test]
    fn merge_and_release_now_closes_the_train_once_the_item_landed() {
        let mut t = Trains::default();
        land(&mut t, "A", 0);
        assert!(
            t.apply(
                &p(),
                Event::NowFor {
                    id: "B".into(),
                    at: 1
                }
            )
            .is_empty(),
            "B is not merged yet"
        );
        assert_eq!(
            land(&mut t, "B", 2),
            [Action::Ask {
                train: 1,
                members: ids(&["A", "B"])
            }]
        );
        assert!(t.wanted_now.is_empty());
    }

    #[test]
    fn a_failed_release_waits_for_retry_or_gives_its_changes_to_the_next() {
        let mut t = Trains::default();
        land(&mut t, "A", 0);
        t.apply(
            &p(),
            Event::Now {
                at: 1,
                approved: true,
            },
        );
        assert_eq!(
            t.apply(
                &p(),
                Event::Cut {
                    train: 1,
                    result: CutResult::Failed {
                        why: "ci red".into()
                    }
                }
            ),
            [Action::Card {
                train: 1,
                kind: CardKind::Failed("ci red".into())
            }]
        );
        assert_eq!(
            t.apply(&p(), Event::Retry { train: 1 }),
            [Action::Cut {
                train: 1,
                members: ids(&["A"])
            }]
        );
        t.apply(
            &p(),
            Event::Cut {
                train: 1,
                result: CutResult::Failed {
                    why: "again".into(),
                },
            },
        );
        land(&mut t, "B", 5);
        assert_eq!(
            t.apply(&p(), Event::GiveUp { train: 1, at: 6 }),
            [Action::Carry {
                from: 1,
                into: 2,
                members: ids(&["A"])
            }]
        );
        assert_eq!(t.open().unwrap().members, ids(&["A", "B"]));
        assert_eq!(
            t.open().unwrap().opened,
            5,
            "timed from B, the earlier landing"
        );
    }

    #[test]
    fn an_unknown_release_asks_and_is_not_cut_again_by_itself() {
        let mut t = Trains::default();
        land(&mut t, "A", 0);
        t.apply(
            &p(),
            Event::Now {
                at: 1,
                approved: true,
            },
        );
        t.apply(
            &p(),
            Event::Cut {
                train: 1,
                result: CutResult::Unknown { why: "push".into() },
            },
        );
        assert!(t.apply(&p(), Event::Tick { at: 9 * H }).is_empty());
        // you looked: it was released
        assert_eq!(
            t.apply(
                &p(),
                Event::Cut {
                    train: 1,
                    result: CutResult::Released { version: None }
                }
            ),
            [Action::Released {
                train: 1,
                members: ids(&["A"]),
                version: None
            }]
        );
    }

    #[test]
    fn a_red_branch_halts_the_train_and_a_fix_joins_it() {
        let mut t = Trains::default();
        land(&mut t, "A", 0);
        assert_eq!(
            t.apply(&p(), Event::Red { why: "ci".into() }),
            [Action::Card {
                train: 1,
                kind: CardKind::Red("ci".into())
            }]
        );
        assert!(land(&mut t, "FIX", 1).is_empty());
        assert!(
            t.apply(&p(), Event::Tick { at: 3 * H }).is_empty(),
            "halted"
        );
        assert_eq!(
            t.apply(&p(), Event::Green { at: 3 * H }),
            [Action::Ask {
                train: 1,
                members: ids(&["A", "FIX"])
            }]
        );
    }

    #[test]
    fn not_this_time_carries_the_changes_to_the_next_train() {
        let mut t = Trains::default();
        land(&mut t, "A", 0);
        t.apply(
            &p(),
            Event::Now {
                at: 1,
                approved: false,
            },
        );
        assert_eq!(
            t.apply(
                &p(),
                Event::Answer {
                    train: 1,
                    release: false,
                    at: 10
                }
            ),
            [Action::Carry {
                from: 1,
                into: 2,
                members: ids(&["A"])
            }]
        );
        assert_eq!(t.closes_at(&p()), Some(10 + 2 * H));
        t.compact();
        assert_eq!(t.trains.len(), 1);
    }
}
