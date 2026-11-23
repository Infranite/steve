//! The derived state in shadow (#327, crate::derive): the three acceptance cases every later step of
//! the redesign keeps — a merge is a condition of closing but not of review; a late fact of an old
//! turn never closes or moves the new one; a dispatch the Node took without the Hub getting its
//! receipt is reconciled under the same operation id, never sent again — and the completeness of
//! the next step over fixtures of every open state today's system reaches.

use serde_json::json;
use steve_store::ItemOperation;
use steve_store::core_fixture::runtime;
use steve_store::runtime::{NewAttempt, NewCommand};

use crate::cycle::{OP_BUILD, OP_RELEASE};
use crate::derive::{
    Card, Delivered, Derived, Dispatch, Due, Facts, ItemState, Judged, Lane, NextStep,
    OpenOperation, Owner, SessionState, Settled, VERIFIED, Wait, check_next_step, derive,
};
use crate::proof::OP_DEPLOY;
use crate::state_view::from_operations;
use crate::testkit::Kit;
use crate::work_policy::WorkPolicy;

pub(super) const NOW: i64 = 1_800_000_000_000;
const HEAD: &str = "c0ffee1";
const OLD_HEAD: &str = "bead001";

pub(super) fn policy() -> WorkPolicy {
    WorkPolicy::default()
}

pub(super) fn session(turn: &str) -> SessionState {
    SessionState {
        id: "s1".into(),
        turn: Some(turn.into()),
        last_activity: NOW - 1_000,
        watched: true,
        ..Default::default()
    }
}

/// An open code item of cycle 2 with an idle, settled session.
pub(super) fn base() -> Facts {
    Facts {
        now: NOW,
        item: ItemState {
            id: "i1".into(),
            cycle: 2,
            phase: "active".into(),
            paused: false,
            code: true,
            scope_revision: 0,
        },
        session: Some(session("t1")),
        continues_cap: 2,
        stall_after: crate::stall::STALL_AFTER_MS,
        ..Default::default()
    }
}

fn fact(kind: &str, head: Option<&str>, state: &str, provenance: &str) -> Delivered {
    Delivered {
        cycle: 2,
        kind: kind.into(),
        head: head.map(str::to_owned),
        ref_: None,
        state: state.into(),
        provenance: provenance.into(),
        observed_at: NOW - 10_000,
    }
}

pub(super) fn card(id: &str, question: &str) -> Card {
    Card {
        id: id.into(),
        stop: false,
        question: question.into(),
    }
}

/// A delivered change on `HEAD`: committed, the worktree all in it, its PR open and CI green.
fn delivered_change(f: &mut Facts) {
    f.required.merge = true;
    f.delivery.extend([
        fact("commit", Some(HEAD), "pushed", VERIFIED),
        fact("tree", Some(HEAD), "committed", VERIFIED),
        fact("pr", Some(HEAD), "open", VERIFIED),
        Delivered {
            ref_: Some("test".into()),
            ..fact("ci", Some(HEAD), "passed", VERIFIED)
        },
    ]);
}

#[test]
fn a_merge_is_a_condition_of_closing_not_of_review() {
    let mut f = base();
    delivered_change(&mut f);
    // Q4: a code PR always waits on your merge card
    f.cards.push(card("d-merge", "PR #12 等你合并"));
    let d = derive(&f, &policy());
    assert!(
        d.reviewable.holds,
        "review does not wait for the merge: {d:?}"
    );
    assert_eq!(d.reviewable.head.as_deref(), Some(HEAD));
    assert!(!d.mergeable.holds);
    assert_eq!(d.mergeable.missing, vec!["approval_of_head"]);
    assert!(!d.closable.predicate.holds);
    assert!(d.closable.predicate.missing.contains(&"merged"));
    assert_eq!((d.row, d.lane, d.owner), (3, Lane::NeedsYou, Owner::You));
    assert_eq!(
        d.next_step,
        Some(NextStep::Card {
            decision_id: "d-merge".into()
        })
    );

    // an approval of another head, or of no head at all, is not an approval of this one
    f.delivery
        .push(fact("approval", Some(OLD_HEAD), "approved", VERIFIED));
    f.delivery
        .push(fact("approval", None, "approved", VERIFIED));
    assert!(!derive(&f, &policy()).mergeable.holds);
    // nor is one the executor reported
    f.delivery
        .push(fact("approval", Some(HEAD), "approved", "reported"));
    assert!(!derive(&f, &policy()).mergeable.holds);
    let mut approved = fact("approval", Some(HEAD), "approved", VERIFIED);
    approved.observed_at = NOW - 5_000;
    f.delivery.push(approved);
    let d = derive(&f, &policy());
    assert!(d.mergeable.holds, "{:?}", d.mergeable);
    // mergeable is still not closable: the merge itself is the condition
    assert!(!d.closable.predicate.holds);

    // the merge as the executor reports it does not close
    f.cards.clear();
    let mut reported = fact("pr", Some(HEAD), "merged", "reported");
    reported.observed_at = NOW - 4_000;
    f.delivery.push(reported);
    let d = derive(&f, &policy());
    assert!(d.closable.predicate.missing.contains(&"merged"), "{d:?}");
    // the merge as the Hub read it off the host does, on hard facts alone
    let mut merged = fact("pr", Some(HEAD), "merged", VERIFIED);
    merged.observed_at = NOW - 3_000;
    f.delivery.push(merged);
    let d = derive(&f, &policy());
    assert!(d.closable.predicate.holds, "{:?}", d.closable);
    assert_eq!(d.closable.by, Some("hard_facts"));
}

#[test]
fn a_worktree_not_all_in_the_commit_is_not_reviewable() {
    let mut f = base();
    delivered_change(&mut f);
    f.delivery.retain(|d| d.kind != "tree");
    f.delivery
        .push(fact("tree", Some(HEAD), "uncommitted", VERIFIED));
    let d = derive(&f, &policy());
    assert_eq!(d.reviewable.missing, vec!["worktree_in_commit"]);
    // and an unknown effect holds review and close alike
    f.delivery.retain(|d| d.kind != "tree");
    f.delivery
        .push(fact("tree", Some(HEAD), "committed", VERIFIED));
    f.unknown_effects.push("op-release".into());
    let d = derive(&f, &policy());
    assert_eq!(d.reviewable.missing, vec!["unknown_effects"]);
    assert!(d.closable.predicate.missing.contains(&"unknown_effects"));
}

#[test]
fn a_late_fact_of_an_old_turn_never_closes_or_moves_the_new_turn() {
    let mut f = base();
    f.item.code = false;
    // the new turn t2 runs (you sent a message after t1 ended)
    f.session = Some(SessionState {
        running: true,
        ..session("t2")
    });
    let judged = |id: &str, cycle: i64, session: &str, turn: &str| Judged {
        id: id.into(),
        cycle,
        session_id: session.into(),
        turn_id: turn.into(),
        scope_revision: 0,
        verdict: "done".into(),
        created: NOW - 500,
    };
    // late answers: t1's judge, a judge of an earlier session, one of the earlier cycle
    f.judgements = vec![
        judged("j-old-turn", 2, "s1", "t1"),
        judged("j-old-session", 2, "s0", "t2"),
        judged("j-old-cycle", 1, "s1", "t2"),
    ];
    // and facts of the earlier cycle: its merged PR, its wait
    f.delivery.push(Delivered {
        cycle: 1,
        ..fact("pr", Some(OLD_HEAD), "merged", VERIFIED)
    });
    f.required.merge = true;
    f.waits.push(Wait {
        id: "w-old".into(),
        cycle: 1,
        on: "ci".into(),
        what: "CI of the old head".into(),
        next_check: NOW + 60_000,
        ..Default::default()
    });
    let d = derive(&f, &policy());
    assert_eq!(
        (d.row, d.code, d.lane),
        (5, "running", Lane::Running),
        "{d:?}"
    );
    assert_eq!(d.settled, Settled::Busy);
    assert!(!d.closable.predicate.holds);
    assert_eq!(
        d.next_step,
        Some(NextStep::Turn {
            session_id: "s1".into(),
            turn_id: Some("t2".into())
        })
    );

    // t2 ends: t1's judgement still speaks for t1, not for t2
    f.session.as_mut().unwrap().running = false;
    f.required.merge = false;
    let d = derive(&f, &policy());
    assert_ne!(d.row, 2, "{d:?}");
    assert_eq!(d.closable.by, None);
    assert_eq!(d.closable.predicate.missing, vec!["judged_done"]);
    assert_eq!(d.code, "follow_up");

    // only t2's own judgement counts, and even then the close waits out the undo window
    f.judgements.push(judged("j-t2", 2, "s1", "t2"));
    let d = derive(&f, &policy());
    assert_eq!((d.row, d.code), (2, "judged_done"));
    assert_eq!(d.next_at, Some(NOW - 500 + policy().close_grace()));
    // and a turn starting again takes it back at once
    f.session.as_mut().unwrap().running = true;
    assert_eq!(derive(&f, &policy()).row, 5);
}

#[test]
fn no_session_is_not_settled_but_unknown() {
    // an open code item whose session is gone, with a verified commit all in its tree and a merged PR
    let mut f = base();
    f.session = None;
    f.required.merge = true;
    f.delivery = vec![
        fact("commit", Some(HEAD), "committed", VERIFIED),
        fact("tree", Some(HEAD), "committed", VERIFIED),
        fact("pr", Some(HEAD), "merged", VERIFIED),
    ];
    let d = derive(&f, &policy());
    assert_eq!(d.settled, Settled::Unknown, "{d:?}");
    // nothing is reviewable, closable or judged on a turn nobody can see
    assert!(d.reviewable.missing.contains(&"not_settled"), "{d:?}");
    assert!(!d.closable.predicate.holds);
    assert!(d.closable.predicate.missing.contains(&"not_settled"));
    assert_eq!(d.closable.by, None);
    assert_eq!((d.row, d.code, d.lane), (9, "nobody", Lane::NeedsYou));
    // a judgement can never speak for it either
    f.item.code = false;
    f.required.merge = false;
    f.judgements.push(Judged {
        id: "j".into(),
        cycle: 2,
        session_id: "s1".into(),
        turn_id: "t1".into(),
        scope_revision: 0,
        verdict: "done".into(),
        created: NOW,
    });
    let d = derive(&f, &policy());
    assert_ne!(d.row, 2, "{d:?}");
    assert_eq!(d.closable.by, None);
    // the Hub still follows its own closing operations: no turn is in their way
    f.item.phase = "closing".into();
    f.closing_ops.push(OpenOperation {
        id: "op-build".into(),
        kind: OP_BUILD.into(),
        label: "build".into(),
    });
    let d = derive(&f, &policy());
    assert_eq!(
        (d.row, d.code, d.lane),
        (4, "closing", Lane::Running),
        "{d:?}"
    );
    assert!(check_next_step(&f, &d).is_ok());
}

#[test]
fn a_judgement_of_an_earlier_scope_never_speaks_for_the_current_one() {
    let mut f = base();
    f.item.code = false;
    f.item.scope_revision = 3;
    let judged = |id: &str, scope: i64| Judged {
        id: id.into(),
        cycle: 2,
        session_id: "s1".into(),
        turn_id: "t1".into(),
        scope_revision: scope,
        verdict: "done".into(),
        created: NOW - 500,
    };
    // the same cycle, session and turn, but judged against the goal before you changed it
    f.judgements.push(judged("j-old-scope", 2));
    let d = derive(&f, &policy());
    assert_ne!(d.row, 2, "{d:?}");
    assert_eq!(d.closable.by, None);
    assert_eq!(d.closable.predicate.missing, vec!["judged_done"]);
    // a newer scope's judgement does not count either: only the current one's
    f.judgements.push(judged("j-new-scope", 4));
    assert_ne!(derive(&f, &policy()).row, 2);
    // the judgement of the current scope does
    f.judgements.push(judged("j-this-scope", 3));
    let d = derive(&f, &policy());
    assert_eq!((d.row, d.code), (2, "judged_done"), "{d:?}");
    assert_eq!(
        d.next_step,
        Some(NextStep::Due {
            kind: Due::Grace,
            id: Some("j-this-scope".into()),
            at: Some(NOW - 500 + policy().close_grace()),
        })
    );
}

/// A release, build or deploy operation of `revision` in cycle 2, as today's gate records them.
pub(super) fn op(
    id: &str,
    revision: i64,
    kind: &str,
    status: &str,
    evidence: serde_json::Value,
) -> ItemOperation {
    ItemOperation {
        id: id.into(),
        item_id: "i1".into(),
        cycle: 2,
        revision,
        kind: kind.into(),
        effect_id: None,
        required: true,
        status: status.into(),
        label: kind.into(),
        evidence,
        created: NOW - 20_000,
        updated: NOW - 20_000,
    }
}

#[test]
fn a_fact_of_an_earlier_result_of_the_same_cycle_never_closes_or_holds_the_current_one() {
    // revision 1: Steve merged its PR while it went void, its build passed, it went live, and you
    // declined to merge it; revision 2 (the current result) has opened its release, no PR yet
    let ops = vec![
        op(
            "op-r1-release",
            1,
            OP_RELEASE,
            "pending",
            json!({"pr": 7, "pr_state": "MERGED", "pr_merged": true, "pr_merged_void": true}),
        ),
        op(
            "op-r1-declined",
            1,
            OP_RELEASE,
            "pending",
            json!({"pr": 6, "pr_state": "OPEN", "pr_merge_declined": true}),
        ),
        op("op-r1-build", 1, OP_BUILD, "succeeded", json!({})),
        op("op-r1-deploy", 1, OP_DEPLOY, "succeeded", json!({})),
        op(
            "op-r2-release",
            2,
            OP_RELEASE,
            "pending",
            json!({"pr_before": 7}),
        ),
    ];
    let of = from_operations(2, 2, true, &ops);
    assert!(of.delivery.is_empty(), "{:?}", of.delivery);
    assert!(!of.merge_declined, "the decline was for revision 1's PR");
    assert_eq!(
        of.closing_ops
            .iter()
            .map(|o| o.id.as_str())
            .collect::<Vec<_>>(),
        vec!["op-r2-release"]
    );
    let mut f = base();
    f.item.phase = "closing".into();
    f.required.merge = true;
    f.required.build = true;
    f.required.deploy = true;
    f.delivery = of.delivery;
    f.closing_ops = of.closing_ops;
    f.merge_declined = of.merge_declined;
    let d = derive(&f, &policy());
    assert!(!d.closable.predicate.holds, "{:?}", d.closable);
    assert_eq!(
        d.closable.predicate.missing,
        vec!["merged", "build_passed", "deployed"]
    );
    assert_eq!((d.row, d.code), (4, "closing"), "{d:?}");

    // the same operations read for revision 1 are its own facts (a void merge never counts)
    let of1 = from_operations(2, 1, true, &ops);
    assert!(of1.merge_declined);
    let kinds: Vec<_> = of1
        .delivery
        .iter()
        .map(|d| (d.kind.as_str(), d.state.as_str()))
        .collect();
    assert_eq!(
        kinds,
        vec![
            ("pr", "open"),
            ("approval", "declined"),
            ("build", "passed"),
            ("deploy", "live")
        ]
    );
    // an effect nobody knows holds the current result whatever result it was for
    let mut unknown = ops.clone();
    unknown.push(op("op-r1-unknown", 1, OP_DEPLOY, "unknown", json!({})));
    assert_eq!(
        from_operations(2, 2, true, &unknown).unknown_effects,
        vec!["op-r1-unknown"]
    );

    // facts tied to an earlier head of this cycle are not this result's: not with its commit known,
    // and not while none is
    let mut f = base();
    f.item.code = false;
    f.required.merge = true;
    f.required.build = true;
    f.delivery = vec![
        fact("pr", Some(OLD_HEAD), "merged", VERIFIED),
        fact("build", Some(OLD_HEAD), "passed", VERIFIED),
    ];
    let d = derive(&f, &policy());
    assert_eq!(d.closable.predicate.missing, vec!["merged", "build_passed"]);
    f.delivery
        .push(fact("commit", Some(HEAD), "pushed", VERIFIED));
    let d = derive(&f, &policy());
    assert_eq!(d.closable.predicate.missing, vec!["merged", "build_passed"]);
    assert_eq!(d.closable.predicate.head.as_deref(), Some(HEAD));
    // the same facts of this head close it
    f.delivery.extend([
        fact("pr", Some(HEAD), "merged", VERIFIED),
        fact("build", Some(HEAD), "passed", VERIFIED),
    ]);
    let d = derive(&f, &policy());
    assert!(d.closable.predicate.holds, "{:?}", d.closable);
    assert_eq!(d.closable.by, Some("hard_facts"));
}

#[test]
fn a_dispatch_the_node_took_without_a_receipt_is_reconciled_not_sent_again() {
    let k = Kit::new();
    let item = k.item("run", "执行", None);
    let sid = k.session(&item, "idle");
    let (node, turn, attempt, op) = ("node-a", "turn-1", "att-1", "op-start-1");
    let request = json!({"method": "start_turn", "body": {"session": sid}});
    let command = NewCommand {
        operation: op,
        node,
        method: "start_turn",
        request: &request,
        digest: "d1",
        target_attempt: None,
        dependency: None,
    };
    let store = k.db().store();
    store
        .write(|w| {
            runtime::pending_start(
                w,
                &sid,
                &NewAttempt {
                    id: attempt,
                    turn,
                    ordinal: 0,
                    node,
                    operation: op,
                    execution: &json!({}),
                    spec: &json!({}),
                    digest: "d1",
                },
                &command,
                NOW,
            )?;
            // Leased and sent: the Node took it. Its receipt never reached the Hub.
            runtime::mark_command_sent(w, op, NOW + 30_000)
        })
        .unwrap();

    let i = k.db().item(&item).unwrap().unwrap();
    let facts = k.core.tx().item_facts(&i, NOW).unwrap();
    let d = derive(&facts, &policy());
    assert_eq!(d.settled, Settled::Unknown, "{d:?}");
    assert_eq!((d.row, d.code, d.owner), (5, "reconciling", Owner::Hub));
    assert_eq!(
        d.next_step,
        Some(NextStep::Due {
            kind: Due::Reconcile,
            id: Some(op.into()),
            at: Some(NOW + 30_000)
        }),
        "the next step is the same operation, never a new dispatch"
    );
    // nothing about it is evaluable: not judged, not closed
    assert!(!d.closable.predicate.holds);
    assert!(d.closable.predicate.missing.contains(&"not_settled"));

    // sending it again is the same operation: one row, nothing new for the Node to run twice
    let again = store
        .write(|w| runtime::put_node_command(w, &command, NOW + 1))
        .unwrap();
    assert!(!again);
    let other = store.write(|w| {
        runtime::put_node_command(
            w,
            &NewCommand {
                request: &json!({"method": "start_turn", "body": {"session": "another"}}),
                digest: "d2",
                ..command
            },
            NOW + 1,
        )
    });
    assert!(
        other.is_err(),
        "an operation id is never reused for other work"
    );
    let rows = runtime::node_command_count(store);
    assert_eq!(rows, 1);

    // the receipt comes: the same operation settles, and only then is the turn known to run
    runtime::accept_node_command(store, op);
    let facts = k.core.tx().item_facts(&i, NOW).unwrap();
    let d = derive(&facts, &policy());
    assert_eq!((d.settled, d.code), (Settled::Busy, "running"));
    assert_eq!(
        d.next_step,
        Some(NextStep::Turn {
            session_id: sid.clone(),
            turn_id: Some(turn.into())
        })
    );
}

#[test]
fn the_view_shows_the_derived_state() {
    let k = Kit::new();
    let item = k.item("run", "执行", None);
    k.session(&item, "idle");
    k.decision(&item, None, "ask", "question", &["a", "b"]);
    let v = k.core.derived_state(&item).unwrap().unwrap();
    assert_eq!(v["lane"], json!("needs_you"));
    assert_eq!(v["code"], json!("card"));
    assert!(v["label"].as_str().is_some_and(|l| !l.is_empty()));
    // the debug view carries it too
    let snap = k.core.diagnostic_snapshot(&item).unwrap();
    assert_eq!(snap["derived_state"]["code"], json!("card"));
    // the operator's overrides apply by name; an unknown one is reported, not taken
    let facts = k.db().store().item_facts();
    facts.set_policy("close_grace", 1_000).unwrap();
    facts.set_policy("no_such_entry", 1).unwrap();
    let v = k.core.derived_state(&item).unwrap().unwrap();
    assert_eq!(v["policy_ignored"], json!([["no_such_entry", 1]]));
    // the scope a judgement is matched against is the version of the item's goal
    let i = k.db().item(&item).unwrap().unwrap();
    let before = k.core.tx().item_facts(&i, NOW).unwrap().item.scope_revision;
    let version = k.db().add_goal(&item, "换个目标").unwrap();
    let after = k.core.tx().item_facts(&i, NOW).unwrap().item.scope_revision;
    assert_eq!(after, version);
    assert!(after > before, "{before} -> {after}");
}

#[test]
fn the_fact_records_keep_their_cycle_and_refuse_unknown_kinds() {
    let k = Kit::new();
    let item = k.item("run", "执行", None);
    let facts = k.db().store().item_facts();
    use steve_store::item_facts::{NewDeliveryFact, NewJudgement, NewWait};
    facts
        .record_delivery(&NewDeliveryFact {
            item_id: &item,
            cycle: 1,
            kind: "commit",
            head: Some(HEAD),
            state: "pushed",
            provenance: VERIFIED,
            source: "test",
            observed_at: NOW,
            ..Default::default()
        })
        .unwrap();
    assert!(
        facts
            .record_delivery(&NewDeliveryFact {
                item_id: &item,
                cycle: 1,
                kind: "commit",
                state: "pushed",
                provenance: "trust_me",
                source: "test",
                ..Default::default()
            })
            .is_err()
    );
    assert_eq!(facts.deliveries(&item, 1).unwrap().len(), 1);
    assert!(facts.deliveries(&item, 2).unwrap().is_empty());
    let w = facts
        .add_wait(&NewWait {
            item_id: &item,
            cycle: 1,
            waits_on: "ci",
            what: "CI",
            next_check: NOW,
            cap: Some(3),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(facts.waits_open(&item, 1).unwrap().len(), 1);
    assert!(facts.resolve_wait(&w, "passed").unwrap());
    assert!(!facts.resolve_wait(&w, "passed").unwrap());
    assert!(facts.waits_open(&item, 1).unwrap().is_empty());
    let j = NewJudgement {
        item_id: &item,
        cycle: 1,
        session_id: "s1",
        turn_id: "t1",
        verdict: "done",
        ..Default::default()
    };
    assert!(facts.record_judgement(&j).unwrap().is_some());
    // one judgement per turn and scope: a second one changes nothing
    assert!(facts.record_judgement(&j).unwrap().is_none());
    assert_eq!(facts.judgements(&item, 1).unwrap().len(), 1);
}

/// An open item today's system can hold, as facts: each is the state some code path leaves it in.
pub(super) fn fixtures() -> Vec<(&'static str, Facts)> {
    let mut out = vec![];
    let mut add = |name, edit: &dyn Fn(&mut Facts)| {
        let mut f = base();
        edit(&mut f);
        out.push((name, f));
    };
    add("turn running", &|f| {
        f.session.as_mut().unwrap().running = true
    });
    add("input queued", &|f| f.session.as_mut().unwrap().queued = 1);
    add("input queued, Node away", &|f| {
        let s = f.session.as_mut().unwrap();
        s.queued = 1;
        s.offline_since = Some(NOW - 60_000);
    });
    add("turn sent, not taken yet", &|f| {
        f.session.as_mut().unwrap().dispatches.push(Dispatch {
            operation_id: "op".into(),
            state: "pending".into(),
            retry_at: NOW + 5_000,
            ..Default::default()
        })
    });
    add("turn taken, receipt lost", &|f| {
        f.session.as_mut().unwrap().dispatches.push(Dispatch {
            operation_id: "op".into(),
            state: "pending".into(),
            maybe_sent: true,
            retry_at: NOW + 5_000,
            ..Default::default()
        })
    });
    add("runtime unsure", &|f| {
        f.session.as_mut().unwrap().uncertain = true
    });
    add("a question for you", &|f| {
        f.cards.push(card("d1", "要用哪个方案？"))
    });
    add("a stop card", &|f| {
        f.cards.push(Card {
            stop: true,
            ..card("d2", "它停下了")
        })
    });
    add("PR awaiting your merge", &|f| {
        delivered_change(f);
        f.item.phase = "closing".into();
        f.cards.push(card("d3", "PR #12 等你合并"));
    });
    add("closing, release followed", &|f| {
        f.item.phase = "closing".into();
        f.closing_ops.push(OpenOperation {
            id: "op-rel".into(),
            kind: "release".into(),
            label: "PR #12".into(),
        });
    });
    add("prerequisites", &|f| f.dependency_wait = true);
    add("quota", &|f| f.quota_until = Some(NOW + 3_600_000));
    add("a registered wait", &|f| {
        f.waits.push(Wait {
            id: "w1".into(),
            cycle: 2,
            on: "external".into(),
            what: "对方回复".into(),
            next_check: NOW + 120_000,
            cap: None,
            ..Default::default()
        })
    });
    add("paused by you", &|f| f.item.paused = true);
    add("your Stop", &|f| {
        f.session.as_mut().unwrap().held_by_you = true
    });
    add("settled, Steve follows up", &|_| {});
    add("work graph at work", &|f| {
        f.session.as_mut().unwrap().watched = false;
        f.graph_busy = Some("review".into());
    });
    add("not started", &|f| {
        f.item.phase = "ready".into();
        f.session = None;
    });
    add("judged done, undo window", &|f| {
        f.item.code = false;
        f.judgements.push(Judged {
            id: "j".into(),
            cycle: 2,
            session_id: "s1".into(),
            turn_id: "t1".into(),
            scope_revision: 0,
            verdict: "done".into(),
            created: NOW,
        });
    });
    // known gaps: open states with nothing that moves them on today (each closed by a later step)
    add("you declined the merge", &|f| f.merge_declined = true);
    add("Node away past the policy", &|f| {
        f.session.as_mut().unwrap().offline_since = Some(NOW - 2 * policy().nobody_offline())
    });
    add("active, no session", &|f| f.session = None);
    add("settled, follow-ups used up", &|f| f.continues_spent = 2);
    add("settled, not watched", &|f| {
        f.session.as_mut().unwrap().watched = false
    });
    out
}

/// Open states that have no next step today, and the step of the redesign that gives them one. The
/// list may only shrink: a fixture that gains a next step must be taken off, one that loses it fails.
pub(super) const KNOWN_GAPS: &[(&str, &str)] = &[
    (
        "you declined the merge",
        "PLAN 2.4, step 3: a declined merge raises rework / close / keep actions",
    ),
    (
        "Node away past the policy",
        "PLAN 2.4, step 2: the patrol raises a card for an item nobody can bring back",
    ),
    (
        "active, no session",
        "PLAN 2.4, step 2: the patrol raises a card for an item nobody can bring back",
    ),
    (
        "settled, follow-ups used up",
        "today the takeover opens a card in the same pass; step 2 persists it as one action",
    ),
    (
        "settled, not watched",
        "PLAN 2.5, step 2: the patrol covers every open item, not only `run` ones",
    ),
];

#[test]
fn every_open_item_in_the_fixtures_has_a_next_step() {
    let mut gaps = vec![];
    for (name, f) in fixtures() {
        let d: Derived = derive(&f, &policy());
        assert_ne!(d.lane, Lane::Done, "{name} is open");
        if let Err(why) = check_next_step(&f, &d) {
            gaps.push((name, why));
        }
    }
    let known: Vec<_> = KNOWN_GAPS.iter().map(|(n, _)| *n).collect();
    let found: Vec<_> = gaps.iter().map(|(n, _)| *n).collect();
    assert_eq!(
        found, known,
        "open items without a next step (only KNOWN_GAPS may lack one): {gaps:?}"
    );
}

#[test]
fn the_completeness_check_is_not_vacuous() {
    // a fallback row, a card that is not open, a due action with no time, a turn that is not running
    let f = base();
    let mut d = derive(&f, &policy());
    d.next_step = None;
    assert!(check_next_step(&f, &d).is_err());
    d.next_step = Some(NextStep::Card {
        decision_id: "gone".into(),
    });
    assert!(check_next_step(&f, &d).is_err());
    d.next_step = Some(NextStep::Due {
        kind: Due::Wait,
        id: None,
        at: None,
    });
    assert!(check_next_step(&f, &d).is_err());
    d.next_step = Some(NextStep::Due {
        kind: Due::Dependency,
        id: None,
        at: None,
    });
    assert!(check_next_step(&f, &d).is_err());
    d.next_step = Some(NextStep::Turn {
        session_id: "s1".into(),
        turn_id: None,
    });
    assert!(check_next_step(&f, &d).is_err());
    // a closed item needs none
    let mut closed = base();
    closed.item.phase = "closed".into();
    let d = derive(&closed, &policy());
    assert_eq!((d.row, d.lane), (1, Lane::Done));
    assert!(check_next_step(&closed, &d).is_ok());
}
