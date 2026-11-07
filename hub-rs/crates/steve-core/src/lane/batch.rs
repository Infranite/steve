//! The merge window of one lane (one target: upstream + branch), as a pure state machine: what happens
//! to a merge request is decided here from the lane's state, the time and the lane's policy alone. No
//! git, no store, no clock of its own: the caller feeds events and carries out the actions it gets
//! back (merging, releasing, recording), so the rules are exactly what the tests below say.
//!
//! The rules (the user, 2026-11-04):
//! - nothing in flight: a request goes straight through, as a unit of one (merge, then release);
//! - a request arriving while a unit is in flight opens the collecting batch, timed from that first
//!   request; later ones join it until it closes: `window` after its first request or `max_members`
//!   requests, whichever comes first (no early close: those are upper bounds, the aim is fewer
//!   releases); what arrives after it closed waits for the next batch;
//! - the unit in flight is not interrupted, but when it finishes merging while a batch is collecting,
//!   its release is put off and goes out with that batch;
//! - a closed batch starts once the unit before it has ended; one unit is in flight at a time.
//!
//! Changed by the user, 2026-11-06:
//! - a unit whose approval card is not answered yet is not under way: a request arriving then joins
//!   that unit (and its one card), up to `max_members`, instead of opening a batch of its own;
//! - "merge and release now" ([`Event::Now`]) closes the collecting batch at once: it starts as soon as
//!   nothing is in flight.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

/// A lane's limits. Both come from the lane's settings (falling back to the workspace's).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Policy {
    /// How long a batch collects, from its first request.
    pub window_ms: i64,
    /// How many requests a batch takes at most.
    pub max_members: usize,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            window_ms: 2 * 3_600_000,
            max_members: 10,
        }
    }
}

/// A batch (a straight-through request is a batch of one).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unit {
    pub id: u64,
    /// The requests it merges, in arrival order.
    pub members: Vec<String>,
    /// Requests merged by an earlier unit whose release was put off into this one.
    pub deferred: Vec<String>,
    /// When its first request arrived.
    pub opened_at: i64,
    /// It takes no more requests.
    pub closed: bool,
    /// Its merge is done (it only releases now).
    pub merged: bool,
    /// You approved its merge. Until then it is not under way: requests that arrive join it. A
    /// snapshot from before this field counts as approved (what it meant then).
    #[serde(default = "approved_before")]
    pub approved: bool,
}

fn approved_before() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A merge request for this lane (`id` unique within the lane).
    Arrive { id: String, at: i64 },
    /// The unit in flight finished merging.
    Merged { unit: u64, at: i64 },
    /// The unit in flight finished releasing (or failing to: its outcome is recorded elsewhere).
    Released { unit: u64, at: i64 },
    /// A member left the unit before it merged (ejected for a conflict or a failed check, withdrawn).
    Left { unit: u64, id: String, at: i64 },
    /// Time passed (a batch may close, a closed one may start).
    Tick { at: i64 },
    /// You approved the merge of the unit in flight: from now on it is under way.
    Approved { unit: u64 },
    /// You asked to merge and release now: the collecting batch closes (and starts when it may).
    Now { at: i64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Start merging `unit` (its members, in order).
    Merge { unit: u64, members: Vec<String> },
    /// Release `unit`: its own members and the ones whose release was put off into it.
    Release { unit: u64, members: Vec<String> },
    /// `members` merged in `from`; their release goes out with `into`.
    PutOff {
        from: u64,
        into: u64,
        members: Vec<String>,
    },
    /// `unit` closed: no more requests join it.
    Close { unit: u64 },
    /// `id` waits for the next batch (the collecting one is closed).
    Queue { id: String },
    /// `id` joined the collecting `unit`.
    Join { unit: u64, id: String },
    /// `id` joined `unit`, in flight but still waiting for your approval (its card now lists it).
    Add { unit: u64, id: String },
}

/// One lane's state.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lane {
    pub policy: Policy,
    /// The unit in flight (merging or releasing).
    pub running: Option<Unit>,
    /// The batch collecting requests (or closed and waiting for `running` to end).
    pub collecting: Option<Unit>,
    /// Requests waiting for the next batch, with when they arrived.
    pub queue: VecDeque<(String, i64)>,
    #[serde(default)]
    next: u64,
}

impl Lane {
    pub fn new(policy: Policy) -> Self {
        Self {
            policy,
            ..Default::default()
        }
    }

    fn unit(&mut self, first: String, at: i64) -> Unit {
        self.next += 1;
        Unit {
            id: self.next,
            members: vec![first],
            deferred: vec![],
            opened_at: at,
            closed: false,
            merged: false,
            approved: false,
        }
    }

    /// Every request the lane holds (in flight, collecting, queued).
    pub fn holds(&self, id: &str) -> bool {
        let has = |u: &Option<Unit>| {
            u.as_ref()
                .is_some_and(|u| u.members.iter().any(|m| m == id))
        };
        has(&self.running) || has(&self.collecting) || self.queue.iter().any(|(q, _)| q == id)
    }

    /// Apply `ev`; what the caller has to do, in order.
    pub fn apply(&mut self, ev: Event) -> Vec<Action> {
        let mut out = vec![];
        match ev {
            Event::Arrive { id, at } => {
                if self.holds(&id) {
                    return out;
                }
                // a window that ran out before this request is closed first: it doesn't take it
                self.close_if_due(at, &mut out);
                if self.running.is_none() && self.collecting.is_none() {
                    let u = self.unit(id, at);
                    out.push(Action::Merge {
                        unit: u.id,
                        members: u.members.clone(),
                    });
                    self.running = Some(u);
                } else if let Some(r) = self.running.as_mut().filter(|r| {
                    !r.merged && !r.approved && r.members.len() < self.policy.max_members
                }) {
                    // nothing is under way until you approve: it goes in with that unit
                    r.members.push(id.clone());
                    out.push(Action::Add { unit: r.id, id });
                } else if let Some(c) = self.collecting.as_mut().filter(|c| !c.closed) {
                    c.members.push(id.clone());
                    out.push(Action::Join { unit: c.id, id });
                } else if self.collecting.is_some() {
                    self.queue.push_back((id.clone(), at));
                    out.push(Action::Queue { id });
                } else {
                    let u = self.unit(id.clone(), at);
                    out.push(Action::Join { unit: u.id, id });
                    self.collecting = Some(u);
                }
                self.close_if_due(at, &mut out);
            }
            Event::Merged { unit, at } => {
                let Some(r) = self.running.as_mut().filter(|r| r.id == unit && !r.merged) else {
                    return out;
                };
                r.merged = true;
                match self.collecting.as_mut() {
                    // a batch is collecting: this release goes out with it
                    Some(c) => {
                        let r = self.running.take().expect("running");
                        let members: Vec<String> =
                            r.deferred.into_iter().chain(r.members).collect();
                        c.deferred.extend(members.iter().cloned());
                        out.push(Action::PutOff {
                            from: r.id,
                            into: c.id,
                            members,
                        });
                    }
                    None => {
                        let members = r.deferred.iter().chain(&r.members).cloned().collect();
                        out.push(Action::Release {
                            unit: r.id,
                            members,
                        });
                    }
                }
                self.close_if_due(at, &mut out);
            }
            Event::Released { unit, at } => {
                if self
                    .running
                    .as_ref()
                    .is_some_and(|r| r.id == unit && r.merged)
                {
                    self.running = None;
                }
                self.close_if_due(at, &mut out);
            }
            Event::Left { unit, id, at } => {
                for u in [self.running.as_mut(), self.collecting.as_mut()]
                    .into_iter()
                    .flatten()
                    .filter(|u| u.id == unit && !u.merged)
                {
                    u.members.retain(|m| *m != id);
                }
                // a unit in flight that lost every member has nothing to merge: its put-off releases
                // still go out
                if let Some(r) = self
                    .running
                    .as_ref()
                    .filter(|r| r.id == unit && r.members.is_empty())
                {
                    let r = r.clone();
                    self.running = None;
                    if !r.deferred.is_empty() {
                        match self.collecting.as_mut() {
                            Some(c) => {
                                c.deferred.extend(r.deferred.iter().cloned());
                                out.push(Action::PutOff {
                                    from: r.id,
                                    into: c.id,
                                    members: r.deferred,
                                });
                            }
                            None => {
                                out.push(Action::Release {
                                    unit: r.id,
                                    members: r.deferred.clone(),
                                });
                                // releasing: the lane is in flight until that release ends
                                self.running = Some(Unit { merged: true, ..r });
                            }
                        }
                    }
                }
                if self
                    .collecting
                    .as_ref()
                    .is_some_and(|c| c.members.is_empty() && c.deferred.is_empty())
                {
                    self.collecting = None;
                }
                self.close_if_due(at, &mut out);
            }
            Event::Tick { at } => self.close_if_due(at, &mut out),
            Event::Approved { unit } => {
                if let Some(r) = self.running.as_mut().filter(|r| r.id == unit) {
                    r.approved = true;
                }
            }
            Event::Now { at } => {
                if let Some(c) = self.collecting.as_mut().filter(|c| !c.closed) {
                    c.closed = true;
                    out.push(Action::Close { unit: c.id });
                }
                self.close_if_due(at, &mut out);
            }
        }
        out
    }

    /// Close the collecting batch when it is due; start it when it is closed and nothing is in flight;
    /// open the next one from the queue.
    fn close_if_due(&mut self, at: i64, out: &mut Vec<Action>) {
        loop {
            if let Some(c) = self.collecting.as_mut().filter(|c| !c.closed)
                && (c.members.len() >= self.policy.max_members
                    || at - c.opened_at >= self.policy.window_ms)
            {
                c.closed = true;
                out.push(Action::Close { unit: c.id });
            }
            let startable =
                self.running.is_none() && self.collecting.as_ref().is_some_and(|c| c.closed);
            if !startable {
                return;
            }
            let c = self.collecting.take().expect("collecting");
            if c.members.is_empty() {
                // only put-off releases left: they go out now
                out.push(Action::Release {
                    unit: c.id,
                    members: c.deferred.clone(),
                });
                self.running = Some(Unit { merged: true, ..c });
            } else {
                out.push(Action::Merge {
                    unit: c.id,
                    members: c.members.clone(),
                });
                self.running = Some(c);
            }
            // the queue becomes the next collecting batch, timed from its first request
            if let Some((first, t)) = self.queue.pop_front() {
                let mut u = self.unit(first.clone(), t);
                out.push(Action::Join {
                    unit: u.id,
                    id: first,
                });
                // the ones that came within that request's window, up to the limit
                while u.members.len() < self.policy.max_members
                    && self
                        .queue
                        .front()
                        .is_some_and(|(_, a)| a - t < self.policy.window_ms)
                {
                    let (id, _) = self.queue.pop_front().expect("front");
                    u.members.push(id.clone());
                    out.push(Action::Join { unit: u.id, id });
                }
                self.collecting = Some(u);
            }
            // loop: the new batch may already be due
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: i64 = 3_600_000;

    fn lane() -> Lane {
        Lane::new(Policy::default())
    }

    fn arrive(l: &mut Lane, id: &str, at: i64) -> Vec<Action> {
        l.apply(Event::Arrive { id: id.into(), at })
    }

    /// You answered the card of `unit`: it is under way.
    fn approve(l: &mut Lane, unit: u64) {
        assert!(l.apply(Event::Approved { unit }).is_empty());
    }

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn alone_a_request_goes_straight_through() {
        let mut l = lane();
        assert_eq!(
            arrive(&mut l, "A", 0),
            [Action::Merge {
                unit: 1,
                members: ids(&["A"])
            }]
        );
        assert_eq!(
            l.apply(Event::Merged { unit: 1, at: 10 }),
            [Action::Release {
                unit: 1,
                members: ids(&["A"])
            }]
        );
        assert!(l.apply(Event::Released { unit: 1, at: 20 }).is_empty());
        assert_eq!(l, Lane { next: 1, ..lane() });
    }

    #[test]
    fn a_request_during_a_merge_opens_a_batch_and_takes_the_release_with_it() {
        let mut l = lane();
        arrive(&mut l, "A", 0);
        approve(&mut l, 1);
        assert_eq!(
            arrive(&mut l, "B", 5),
            [Action::Join {
                unit: 2,
                id: "B".into()
            }]
        );
        assert_eq!(
            arrive(&mut l, "C", 6),
            [Action::Join {
                unit: 2,
                id: "C".into()
            }]
        );
        // A merges, its release waits for the batch
        assert_eq!(
            l.apply(Event::Merged { unit: 1, at: 10 }),
            [Action::PutOff {
                from: 1,
                into: 2,
                members: ids(&["A"])
            }]
        );
        assert!(l.running.is_none());
        // the window is timed from B: still collecting an hour later
        assert!(l.apply(Event::Tick { at: 5 + H }).is_empty());
        assert_eq!(
            l.apply(Event::Tick { at: 5 + 2 * H }),
            [
                Action::Close { unit: 2 },
                Action::Merge {
                    unit: 2,
                    members: ids(&["B", "C"])
                }
            ]
        );
        assert_eq!(
            l.apply(Event::Merged {
                unit: 2,
                at: 6 + 2 * H
            }),
            [Action::Release {
                unit: 2,
                members: ids(&["A", "B", "C"])
            }],
            "one release for A, B and C"
        );
    }

    #[test]
    fn a_full_batch_closes_and_what_comes_after_waits_for_the_next() {
        let mut l = Lane::new(Policy {
            window_ms: 2 * H,
            max_members: 3,
        });
        arrive(&mut l, "A", 0);
        approve(&mut l, 1);
        arrive(&mut l, "B", 1);
        arrive(&mut l, "C", 2);
        assert_eq!(
            arrive(&mut l, "D", 3),
            [
                Action::Join {
                    unit: 2,
                    id: "D".into()
                },
                Action::Close { unit: 2 }
            ]
        );
        assert_eq!(arrive(&mut l, "E", 4), [Action::Queue { id: "E".into() }]);
        // A merged: its release goes with batch 2, which starts now (closed, nothing in flight)
        assert_eq!(
            l.apply(Event::Merged { unit: 1, at: 10 }),
            [
                Action::PutOff {
                    from: 1,
                    into: 2,
                    members: ids(&["A"])
                },
                Action::Merge {
                    unit: 2,
                    members: ids(&["B", "C", "D"])
                },
                Action::Join {
                    unit: 3,
                    id: "E".into()
                },
            ]
        );
        // E's batch is timed from E's arrival
        assert_eq!(l.collecting.as_ref().unwrap().opened_at, 4);
    }

    #[test]
    fn a_closed_batch_waits_for_the_unit_in_flight_to_end() {
        let mut l = lane();
        arrive(&mut l, "A", 0);
        approve(&mut l, 1);
        arrive(&mut l, "B", 1);
        // closed by time while A still merges: it does not start
        assert_eq!(
            l.apply(Event::Tick { at: 1 + 2 * H }),
            [Action::Close { unit: 2 }]
        );
        assert!(l.running.as_ref().is_some_and(|r| r.id == 1));
        assert_eq!(
            l.apply(Event::Merged { unit: 1, at: 3 * H }),
            [
                Action::PutOff {
                    from: 1,
                    into: 2,
                    members: ids(&["A"])
                },
                Action::Merge {
                    unit: 2,
                    members: ids(&["B"])
                },
            ]
        );
    }

    #[test]
    fn a_release_in_flight_is_not_put_off() {
        let mut l = lane();
        arrive(&mut l, "A", 0);
        l.apply(Event::Merged { unit: 1, at: 10 });
        // A is releasing; B comes: A's release goes on, B collects
        assert_eq!(
            arrive(&mut l, "B", 11),
            [Action::Join {
                unit: 2,
                id: "B".into()
            }]
        );
        assert!(
            l.apply(Event::Released { unit: 1, at: 12 }).is_empty(),
            "B's window is still open"
        );
        assert_eq!(
            l.apply(Event::Tick { at: 11 + 2 * H }),
            [
                Action::Close { unit: 2 },
                Action::Merge {
                    unit: 2,
                    members: ids(&["B"])
                }
            ]
        );
    }

    #[test]
    fn a_member_that_leaves_is_not_merged_and_an_emptied_unit_still_releases_what_it_carried() {
        let mut l = lane();
        arrive(&mut l, "A", 0);
        approve(&mut l, 1);
        arrive(&mut l, "B", 1);
        l.apply(Event::Merged { unit: 1, at: 2 });
        l.apply(Event::Tick { at: 1 + 2 * H });
        // batch 2 merges B and carries A's release; B is ejected (conflict)
        assert_eq!(
            l.apply(Event::Left {
                unit: 2,
                id: "B".into(),
                at: 2 * H + 5
            }),
            [Action::Release {
                unit: 2,
                members: ids(&["A"])
            }]
        );
        // the lane stays in flight while that release runs
        assert!(l.running.as_ref().is_some_and(|r| r.id == 2 && r.merged));
        l.apply(Event::Released {
            unit: 2,
            at: 2 * H + 9,
        });
        assert!(l.running.is_none() && l.collecting.is_none());
    }

    #[test]
    fn a_request_after_the_window_ran_out_waits_for_the_next_batch() {
        let mut l = lane();
        arrive(&mut l, "A", 0);
        approve(&mut l, 1);
        arrive(&mut l, "B", 1);
        // no tick came; C arrives after B's window: B's batch closes, C waits
        assert_eq!(
            arrive(&mut l, "C", 1 + 2 * H),
            [Action::Close { unit: 2 }, Action::Queue { id: "C".into() }]
        );
        assert_eq!(l.collecting.as_ref().unwrap().members, ids(&["B"]));
    }

    #[test]
    fn a_queue_spanning_windows_makes_one_batch_per_window() {
        let mut l = Lane::new(Policy {
            window_ms: 2 * H,
            max_members: 2,
        });
        arrive(&mut l, "A", 0);
        approve(&mut l, 1);
        arrive(&mut l, "B", 1);
        arrive(&mut l, "C", 2); // closes B's batch (2 members)
        arrive(&mut l, "D", 3); // queued
        arrive(&mut l, "E", 3 * H); // queued, outside D's window
        l.apply(Event::Merged {
            unit: 1,
            at: 3 * H + 1,
        });
        // B and C start; D alone opens the next batch: E came after D's window
        assert_eq!(l.collecting.as_ref().unwrap().members, ids(&["D"]));
        assert_eq!(
            l.queue.iter().map(|(i, _)| i.as_str()).collect::<Vec<_>>(),
            ["E"]
        );
    }

    #[test]
    fn an_emptied_unit_releasing_what_it_carried_keeps_the_lane_until_that_release_ends() {
        let mut l = lane();
        arrive(&mut l, "A", 0);
        approve(&mut l, 1);
        arrive(&mut l, "B", 1);
        l.apply(Event::Merged { unit: 1, at: 2 });
        l.apply(Event::Tick { at: 1 + 2 * H });
        l.apply(Event::Left {
            unit: 2,
            id: "B".into(),
            at: 2 * H + 5,
        });
        // A's release goes out from unit 2; C waits for it instead of merging alongside
        assert_eq!(
            arrive(&mut l, "C", 2 * H + 6),
            [Action::Join {
                unit: 3,
                id: "C".into()
            }]
        );
        l.apply(Event::Released {
            unit: 2,
            at: 2 * H + 7,
        });
        assert!(l.running.is_none());
    }

    #[test]
    fn a_request_already_in_the_lane_is_not_taken_twice() {
        let mut l = lane();
        arrive(&mut l, "A", 0);
        assert!(arrive(&mut l, "A", 1).is_empty());
        arrive(&mut l, "B", 2);
        assert!(arrive(&mut l, "B", 3).is_empty());
    }

    #[test]
    fn a_request_while_the_card_is_unanswered_joins_that_unit() {
        let mut l = lane();
        arrive(&mut l, "A", 0);
        assert_eq!(
            arrive(&mut l, "B", 5),
            [Action::Add {
                unit: 1,
                id: "B".into()
            }],
            "no batch of its own, no window: it goes in with A"
        );
        assert!(l.collecting.is_none());
        approve(&mut l, 1);
        // under way now: the next one collects, as before
        assert_eq!(
            arrive(&mut l, "C", 6),
            [Action::Join {
                unit: 2,
                id: "C".into()
            }]
        );
        assert_eq!(
            l.apply(Event::Merged { unit: 1, at: 7 }),
            [Action::PutOff {
                from: 1,
                into: 2,
                members: ids(&["A", "B"])
            }]
        );
    }

    #[test]
    fn a_unit_waiting_for_its_card_takes_no_more_than_the_limit() {
        let mut l = Lane::new(Policy {
            window_ms: 2 * H,
            max_members: 2,
        });
        arrive(&mut l, "A", 0);
        arrive(&mut l, "B", 1);
        assert_eq!(
            arrive(&mut l, "C", 2),
            [Action::Join {
                unit: 2,
                id: "C".into()
            }]
        );
        assert_eq!(l.running.as_ref().unwrap().members, ids(&["A", "B"]));
    }

    #[test]
    fn now_closes_the_collecting_batch_and_starts_it_when_nothing_is_in_flight() {
        let mut l = lane();
        assert!(
            l.apply(Event::Now { at: 0 }).is_empty(),
            "nothing collecting"
        );
        arrive(&mut l, "A", 0);
        approve(&mut l, 1);
        arrive(&mut l, "B", 1);
        // A still merging: the batch closes now and waits for A, not for its window
        assert_eq!(l.apply(Event::Now { at: 2 }), [Action::Close { unit: 2 }]);
        assert_eq!(arrive(&mut l, "C", 3), [Action::Queue { id: "C".into() }]);
        assert_eq!(
            l.apply(Event::Merged { unit: 1, at: 4 }),
            [
                Action::PutOff {
                    from: 1,
                    into: 2,
                    members: ids(&["A"])
                },
                Action::Merge {
                    unit: 2,
                    members: ids(&["B"])
                },
                Action::Join {
                    unit: 3,
                    id: "C".into()
                },
            ]
        );
        // C's batch closes now too: it starts once B merged, carrying A's and B's release
        approve(&mut l, 2);
        assert_eq!(l.apply(Event::Now { at: 5 }), [Action::Close { unit: 3 }]);
        assert_eq!(
            l.apply(Event::Merged { unit: 2, at: 6 }),
            [
                Action::PutOff {
                    from: 2,
                    into: 3,
                    members: ids(&["A", "B"])
                },
                Action::Merge {
                    unit: 3,
                    members: ids(&["C"])
                },
            ]
        );
    }

    #[test]
    fn a_unit_kept_before_approval_was_recorded_counts_as_approved() {
        let mut l = lane();
        arrive(&mut l, "A", 0);
        let mut v = serde_json::to_value(&l).unwrap();
        v["running"].as_object_mut().unwrap().remove("approved");
        let mut old: Lane = serde_json::from_value(v).unwrap();
        assert_eq!(
            arrive(&mut old, "B", 1),
            [Action::Join {
                unit: 2,
                id: "B".into()
            }]
        );
    }
}
