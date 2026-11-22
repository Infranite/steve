//! The sweep (#334, crate::sweep), observe-only: every silent stuck state today's system leaves an
//! item in becomes a card it would raise; each gap of the derived state gets something it would do;
//! an open card means it adds nothing; and in the store a decision is recorded once per key, cleared
//! when it stops holding, and nothing is ever carried out.

use serde_json::{Value, json};
use steve_store::ItemOperation;

use super::derive::{KNOWN_GAPS, NOW, base, card, fixtures, op, policy};
use crate::ItemAction;
use crate::cycle::{OP_BUILD, OP_HOOKS, OP_RELEASE};
use crate::derive::{Facts, derive};
use crate::proof::OP_DEPLOY;
use crate::repo::Decision;
use crate::shadow::from_operations as shadow_ops;
use crate::sweep::{
    Action, Clock, EXECUTE, RULES, Signals, WouldDo, from_operations, label, pause_and_answer, plan,
};
use crate::testkit::Kit;

const MIN: i64 = 60_000;
const HOUR: i64 = 60 * MIN;
const REVISION: i64 = 1;

/// A card of the item, resolved at `at` with `result`.
fn resolved(id: &str, kind: &str, at: i64, result: Value) -> Decision {
    Decision {
        id: id.into(),
        workspace_id: "w".into(),
        kind: kind.into(),
        subject_type: "item".into(),
        subject_id: "i1".into(),
        item_id: Some("i1".into()),
        session_id: Some("s1".into()),
        question: "q".into(),
        draft: String::new(),
        options: vec![],
        status: "resolved".into(),
        result,
        requested_by: "steve".into(),
        created: at - MIN,
        resolved: Some(at),
        undo: Value::Null,
    }
}

/// One stuck state: the item's facts, the operations of its cycle, its cards, and when the item
/// action last paused it.
struct Case {
    facts: Facts,
    ops: Vec<ItemOperation>,
    decisions: Vec<Decision>,
    paused_by_action: Option<i64>,
}

impl Case {
    fn new(edit: impl Fn(&mut Facts)) -> Self {
        let mut facts = base();
        edit(&mut facts);
        Case {
            facts,
            ops: vec![],
            decisions: vec![],
            paused_by_action: None,
        }
    }
    fn ops(mut self, ops: Vec<ItemOperation>) -> Self {
        self.ops = ops;
        self
    }
    fn decisions(mut self, d: Vec<Decision>, paused_by_action: Option<i64>) -> Self {
        self.decisions = d;
        self.paused_by_action = paused_by_action;
        self
    }

    /// What the sweep would do, gathered the way `Tx::sweep_signals` gathers it.
    fn plan(mut self) -> Option<WouldDo> {
        let closing = self.facts.item.phase == "closing";
        let o = shadow_ops(self.facts.item.cycle, REVISION, closing, &self.ops);
        self.facts.delivery.extend(o.delivery);
        self.facts.unknown_effects = o.unknown_effects;
        self.facts.closing_ops = o.closing_ops;
        self.facts.merge_declined = o.merge_declined;
        let mut s: Signals = from_operations(REVISION, &self.ops);
        if self.facts.item.paused {
            (s.paused_at, s.answered) = pause_and_answer(self.paused_by_action, &self.decisions);
        }
        s.last_record = s
            .last_record
            .max(self.facts.session.as_ref().map_or(0, |x| x.last_activity));
        let d = derive(&self.facts, &policy());
        plan(&self.facts, &d, &s, &policy())
    }
}

fn closing(f: &mut Facts) {
    f.item.phase = "closing".into();
    f.session.as_mut().unwrap().watched = false;
}

fn quiet_for(f: &mut Facts, ms: i64) {
    f.session.as_mut().unwrap().last_activity = NOW - ms;
}

fn at(mut o: ItemOperation, updated: i64) -> ItemOperation {
    o.updated = updated;
    o
}

/// The release of the current result, pending, with `evidence`.
fn release(evidence: Value) -> ItemOperation {
    op("op-rel", REVISION, OP_RELEASE, "pending", evidence)
}

#[test]
fn every_silent_stuck_state_is_a_card_the_sweep_would_raise() {
    let cases: Vec<(&str, &str, Case)> = vec![
        (
            "closing, a pull request problem the agent was told of, same head after its turn",
            "pr_unfixed",
            Case::new(|f| {
                closing(f);
                quiet_for(f, 10 * MIN);
            })
            .ops(vec![release(json!({
                "pr": 12, "pr_state": "OPEN", "pr_problem": "conflict",
                "pr_told": ["conflict:c0ffee1"], "pr_tells": 1
            }))]),
        ),
        (
            "you declined the merge",
            "merge_declined",
            Case::new(closing).ops(vec![release(json!({
                "pr": 12, "pr_state": "OPEN", "pr_merge_declined": true, "pr_merge_by": "you",
                "pr_told": ["awaiting"]
            }))]),
        ),
        (
            "a deploy requirement waits for this Hub to run the result",
            "deploy_waiting",
            Case::new(closing).ops(vec![
                op(
                    "op-rel",
                    REVISION,
                    OP_RELEASE,
                    "succeeded",
                    json!({"tag": "v0.3.1"}),
                ),
                op(
                    "op-dep",
                    REVISION,
                    OP_DEPLOY,
                    "pending",
                    json!({"tag": "v0.3.1"}),
                ),
            ]),
        ),
        (
            "a required build failed and the agent's turn after it is over",
            "required_failed",
            Case::new(|f| {
                closing(f);
                quiet_for(f, 10 * MIN);
            })
            .ops(vec![
                op(
                    "op-rel",
                    REVISION,
                    OP_RELEASE,
                    "succeeded",
                    json!({"tag": "v0.3.1"}),
                ),
                at(
                    op("op-build", REVISION, OP_BUILD, "failed", json!({})),
                    NOW - 10 * MIN,
                ),
            ]),
        ),
        (
            "the wrap-up module took the close over and went silent",
            "hooks_silent",
            Case::new(closing).ops(vec![at(
                op(
                    "op-hooks",
                    REVISION,
                    OP_HOOKS,
                    "running",
                    json!({"note": "由收尾模块接手"}),
                ),
                NOW - 3 * HOUR,
            )]),
        ),
        (
            "the Node of the session is gone while closing",
            "node_gone_closing",
            Case::new(|f| {
                closing(f);
                f.session.as_mut().unwrap().offline_since = Some(NOW - HOUR);
            })
            .ops(vec![release(json!({"pr": 12, "pr_state": "OPEN"}))]),
        ),
        (
            "paused when you dismissed a card, and nothing since",
            "paused_silent",
            Case::new(|f| {
                f.item.paused = true;
                quiet_for(f, 3 * HOUR);
            })
            .decisions(
                vec![resolved(
                    "d-stall",
                    "fail",
                    NOW - 3 * HOUR,
                    json!({"dismissed": true}),
                )],
                None,
            ),
        ),
        (
            "a card you answered on a paused item",
            "answered_while_paused",
            Case::new(|f| {
                f.item.paused = true;
                quiet_for(f, 20 * MIN);
            })
            .decisions(
                vec![resolved(
                    "d-ask",
                    "ask",
                    NOW - 30 * MIN,
                    json!({"choice": "a", "text": ""}),
                )],
                Some(NOW - HOUR),
            ),
        ),
    ];
    for (name, reason, case) in cases {
        let w = case.plan().unwrap_or_else(|| panic!("{name}: nothing"));
        assert_eq!(
            (w.action, w.reason),
            (Action::Card, reason),
            "{name}: {w:?}"
        );
        assert!(w.due(NOW), "{name}: due {} > now", w.due_at);
    }
}

#[test]
fn a_decision_names_the_thing_it_is_about_and_the_scope_it_was_made_on() {
    let failed = |id: &str| {
        Case::new(|f| {
            closing(f);
            quiet_for(f, 10 * MIN);
            f.item.scope_revision = 3;
        })
        .ops(vec![at(
            op(id, REVISION, OP_BUILD, "failed", json!({})),
            NOW - 20 * MIN,
        )])
        .plan()
        .unwrap()
    };
    // the same reason on the same turn, about two different builds: two different decisions
    let (a, b) = (failed("op-build"), failed("op-build-2"));
    assert_eq!((a.reason, b.reason), ("required_failed", "required_failed"));
    assert_eq!(
        (a.session_id.as_str(), a.turn_id.as_str()),
        (b.session_id.as_str(), b.turn_id.as_str())
    );
    assert_eq!(
        (a.target.as_str(), b.target.as_str()),
        ("op-build", "op-build-2")
    );
    assert_eq!(a.scope_revision, 3);
    // a wait looked at often names that wait
    let w = Case::new(|f| {
        f.session.as_mut().unwrap().watched = false;
        f.waits.push(crate::derive::Wait {
            id: "wait-ci".into(),
            cycle: 2,
            on: "ci".into(),
            what: "CI".into(),
            next_check: NOW + MIN,
            wakes: policy().wait_note_after(),
            cap: None,
        });
    })
    .plan()
    .unwrap();
    assert_eq!(
        (w.reason, w.target.as_str()),
        ("long_wait", "wait-ci"),
        "{w:?}"
    );
    // a decision about the item as a whole names nothing
    let w = Case::new(|_| {}).plan().unwrap();
    assert_eq!((w.reason, w.target.as_str()), ("follow_up", ""));
}

#[test]
fn the_cards_wait_for_what_would_still_move_the_item_on() {
    // told of nothing yet: the pull request watch tells the agent first
    let w = Case::new(|f| {
        closing(f);
        quiet_for(f, 10 * MIN);
    })
    .ops(vec![release(
        json!({"pr": 12, "pr_problem": "conflict", "pr_told": []}),
    )])
    .plan();
    assert!(w.as_ref().is_none_or(|w| w.reason != "pr_unfixed"), "{w:?}");
    // the agent is at it (a new head was pushed, or it is fixing): no card, at most a question
    let w = Case::new(|f| {
        closing(f);
        f.session.as_mut().unwrap().running = true;
    })
    .ops(vec![release(
        json!({"pr": 12, "pr_problem": "conflict", "pr_told": ["conflict:c0ffee1"]}),
    )])
    .plan()
    .unwrap();
    assert_eq!((w.action, w.reason), (Action::Wake, "stall_question"));
    assert!(!w.due(NOW), "a turn with fresh output is not asked");
    // the Node is away, but not past the policy: only once it is
    let w = Case::new(|f| {
        closing(f);
        f.session.as_mut().unwrap().offline_since = Some(NOW - MIN);
    })
    .plan()
    .unwrap();
    assert_eq!(w.reason, "node_gone_closing");
    assert_eq!(w.due_at, NOW - MIN + policy().nobody_offline());
    // a turn your answer started is not stuck
    let w = Case::new(|f| {
        f.item.paused = true;
        f.session.as_mut().unwrap().running = true;
    })
    .decisions(
        vec![resolved("d-ask", "ask", NOW - MIN, json!({"choice": "a"}))],
        Some(NOW - HOUR),
    )
    .plan()
    .unwrap();
    assert_eq!(w.reason, "paused_silent");
    // an answer from before the pause is not an answer to the paused item
    let w = Case::new(|f| f.item.paused = true)
        .decisions(
            vec![resolved(
                "d-ask",
                "ask",
                NOW - 2 * HOUR,
                json!({"choice": "a"}),
            )],
            Some(NOW - HOUR),
        )
        .plan()
        .unwrap();
    assert_eq!(w.reason, "paused_silent");
    // an earlier result's declined merge or failed build is not the current result's
    let w = Case::new(|f| {
        closing(f);
        quiet_for(f, 10 * MIN);
    })
    .ops(vec![
        op(
            "old-rel",
            0,
            OP_RELEASE,
            "pending",
            json!({"pr": 11, "pr_merge_declined": true}),
        ),
        op("old-build", 0, OP_BUILD, "failed", json!({})),
    ])
    .plan();
    assert!(
        w.as_ref()
            .is_none_or(|w| !matches!(w.reason, "merge_declined" | "required_failed")),
        "{w:?}"
    );
}

#[test]
fn an_open_card_means_the_sweep_adds_nothing() {
    for (name, mut f) in fixtures() {
        f.cards.push(card("d-open", "要用哪个方案？"));
        let d = derive(&f, &policy());
        let s = Signals {
            last_record: NOW - 10 * HOUR,
            ..Default::default()
        };
        assert_eq!(plan(&f, &d, &s, &policy()), None, "{name}");
    }
}

#[test]
fn each_gap_of_the_derived_state_gets_something_the_sweep_would_do() {
    let gaps: Vec<_> = KNOWN_GAPS.iter().map(|(n, _)| *n).collect();
    let mut covered = vec![];
    for (name, f) in fixtures() {
        if !gaps.contains(&name) {
            continue;
        }
        let d = derive(&f, &policy());
        let s = Signals {
            last_record: f.session.as_ref().map_or(0, |x| x.last_activity),
            ..Default::default()
        };
        let w = plan(&f, &d, &s, &policy()).unwrap_or_else(|| panic!("{name}: nothing"));
        assert_eq!(w.action, Action::Card, "{name}: {w:?}");
        covered.push(name);
    }
    assert_eq!(covered, gaps);
}

#[test]
fn every_execution_switch_is_off_and_every_rule_is_named_and_said() {
    // observe-only: nothing the sweep decides is carried out in this release
    for a in [Action::Wake, Action::Card, Action::Remind] {
        assert!(
            !EXECUTE.on(a),
            "{a:?} must stay off until its executor exists"
        );
    }
    let mut reasons: Vec<_> = RULES.iter().map(|r| r.reason).collect();
    reasons.sort_unstable();
    reasons.dedup();
    assert_eq!(reasons.len(), RULES.len(), "reasons are unique");
    let p = policy();
    for r in RULES {
        if let Clock::Policy(entry) = r.clock {
            assert!(
                p.entries().iter().any(|(n, _)| *n == entry),
                "{}: {entry} is a work policy entry",
                r.reason
            );
        }
        for locale in [steve_i18n::ZH_CN, steve_i18n::EN] {
            let text = steve_i18n::with(locale, || label(r.reason));
            assert!(
                !text.is_empty() && text != r.reason && !text.starts_with("core.sweep."),
                "{} has no {locale} text",
                r.reason
            );
        }
    }
}

/* ── in the store ── */

/// A kit where Steve follows up a settled item, as by default (the kit turns it off).
fn following() -> Kit {
    Kit::with(|c| c.takeover_continues = 2)
}

fn log(k: &Kit, item: &str) -> Vec<steve_store::would_do::WouldDoRow> {
    k.db().store().would_do().log(item, 50).unwrap()
}

#[test]
fn a_decision_is_recorded_once_per_key_cleared_when_it_stops_and_never_carried_out() {
    let k = following();
    let item = k.item("run", "执行", None);
    let sid = k.session(&item, "idle");
    let p = policy();
    let later = crate::util::now() + HOUR;
    let before = k.timeline_texts(&item);
    let sweep = |at: i64| k.core.tx().sweep_item(&item, at, &p).unwrap();

    // settled with nothing asked: Steve's follow-up is what it would do (today's takeover does it)
    let w = sweep(later).unwrap();
    assert_eq!((w.action, w.reason), (Action::Wake, "follow_up"));
    assert_eq!(w.session_id, sid);
    sweep(later + 30_000);
    let rows = log(&k, &item);
    assert_eq!(
        rows.len(),
        1,
        "the same decision is the same record: {rows:?}"
    );
    assert_eq!((rows[0].episodes, rows[0].executed), (1, false));
    assert!(rows[0].cleared_at.is_none());

    // nothing was carried out: no prompt, no card, no timeline line, the item as it was
    assert!(k.rt.prompts_of(&sid).is_empty());
    assert!(k.db().decisions_of_item(&item).unwrap().is_empty());
    assert_eq!(k.timeline_texts(&item), before);
    assert_eq!(k.db().item(&item).unwrap().unwrap().status, "run");

    // a card asks you: the sweep has nothing to add, and its decision is cleared
    let d = k.decision(&item, Some(&sid), "ask", "question", &["a", "b"]);
    assert!(sweep(later + 60_000).is_none());
    assert!(log(&k, &item)[0].cleared_at.is_some());
    // the card goes: the same decision comes back as a second episode of the same record
    k.db()
        .decision_resolve(&d, json!({"superseded": "x"}), "expired", None)
        .unwrap();
    sweep(later + 90_000).unwrap();
    let rows = log(&k, &item);
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].episodes, rows[0].cleared_at), (2, None));

    // the item closes: the sweep no longer looks at it and its open decisions are cleared
    steve_store::core_fixture::item(
        k.db().store(),
        &item,
        steve_store::core_fixture::ItemState::Phase("closed"),
    );
    k.db()
        .store()
        .would_do()
        .clear_gone(later + 120_000)
        .unwrap();
    assert!(log(&k, &item)[0].cleared_at.is_some());
}

#[test]
fn the_same_reason_about_another_target_or_scope_is_another_decision() {
    let k = following();
    let item = k.item("run", "执行", None);
    let sid = k.session(&item, "idle");
    let p = policy();
    let later = crate::util::now() + HOUR;
    let sweep = |at: i64| k.core.tx().sweep_item(&item, at, &p).unwrap().unwrap();
    let first = sweep(later);
    assert_eq!(first.reason, "follow_up");
    // the goal changes on the same turn: the decision made on the old goal is not this one
    let version = k.db().add_goal(&item, "换个目标").unwrap();
    let second = sweep(later + 30_000);
    assert_eq!(
        (
            second.reason,
            second.turn_id.as_str(),
            second.scope_revision
        ),
        ("follow_up", first.turn_id.as_str(), version)
    );
    let rows = log(&k, &item);
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert!(
        rows.iter()
            .any(|r| r.scope_revision == first.scope_revision && r.cleared_at.is_some())
    );
    assert!(
        rows.iter()
            .any(|r| r.scope_revision == version && r.cleared_at.is_none())
    );
    // seen again unchanged: still the same record
    sweep(later + 60_000);
    assert_eq!(log(&k, &item).len(), 2);

    // in the store: the same turn and reason about another operation is a new record, and the old
    // one's coming back is a new episode of the old one
    let store = k.db().store();
    let log_ = store.would_do();
    let rec = |target: &str, at: i64| {
        log_.record(
            &steve_store::would_do::NewWouldDo {
                item_id: &item,
                cycle: 1,
                session_id: &sid,
                turn_id: "t1",
                scope_revision: version,
                target,
                action: "card",
                reason: "required_failed",
                row: 9,
                code: "closing",
                due_at: at,
                ..Default::default()
            },
            at,
        )
        .unwrap()
    };
    let (a, new_a) = rec("op-build", later + 90_000);
    assert!(new_a);
    assert_eq!(rec("op-build", later + 91_000), (a.clone(), false));
    let (b, new_b) = rec("op-build-2", later + 92_000);
    assert!(new_b && b != a, "another target is another record");
    let (a2, back) = rec("op-build", later + 93_000);
    assert!(back && a2 == a, "the old target's record comes back");
    let open = log_.open(&item).unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!((open[0].id.as_str(), open[0].episodes), (a.as_str(), 2));
}

#[test]
fn a_dismissed_card_and_an_answer_on_a_paused_item_are_cards_from_the_real_store() {
    let k = Kit::new();
    let p = policy();
    let now = crate::util::now();

    // dismissing the only card parks the item; hours later nothing happened
    let parked = k.item("run", "执行", None);
    let s1 = k.session(&parked, "idle");
    let d = k.decision(
        &parked,
        Some(&s1),
        "fail",
        "stall",
        &["continue", "look", "drop"],
    );
    k.core.dismiss_decision(&d, "dev").unwrap();
    assert_eq!(k.db().item(&parked).unwrap().unwrap().status, "pause");
    let t = k.core.tx();
    assert!(
        t.sweep_item(&parked, now + MIN, &p).unwrap().is_none(),
        "not yet"
    );
    let w = t
        .sweep_item(&parked, now + p.silent_min() + MIN, &p)
        .unwrap()
        .unwrap();
    drop(t);
    assert_eq!((w.action, w.reason), (Action::Card, "paused_silent"));

    // you paused it with a question open, then answered the question: it stays paused
    let asked = k.item("run", "执行", None);
    let s2 = k.session(&asked, "idle");
    let q = k.decision(&asked, Some(&s2), "ask", "question", &["a", "b"]);
    k.core
        .item_action(&asked, ItemAction::Pause, "dev")
        .unwrap();
    let paused_at = k
        .db()
        .store()
        .would_do()
        .paused_at(&asked)
        .unwrap()
        .unwrap();
    k.core.resolve(&q, "a", "", "dev").unwrap();
    k.rt.end_turn(&k.core, &s2, "end_turn", None);
    let resolved_at = k
        .db()
        .decision(&q)
        .unwrap()
        .unwrap()
        .resolved
        .unwrap()
        .max(paused_at + 1);
    steve_store::core_fixture::decision(
        k.db().store(),
        &q,
        steve_store::core_fixture::DecisionState::Resolved(resolved_at),
    );
    assert_eq!(k.db().item(&asked).unwrap().unwrap().status, "pause");
    let w = k
        .core
        .tx()
        .sweep_item(&asked, crate::util::now().max(resolved_at), &p)
        .unwrap()
        .unwrap();
    assert_eq!(
        (w.action, w.reason),
        (Action::Card, "answered_while_paused")
    );
    assert_eq!(w.detail.as_deref(), Some(q.as_str()));
}

#[test]
fn the_sweep_looks_at_every_open_item_and_the_views_show_what_it_would_do() {
    let k = following();
    let run = k.item("run", "执行", None);
    k.session(&run, "idle");
    let ready = k.item("prop", "计划", None);
    let closing = k.item("run", "收尾", None);
    let archived = k.item("run", "执行", None);
    let done = k.item("done", "完成", None);
    let store = k.db().store();
    steve_store::core_fixture::item(
        store,
        &ready,
        steve_store::core_fixture::ItemState::Phase("ready"),
    );
    steve_store::core_fixture::item(
        store,
        &closing,
        steve_store::core_fixture::ItemState::Phase("closing"),
    );
    steve_store::core_fixture::item(
        store,
        &archived,
        steve_store::core_fixture::ItemState::Archive(None),
    );
    let open = k.db().store().would_do().open_items().unwrap();
    for id in [&run, &ready, &closing] {
        assert!(open.contains(id), "{id} is open");
    }
    assert!(!open.contains(&archived) && !open.contains(&done));
    // one pass over all of them; nothing carried out
    k.core.sweep().unwrap();
    assert!(k.db().decisions_of_item(&closing).unwrap().is_empty());

    // the item API's shadow block and the debug view carry the would-do and the derived next step
    let v = k.core.shadow_view(&run).unwrap().unwrap();
    assert_eq!(v["would_do"]["current"]["reason"], json!("follow_up"));
    assert_eq!(v["would_do"]["current"]["action"], json!("wake"));
    assert_eq!(v["would_do"]["current"]["executes"], json!(false));
    assert_eq!(v["would_do"]["execute"]["card"], json!(false));
    assert!(v["would_do"]["log"].is_array());
    assert!(v["next_step"].is_object());
    let snap = k.core.diagnostic_snapshot(&run).unwrap();
    assert_eq!(
        snap["shadow"]["would_do"]["current"]["reason"],
        json!("follow_up")
    );
}
