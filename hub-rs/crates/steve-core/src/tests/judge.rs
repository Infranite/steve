//! The completion judge in shadow (#335, crate::judge): settled detection with three values, a
//! candidate per (item, cycle, session, logical turn, scope revision) registered at the turn end or
//! by the sweep, facts before the model, a strict reply, compare-and-swap on write — and nothing
//! of today's state changes because of it.

use serde_json::json;
use steve_store::item_facts::NewCandidate;

use crate::derive::{
    Card, Delivered, Due, Facts, ItemState, NextStep, Required, SessionState, Settled, VERIFIED,
    derive, judge_gate,
};
use crate::judge::{
    Bundle, Called, Checked, Request, Status, Worker, choose, criteria, evidence, fact_checks,
    independent, parse, ruled, would,
};
use crate::models::Choice;
use crate::testkit::Kit;
use crate::work_policy::WorkPolicy;

const NOW: i64 = 1_800_000_000_000;

fn policy() -> WorkPolicy {
    WorkPolicy::default()
}

/// An open item whose session is idle on its newest turn `t1`.
fn facts() -> Facts {
    Facts {
        now: NOW,
        item: ItemState {
            id: "i1".into(),
            cycle: 1,
            phase: "active".into(),
            ..Default::default()
        },
        session: Some(SessionState {
            id: "s1".into(),
            turn: Some("t1".into()),
            last_activity: NOW - 1_000,
            watched: true,
            ..Default::default()
        }),
        continues_cap: 2,
        stall_after: crate::stall::STALL_AFTER_MS,
        ..Default::default()
    }
}

fn fact(kind: &str, state: &str, provenance: &str) -> Delivered {
    Delivered {
        cycle: 1,
        kind: kind.into(),
        head: None,
        ref_: None,
        state: state.into(),
        provenance: provenance.into(),
        observed_at: NOW - 5_000,
    }
}

fn bundle(f: &Facts, required: Required, text: &str) -> (Bundle, Vec<Checked>) {
    let criteria = criteria(required, Some((2, "写好说明并合并")), "t");
    let checked = fact_checks(f, &criteria);
    let b = Bundle {
        item: "i1".into(),
        cycle: 1,
        revision: 1,
        scope_revision: 2,
        session: "s1".into(),
        turn: "t1".into(),
        attempt: None,
        request: Request {
            source: ("chat".into(), "m1".into()),
            title: "t".into(),
            text: "写好说明".into(),
        },
        criteria,
        final_text: text.into(),
        final_text_source: "attempt",
        stop_reason: "end_turn".into(),
        error: None,
        delivered_this_turn: false,
        evidence: evidence(f, &["单测通过".into()], &[]),
    };
    (b, checked)
}

#[test]
fn settled_has_three_values_and_only_a_quiescent_turn_is_evaluable() {
    let p = policy();
    assert_eq!(judge_gate(&facts(), &p), (Settled::Evaluable, "settled"));

    let mut f = facts();
    f.session.as_mut().unwrap().running = true;
    assert_eq!(judge_gate(&f, &p), (Settled::Busy, "running"));

    // the Node may have taken the turn and its receipt never came: nobody can tell
    let mut f = facts();
    f.session
        .as_mut()
        .unwrap()
        .dispatches
        .push(crate::derive::Dispatch {
            operation_id: "op1".into(),
            state: "pending".into(),
            maybe_sent: true,
            receipt: false,
            retry_at: NOW,
        });
    assert_eq!(judge_gate(&f, &p), (Settled::Unknown, "reconciling"));

    let mut f = facts();
    f.session.as_mut().unwrap().offline_since = Some(NOW - 1_000);
    assert_eq!(judge_gate(&f, &p).0, Settled::Unknown);

    let mut f = facts();
    f.session.as_mut().unwrap().queued = 1;
    assert_eq!(judge_gate(&f, &p), (Settled::Busy, "queued"));

    let mut f = facts();
    f.cards.push(Card {
        id: "d1".into(),
        stop: false,
        question: "?".into(),
    });
    assert_eq!(judge_gate(&f, &p), (Settled::Busy, "card"));

    let mut f = facts();
    f.session.as_mut().unwrap().held_by_you = true;
    assert_eq!(judge_gate(&f, &p), (Settled::Busy, "your_stop"));

    let mut f = facts();
    f.unknown_effects.push("op-x".into());
    assert_eq!(judge_gate(&f, &p), (Settled::Unknown, "unknown_effects"));

    let mut f = facts();
    f.graph_busy = Some("unit runs".into());
    assert_eq!(judge_gate(&f, &p), (Settled::Busy, "workflow"));

    let mut f = facts();
    f.session.as_mut().unwrap().turn = None;
    assert_eq!(judge_gate(&f, &p), (Settled::Unknown, "no_turn"));
}

#[test]
fn a_settled_turn_waiting_for_its_judge_is_row_6_with_a_due_time() {
    let mut f = facts();
    f.judging = Some(crate::derive::Judging {
        id: "jc1".into(),
        cycle: 1,
        session_id: "s1".into(),
        turn_id: "t1".into(),
        next_check: NOW + 5_000,
        failures: 0,
    });
    let d = derive(&f, &policy());
    assert_eq!((d.row, d.code), (6, "judging"));
    assert_eq!(
        d.next_step,
        Some(NextStep::Due {
            kind: Due::Judge,
            id: Some("jc1".into()),
            at: Some(NOW + 5_000)
        })
    );
    assert!(crate::derive::check_next_step(&f, &d).is_ok());
    // a candidate of an older turn is not this turn's
    f.judging.as_mut().unwrap().turn_id = "t0".into();
    assert_ne!(derive(&f, &policy()).row, 6);
}

#[test]
fn criteria_have_stable_ids_kinds_and_the_scope_version() {
    let c = criteria(
        Required {
            merge: true,
            build: false,
            deploy: true,
            operate: true,
        },
        Some((3, "目标")),
        "t",
    );
    let ids: Vec<_> = c
        .iter()
        .map(|c| (c.id.as_str(), c.kind, c.version))
        .collect();
    assert_eq!(
        ids,
        vec![
            ("merge", "merge", 3),
            ("deploy", "deploy", 3),
            ("operated", "operated", 3),
            ("goal", "free", 3)
        ]
    );
    // without a goal, the title stands for it
    let c = criteria(Required::default(), None, "标题");
    assert_eq!(
        (c[0].id.as_str(), c[0].text.as_str(), c[0].version),
        ("goal", "标题", 0)
    );
}

#[test]
fn only_verified_facts_settle_a_criterion() {
    let required = Required {
        merge: true,
        ..Default::default()
    };
    let crit = criteria(required, None, "t");
    let mut f = facts();
    // the agent said it merged: reported, so nobody knows
    f.delivery.push(fact("pr", "merged", "reported"));
    let k = fact_checks(&f, &crit);
    assert_eq!((k[0].status, k[0].by), (Status::Unknown, "none"));
    // the Hub could not read it
    f.delivery.push(Delivered {
        observed_at: NOW - 1_000,
        ..fact("pr", "unknown", "unknown")
    });
    assert_eq!(fact_checks(&f, &crit)[0].status, Status::Unknown);
    // the Hub read it open, then merged
    f.delivery.push(Delivered {
        observed_at: NOW - 500,
        ..fact("pr", "open", VERIFIED)
    });
    let k = fact_checks(&f, &crit);
    assert_eq!(
        (k[0].status, k[0].state.as_deref()),
        (Status::Unmet, Some("open"))
    );
    assert_eq!(k[0].evidence, vec!["e3".to_string()]);
    f.delivery.push(Delivered {
        observed_at: NOW - 100,
        ..fact("pr", "merged", VERIFIED)
    });
    assert_eq!(fact_checks(&f, &crit)[0].status, Status::Met);
    // a goal in words is never a fact
    assert_eq!(fact_checks(&f, &crit)[1].status, Status::Unknown);
}

/// Only the hard conditions: the criteria of an item whose goal in words is left out.
fn hard_only(f: &Facts, required: Required) -> (Bundle, Vec<Checked>) {
    let (mut b, _) = bundle(f, required, "做好了");
    b.criteria.retain(|c| crate::judge::is_hard(c.kind));
    let k = fact_checks(f, &b.criteria);
    (b, k)
}

#[test]
fn facts_and_rules_decide_what_needs_no_model() {
    let required = Required {
        merge: true,
        build: true,
        ..Default::default()
    };
    let mut f = facts();
    f.delivery.push(fact("pr", "merged", VERIFIED));
    f.delivery.push(fact("build", "passed", VERIFIED));
    let (b, k) = hard_only(&f, required);
    assert_eq!(
        ruled(&b, &k, true),
        Some(("done", "hard_facts_met", "facts"))
    );

    // the build still runs: waiting, not failed
    let mut f = facts();
    f.delivery.push(fact("pr", "merged", VERIFIED));
    f.delivery.push(fact("build", "running", VERIFIED));
    let (b, k) = hard_only(&f, required);
    assert_eq!(
        ruled(&b, &k, true),
        Some(("waiting", "hard_facts_underway", "facts"))
    );

    let mut f = facts();
    f.delivery.push(fact("pr", "merged", VERIFIED));
    f.delivery.push(fact("build", "failed", VERIFIED));
    let (b, k) = hard_only(&f, required);
    assert_eq!(
        ruled(&b, &k, true),
        Some(("not_done", "hard_facts_failed", "facts"))
    );

    // a hard criterion nobody can tell is left to the model, and an operation always is
    let mut f = facts();
    f.delivery.push(fact("pr", "merged", VERIFIED));
    let (b, k) = hard_only(&f, required);
    assert_eq!(ruled(&b, &k, true), None);
    let mut f = facts();
    f.delivery.push(fact("pr", "merged", VERIFIED));
    let (b, k) = bundle(
        &f,
        Required {
            merge: true,
            operate: true,
            ..Default::default()
        },
        "做好了",
    );
    assert_eq!(ruled(&b, &k, true), None);

    // the turn itself
    let (mut b, k) = bundle(&facts(), Required::default(), "做好了");
    assert_eq!(ruled(&b, &k, true), None);
    assert_eq!(ruled(&b, &k, false), Some(("unknown", "model_off", "rule")));
    b.stop_reason = "error".into();
    assert_eq!(
        ruled(&b, &k, true),
        Some(("not_done", "turn_failed", "rule"))
    );
    b.stop_reason = "cancelled".into();
    assert_eq!(ruled(&b, &k, true), Some(("unknown", "cancelled", "rule")));
    let (b, k) = bundle(&facts(), Required::default(), "");
    assert_eq!(ruled(&b, &k, true), Some(("unknown", "no_reply", "rule")));
    // the turn's words could not be read (its end was not kept): not known, not "nothing said"
    let (mut b, k) = bundle(&facts(), Required::default(), "");
    b.final_text_source = "unknown";
    assert_eq!(
        ruled(&b, &k, true),
        Some(("unknown", "reply_unknown", "rule"))
    );
}

/// Finding: once the hard conditions were met, the goal in words was never judged. Every criterion
/// is checked one by one: the hard ones by facts, the goal in words always by the model.
#[test]
fn the_goal_in_words_is_judged_also_when_every_hard_condition_is_met() {
    let required = Required {
        merge: true,
        build: true,
        ..Default::default()
    };
    let mut f = facts();
    f.delivery.push(fact("pr", "merged", VERIFIED));
    f.delivery.push(fact("build", "passed", VERIFIED));
    let (b, k) = bundle(&f, required, "合并了，但说明还没写");
    // merged and built is not the goal: the model is asked
    assert_eq!(ruled(&b, &k, true), None);
    let ids: Vec<_> = k.iter().map(|c| (c.id.as_str(), c.status, c.by)).collect();
    assert_eq!(
        ids,
        vec![
            ("merge", Status::Met, "facts"),
            ("build", Status::Met, "facts"),
            ("goal", Status::Unknown, "none")
        ]
    );
    let prompt = crate::judge::prompt(&b, &k);
    assert!(prompt.contains(r#""checked":[{"id":"merge","status":"met","evidence":["e1"]},{"id":"build","status":"met","evidence":["e2"]}]"#), "{prompt}");
    // and its answer about the goal counts: not done, though the hard conditions are met
    let v = parse(
        r#"{"verdict":"not_done","criteria":[{"id":"merge","status":"met","evidence":["e1"]},{"id":"build","status":"met","evidence":["e2"]},{"id":"goal","status":"unmet"}],"reason":"说明没写"}"#,
        &b,
        &k,
    )
    .unwrap();
    assert_eq!(v.verdict, "not_done");
    assert_eq!(
        v.criteria
            .iter()
            .map(|c| (c.id.as_str(), c.status, c.by))
            .collect::<Vec<_>>(),
        vec![
            ("merge", Status::Met, "facts"),
            ("build", Status::Met, "facts"),
            ("goal", Status::Unmet, "model")
        ]
    );
}

/// Finding: the model could mark a hard condition facts left unknown as met. Unknown stays unknown:
/// the model has to repeat it, and with it unknown nothing is done.
#[test]
fn the_model_never_upgrades_an_unknown_hard_condition() {
    let required = Required {
        merge: true,
        ..Default::default()
    };
    let mut f = facts();
    // the agent says it merged: reported, so a hard condition nobody verified
    f.delivery.push(fact("pr", "merged", "reported"));
    let (b, k) = bundle(&f, required, "已经合并");
    assert_eq!((k[0].status, k[0].by), (Status::Unknown, "none"));
    // the model is shown it as checked and unknown
    let prompt = crate::judge::prompt(&b, &k);
    assert!(
        prompt.contains(r#""checked":[{"id":"merge","status":"unknown","evidence":["e1"]}]"#),
        "{prompt}"
    );
    let refused = |raw: &str| parse(raw, &b, &k).unwrap_err();
    assert_eq!(
        refused(
            r#"{"verdict":"done","criteria":[{"id":"merge","status":"met","evidence":["e1"]},{"id":"goal","status":"met","evidence":["final_text"]}]}"#
        ),
        "contradicts_facts:merge"
    );
    assert_eq!(
        refused(
            r#"{"verdict":"not_done","criteria":[{"id":"merge","status":"unmet","evidence":["e1"]},{"id":"goal","status":"met","evidence":["final_text"]}]}"#
        ),
        "contradicts_facts:merge"
    );
    // repeated as unknown: accepted, and it stays the facts' unknown
    let v = parse(
        r#"{"verdict":"waiting","criteria":[{"id":"merge","status":"unknown"},{"id":"goal","status":"met","evidence":["final_text"]}],"reason":"等确认合并"}"#,
        &b,
        &k,
    )
    .unwrap();
    assert_eq!(
        (v.criteria[0].status, v.criteria[0].by),
        (Status::Unknown, "none")
    );
    assert_eq!(v.criteria[0].evidence, vec!["e1".to_string()]);
    // and done needs it met, which it is not
    assert_eq!(
        refused(
            r#"{"verdict":"done","criteria":[{"id":"merge","status":"unknown"},{"id":"goal","status":"met","evidence":["final_text"]}]}"#
        ),
        "done_with_open_criteria"
    );
}

#[test]
fn the_model_reply_is_strict() {
    let required = Required {
        merge: true,
        ..Default::default()
    };
    let mut f = facts();
    f.delivery.push(fact("pr", "open", VERIFIED));
    let (b, k) = bundle(&f, required, "说明已写好，PR 等合并");
    let ok = r#"好的 {"verdict":"waiting","criteria":[{"id":"merge","status":"unmet","evidence":["e1"]},{"id":"goal","status":"met","evidence":["final_text","c1"]}],"reason":"等合并"}"#;
    let v = parse(ok, &b, &k).unwrap();
    assert_eq!(v.verdict, "waiting");
    assert_eq!(v.criteria.len(), 2);
    // what facts settled stays theirs
    assert_eq!(
        (v.criteria[0].id.as_str(), v.criteria[0].by),
        ("merge", "facts")
    );
    assert_eq!(v.criteria[1].by, "model");

    let refused = |raw: &str| parse(raw, &b, &k).unwrap_err();
    assert_eq!(refused("我觉得做完了"), "no_json");
    assert!(refused(r#"{"verdict":"finished","criteria":[]}"#).starts_with("bad_verdict"));
    assert_eq!(refused(r#"{"verdict":"unknown"}"#), "no_criteria");
    assert_eq!(
        refused(
            r#"{"verdict":"unknown","criteria":[{"id":"merge","status":"unmet","evidence":["e1"]}]}"#
        ),
        "missing_criterion:goal"
    );
    assert_eq!(
        refused(
            r#"{"verdict":"unknown","criteria":[{"id":"merge","status":"unmet"},{"id":"merge","status":"unmet"},{"id":"goal","status":"unknown"}]}"#
        ),
        "criterion_twice:merge"
    );
    assert!(
        refused(r#"{"verdict":"unknown","criteria":[{"id":"merge","status":"unmet"},{"id":"x","status":"met","evidence":["e1"]}]}"#)
            .starts_with("unknown_criterion")
    );
    assert_eq!(
        refused(
            r#"{"verdict":"unknown","criteria":[{"id":"merge","status":"unmet"},{"id":"goal","status":"done"}]}"#
        ),
        "bad_status:goal"
    );
    assert!(
        refused(r#"{"verdict":"unknown","criteria":[{"id":"merge","status":"unmet"},{"id":"goal","status":"met","evidence":["e9"]}]}"#)
            .starts_with("unknown_ref")
    );
    assert_eq!(
        refused(
            r#"{"verdict":"unknown","criteria":[{"id":"merge","status":"unmet"},{"id":"goal","status":"met"}]}"#
        ),
        "met_without_evidence:goal"
    );
    // the PR is open (a verified fact): the model may not call it merged
    assert_eq!(
        refused(
            r#"{"verdict":"done","criteria":[{"id":"merge","status":"met","evidence":["e1"]},{"id":"goal","status":"met","evidence":["final_text"]}]}"#
        ),
        "contradicts_facts:merge"
    );
    // done needs every criterion met
    let (b2, k2) = bundle(&facts(), Required::default(), "写了一半");
    assert_eq!(
        parse(
            r#"{"verdict":"done","criteria":[{"id":"goal","status":"unknown"}]}"#,
            &b2,
            &k2
        )
        .unwrap_err(),
        "done_with_open_criteria"
    );
}

#[test]
fn what_a_verdict_would_trigger_is_a_fixed_table() {
    assert_eq!(would("done", true, true, 1, true), "close");
    assert_eq!(would("done", false, true, 1, true), "review");
    assert_eq!(would("done", false, false, 2, true), "confirm");
    assert_eq!(would("done", false, false, 1, true), "close_after_grace");
    assert_eq!(would("not_done", false, false, 1, true), "continue");
    assert_eq!(would("not_done", false, false, 1, false), "needs_you");
    assert_eq!(would("unknown", false, false, 1, true), "follow_up");
    assert_eq!(would("unknown", false, false, 1, false), "needs_you");
    assert_eq!(would("waiting", false, false, 1, false), "wait");
    assert_eq!(would("needs_you", false, false, 1, true), "needs_you");
}

#[test]
fn the_judge_keeps_off_the_workers_model_unless_you_chose_it() {
    let worker = Worker {
        harness: "h".into(),
        model: Some("big".into()),
    };
    let steve = Choice {
        harness: "h".into(),
        models: vec!["big".into(), "other".into()],
        effort: None,
    };
    assert_eq!(
        choose(steve.clone(), false, &worker, &[], None).models,
        vec!["other"]
    );
    // your own setting for the judge is taken as it is
    assert_eq!(choose(steve.clone(), true, &worker, &[], None), steve);
    // nothing else on the list: the harness's quick model, else another it offers
    let only = Choice {
        models: vec!["big".into()],
        ..steve.clone()
    };
    assert_eq!(
        choose(
            only.clone(),
            false,
            &worker,
            &["big".into()],
            Some("mini".into())
        )
        .models,
        vec!["mini"]
    );
    assert_eq!(
        choose(
            only.clone(),
            false,
            &worker,
            &["big".into(), "mid".into()],
            None
        )
        .models,
        vec!["mid"]
    );
    // no other model: as resolved, and the judgement says it was not independent
    assert_eq!(
        choose(only.clone(), false, &worker, &["big".into()], None),
        only
    );
    assert_eq!(independent("h", "big", &worker), "no");
    assert_eq!(independent("h", "other", &worker), "yes");
    assert_eq!(independent("h", "", &worker), "unknown");
    // another harness is another judge already
    let other = Choice {
        harness: "g".into(),
        ..steve
    };
    assert_eq!(choose(other.clone(), false, &worker, &[], None), other);
}

/// A finished logical turn `turn` of session `sid` on the Node runtime's ledger.
fn finished_turn(k: &Kit, sid: &str, turn: &str, at: i64) {
    steve_store::core_fixture::runtime::finished_turn(
        k.db().store(),
        steve_store::core_fixture::runtime::FinishedTurn {
            session: sid,
            turn,
            attempt: &attempt_of(turn),
            node: "node-a",
            node_session: "ns-1",
            ordinal: 0,
            operation: &format!("op-{turn}"),
            at,
        },
    );
}

/// The one attempt of logical turn `turn` ([`finished_turn`]).
fn attempt_of(turn: &str) -> String {
    format!("{turn}-a")
}

/// What attempt `attempt` said, as a Node tells it: a session event made of an entry of
/// its event stream, with that stream position as its source and the attempt it names. Empty
/// `text`: nothing it said is a session event.
fn attempt_says(k: &Kit, sid: &str, attempt: &str, text: &str) {
    if text.is_empty() {
        return;
    }
    steve_store::core_fixture::runtime::reported_text(
        k.db().store(),
        sid,
        attempt,
        text,
        steve_store::now(),
    );
}

/// The end of `turn`'s attempt as the Hub keeps it (`session_stops`): its events so far are its own.
fn ended(k: &Kit, sid: &str, turn: &str, stop_reason: &str) {
    k.db()
        .store()
        .sessions()
        .stop_ended(sid, Some(&attempt_of(turn)), stop_reason, None, false)
        .unwrap();
}

/// An open item with an idle session whose turn `t1` is over and said `text`; the judge's debounce
/// is off so its candidate is due at once.
fn item_with_turn(k: &Kit, text: &str) -> (String, String) {
    let item = k.item("run", "执行", None);
    let sid = k.session(&item, "idle");
    finished_turn(k, &sid, "t1", steve_store::now() - 1_000);
    attempt_says(k, &sid, &attempt_of("t1"), text);
    ended(k, &sid, "t1", "end_turn");
    k.db()
        .store()
        .item_facts()
        .set_policy("settle_debounce", 0)
        .unwrap();
    (item, sid)
}

/// What today's state is, to show the judge changed none of it.
fn today(k: &Kit, item: &str, sid: &str) -> serde_json::Value {
    let i = k.db().item(item).unwrap().unwrap();
    json!({
        "phase": i.phase, "status": i.status, "cycle": i.cycle, "revision": i.revision,
        "now": i.now, "decisions": k.db().decisions_of_item(item).unwrap().len(),
        "timeline": k.db().timeline(item).unwrap().len(),
        "prompts": k.rt.prompts_of(sid).len(),
    })
}

const DONE: &str = r#"{"verdict":"done","criteria":[{"id":"goal","status":"met","evidence":["final_text"]}],"reason":"写好了"}"#;

#[tokio::test(flavor = "multi_thread")]
async fn a_settled_turn_is_judged_once_by_the_model_and_nothing_acts_on_it() {
    let k = Kit::new();
    let (item, sid) = item_with_turn(&k, "说明写好了，放在 README。");
    k.gw.set(|_, _| Ok(DONE.into()));
    let before = today(&k, &item, &sid);

    // the sweep registers the turn nobody registered, and the pass judges it
    assert_eq!(k.core.check_judges().await.unwrap(), 1);
    let facts = k.db().store().item_facts();
    let cands = facts.candidates(&item, 1).unwrap();
    assert_eq!(cands.len(), 1);
    assert_eq!(
        (
            cands[0].registered_by.as_str(),
            cands[0].state.as_str(),
            cands[0].turn_id.as_str()
        ),
        ("sweep", "judged", "t1")
    );
    let j = &facts.judgements(&item, 1).unwrap()[0];
    assert_eq!(j.verdict, "done");
    assert_eq!(j.decided_by.as_deref(), Some("model"));
    assert_eq!(
        j.model.as_deref(),
        Some("fake-model"),
        "the model that answered, as it named itself"
    );
    // the fake gateway does not say which harness answered
    assert_eq!(j.harness.as_deref(), Some("unknown"));
    // the worker ran the harness default: whether the judge was another model is not known
    assert_eq!(j.independent.as_deref(), Some("unknown"));
    assert_eq!(j.would.as_deref(), Some("close_after_grace"));
    assert_eq!(j.basis["criteria"][0]["id"], json!("goal"));
    assert_eq!(
        j.basis["bundle"]["final_text"],
        json!("说明写好了，放在 README。")
    );
    // one call, without tools, on the judge's purpose
    assert_eq!(k.gw.calls.lock().unwrap().len(), 1);
    assert!(!k.gw.tools.lock().unwrap()[0].any());

    // judged once: the next passes leave it
    assert_eq!(k.core.check_judges().await.unwrap(), 0);
    assert_eq!(k.gw.calls.lock().unwrap().len(), 1);
    assert_eq!(facts.judgements(&item, 1).unwrap().len(), 1);

    // shadow: today's state is what it was — nothing closed, asked, sent or noted
    assert_eq!(today(&k, &item, &sid), before);
    let v = k.core.derived_state(&item).unwrap().unwrap();
    assert_eq!(v["judge"]["judgement"]["verdict"], json!("done"));
    assert_eq!(v["judge"]["current"], json!(true));
    // the derived state shows the judged close behind its undo window, still only in shadow
    assert_eq!(v["code"], json!("judged_done"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_busy_or_unknown_turn_is_not_judged() {
    let k = Kit::new();
    let (item, sid) = item_with_turn(&k, "我先问一下");
    k.gw.set(|_, _| Ok(DONE.into()));
    let d = k.decision(&item, Some(&sid), "ask", "question", &["a", "b"]);
    assert_eq!(k.core.check_judges().await.unwrap(), 0);
    let facts = k.db().store().item_facts();
    let c = &facts.candidates(&item, 1).unwrap()[0];
    assert_eq!(
        (
            c.state.as_str(),
            c.settled.as_deref(),
            c.settled_reason.as_deref()
        ),
        ("pending", Some("busy"), Some("card"))
    );
    assert!(c.next_check > steve_store::now(), "looked at again later");
    assert!(k.gw.calls.lock().unwrap().is_empty());
    let v = k.core.derived_state(&item).unwrap().unwrap();
    assert_eq!(v["judge"]["candidate"]["settled"], json!("busy"));

    // the card is answered and its time comes: now it is judged
    k.db()
        .decision_resolve(&d, json!({"choice": "a"}), "answered", None)
        .unwrap();
    steve_store::core_fixture::work::run_judges_now(k.db().store());
    assert_eq!(k.core.check_judges().await.unwrap(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_verdict_for_a_turn_that_is_no_longer_current_is_thrown_away() {
    let k = Kit::new();
    let (item, sid) = item_with_turn(&k, "做好了");
    k.gw.set(|_, _| Ok(DONE.into()));
    let prepared = k.core.tx().judge_prepare(steve_store::now()).unwrap();
    assert_eq!(prepared.len(), 1);
    // a new turn started while the model ran: the old turn's answer is late
    finished_turn(&k, &sid, "t2", steve_store::now());
    attempt_says(&k, &sid, &attempt_of("t2"), "第二轮也做好了");
    ended(&k, &sid, "t2", "end_turn");
    let p = prepared.into_iter().next().unwrap();
    let called = k.core.judge_call(&p).await;
    assert!(k.core.tx().judge_commit(p, called).unwrap().is_none());
    let facts = k.db().store().item_facts();
    assert!(facts.judgements(&item, 1).unwrap().is_empty());
    let old = facts
        .candidate_of(&item, 1, &sid, "t1", 1)
        .unwrap()
        .unwrap();
    assert_eq!(
        (old.state.as_str(), old.settled_reason.as_deref()),
        ("superseded", Some("turn"))
    );

    // the new turn has its own candidate; its inputs change while the model runs (the agent said
    // more): the result is thrown away and it is judged again on the new inputs
    let prepared = k.core.tx().judge_prepare(steve_store::now()).unwrap();
    assert_eq!(prepared[0].candidate.turn_id, "t2");
    k.rt.say(&sid, "还有一点补充");
    let p = prepared.into_iter().next().unwrap();
    let called = k.core.judge_call(&p).await;
    assert!(k.core.tx().judge_commit(p, called).unwrap().is_none());
    let c = facts
        .candidate_of(&item, 1, &sid, "t2", 1)
        .unwrap()
        .unwrap();
    assert_eq!(
        (c.state.as_str(), c.failures, c.last_error.as_deref()),
        ("pending", 1, Some("inputs_changed"))
    );
    assert!(facts.judgements(&item, 1).unwrap().is_empty());
    assert_eq!(k.core.check_judges().await.unwrap(), 1);
    let j = &facts.judgements(&item, 1).unwrap()[0];
    assert_eq!(j.turn_id, "t2");

    // a candidate of the old turn registered late (its turn end told again) is superseded, never judged
    assert!(
        facts
            .register_candidate(&NewCandidate {
                item_id: &item,
                cycle: 1,
                session_id: &sid,
                turn_id: "t1",
                scope_revision: 9,
                registered_by: "turn_end",
                next_check: 0,
                ..Default::default()
            })
            .unwrap()
    );
    assert_eq!(k.core.check_judges().await.unwrap(), 0);
    let late = facts
        .candidate_of(&item, 1, &sid, "t1", 9)
        .unwrap()
        .unwrap();
    assert_eq!(late.state, "superseded");
    assert_eq!(facts.judgements(&item, 1).unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn one_pass_holds_a_candidate_at_a_time() {
    let k = Kit::new();
    let (item, sid) = item_with_turn(&k, "做好了");
    k.gw.set(|_, _| Ok(DONE.into()));
    let first = k.core.tx().judge_prepare(steve_store::now()).unwrap();
    assert_eq!(first.len(), 1);
    // a second pass meanwhile does not claim it again
    assert!(
        k.core
            .tx()
            .judge_prepare(steve_store::now())
            .unwrap()
            .is_empty()
    );
    // the claim of a pass that never came back is taken over once it is stale
    let stale = steve_store::now() + policy().judge_claim_stale() + 1;
    let again = k.core.tx().judge_prepare(stale).unwrap();
    assert_eq!(again.len(), 1);
    // the first pass comes back late: its claim is gone, it writes nothing
    let p = first.into_iter().next().unwrap();
    assert!(
        k.core
            .tx()
            .judge_commit(
                p,
                Called {
                    result: Ok(DONE.into()),
                    harness: "h".into(),
                    model: "m".into()
                }
            )
            .unwrap()
            .is_none()
    );
    let p = again.into_iter().next().unwrap();
    let called = k.core.judge_call(&p).await;
    assert!(k.core.tx().judge_commit(p, called).unwrap().is_some());
    let facts = k.db().store().item_facts();
    assert_eq!(facts.judgements(&item, 1).unwrap().len(), 1);
    let c = facts
        .candidate_of(&item, 1, &sid, "t1", 1)
        .unwrap()
        .unwrap();
    assert_eq!(c.failures, 1, "the stale claim counts as a failure");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_invalid_reply_is_a_failure_and_the_judge_gives_up_after_the_cap() {
    let k = Kit::new();
    let (item, sid) = item_with_turn(&k, "做好了");
    k.gw.set(|_, _| Ok("当然做完了".into()));
    let facts = k.db().store().item_facts();
    for n in 1..=3 {
        assert_eq!(k.core.check_judges().await.unwrap(), 0);
        let c = facts
            .candidate_of(&item, 1, &sid, "t1", 1)
            .unwrap()
            .unwrap();
        assert_eq!(c.failures, n);
        assert_eq!(c.last_error.as_deref(), Some("invalid_output:no_json"));
        if n < 3 {
            assert_eq!(c.state, "pending");
            assert!(c.next_check > steve_store::now());
            steve_store::core_fixture::work::run_judges_now(k.db().store());
        } else {
            assert_eq!(c.state, "failed", "given up after judge_failures");
        }
    }
    assert!(facts.judgements(&item, 1).unwrap().is_empty());
    // a model that fails is the same
    let k = Kit::new();
    let (item, sid) = item_with_turn(&k, "做好了");
    k.gw.set(|_, _| Err(crate::error::msg("503")));
    k.core.check_judges().await.unwrap();
    let c = k
        .db()
        .store()
        .item_facts()
        .candidate_of(&item, 1, &sid, "t1", 1)
        .unwrap()
        .unwrap();
    assert_eq!((c.state.as_str(), c.failures), ("pending", 1));
}

#[tokio::test(flavor = "multi_thread")]
async fn rules_need_no_model_call() {
    let k = Kit::new();
    let (item, _) = item_with_turn(&k, "");
    k.gw.set(|_, _| Ok(DONE.into()));
    assert_eq!(k.core.check_judges().await.unwrap(), 0);
    let facts = k.db().store().item_facts();
    let j = &facts.judgements(&item, 1).unwrap()[0];
    assert_eq!(
        (
            j.verdict.as_str(),
            j.decided_by.as_deref(),
            j.basis["reason"].as_str()
        ),
        ("unknown", Some("rule"), Some("no_reply"))
    );
    assert!(k.gw.calls.lock().unwrap().is_empty());

    // the model switched off: only what facts and rules decide
    let k = Kit::new();
    let (item, _) = item_with_turn(&k, "做好了");
    k.db()
        .store()
        .item_facts()
        .set_policy("judge_model", 0)
        .unwrap();
    k.core.check_judges().await.unwrap();
    let j = &k.db().store().item_facts().judgements(&item, 1).unwrap()[0];
    assert_eq!(j.basis["reason"], json!("model_off"));
    assert!(k.gw.calls.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_turn_end_registers_the_candidate_and_the_sweep_only_what_is_missing() {
    let k = Kit::new();
    let item = k.item("run", "执行", None);
    let sid = k.session(&item, "idle");
    finished_turn(&k, &sid, "t1", steve_store::now());
    k.rt.end_turn(&k.core, &sid, "end_turn", None);
    let facts = k.db().store().item_facts();
    let c = facts.candidates(&item, 1).unwrap();
    assert_eq!(c.len(), 1);
    assert_eq!(
        (c[0].registered_by.as_str(), c[0].turn_id.as_str()),
        ("turn_end", "t1")
    );
    assert!(c[0].next_check + 1_000 >= c[0].created + policy().settle_debounce());
    // the sweep adds nothing for a turn that has its candidate
    assert_eq!(k.core.tx().judge_sweep().unwrap(), 0);
    // a conversation item has no candidate
    let k = Kit::new();
    let chat = k.item("run", "执行", None);
    steve_store::core_fixture::item(
        k.db().store(),
        &chat,
        steve_store::core_fixture::ItemState::Kind("chat"),
    );
    let sid = k.session(&chat, "idle");
    finished_turn(&k, &sid, "t1", steve_store::now());
    assert_eq!(k.core.tx().judge_sweep().unwrap(), 0);
}

#[test]
fn candidates_are_one_per_key_and_refuse_unknown_values() {
    let k = Kit::new();
    let item = k.item("run", "执行", None);
    let facts = k.db().store().item_facts();
    let c = NewCandidate {
        item_id: &item,
        cycle: 1,
        session_id: "s1",
        turn_id: "t1",
        scope_revision: 1,
        registered_by: "turn_end",
        next_check: 0,
        ..Default::default()
    };
    assert!(facts.register_candidate(&c).unwrap());
    assert!(
        !facts
            .register_candidate(&NewCandidate {
                registered_by: "sweep",
                ..c.clone()
            })
            .unwrap()
    );
    assert!(
        facts
            .register_candidate(&NewCandidate {
                registered_by: "guess",
                ..c.clone()
            })
            .is_err()
    );
    let id = facts.candidates(&item, 1).unwrap()[0].id.clone();
    assert!(facts.candidate_settled(&id, "maybe", "x", 0).is_err());
    // only the claim holder releases or writes
    assert!(facts.candidate_claim(&id, "k1", 1, "fp", 0).unwrap());
    assert!(!facts.candidate_claim(&id, "k2", 1, "fp", 0).unwrap());
    assert!(!facts.candidate_release(&id, "k2", None, false, 0).unwrap());
    assert!(facts.candidate_release(&id, "k1", None, false, 0).unwrap());
}

/// The judge runs in its own watch: while one of its model calls hangs, the stall watch still
/// opens the card of an item that stopped after the call began (in one loop it would wait behind
/// the call, and so would the Release build and pull request checks).
#[tokio::test(flavor = "multi_thread")]
async fn a_hanging_judge_call_never_holds_up_the_stall_watch() {
    let k = Kit::new();
    let (item, _) = item_with_turn(&k, "做好了");
    k.gw.set(|_, _| Ok(DONE.into()));
    *k.gw.slow.lock().unwrap() = Some(("You judge whether", std::time::Duration::from_secs(3_600)));
    let judge = tokio::spawn(k.core.clone().run_judge_watch());
    let stall = tokio::spawn(
        k.core
            .clone()
            .stall_watch(std::time::Duration::from_millis(50)),
    );
    assert!(
        steve_runtime::testkit::eventually_sync("The judge call is under way.", || k
            .gw
            .calls
            .lock()
            .unwrap()
            .len()
            == 1)
        .await,
        "the judge's call is under way"
    );

    // now an item stops (you cancelled its turn) and has been quiet past the stall time
    let stopped = k.item("run", "另一件事", None);
    let s = k.session(&stopped, "idle");
    crate::AgentRuntime::prompt(&*k.rt, &s, "做事", "me", &[]).unwrap();
    k.rt.end_turn(&k.core, &s, "cancelled", None);
    steve_store::core_fixture::runtime::retime_session(k.db().store(), &s, 0);
    let card = || {
        k.db()
            .decisions_of_item(&stopped)
            .unwrap()
            .iter()
            .any(|d| d.subject_type == crate::stall::STALL)
    };
    assert!(
        steve_runtime::testkit::eventually_sync(
            "The stall card opens while the judge waits.",
            card
        )
        .await,
        "the stall card opens while the judge waits"
    );
    // the judge call is still hanging: nothing of it was written, and no second pass began
    assert_eq!(k.gw.calls.lock().unwrap().len(), 1);
    let facts = k.db().store().item_facts();
    assert!(facts.judgements(&item, 1).unwrap().is_empty());
    judge.abort();
    stall.abort();
}

#[test]
fn no_session_is_unknown_with_its_own_reason() {
    let mut f = facts();
    f.session = None;
    assert_eq!(judge_gate(&f, &policy()), (Settled::Unknown, "no_session"));
}

/// Finding: the final text and the stop reason were the session's, whichever turn they came from.
/// They are the candidate's own turn and attempt: words said after its end are not its words, and
/// when the session's last end is another attempt's, its end and words are unknown.
#[tokio::test(flavor = "multi_thread")]
async fn the_bundle_reads_the_candidates_own_turn_and_attempt() {
    let k = Kit::new();
    let (_, sid) = item_with_turn(&k, "第一轮做好了");
    // said after the attempt ended (a later turn's beginning): not this turn's words
    k.rt.say(&sid, "之后又说的话");
    let p = k
        .core
        .tx()
        .judge_prepare(steve_store::now())
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    assert_eq!(p.bundle.final_text, "第一轮做好了");
    assert_eq!(
        (p.bundle.final_text_source, p.bundle.stop_reason.as_str()),
        ("attempt", "end_turn")
    );
    assert_eq!(p.bundle.attempt.as_deref(), Some("t1-a"));

    // the session's last end is another attempt's: this one's end and words are not known
    let k = Kit::new();
    let (item, sid) = item_with_turn(&k, "做好了");
    k.db()
        .store()
        .sessions()
        .stop_ended(&sid, Some("another-attempt"), "error", Some("boom"), false)
        .unwrap();
    k.gw.set(|_, _| Ok(DONE.into()));
    assert_eq!(k.core.check_judges().await.unwrap(), 0);
    let j = &k.db().store().item_facts().judgements(&item, 1).unwrap()[0];
    // not the other attempt's failure, and not "nothing said": unknown, with no model call
    assert_eq!(
        (j.verdict.as_str(), j.basis["reason"].as_str()),
        ("unknown", Some("reply_unknown"))
    );
    assert_eq!(j.basis["bundle"]["stop_reason"], json!("unknown"));
    assert_eq!(j.basis["bundle"]["final_text_source"], json!("unknown"));
    assert_eq!(j.basis["bundle"]["final_text"], json!(""));
    assert!(j.basis["bundle"]["error"].is_null());
    assert!(k.gw.calls.lock().unwrap().is_empty());

    // the attempt's own end says it failed: that is what the bundle reads
    let k = Kit::new();
    let (item, sid) = item_with_turn(&k, "做了一半");
    ended(&k, &sid, "t1", "error");
    k.core.check_judges().await.unwrap();
    let j = &k.db().store().item_facts().judgements(&item, 1).unwrap()[0];
    assert_eq!(j.basis["reason"], json!("turn_failed"));
    assert_eq!(j.basis["bundle"]["stop_reason"], json!("error"));
}

/// Finding: the write's transaction re-checked only the claim. It re-checks the cycle, the current
/// logical turn, the scope revision, the input version and the evidence version, and writes nothing
/// when any of them moved since the claim.
#[tokio::test(flavor = "multi_thread")]
async fn the_write_rechecks_turn_scope_inputs_and_evidence_in_its_own_transaction() {
    use steve_store::item_facts::{Judged, NewDeliveryFact, NewJudgement};
    let k = Kit::new();
    let (item, sid) = item_with_turn(&k, "做好了");
    // an operation of the cycle the derived state reads (the acceptance hooks, done)
    let t0 = steve_store::now();
    steve_store::core_fixture::completed_hooks(k.db().store(), "op1", &item, t0);
    let p = k
        .core
        .tx()
        .judge_prepare(steve_store::now())
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let facts = k.db().store().item_facts();
    let c = p.candidate.clone();
    let write = || {
        facts
            .candidate_judged(
                &c.id,
                &p.claim,
                &p.evidence,
                &NewJudgement {
                    item_id: &item,
                    cycle: 1,
                    session_id: &sid,
                    turn_id: "t1",
                    scope_revision: c.scope_revision,
                    verdict: "done",
                    basis: json!({}),
                    candidate_id: Some(&c.id),
                    decided_by: Some("model"),
                    ..Default::default()
                },
            )
            .unwrap()
    };
    // each change alone is caught: made from the last check to the first. The operation the
    // judgement read moved on (its status and when it changed): the evidence is another
    steve_store::core_fixture::fail_operation(k.db().store(), "op1");
    assert_eq!(write(), Judged::Stale("evidence"));
    facts
        .record_delivery(&NewDeliveryFact {
            item_id: &item,
            cycle: 1,
            kind: "pr",
            state: "open",
            provenance: VERIFIED,
            source: "test",
            observed_at: steve_store::now(),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(write(), Judged::Stale("evidence"));
    k.rt.say(&sid, "补充一句");
    assert_eq!(write(), Judged::Stale("inputs"));
    k.db().add_goal(&item, "改了范围").unwrap();
    assert_eq!(write(), Judged::Stale("scope"));
    finished_turn(&k, &sid, "t2", steve_store::now());
    assert_eq!(write(), Judged::Stale("turn"));
    // the item moved to another session: the candidate's is not its current one
    k.session(&item, "idle");
    assert_eq!(write(), Judged::Stale("session"));
    steve_store::core_fixture::item(
        k.db().store(),
        &item,
        steve_store::core_fixture::ItemState::Cycle(2),
    );
    assert_eq!(write(), Judged::Stale("cycle"));
    // nothing was written, and the claim is still the pass's to give back
    assert!(facts.judgements(&item, 1).unwrap().is_empty());
    let now = facts.candidate(&c.id).unwrap().unwrap();
    assert_eq!(now.state, "judging");

    // unchanged, it is written once; a claim that is gone writes nothing
    let k = Kit::new();
    let (item, sid) = item_with_turn(&k, "做好了");
    let p = k
        .core
        .tx()
        .judge_prepare(steve_store::now())
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let facts = k.db().store().item_facts();
    let j = NewJudgement {
        item_id: &item,
        cycle: 1,
        session_id: &sid,
        turn_id: "t1",
        scope_revision: p.candidate.scope_revision,
        verdict: "done",
        basis: json!({}),
        ..Default::default()
    };
    assert_eq!(
        facts
            .candidate_judged(&p.candidate.id, "not-the-claim", &p.evidence, &j)
            .unwrap(),
        Judged::Lost
    );
    assert!(matches!(
        facts
            .candidate_judged(&p.candidate.id, &p.claim, &p.evidence, &j)
            .unwrap(),
        Judged::Written(_)
    ));
}

/// A stale write found by the transaction is handled as the checks before it: evidence that
/// changed while the model ran has the candidate judged again, counted as a failure.
#[tokio::test(flavor = "multi_thread")]
async fn evidence_that_changes_while_the_model_runs_is_judged_again() {
    use steve_store::item_facts::NewDeliveryFact;
    let k = Kit::new();
    let (item, sid) = item_with_turn(&k, "做好了");
    k.gw.set(|_, _| Ok(DONE.into()));
    let p = k
        .core
        .tx()
        .judge_prepare(steve_store::now())
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let called = k.core.judge_call(&p).await;
    k.db()
        .store()
        .item_facts()
        .record_delivery(&NewDeliveryFact {
            item_id: &item,
            cycle: 1,
            kind: "commit",
            head: Some("abc"),
            state: "pushed",
            provenance: "reported",
            source: "test",
            observed_at: steve_store::now(),
            ..Default::default()
        })
        .unwrap();
    assert!(k.core.tx().judge_commit(p, called).unwrap().is_none());
    let facts = k.db().store().item_facts();
    assert!(facts.judgements(&item, 1).unwrap().is_empty());
    let c = facts
        .candidate_of(&item, 1, &sid, "t1", 1)
        .unwrap()
        .unwrap();
    assert_eq!((c.state.as_str(), c.failures), ("pending", 1));
}

/// A one-shot runner whose harness names `reported` itself, whatever model was asked for.
struct Serves(Option<&'static str>);

#[async_trait::async_trait]
impl crate::gateway::OneModel for Serves {
    async fn once(
        &self,
        _: &crate::gateway::OneCall<'_>,
        _: &str,
        _: Option<&str>,
        _: Option<crate::gateway::OnText<'_>>,
    ) -> crate::gateway::Tried {
        crate::gateway::Tried {
            reply: Ok((DONE.into(), None)),
            usage: vec![],
            timed_out: false,
            reported_model: self.0.map(String::from),
        }
    }
}

/// Finding: the judgement recorded the model that was asked for. It records the harness and model
/// that reported back, and `unknown` for what did not.
#[tokio::test]
async fn the_judgement_records_who_reported_back_not_who_was_asked() {
    use crate::gateway::{Fallback, ModelGateway, ThinkTools, Tier};
    let asked = |one| Fallback {
        one,
        fast: vec![],
        smart: vec!["asked-model".into()],
        models: None,
    };
    let call = |g: Fallback<Serves>| async move {
        g.attempt(
            "q",
            Tier::Smart,
            std::time::Duration::from_secs(5),
            &ThinkTools::None,
            None,
        )
        .await
    };
    let a = call(asked(Serves(Some("served-model")))).await;
    let p = a.result.as_ref().unwrap();
    // booked under the model asked for, answered by the one the harness named
    assert_eq!(
        (p.model.as_str(), p.reported_model.as_deref()),
        ("asked-model", Some("served-model"))
    );
    let (harness, model) = crate::judge::answered_by(&a);
    assert_eq!(model, "served-model");
    assert!(!harness.is_empty() && harness != "unknown", "{harness}");
    // a harness that names no model: unknown, not the model asked for
    let a = call(asked(Serves(None))).await;
    assert_eq!(crate::judge::answered_by(&a).1, "unknown");
    // no answer: neither is known
    let failed = crate::gateway::Attempt {
        result: Err(crate::error::msg("503")),
        attempts: 1,
        duration_ms: 0,
        usage: vec![],
        model: "asked-model".into(),
        failures: vec![],
        harness: "h".into(),
    };
    assert_eq!(
        crate::judge::answered_by(&failed),
        ("unknown".to_string(), "unknown".to_string())
    );
    // which model reported is read from the harness's own usage reports, its main one first
    let report = |scope, model: Option<&str>| steve_node_api::UsageReport {
        report_id: format!("{scope:?}{model:?}"),
        model: model.map(String::from),
        meter_epoch: steve_node_api::Epoch::new("m").unwrap(),
        scope,
        measurement: steve_node_api::Measurement::Delta,
        tokens: Default::default(),
        context_used: None,
        context_limit: None,
        cost: None,
    };
    use steve_node_api::UsageScope::{All, Main};
    assert_eq!(
        crate::gateway::reported_model(&[
            report(All, Some("sum")),
            report(Main, Some("main")),
            report(Main, None)
        ]),
        Some("main".into())
    );
    assert_eq!(
        crate::gateway::reported_model(&[report(All, Some("sum"))]),
        Some("sum".into())
    );
    assert_eq!(crate::gateway::reported_model(&[report(Main, None)]), None);

    // end to end: the fake harness names no model, so the judgement says unknown
    let k = Kit::new();
    let (item, _) = item_with_turn(&k, "说明写好了");
    k.gw.set(|_, _| Ok(DONE.into()));
    *k.gw.reported.lock().unwrap() = None;
    assert_eq!(k.core.check_judges().await.unwrap(), 1);
    let j = &k.db().store().item_facts().judgements(&item, 1).unwrap()[0];
    assert_eq!(
        (
            j.harness.as_deref(),
            j.model.as_deref(),
            j.independent.as_deref()
        ),
        (Some("unknown"), Some("unknown"), Some("unknown"))
    );
    assert_eq!(
        independent(
            "unknown",
            "big",
            &Worker {
                harness: "unknown".into(),
                model: Some("big".into()),
            }
        ),
        "unknown"
    );
}

/// Finding: a retry within the same turn left the earlier attempt's words in the turn's events, and
/// the final text mixed them in. The text is the terminal attempt's own events only, as their Node
/// stream attributes them; when it cannot be told whose they are, it is unknown.
#[tokio::test(flavor = "multi_thread")]
async fn a_retried_turn_is_judged_on_its_last_attempts_own_words() {
    /// Turn `t1` ran twice: `t1-a` said something and failed, its retry `t1-b` finished.
    fn retried(k: &Kit) -> (String, String) {
        let item = k.item("run", "执行", None);
        let sid = k.session(&item, "idle");
        let at = steve_store::now() - 1_000;
        finished_turn(k, &sid, "t1", at);
        attempt_says(k, &sid, &attempt_of("t1"), "第一次的半截输出，");
        steve_store::core_fixture::runtime::finished_turn(
            k.db().store(),
            steve_store::core_fixture::runtime::FinishedTurn {
                session: &sid,
                turn: "t1",
                attempt: "t1-b",
                node: "node-a",
                node_session: "ns-1",
                ordinal: 1,
                operation: "op-t1-b",
                at,
            },
        );
        attempt_says(k, &sid, "t1-b", "重试后做好了");
        k.db()
            .store()
            .sessions()
            .stop_ended(&sid, Some("t1-b"), "end_turn", None, false)
            .unwrap();
        k.db()
            .store()
            .item_facts()
            .set_policy("settle_debounce", 0)
            .unwrap();
        (item, sid)
    }
    let prepared = |k: &Kit| {
        k.core
            .tx()
            .judge_prepare(steve_store::now())
            .unwrap()
            .into_iter()
            .next()
    };

    let k = Kit::new();
    let (_, sid) = retried(&k);
    // the session's text, attempt by attempt, would read both
    assert_eq!(
        k.db().final_reply(&sid).unwrap(),
        "第一次的半截输出，重试后做好了"
    );
    let p = prepared(&k).expect("judged by the model");
    assert_eq!(p.bundle.attempt.as_deref(), Some("t1-b"));
    assert_eq!(
        (p.bundle.final_text.as_str(), p.bundle.final_text_source),
        ("重试后做好了", "attempt")
    );

    // events the Hub cannot attribute to any attempt (not from a Node stream) are not the attempt's
    let k = Kit::new();
    let item = k.item("run", "执行", None);
    let sid = k.session(&item, "idle");
    finished_turn(&k, &sid, "t1", steve_store::now() - 1_000);
    k.rt.say(&sid, "谁说的不知道");
    ended(&k, &sid, "t1", "end_turn");
    k.db()
        .store()
        .item_facts()
        .set_policy("settle_debounce", 0)
        .unwrap();
    assert!(prepared(&k).is_none(), "no model call");
    let j = &k.db().store().item_facts().judgements(&item, 1).unwrap()[0];
    assert_eq!(j.basis["reason"], json!("reply_unknown"));
}

/// Re-review: words of the attempt's range that its Node stream does not attribute to it were read
/// as its words. Only confirmed text counts: mixed with unattributed assistant text the reply is
/// unknown, and when every text is the attempt's own, all of it (and nothing else) is read.
#[tokio::test(flavor = "multi_thread")]
async fn unattributed_words_in_the_attempts_range_make_its_reply_unknown() {
    /// Turn `t1` whose one attempt said `words`, each attributed (`true`) or not.
    fn said(k: &Kit, words: &[(&str, bool)]) -> (String, String) {
        let item = k.item("run", "执行", None);
        let sid = k.session(&item, "idle");
        finished_turn(k, &sid, "t1", steve_store::now() - 1_000);
        for (text, attributed) in words {
            if *attributed {
                attempt_says(k, &sid, &attempt_of("t1"), text);
            } else {
                k.rt.say(&sid, text);
            }
        }
        ended(k, &sid, "t1", "end_turn");
        k.db()
            .store()
            .item_facts()
            .set_policy("settle_debounce", 0)
            .unwrap();
        (item, sid)
    }

    // attributed and unattributed text in the same range: not known, and no model is asked
    let k = Kit::new();
    let (item, sid) = said(
        &k,
        &[
            ("第一句，", true),
            ("不知是谁说的，", false),
            ("最后一句", true),
        ],
    );
    let facts = k.db().store().item_facts();
    let ev = facts
        .attempt_events(&sid, "t1-a", i64::MAX)
        .unwrap()
        .unwrap();
    assert_eq!(
        ev.own.len(),
        2,
        "only the two attributed events are its own"
    );
    k.gw.set(|_, _| Ok(DONE.into()));
    assert_eq!(k.core.check_judges().await.unwrap(), 0);
    let j = &facts.judgements(&item, 1).unwrap()[0];
    assert_eq!(j.basis["reason"], json!("reply_unknown"));
    assert_eq!(j.basis["bundle"]["final_text_source"], json!("unknown"));
    assert_eq!(j.basis["bundle"]["final_text"], json!(""));
    assert!(k.gw.calls.lock().unwrap().is_empty());

    // every text attributed: exactly the attempt's own words
    let k = Kit::new();
    let (_, _) = said(&k, &[("第一句，", true), ("最后一句", true)]);
    let p = k
        .core
        .tx()
        .judge_prepare(steve_store::now())
        .unwrap()
        .into_iter()
        .next()
        .expect("judged by the model");
    assert_eq!(
        (p.bundle.final_text.as_str(), p.bundle.final_text_source),
        ("第一句，最后一句", "attempt")
    );
}

#[tokio::test]
async fn r7b_judge_watch_rereads_the_delay_after_each_pass_and_owns_its_stop() {
    use std::sync::Arc;
    use std::time::Duration;
    use steve_runtime::{Clock, FakeClock};
    struct Observed {
        clock: Arc<FakeClock>,
        waits: tokio::sync::mpsc::UnboundedSender<(i64, Duration)>,
    }
    impl Clock for Observed {
        fn now(&self) -> i64 {
            self.clock.now()
        }
        fn sleep(
            &self,
            duration: Duration,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
            let _ = self.waits.send((self.now(), duration));
            self.clock.sleep(duration)
        }
    }
    let k = Kit::new();
    k.db()
        .store()
        .item_facts()
        .set_policy("judge_every", 5000)
        .unwrap();
    let clock = Arc::new(FakeClock::new(0));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let m = steve_modules::Modules::with_clock(
        k.db().store().clone(),
        k.core.module_gateway(),
        steve_modules::Registry::new(),
        Arc::new(Observed {
            clock: clock.clone(),
            waits: tx,
        }),
    );
    k.core.attach_modules(m);
    let task = steve_runtime::spawn("test.judge.watch", k.core.clone().run_judge_watch());
    assert_eq!(rx.recv().await.unwrap(), (0, Duration::from_secs(4)));
    clock.advance(4000);
    assert_eq!(rx.recv().await.unwrap(), (4000, Duration::from_secs(1)));
    k.db()
        .store()
        .item_facts()
        .set_policy("judge_every", 0)
        .unwrap();
    clock.advance(1000);
    assert_eq!(rx.recv().await.unwrap(), (5000, Duration::ZERO));
    assert_eq!(rx.recv().await.unwrap(), (5000, Duration::from_secs(1)));
    task.stop();
    task.await;
    clock.advance(60_000);
    tokio::task::yield_now().await;
    assert!(
        rx.try_recv().is_err(),
        "the stopped watch cannot start another pass"
    );
}
