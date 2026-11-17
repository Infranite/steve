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
    let (b, k) = bundle(&f, required, "做好了");
    assert_eq!(
        ruled(&b, &k, true),
        Some(("done", "hard_facts_met", "facts"))
    );

    // the build still runs: waiting, not failed
    let mut f = facts();
    f.delivery.push(fact("pr", "merged", VERIFIED));
    f.delivery.push(fact("build", "running", VERIFIED));
    let (b, k) = bundle(&f, required, "做好了");
    assert_eq!(
        ruled(&b, &k, true),
        Some(("waiting", "hard_facts_underway", "facts"))
    );

    let mut f = facts();
    f.delivery.push(fact("pr", "merged", VERIFIED));
    f.delivery.push(fact("build", "failed", VERIFIED));
    let (b, k) = bundle(&f, required, "做好了");
    assert_eq!(
        ruled(&b, &k, true),
        Some(("not_done", "hard_facts_failed", "facts"))
    );

    // a hard criterion nobody can tell is left to the model, and an operation always is
    let mut f = facts();
    f.delivery.push(fact("pr", "merged", VERIFIED));
    let (b, k) = bundle(&f, required, "做好了");
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
    k.db()
        .store()
        .transaction(|tx| {
            tx.execute(
                "INSERT OR IGNORE INTO runtime_sessions(session_id,node_id,workspace_ref,node_session_id,config_revision,phase,updated)
                 VALUES (?,'node-a','ws','ns-1','r1','ready',?)",
                rusqlite::params![sid, at],
            )?;
            tx.execute(
                "INSERT INTO runtime_turns(turn_id,session_id,state,created,updated) VALUES (?,?,'finished',?,?)",
                rusqlite::params![turn, sid, at, at],
            )?;
            Ok::<_, steve_store::StoreError>(())
        })
        .unwrap();
}

/// An open item with an idle session whose turn `t1` is over and said `text`; the judge's debounce
/// is off so its candidate is due at once.
fn item_with_turn(k: &Kit, text: &str) -> (String, String) {
    let item = k.item("run", "执行", None);
    let sid = k.session(&item, "idle");
    finished_turn(k, &sid, "t1", steve_store::now() - 1_000);
    k.rt.say(&sid, text);
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
        "the model that answered"
    );
    assert!(j.harness.is_some());
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
    let v = k.core.shadow_view(&item).unwrap().unwrap();
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
    let v = k.core.shadow_view(&item).unwrap().unwrap();
    assert_eq!(v["judge"]["candidate"]["settled"], json!("busy"));

    // the card is answered and its time comes: now it is judged
    k.db()
        .decision_resolve(&d, json!({"choice": "a"}), "answered", None)
        .unwrap();
    k.db()
        .store()
        .transaction(|tx| {
            tx.execute("UPDATE item_judge_candidates SET next_check=0", [])?;
            Ok::<_, steve_store::StoreError>(())
        })
        .unwrap();
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
            k.db()
                .store()
                .transaction(|tx| {
                    tx.execute("UPDATE item_judge_candidates SET next_check=0", [])?;
                    Ok::<_, steve_store::StoreError>(())
                })
                .unwrap();
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
    k.db()
        .exec(
            "UPDATE items SET kind='chat' WHERE id=?",
            &[&chat as &dyn rusqlite::ToSql],
        )
        .unwrap();
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
