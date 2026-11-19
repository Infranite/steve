use rusqlite::params;
use serde_json::{Value, json};
use steve_store::{UnitSpec, WorkUnit, Workflow};

use crate::testkit::Kit;
use crate::{Check, ItemAction, Output};

fn setup(ready: bool) -> (Kit, String, Vec<UnitSpec>) {
    let k = Kit::new();
    k.rt.st().workflow_execution_ready = ready;
    let item = k.item("run", "执行", None);
    let spec = |id: &str, bot: &str, kind: &str, deps: &[&str], resources: &[&str]| UnitSpec {
        id: id.into(),
        title: id.into(),
        goal: format!("完成 {id}"),
        bot_id: bot.into(),
        kind: kind.into(),
        depends_on: deps.iter().map(|s| (*s).into()).collect(),
        resources: resources.iter().map(|s| (*s).into()).collect(),
        required: true,
    };
    let specs = vec![
        spec("a", "dev", "work", &[], &["src/a"]),
        spec("b", "dev", "work", &[], &["src/b"]),
        spec("review", "steve", "review", &["a", "b"], &[]),
        spec("integrate", "dev", "integrate", &["review"], &[]),
    ];
    (k, item, specs)
}

fn plan(k: &Kit, item: &str, specs: &[UnitSpec]) -> Value {
    k.core
        .workflow_change(
            item,
            json!({
                "action":"plan","reason":"拆分独立工作","expected_revision":0,
                "units":specs,"max_parallel":2,"max_attempts":3
            }),
            "me",
        )
        .unwrap()
}

fn graph(k: &Kit, item: &str) -> Workflow {
    k.db().store().workflows().get(item).unwrap().unwrap()
}

fn disabled_core(k: &Kit) -> crate::Core {
    let mut cfg = k.core.config().clone();
    cfg.dynamic_workflows = false;
    crate::Core::new(k.db().clone(), k.rt.clone(), k.gw.clone(), cfg)
}

#[test]
fn dynamic_workflows_default_off_and_stale_mutations_have_no_effect() {
    assert!(!crate::CoreConfig::for_home(std::path::Path::new("/unused")).dynamic_workflows);
    let k = Kit::with(|c| c.dynamic_workflows = false);
    let parent = k.item("run", "执行", None);
    for action in ["plan", "retry", "resume"] {
        assert!(
            k.core
                .workflow_change(&parent, json!({"action":action}), "me")
                .is_err()
        );
    }
    assert!(k.db().store().workflows().get(&parent).unwrap().is_none());
    assert!(k.rt.st().created.is_empty());
    // An ordinary item still starts without a graph or independent-review claim.
    let session = k.core.start_session(&parent, None, None, &[]).unwrap();
    assert_eq!(session.item_id.as_deref(), Some(parent.as_str()));
}

#[test]
fn disabling_preserves_graph_blocks_cached_actions_and_allows_explicit_takeover() {
    let (k, parent, specs) = setup(false);
    plan(&k, &parent, &specs);
    k.rt.st().workflow_execution_ready = true;
    let off = disabled_core(&k);
    let before = graph(&k, &parent);
    let parent_revision = k.db().item(&parent).unwrap().unwrap().revision;
    for action in ["plan", "retry", "resume"] {
        assert!(
            off.workflow_change(
                &parent,
                json!({
                    "action":action,"expected_revision":before.revision,"reason":"cached request",
                    "units":specs,"unit_id":"a"
                }),
                "me"
            )
            .is_err()
        );
        assert_eq!(graph(&k, &parent), before);
        assert_eq!(
            k.db().item(&parent).unwrap().unwrap().revision,
            parent_revision
        );
    }
    off.lifecycle_tick().unwrap();
    assert!(k.rt.st().created.is_empty());
    let view = off.workflow_get(&parent).unwrap();
    assert_eq!(view["enabled"], false);
    assert_eq!(view["execution_ready"], false);
    assert!(!view["blockers"].as_array().unwrap().is_empty());
    assert!(off.item_action(&parent, ItemAction::Finish, "me").is_err());
    off.workflow_change(
        &parent,
        json!({"action":"pause","expected_revision":before.revision,"reason":"take over"}),
        "me",
    )
    .unwrap();
    let paused = graph(&k, &parent);
    let args = json!({"action":"single_session","expected_revision":paused.revision,"reason":"explicit review waiver"});
    let sid = k.session(&parent, "idle");
    let s = k.db().session(&sid).unwrap().unwrap();
    assert!(off.tool_workflow(&s, args.clone()).is_err());
    off.workflow_change(&parent, args, "me").unwrap();
    assert!(graph(&k, &parent).single_session());
    assert!(
        off.tx()
            .workflow_blockers(&parent, true)
            .unwrap()
            .is_empty()
    );
    assert!(k.rt.st().created.is_empty());
}

#[test]
fn disabled_workflows_do_not_waive_unsettled_execution_or_revision_checks() {
    let (k, parent, specs) = setup(true);
    plan(&k, &parent, &specs);
    let off = disabled_core(&k);
    let rev = graph(&k, &parent).revision;
    off.workflow_change(
        &parent,
        json!({"action":"pause","expected_revision":rev,"reason":"recover"}),
        "me",
    )
    .unwrap();
    let paused = graph(&k, &parent);
    for revision in [rev, paused.revision] {
        assert!(
            off.workflow_change(
                &parent,
                json!({"action":"single_session","expected_revision":revision,"reason":"explicit"}),
                "me"
            )
            .is_err()
        );
    }
    assert!(!graph(&k, &parent).single_session());
    assert!(graph(&k, &parent).paused);
    assert!(off.item_action(&parent, ItemAction::Finish, "me").is_err());
}

#[test]
fn disabling_allows_inflight_results_to_settle_without_starting_successors() {
    let (k, parent, specs) = setup(true);
    plan(&k, &parent, &specs);
    let off = disabled_core(&k);
    for id in ["a", "b"] {
        let u = unit(&k, &parent, id);
        let s = k
            .db()
            .session(u.session_id.as_deref().unwrap())
            .unwrap()
            .unwrap();
        off.tx()
            .workflow_report(
                &s,
                "retained result",
                &[Output {
                    kind: "文件".into(),
                    title: "retained".into(),
                    location: Some(format!("{id}.txt")),
                    role: None,
                }],
                &[Check {
                    name: "unit check".into(),
                    passed: true,
                    operated: true,
                    scope: None,
                }],
                true,
            )
            .unwrap();
        k.rt.end_turn(&off, &s.id, "end_turn", None);
        assert_eq!(unit(&k, &parent, id).status, "succeeded");
    }
    off.lifecycle_tick().unwrap();
    assert_eq!(unit(&k, &parent, "review").status, "pending");
    assert_eq!(k.rt.st().created.len(), 2);
    assert!(off.item_action(&parent, ItemAction::Finish, "me").is_err());
}

#[test]
fn confirmed_workflow_activity_needs_the_current_applied_attempt_and_fresh_agent_event() {
    let k = Kit::new();
    let child = k.item("run", "执行", None);
    let sid = k.session(&child, "idle");
    let at = 2_000_000_i64;
    let store = k.db().store();
    store.with_conn(|c| {
        c.execute("INSERT INTO nodes(node_id,owner,name,pubkey,fingerprint,state,created,last_seen) VALUES ('n','me','node',zeroblob(32),'fingerprint','active',?1,?1)", [at])?;
        c.execute("INSERT INTO runtime_sessions(session_id,node_id,workspace_ref,node_session_id,config_revision,phase,active_attempt,updated) VALUES (?1,'n','{}',?1,'r','active','a_current',?2)", params![sid, at])?;
        c.execute("INSERT INTO runtime_turns(turn_id,session_id,state,active_attempt,created,updated) VALUES ('t_current',?1,'running','a_current',?2,?2)", params![sid, at])?;
        c.execute("INSERT INTO runtime_attempts(attempt_id,turn_id,ordinal,node_id,operation_id,execution,spec,digest,state,created,updated) VALUES ('a_current','t_current',0,'n','op','{}','{}','digest','running',?1,?1)", [at])?;
        c.execute("INSERT INTO node_streams(node_id,stream,epoch,durable,applied) VALUES ('n','stream','epoch',10,10)", [])?;
        Ok(())
    }).unwrap();
    let event = |seq: i64, received: i64, attempt: &str, body: Value| {
        let payload = json!({"subject":{"of":"execution","type":"turn","session":sid,"turn":"t_current","attempt":attempt},"body":body}).to_string();
        store.with_conn(|c| {
            c.execute("INSERT INTO node_event_inbox(node_id,stream,epoch,seq,event_id,payload,applied,received) VALUES ('n','stream','epoch',?1,?2,?3,1,?4)", params![seq, format!("event-{seq}"), payload, received])?;
            Ok(())
        }).unwrap();
    };
    event(
        1,
        at - 900_000,
        "a_current",
        json!({"type":"attempt_started"}),
    );
    event(
        2,
        at - 599_000,
        "a_current",
        json!({"type":"message_started","message":"assistant","block":"b","role":"assistant"}),
    );
    assert!(
        store
            .workflows()
            .confirmed_activity(&child, &sid, at)
            .unwrap()
            .is_some()
    );
    assert!(
        store
            .workflows()
            .confirmed_activity(&child, &sid, at + 1_000)
            .unwrap()
            .is_none(),
        "600 seconds is stale"
    );
    event(
        3,
        at + 1_001,
        "a_current",
        json!({"type":"message_started","message":"user","block":"b","role":"user"}),
    );
    event(
        4,
        at + 1_002,
        "a_retired",
        json!({"type":"tool_changed","tool":{}}),
    );
    assert!(
        store
            .workflows()
            .confirmed_activity(&child, &sid, at + 1_003)
            .unwrap()
            .is_none()
    );
    event(
        5,
        at + 1_004,
        "a_current",
        json!({"type":"tool_changed","tool":{}}),
    );
    assert!(
        store
            .workflows()
            .confirmed_activity(&child, &sid, at + 1_005)
            .unwrap()
            .is_some()
    );
    store
        .with_conn(|c| {
            c.execute("UPDATE nodes SET last_seen=0 WHERE node_id='n'", [])?;
            Ok(())
        })
        .unwrap();
    assert!(
        store
            .workflows()
            .confirmed_activity(&child, &sid, at + 1_005)
            .unwrap()
            .is_some(),
        "a connection timestamp is not a heartbeat"
    );
    store
        .with_conn(|c| {
            c.execute("UPDATE node_streams SET fault='gap' WHERE node_id='n'", [])?;
            Ok(())
        })
        .unwrap();
    assert!(
        store
            .workflows()
            .confirmed_activity(&child, &sid, at + 1_005)
            .unwrap()
            .is_none(),
        "a faulted event stream cannot confirm activity"
    );
    store
        .with_conn(|c| {
            c.execute("UPDATE node_streams SET fault=NULL WHERE node_id='n'", [])?;
            Ok(())
        })
        .unwrap();
    store
        .with_conn(|c| {
            c.execute(
                "UPDATE runtime_attempts SET cancel_requested=1 WHERE attempt_id='a_current'",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    assert!(
        store
            .workflows()
            .confirmed_activity(&child, &sid, at + 1_005)
            .unwrap()
            .is_none()
    );
    store
        .with_conn(|c| {
            c.execute(
                "UPDATE runtime_attempts SET cancel_requested=0 WHERE attempt_id='a_current'",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    event(
        6,
        at + 20_000,
        "a_current",
        json!({"type":"tool_changed","tool":{}}),
    );
    assert!(
        store
            .workflows()
            .confirmed_activity(&child, &sid, at + 10_000)
            .unwrap()
            .is_some(),
        "future evidence must not replace a valid earlier event"
    );
    store
        .with_conn(|c| {
            c.execute("DELETE FROM node_event_inbox WHERE seq=5", [])?;
            Ok(())
        })
        .unwrap();
    assert!(
        store
            .workflows()
            .confirmed_activity(&child, &sid, at + 10_000)
            .unwrap()
            .is_none(),
        "future evidence cannot confirm activity"
    );
}

#[test]
fn long_workflow_history_is_bounded_without_erasing_evidence() {
    let (k, parent, specs) = setup(false);
    plan(&k, &parent, &specs);
    let long_reason = "评审核对记录".repeat(1_000);
    for _ in 0..35 {
        k.db()
            .store()
            .workflows()
            .pause(&parent, true, &long_reason)
            .unwrap();
        k.db()
            .store()
            .workflows()
            .pause(&parent, false, "继续核对")
            .unwrap();
    }
    let full = graph(&k, &parent);
    let view = k.core.workflow_get(&parent).unwrap();
    assert_eq!(view["history_total"], json!(full.history.len()));
    assert_eq!(view["history"].as_array().unwrap().len(), 20);
    assert!(serde_json::to_vec(&view).unwrap().len() < 100_000);
    assert!(full.history.iter().any(|entry| entry.reason == long_reason));
    assert!(
        view["history"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["reason"].as_str().unwrap().contains("截断"))
    );
}

#[test]
fn permanent_provider_refusal_keeps_successes_and_needs_human_retry() {
    for (error, diagnosis) in [
        (
            "Internal error: API Error: the request was blocked by upstream usage policy",
            "需处理上游策略",
        ),
        (
            "Internal error: API Error: 400 the request was rejected as invalid",
            "没有评审结论",
        ),
    ] {
        let (k, parent, specs) = setup(true);
        plan(&k, &parent, &specs);
        finish(&k, &parent, "a");
        finish(&k, &parent, "b");
        let reviewer = unit(&k, &parent, "review");
        let sid = reviewer.session_id.unwrap();
        k.rt.end_turn(&k.core, &sid, "error", Some(error));
        let failed = unit(&k, &parent, "review");
        assert_eq!(failed.status, "failed");
        assert!(failed.summary.contains(diagnosis));
        assert_eq!(unit(&k, &parent, "a").status, "succeeded");
        assert_eq!(unit(&k, &parent, "b").status, "succeeded");
        let args = json!({"action":"retry", "unit_id":"review", "reason":"盲目重试", "expected_revision":graph(&k,&parent).revision});
        assert!(
            k.core
                .workflow_change(&parent, args.clone(), "dev")
                .unwrap_err()
                .to_string()
                .contains("用户确认")
        );
        assert_eq!(unit(&k, &parent, "review").status, "failed");
        k.core.workflow_change(&parent, args, "me").unwrap();
        assert_eq!(unit(&k, &parent, "review").attempt, 2);
        assert_eq!(unit(&k, &parent, "a").status, "succeeded");
    }
}

#[test]
fn older_raw_provider_failure_is_explained_without_rewriting_its_record() {
    let (k, parent, specs) = setup(true);
    plan(&k, &parent, &specs);
    finish(&k, &parent, "a");
    finish(&k, &parent, "b");
    let reviewer = unit(&k, &parent, "review");
    let session = k
        .db()
        .session(reviewer.session_id.as_deref().unwrap())
        .unwrap()
        .unwrap();
    let raw = "Internal error: API Error: 400 the request was rejected as invalid";
    k.core
        .tool_workflow(&session, json!({"action":"fail", "reason":raw}))
        .unwrap();
    assert_eq!(unit(&k, &parent, "review").summary, raw);
    let view = k.core.workflow_get(&parent).unwrap();
    let review = view["units"]
        .as_array()
        .unwrap()
        .iter()
        .find(|u| u["id"] == "review")
        .unwrap();
    assert!(review["summary"].as_str().unwrap().contains("没有评审结论"));
}

fn unit(k: &Kit, item: &str, id: &str) -> WorkUnit {
    graph(k, item)
        .units
        .into_iter()
        .find(|u| u.spec.id == id)
        .unwrap()
}

fn change(k: &Kit, item: &str, action: &str, id: Option<&str>) -> crate::Result<Value> {
    k.core.workflow_change(item, json!({
        "action":action,"reason":"调整工作","unit_id":id,"expected_revision":graph(k,item).revision
    }), "me")
}

fn report(k: &Kit, parent: &str, id: &str) -> crate::Result<()> {
    let u = unit(k, parent, id);
    let s = k
        .db()
        .session(u.session_id.as_deref().unwrap())
        .unwrap()
        .unwrap();
    k.core.tool_deliver(
        &s,
        &format!("{id} 已验证"),
        &[Output {
            kind: "文件".into(),
            title: format!("{id} 的成果"),
            location: Some(format!("{id}.txt")),
            role: None,
        }],
        &[Check {
            name: "检查和实际操作".into(),
            passed: true,
            operated: true,
            scope: None,
        }],
        &[],
    )
}

fn finish(k: &Kit, parent: &str, id: &str) {
    let s = unit(k, parent, id).session_id.unwrap();
    report(k, parent, id).unwrap();
    k.rt.end_turn(&k.core, &s, "end_turn", None);
}

fn parent_deliver(k: &Kit, parent: &str) -> crate::Result<()> {
    k.manual_acceptance();
    let sid = k.session(parent, "idle");
    let s = k.db().session(&sid)?.unwrap();
    k.core.tool_deliver(
        &s,
        "单会话实际核验完成，未做独立评审",
        &[Output {
            kind: "文件".into(),
            title: "已核验成果".into(),
            location: Some("result.txt".into()),
            role: None,
        }],
        &[Check {
            name: "实际操作通过".into(),
            passed: true,
            operated: true,
            scope: None,
        }],
        &[],
    )
}

#[test]
fn cancelled_graph_requires_explicit_human_single_session_acceptance() {
    let (k, parent, specs) = setup(false);
    plan(&k, &parent, &specs);
    change(&k, &parent, "pause", None).unwrap();
    for u in &specs {
        change(&k, &parent, "cancel", Some(&u.id)).unwrap();
    }
    let before = graph(&k, &parent);
    assert!(parent_deliver(&k, &parent).is_err());
    assert!(k.finish(&parent).is_err());
    let sid = k.session(&parent, "idle");
    let s = k.db().session(&sid).unwrap().unwrap();
    let args = json!({"action":"single_session", "reason":"用户确认本次单会话验收，未做独立评审", "expected_revision":before.revision});
    let err = k.core.tool_workflow(&s, args.clone()).unwrap_err();
    assert!(err.to_string().contains("Bot 不能代为批准"));
    assert_eq!(graph(&k, &parent), before);
    let mut stale = args.clone();
    stale["expected_revision"] = json!(before.revision - 1);
    assert!(k.core.workflow_change(&parent, stale, "me").is_err());
    assert_eq!(graph(&k, &parent), before);
    k.core.workflow_change(&parent, args, "me").unwrap();
    let saved = graph(&k, &parent);
    assert_ne!(
        k.phase(&parent),
        "closed",
        "changing mode alone does not accept the item"
    );
    assert!(saved.single_session() && saved.paused);
    assert!(
        saved
            .units
            .iter()
            .all(|u| u.status == "cancelled" && u.spec.required)
    );
    assert_eq!(saved.history.last().unwrap().detail["actor"], "me");
    assert_eq!(saved.history.last().unwrap().action, "single_session");
    assert_eq!(
        saved.units, before.units,
        "cancelled results are not rewritten as success"
    );
    assert!(
        k.core.workflow_get(&parent).unwrap()["blockers"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        k.core.workflow_get(&parent).unwrap()["execution_ready"],
        false
    );
    assert_eq!(
        k.core.workflow_get(&parent).unwrap()["acceptance_mode"],
        "single_session"
    );
    change(&k, &parent, "single_session", None).unwrap();
    assert_eq!(
        graph(&k, &parent),
        saved,
        "repeat confirmation is idempotent"
    );
    assert!(change(&k, &parent, "resume", None).is_err());
    assert!(change(&k, &parent, "retry", Some("a")).is_err());
    k.core.lifecycle_tick().unwrap();
    assert!(k.rt.st().created.is_empty(), "no Node isolation fallback");
    parent_deliver(&k, &parent).unwrap();
    assert!(k.timeline_texts(&parent).join("\n").contains("单会话验收"));
    assert_ne!(
        k.phase(&parent),
        "closed",
        "mode change does not auto-accept delivery"
    );
    k.finish(&parent).unwrap();
    assert_eq!(k.phase(&parent), "closed");
}

#[test]
fn single_session_cancels_unfinished_units_and_replan_restores_review_gate() {
    let (k, parent, specs) = setup(false);
    plan(&k, &parent, &specs);
    assert!(
        change(&k, &parent, "single_session", None).is_err(),
        "must pause first"
    );
    change(&k, &parent, "pause", None).unwrap();
    change(&k, &parent, "single_session", None).unwrap();
    assert!(
        graph(&k, &parent)
            .units
            .iter()
            .all(|u| u.status == "cancelled")
    );
    assert!(parent_deliver(&k, &parent).is_ok());
    let g = graph(&k, &parent);
    k.core
        .workflow_change(
            &parent,
            json!({"action":"plan", "expected_revision":g.revision,
        "reason":"恢复独立评审要求", "units":specs}),
            "me",
        )
        .unwrap();
    assert!(!graph(&k, &parent).single_session());
    assert!(parent_deliver(&k, &parent).is_err());
    assert!(k.finish(&parent).is_err());
}

#[test]
fn single_session_never_ignores_unsettled_cancelled_execution() {
    for unsettled in ["running", "uncertain", "queued"] {
        let (k, parent, specs) = setup(true);
        plan(&k, &parent, &specs);
        change(&k, &parent, "pause", None).unwrap();
        for u in &specs {
            change(&k, &parent, "cancel", Some(&u.id)).unwrap();
        }
        let a = unit(&k, &parent, "a");
        let sid = a.session_id.as_deref().unwrap();
        // Cancel does not establish that the Node stopped. Settle b; leave exactly one blocker.
        {
            let mut rt = k.rt.st();
            rt.turning.clear();
            rt.attempts.clear();
            if unsettled == "running" {
                rt.turning.insert(sid.into());
            }
            if unsettled == "uncertain" {
                rt.uncertain.insert(sid.into());
            }
        }
        if unsettled == "queued" {
            assert!(k.db().queue_add(sid, "未结清输入", "me", &[]).is_err());
            // Preserve the defensive legacy-state check without using a current API to
            // enqueue new input on an ended session.
            k.db().exec("INSERT INTO queued(id,session_id,text,via,created) VALUES ('legacy-unsettled',?,'未结清输入','me',1)", &[&sid]).unwrap();
        }
        let before = graph(&k, &parent);
        assert!(
            change(&k, &parent, "single_session", None)
                .unwrap_err()
                .to_string()
                .contains("仍未确认结束")
        );
        assert_eq!(graph(&k, &parent), before);
        assert!(parent_deliver(&k, &parent).is_err());
        {
            let mut rt = k.rt.st();
            rt.turning.clear();
            rt.uncertain.clear();
        }
        k.db().queue_take(sid).unwrap();
        change(&k, &parent, "single_session", None).unwrap();
        assert!(parent_deliver(&k, &parent).is_ok());
        // A newly reported uncertain execution still blocks close even after mode confirmation.
        k.rt.st().uncertain.insert(sid.into());
        assert!(k.finish(&parent).is_err());
    }
}

#[test]
fn single_session_confirmation_cannot_waive_a_new_goal_or_cycle() {
    for column in ["cycle", "goal"] {
        let (k, parent, specs) = setup(false);
        plan(&k, &parent, &specs);
        change(&k, &parent, "pause", None).unwrap();
        change(&k, &parent, "single_session", None).unwrap();
        if column == "cycle" {
            k.db()
                .exec("UPDATE items SET cycle=cycle+1 WHERE id=?", &[&parent])
                .unwrap();
        } else {
            k.db()
                .exec(
                    "UPDATE workflows SET goal_version=goal_version-1 WHERE item_id=?",
                    &[&parent],
                )
                .unwrap();
        }
        assert!(parent_deliver(&k, &parent).is_err());
        assert!(k.finish(&parent).is_err());
        assert!(change(&k, &parent, "single_session", None).is_err());
        assert!(change(&k, &parent, "resume", None).is_err());
        assert!(change(&k, &parent, "cancel", Some("a")).is_err());
        let view = k.core.workflow_get(&parent).unwrap();
        assert_eq!(view["current_cycle"], false);
        assert!(
            view["blockers"]
                .as_array()
                .unwrap()
                .iter()
                .any(|s| s.as_str().unwrap().contains("周期"))
        );
    }
}

#[test]
fn a_new_cycle_replans_without_reusing_old_successes() {
    let (k, parent, specs) = setup(true);
    plan(&k, &parent, &specs);
    for id in ["a", "b", "review", "integrate"] {
        finish(&k, &parent, id);
    }
    let old_review = unit(&k, &parent, "review");
    let old_session = k
        .db()
        .session(old_review.session_id.as_deref().unwrap())
        .unwrap()
        .unwrap();
    change(&k, &parent, "pause", None).unwrap();
    k.db()
        .exec("UPDATE items SET cycle=cycle+1 WHERE id=?", &[&parent])
        .unwrap();

    let result = k
        .core
        .workflow_change(
            &parent,
            json!({
                "action":"plan", "reason":"新周期重新验收，旧成果只留作历史",
                "expected_revision":graph(&k,&parent).revision, "units":specs,
                "max_parallel":2, "max_attempts":3
            }),
            "me",
        )
        .unwrap();
    assert_eq!(result["current_cycle"], true);
    assert_eq!(result["cycle"], 2);
    assert_eq!(result["paused"], true);
    for u in &graph(&k, &parent).units {
        assert_eq!(u.status, "pending");
        assert_eq!(u.attempt, 0);
        assert!(u.item_id.is_none());
    }
    assert_eq!(result["history_attempts_total"], 4);
    assert!(
        result["history_attempts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|u| u["unit_id"] == "review" && u["snapshot"]["status"] == "succeeded")
    );
    assert!(
        k.core
            .tool_deliver(&old_session, "过期结果", &[], &[], &[])
            .is_err()
    );
    assert!(
        parent_deliver(&k, &parent).is_err(),
        "old successes must not accept the new cycle"
    );
    change(&k, &parent, "resume", None).unwrap();
    assert_eq!(unit(&k, &parent, "a").status, "running");
}

#[test]
fn no_confined_node_plans_but_never_creates_or_runs_children() {
    let (k, parent, specs) = setup(false);
    let v = plan(&k, &parent, &specs);
    assert_eq!(v["execution_ready"], false);
    assert!(
        v["blockers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s.as_str().unwrap().contains("隔离"))
    );
    k.core.lifecycle_tick().unwrap();
    // Planning, pausing and cancelling remain useful before an isolated Node is
    // available. Requests to execute must leave this saved state intact.
    change(&k, &parent, "pause", None).unwrap();
    change(&k, &parent, "cancel", Some("a")).unwrap();
    let blocked = graph(&k, &parent);
    let parent_revision = k.db().item(&parent).unwrap().unwrap().revision;
    assert!(blocked.paused);
    for (action, unit_id) in [("resume", None), ("retry", Some("a"))] {
        let error = change(&k, &parent, action, unit_id).unwrap_err();
        assert!(
            matches!(&error, crate::CoreError::Status(409, reason) if reason.contains("隔离")),
            "{action}: {error}"
        );
        assert_eq!(
            graph(&k, &parent),
            blocked,
            "{action} changed the work graph"
        );
        assert_eq!(
            k.db().item(&parent).unwrap().unwrap().revision,
            parent_revision,
            "{action} changed the parent revision"
        );
    }
    assert!(graph(&k, &parent).units.iter().all(|u| u.item_id.is_none()));
    assert!(k.rt.st().created.is_empty());
    assert!(k.finish(&parent).is_err());
    assert_ne!(k.phase(&parent), "closed");
}

#[test]
fn parallel_work_waits_for_turn_end_review_and_integration_before_parent_can_close() {
    let (k, parent, specs) = setup(true);
    plan(&k, &parent, &specs);
    assert_eq!(k.rt.st().turning.len(), 2);
    assert_eq!(unit(&k, &parent, "review").status, "pending");
    let a = unit(&k, &parent, "a");
    let b = unit(&k, &parent, "b");
    assert_ne!(a.item_id, b.item_id);
    let acwd = k
        .db()
        .session(a.session_id.as_deref().unwrap())
        .unwrap()
        .unwrap()
        .cwd;
    let bcwd = k
        .db()
        .session(b.session_id.as_deref().unwrap())
        .unwrap()
        .unwrap()
        .cwd;
    assert_ne!(acwd, bcwd);
    report(&k, &parent, "a").unwrap();
    finish(&k, &parent, "b");
    assert_eq!(unit(&k, &parent, "review").status, "pending");
    assert!(k.finish(&parent).is_err());
    k.rt.end_turn(&k.core, a.session_id.as_deref().unwrap(), "end_turn", None);
    let review = unit(&k, &parent, "review");
    assert_eq!(review.status, "running");
    let prompt =
        k.rt.prompts_of(review.session_id.as_deref().unwrap())
            .join("\n");
    assert!(prompt.contains("a 的成果") && prompt.contains("b 的成果"));
    assert!(k.finish(&parent).is_err());
    finish(&k, &parent, "review");
    assert!(k.finish(&parent).is_err());
    finish(&k, &parent, "integrate");
    assert!(
        k.core.workflow_get(&parent).unwrap()["blockers"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    k.finish(&parent).unwrap();
    assert_eq!(k.phase(&parent), "closed");
}

#[test]
fn conflicting_resources_serialize_and_attempt_budget_is_enforced() {
    let (k, parent, mut specs) = setup(true);
    specs[1].resources = specs[0].resources.clone();
    plan(&k, &parent, &specs);
    assert_eq!(k.rt.st().turning.len(), 1);
    assert_eq!(unit(&k, &parent, "b").status, "pending");
    finish(&k, &parent, "a");
    assert_eq!(unit(&k, &parent, "b").status, "running");
    let b = unit(&k, &parent, "b");
    k.rt.end_turn(
        &k.core,
        b.session_id.as_deref().unwrap(),
        "error",
        Some("检查失败"),
    );
    assert_eq!(unit(&k, &parent, "b").status, "failed");
    for _ in 0..2 {
        change(&k, &parent, "retry", Some("b")).unwrap();
        let b = unit(&k, &parent, "b");
        k.rt.end_turn(
            &k.core,
            b.session_id.as_deref().unwrap(),
            "error",
            Some("检查失败"),
        );
    }
    assert!(change(&k, &parent, "retry", Some("b")).is_err());
    assert_eq!(unit(&k, &parent, "b").attempt, 3);
}

#[test]
fn stale_replans_and_stale_reports_cannot_change_results_or_publish() {
    let (k, parent, specs) = setup(true);
    plan(&k, &parent, &specs);
    assert!(
        k.core
            .workflow_change(
                &parent,
                json!({
                    "action":"plan","reason":"旧计划","expected_revision":1,"units":specs
                }),
                "me"
            )
            .is_err()
    );
    let a = unit(&k, &parent, "a");
    let old = k
        .db()
        .session(a.session_id.as_deref().unwrap())
        .unwrap()
        .unwrap();
    k.rt.end_turn(&k.core, &old.id, "error", Some("失败"));
    finish(&k, &parent, "b");
    change(&k, &parent, "retry", Some("a")).unwrap();
    assert_ne!(
        unit(&k, &parent, "a").session_id.as_deref(),
        Some(old.id.as_str())
    );
    assert!(k.core.tool_deliver(&old, "旧交付", &[], &[], &[]).is_err());
    assert!(k.core.tool_publish(&old, "branch", "旧发布").is_err());
    assert!(
        k.core
            .tool_workflow(&old, json!({"action":"plan","units":specs}))
            .is_err()
    );
}

#[test]
fn review_failure_blocks_integration_and_child_token_cannot_replan_or_read_siblings() {
    let (k, parent, specs) = setup(true);
    plan(&k, &parent, &specs);
    let a = unit(&k, &parent, "a");
    let s = k
        .db()
        .session(a.session_id.as_deref().unwrap())
        .unwrap()
        .unwrap();
    let context = k.core.tool_workflow(&s, json!({"action":"get"})).unwrap();
    assert!(context.get("units").is_none());
    assert!(context["upstream"].as_array().unwrap().is_empty());
    assert!(
        k.core
            .tool_workflow(&s, json!({"action":"pause","reason":"越权"}))
            .is_err()
    );
    finish(&k, &parent, "a");
    finish(&k, &parent, "b");
    let review = unit(&k, &parent, "review");
    let s = k
        .db()
        .session(review.session_id.as_deref().unwrap())
        .unwrap()
        .unwrap();
    let out = k
        .core
        .tool_workflow(&s, json!({"action":"fail","reason":"缺少关键回归"}))
        .unwrap();
    assert_eq!(out["reported"], true);
    assert!(out.get("units").is_none());
    k.rt.end_turn(&k.core, &s.id, "end_turn", None);
    let child = review.item_id.as_deref().unwrap();
    assert_eq!(k.phase(child), "closed");
    assert_eq!(k.status(child).0, "fail");
    assert_eq!(
        k.db().item(child).unwrap().unwrap().outcome.as_deref(),
        Some("failed")
    );
    assert_eq!(unit(&k, &parent, "integrate").status, "pending");
    assert!(k.finish(&parent).is_err());
}

#[test]
fn a_work_unit_that_stops_without_delivery_closes_its_failed_child() {
    let (k, parent, specs) = setup(true);
    plan(&k, &parent, &specs);
    let work = unit(&k, &parent, "a");
    let child = work.item_id.as_deref().unwrap();
    let session = work.session_id.as_deref().unwrap();
    k.rt.end_turn(&k.core, session, "error", Some("执行失败"));
    assert_eq!(unit(&k, &parent, "a").status, "failed");
    assert_eq!(k.phase(child), "closed");
    assert_eq!(k.status(child).0, "fail");
    assert_eq!(
        k.db().item(child).unwrap().unwrap().outcome.as_deref(),
        Some("failed")
    );
    assert!(k.finish(&parent).is_err());
}

#[test]
fn restart_finishes_a_failed_child_after_its_turn_has_ended() {
    let (k, parent, specs) = setup(true);
    plan(&k, &parent, &specs);
    let work = unit(&k, &parent, "a");
    let child = work.item_id.unwrap();
    let sid = work.session_id.unwrap();
    let s = k.db().session(&sid).unwrap().unwrap();
    k.core
        .tool_workflow(&s, json!({"action":"fail","reason":"历史评审未通过"}))
        .unwrap();
    assert_eq!(unit(&k, &parent, "a").status, "failed");
    assert_eq!(k.phase(&child), "active");

    // A live turn still owns the child even when the failure has already been recorded.
    k.core.tx().workflow_tick().unwrap();
    assert_eq!(k.phase(&child), "active");

    // The old Hub stopped after persisting turn end, before it closed the failed child.
    k.rt.st().turning.remove(&sid);
    k.db()
        .session_patch(&sid, crate::patch![status: "idle"])
        .unwrap();
    k.db()
        .queue_add(&sid, "失败前排队的输入", "me", &[])
        .unwrap();
    k.core.tx().workflow_tick().unwrap();
    assert_eq!(k.phase(&child), "closed");
    assert_eq!(k.status(&child).0, "fail");
    assert_eq!(
        k.db().item(&child).unwrap().unwrap().outcome.as_deref(),
        Some("failed")
    );
    k.core.tx().workflow_tick().unwrap();
    assert_eq!(k.phase(&child), "closed");
}

/// #182: a reset unit keeps its child. Its failed attempt's cycle closes once its turn is over (a
/// queued input does not keep it), then the retry runs in a new cycle and session of the same child.
#[test]
fn a_reset_units_failed_attempt_closes_after_its_turn_and_the_retry_reuses_the_child() {
    let (k, parent, specs) = setup(true);
    plan(&k, &parent, &specs);
    let work = unit(&k, &parent, "a");
    let child = work.item_id.unwrap();
    let sid = work.session_id.unwrap();
    let s = k.db().session(&sid).unwrap().unwrap();
    k.core
        .tool_workflow(&s, json!({"action":"fail","reason":"旧评审未通过"}))
        .unwrap();
    k.db()
        .store()
        .workflows()
        .reset(&parent, "a", "重试旧结果")
        .unwrap();
    assert_eq!(
        unit(&k, &parent, "a").item_id.as_deref(),
        Some(child.as_str())
    );
    let history = k.core.workflow_get(&parent).unwrap();
    assert_eq!(history["history_attempts_total"], 1);
    assert_eq!(history["history_attempts"][0]["item_id"], child);
    assert_eq!(
        history["history_attempts"][0]["snapshot"]["session_id"],
        sid
    );
    // a failure is that attempt's outcome: it stays failed in the archive (ST-10)
    assert_eq!(
        history["history_attempts"][0]["snapshot"]["status"],
        "failed"
    );
    assert!(
        history["history_attempts"][0]["snapshot"]
            .get("outputs")
            .is_none(),
        "history view stays compact"
    );

    k.core.tx().workflow_tick().unwrap();
    assert_eq!(k.phase(&child), "active", "live turn still owns its child");
    assert_eq!(
        unit(&k, &parent, "a").status,
        "pending",
        "the retry waits for it"
    );
    k.rt.st().turning.remove(&sid);
    k.db()
        .session_patch(&sid, crate::patch![status: "idle"])
        .unwrap();
    k.db()
        .queue_add(&sid, "失败前排队的输入", "me", &[])
        .unwrap();
    k.core.tx().workflow_tick().unwrap();
    let cycles = k.db().store().items().cycles(&child).unwrap();
    assert_eq!(
        cycles
            .iter()
            .map(|c| (c.cycle, c.outcome.as_str()))
            .collect::<Vec<_>>(),
        vec![(1, "failed")]
    );
    let retry = unit(&k, &parent, "a");
    assert_eq!(
        (
            retry.status.as_str(),
            retry.attempt,
            retry.item_id.as_deref()
        ),
        ("running", 2, Some(child.as_str()))
    );
    assert_ne!(retry.session_id.as_deref(), Some(sid.as_str()));
    assert_eq!(
        k.db().session(&sid).unwrap().unwrap().status,
        "ended",
        "the earlier attempt's session is over"
    );
    let i = k.db().item(&child).unwrap().unwrap();
    assert_eq!((i.phase.as_str(), i.cycle), ("active", 2));
    k.core.tx().workflow_tick().unwrap();
    assert_eq!(unit(&k, &parent, "a").attempt, 2, "one retry, not more");
}

/// #182: a cancelled attempt of a reset unit closes as cancelled, and the retry reuses its child.
#[test]
fn a_reset_units_cancelled_attempt_closes_cancelled_and_the_retry_reuses_the_child() {
    let (k, parent, specs) = setup(true);
    plan(&k, &parent, &specs);
    let work = unit(&k, &parent, "b");
    let child = work.item_id.unwrap();
    let sid = work.session_id.unwrap();
    k.db()
        .store()
        .workflows()
        .cancel(&parent, "b", "换一轮执行")
        .unwrap();
    k.db()
        .store()
        .workflows()
        .reset(&parent, "b", "换一轮执行")
        .unwrap();
    k.rt.st().turning.remove(&sid);
    k.db()
        .session_patch(&sid, crate::patch![status: "idle"])
        .unwrap();
    k.core.tx().workflow_tick().unwrap();
    let cycles = k.db().store().items().cycles(&child).unwrap();
    assert_eq!(
        cycles
            .iter()
            .map(|c| c.outcome.as_str())
            .collect::<Vec<_>>(),
        vec!["cancelled"]
    );
    let retry = unit(&k, &parent, "b");
    assert_eq!(
        (retry.attempt, retry.item_id.as_deref()),
        (2, Some(child.as_str()))
    );
}

/// #182: a child a retry will run in keeps its worktree while the unit may run again; a unit that
/// succeeded, ran out of attempts or whose graph is no longer current lets it be tidied.
#[test]
fn a_child_awaiting_its_retry_is_kept_for_it() {
    let (k, parent, specs) = setup(true);
    plan(&k, &parent, &specs);
    let work = unit(&k, &parent, "a");
    let child = work.item_id.clone().unwrap();
    let sid = work.session_id.unwrap();
    k.rt.end_turn(&k.core, &sid, "error", Some("检查失败"));
    assert_eq!(k.phase(&child), "closed");
    let i = k.db().item(&child).unwrap().unwrap();
    assert_eq!(unit(&k, &parent, "a").status, "failed");
    assert!(
        k.core.tx().workflow_retry_keeps(&i).unwrap(),
        "attempts left"
    );
    let g = graph(&k, &parent);
    k.db()
        .store()
        .workflows()
        .replace(&parent, g.revision, "收紧预算", &specs, 2, 1)
        .unwrap();
    assert!(
        !k.core.tx().workflow_retry_keeps(&i).unwrap(),
        "no attempt left: tidied as any closed item"
    );
}

#[test]
fn lifecycle_gate_blocks_even_direct_completion_request_and_no_release_is_started() {
    let (k, parent, specs) = setup(false);
    plan(&k, &parent, &specs);
    k.core
        .tx()
        .lc_request_complete(
            &parent,
            "me",
            crate::cycle::Policy::Waived,
            true,
            "直接完成",
            None,
            vec![],
        )
        .unwrap();
    k.core.lifecycle_tick().unwrap();
    assert_eq!(k.phase(&parent), "closing");
    assert!(
        k.core
            .lifecycle_view(&parent)
            .unwrap()
            .to_string()
            .contains("operation")
    );
    assert!(
        k.db()
            .store()
            .items()
            .operations(&parent, 1)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn cancelled_required_work_is_not_completion_and_cancellation_reaches_children() {
    let (k, parent, specs) = setup(true);
    plan(&k, &parent, &specs);
    let a = unit(&k, &parent, "a");
    change(&k, &parent, "cancel", Some("a")).unwrap();
    assert!(k.rt.st().cancelled.contains(a.session_id.as_ref().unwrap()));
    assert!(k.finish(&parent).is_err());
    k.core.item_action(&parent, ItemAction::Drop, "me").unwrap();
    assert_ne!(k.phase(&parent), "closed");
    for u in graph(&k, &parent).units {
        if let Some(s) = u.session_id {
            k.rt.end_turn(&k.core, &s, "cancelled", None);
        }
    }
    k.core.lifecycle_tick().unwrap();
    assert_eq!(k.phase(&parent), "closed");
    assert_eq!(
        k.db().item(&parent).unwrap().unwrap().outcome.as_deref(),
        Some("cancelled")
    );
}

#[test]
fn delivery_and_child_closing_are_atomic_and_repeated_delivery_has_no_effects() {
    let (k, parent, specs) = setup(true);
    plan(&k, &parent, &specs);
    let a = unit(&k, &parent, "a");
    k.db()
        .store()
        .with_conn(|c| {
            c.execute_batch(
                "CREATE TRIGGER fail_workflow_delivery BEFORE INSERT ON workflow_history
            WHEN NEW.action='deliver' BEGIN SELECT RAISE(ABORT,'simulated failure'); END;",
            )?;
            Ok(())
        })
        .unwrap();
    assert!(report(&k, &parent, "a").is_err());
    assert_eq!(unit(&k, &parent, "a").status, "running");
    assert_eq!(k.phase(a.item_id.as_deref().unwrap()), "active");
    assert!(
        k.db()
            .store()
            .artifacts()
            .list(a.item_id.as_deref().unwrap())
            .unwrap()
            .is_empty()
    );
    k.db()
        .exec("DROP TRIGGER fail_workflow_delivery", &[])
        .unwrap();
    report(&k, &parent, "a").unwrap();
    assert_eq!(k.phase(a.item_id.as_deref().unwrap()), "closing");
    let revision = graph(&k, &parent).revision;
    let child = k.db().item(a.item_id.as_deref().unwrap()).unwrap().unwrap();
    report(&k, &parent, "a").unwrap();
    assert_eq!(graph(&k, &parent).revision, revision);
    assert_eq!(
        k.db().item(&child.id).unwrap().unwrap().revision,
        child.revision
    );
    assert_eq!(k.db().store().artifacts().list(&child.id).unwrap().len(), 1);
    // The in-memory marks disappear, but the frozen intent and graph result survive together.
    let restarted = crate::Core::new(
        k.db().clone(),
        k.rt.clone(),
        k.gw.clone(),
        k.core.config().clone(),
    );
    k.rt.end_turn(
        &restarted,
        a.session_id.as_deref().unwrap(),
        "end_turn",
        None,
    );
    assert_eq!(k.phase(&child.id), "closed");
}

#[test]
fn persisted_pause_intent_is_reconciled_after_losing_in_memory_cancels() {
    let (k, parent, specs) = setup(true);
    plan(&k, &parent, &specs);
    k.db()
        .store()
        .workflows()
        .pause(&parent, true, "模拟保存后崩溃")
        .unwrap();
    assert!(k.rt.st().cancelled.is_empty());
    let restarted = disabled_core(&k);
    let turns: Vec<_> = graph(&k, &parent)
        .units
        .into_iter()
        .filter(|u| u.status == "running")
        .map(|u| {
            let session = u.session_id.unwrap();
            let attempt = k.rt.st().attempts[&session].clone();
            (session, attempt)
        })
        .collect();
    assert_eq!(turns.len(), 2, "both independent children were running");
    // Tx::drop dispatches stops outside the core lock. A tick only schedules them; it cannot
    // promise that the runtime has received them yet, regardless of the platform's scheduling.
    let gate = k.rt.stop_gate.write().unwrap();
    restarted.lifecycle_tick().unwrap();
    assert!(k.rt.st().cancelled.is_empty());
    assert!(
        restarted
            .item_action(&parent, ItemAction::Finish, "me")
            .is_err()
    );
    drop(gate);
    assert!(
        k.wait(|| {
            let st = k.rt.st();
            turns.iter().all(|(session, attempt)| {
                st.stops.iter().any(|(turn, result)| {
                    &turn.session == session
                        && &turn.attempt == attempt
                        && *result == crate::Stop::Requested
                })
            })
        }),
        "the restarted core must send a stop for every persisted running attempt"
    );
    // A cancellation request is still not proof that either execution has stopped.
    assert_eq!(k.rt.st().turning.len(), 2);
    assert!(
        restarted
            .item_action(&parent, ItemAction::Finish, "me")
            .is_err()
    );
}

#[tokio::test]
async fn an_old_graph_acceptance_cannot_start_wrapup_hooks_after_a_replan_crash() {
    struct Hook;
    #[async_trait::async_trait]
    impl steve_modules::hooks::ItemHooks for Hook {
        fn name(&self) -> &'static str {
            "workflow-review"
        }
    }
    let (k, parent, specs) = setup(true);
    plan(&k, &parent, &specs);
    for id in ["a", "b", "review", "integrate"] {
        finish(&k, &parent, id);
    }
    // A running coordinator holds the close. Simulate a committed graph edit followed by a crash
    // before Core invalidates the old acceptance.
    k.session(&parent, "running");
    k.core
        .tx()
        .lc_request_complete(
            &parent,
            "me",
            crate::cycle::Policy::Automatic,
            true,
            "已验收旧图",
            None,
            vec![],
        )
        .unwrap();
    let g = graph(&k, &parent);
    k.db()
        .store()
        .workflows()
        .replace(&parent, g.revision, "增加预算", &specs, 2, 4)
        .unwrap();
    let mut registry = steve_modules::Registry::new();
    registry.item_hooks(Box::new(Hook));
    k.core.attach_modules(steve_modules::Modules::new(
        k.db().store().clone(),
        k.core.module_gateway(),
        registry,
    ));
    k.core.lifecycle_tick().unwrap();
    assert!(
        k.db()
            .store()
            .items()
            .operations(&parent, 1)
            .unwrap()
            .is_empty(),
        "stale acceptance must not authorize hooks before the changed gate invalidates it"
    );
    assert_ne!(k.phase(&parent), "closed");
}

#[test]
fn a_plan_that_cannot_run_anything_at_once_is_said_back() {
    let u = |id: &str, deps: &[&str], res: &[&str]| UnitSpec {
        id: id.into(),
        title: id.into(),
        goal: id.into(),
        bot_id: "dev".into(),
        kind: "work".into(),
        depends_on: deps.iter().map(|s| (*s).into()).collect(),
        resources: res.iter().map(|s| (*s).into()).collect(),
        required: true,
    };
    // ST-27's shape: one chain, nothing at once
    let chain = [
        u("evidence", &[], &["a"]),
        u("design", &["evidence"], &["b"]),
        u("review", &["design"], &["c"]),
        u("integrate", &["review"], &["d"]),
    ];
    let notes = crate::workflow::plan_notes(&chain, Some(1));
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(notes[0].contains("一个接一个"), "{notes:?}");
    // independent units that share one key are serialised; a limit below the width is named
    let shared = [
        u("api", &[], &["repo"]),
        u("web", &[], &["repo"]),
        u("review", &["api", "web"], &["r"]),
    ];
    let notes = crate::workflow::plan_notes(&shared, Some(1));
    assert!(
        notes.iter().any(|n| n.contains("同一个资源「repo」")),
        "{notes:?}"
    );
    assert!(
        notes.iter().any(|n| n.contains("max_parallel=1")),
        "{notes:?}"
    );
    // a graph that runs work at once says nothing
    let good = [
        u("api", &[], &["api"]),
        u("web", &[], &["web"]),
        u("review", &["api", "web"], &["r"]),
        u("integrate", &["review"], &["i"]),
    ];
    assert!(crate::workflow::plan_notes(&good, None).is_empty());
}

fn unit_row(k: &Kit, parent: &str, id: &str) -> Value {
    k.core.workflow_get(parent).unwrap()["units"]
        .as_array()
        .unwrap()
        .iter()
        .find(|u| u["id"] == id)
        .unwrap()
        .clone()
}

fn busy_blocker(k: &Kit, parent: &str, id: &str) -> bool {
    k.core.workflow_get(parent).unwrap()["blockers"]
        .as_array()
        .unwrap()
        .iter()
        .any(|b| b.as_str() == Some(&format!("{id}：执行或取消仍未确认结束")))
}

/// What the Hub recorded once a Node let a session go: phase released, nothing in flight.
fn release_binding(k: &Kit, sid: &str) {
    k.db()
        .store()
        .with_conn(|c| {
            c.execute("INSERT OR IGNORE INTO nodes(node_id,owner,name,pubkey,fingerprint,state,created,last_seen) VALUES ('local','me','node',zeroblob(32),'fingerprint','active',1,1)", [])?;
            c.execute("INSERT INTO runtime_sessions(session_id,node_id,workspace_ref,node_session_id,config_revision,phase,updated) VALUES (?1,'local','{}',?1,'r','released',1)", [sid])?;
            Ok(())
        })
        .unwrap();
}

#[test]
fn a_succeeded_unit_is_never_stopping_and_a_released_binding_is_idle_without_its_node() {
    let (k, parent, specs) = setup(true);
    plan(&k, &parent, &specs);
    let sa = unit(&k, &parent, "a").session_id.unwrap();
    let sb = unit(&k, &parent, "b").session_id.unwrap();

    // Delivered while its turn is still ending: done, not "stop not yet confirmed". Completion
    // still waits for the turn, which the blockers say.
    report(&k, &parent, "a").unwrap();
    assert_eq!(unit_row(&k, &parent, "a")["status"], "succeeded");
    assert_eq!(unit_row(&k, &parent, "a")["stop_pending"], false);
    assert!(busy_blocker(&k, &parent, "a"));
    k.rt.end_turn(&k.core, &sa, "end_turn", None);
    assert!(!busy_blocker(&k, &parent, "a"));

    // Its Node is not attached (a snapshot Hub, a Node that is down): the runtime reports a parked
    // session as busy. The binding the Hub recorded says it was let go with nothing in flight, so
    // the finished unit holds nothing up.
    k.rt.st().turning.insert(sa.clone());
    assert!(
        busy_blocker(&k, &parent, "a"),
        "an unreleased parked session stays busy"
    );
    release_binding(&k, &sa);
    assert!(!busy_blocker(&k, &parent, "a"));
    assert_eq!(unit_row(&k, &parent, "a")["stop_pending"], false);

    // A cancelled unit whose execution goes on is the one stop that is pending, until its binding
    // is released with nothing in flight.
    change(&k, &parent, "cancel", Some("b")).unwrap();
    k.rt.st().turning.insert(sb.clone());
    assert_eq!(unit_row(&k, &parent, "b")["status"], "cancelled");
    assert_eq!(unit_row(&k, &parent, "b")["stop_pending"], true);
    release_binding(&k, &sb);
    k.db()
        .store()
        .with_conn(|c| {
            c.execute("UPDATE sessions SET status='idle' WHERE id=?1", [&sb])?;
            Ok(())
        })
        .unwrap();
    assert_eq!(unit_row(&k, &parent, "b")["stop_pending"], false);
    // a turn the Hub still has open on that binding keeps it busy again
    k.db()
        .store()
        .with_conn(|c| {
            c.execute("INSERT INTO runtime_turns(turn_id,session_id,state,created,updated) VALUES ('t_open',?1,'running',1,1)", [&sb])?;
            Ok(())
        })
        .unwrap();
    assert_eq!(unit_row(&k, &parent, "b")["stop_pending"], true);
}

#[test]
fn bot_cannot_raise_max_attempts_on_replan_while_user_can() {
    let (k, parent, specs) = setup(true);
    let coord_sid = k.session(&parent, "idle");
    let coord = k.db().session(&coord_sid).unwrap().unwrap();
    k.core
        .tool_workflow(
            &coord,
            json!({
                "action": "plan",
                "reason": "初始编排",
                "expected_revision": 0,
                "units": specs,
                "max_parallel": 2,
                "max_attempts": 2
            }),
        )
        .unwrap();
    let rev = graph(&k, &parent).revision;
    let err = k
        .core
        .tool_workflow(
            &coord,
            json!({
                "action": "plan",
                "reason": "试图自行提额",
                "expected_revision": rev,
                "units": specs,
                "max_parallel": 2,
                "max_attempts": 4
            }),
        )
        .unwrap_err()
        .to_string();
    assert!(err.contains("Bot 不能自行调高"), "{err}");

    k.core
        .workflow_change(
            &parent,
            json!({
                "action": "plan",
                "reason": "人工提高额度",
                "expected_revision": rev,
                "units": specs,
                "max_parallel": 2,
                "max_attempts": 4
            }),
            "me",
        )
        .unwrap();
    assert_eq!(graph(&k, &parent).max_attempts, 4);
}

#[test]
fn unit_delivery_accepts_baseline_and_external_failures_when_unit_checks_pass() {
    let (k, parent, specs) = setup(true);
    plan(&k, &parent, &specs);
    let u = unit(&k, &parent, "a");
    let sid = u.session_id.unwrap();
    let s = k.db().session(&sid).unwrap().unwrap();
    let err = k
        .core
        .tool_deliver(
            &s,
            "仅报备基线",
            &[Output {
                kind: "file".into(),
                title: "产物".into(),
                location: Some("a.txt".into()),
                role: None,
            }],
            &[Check {
                name: "全仓格式".into(),
                passed: false,
                operated: false,
                scope: Some("baseline".into()),
            }],
            &[],
        )
        .unwrap_err()
        .to_string();
    assert!(err.contains("scope=unit"), "{err}");

    k.core
        .tool_deliver(
            &s,
            "本单元通过且如实记录基线失败",
            &[Output {
                kind: "file".into(),
                title: "产物".into(),
                location: Some("a.txt".into()),
                role: None,
            }],
            &[
                Check {
                    name: "改动模块测试".into(),
                    passed: true,
                    operated: false,
                    scope: None,
                },
                Check {
                    name: "全仓未触碰文件格式".into(),
                    passed: false,
                    operated: false,
                    scope: Some("baseline".into()),
                },
            ],
            &[],
        )
        .unwrap();
    k.rt.end_turn(&k.core, &sid, "end_turn", None);
    let after = unit(&k, &parent, "a");
    assert_eq!(after.status, "succeeded");
    assert_eq!(after.checks.as_array().unwrap().len(), 2);
}

fn tree_kit(cap: usize) -> Kit {
    let k = Kit::with(|cfg| cfg.max_workflow_parallel = cap);
    k.rt.st().workflow_execution_ready = true;
    k
}

fn tree_specs() -> Vec<UnitSpec> {
    [
        ("a", "dev", "work", vec![]),
        ("review", "steve", "review", vec!["a"]),
        ("integrate", "dev", "integrate", vec!["review"]),
    ]
    .into_iter()
    .map(|(id, bot, kind, deps)| UnitSpec {
        id: id.into(),
        title: id.into(),
        goal: format!("完成 {id}"),
        bot_id: bot.into(),
        kind: kind.into(),
        depends_on: deps.into_iter().map(String::from).collect(),
        resources: vec![],
        required: true,
    })
    .collect()
}

fn ordinary_fork(k: &Kit, parent: &str) -> String {
    k.db()
        .item_create(
            crate::repo::WS,
            &crate::repo::NewItem {
                tags: k.task_tags(crate::repo::WS),
                title: "普通派生事项".into(),
                goal: "独立编排工作".into(),
                bot_id: "dev".into(),
                status: Some("run".into()),
                stage: Some("执行".into()),
                source: Some(("派生".into(), "fork".into())),
                parent: Some(crate::repo::Parent {
                    item: Some(Some(parent.into())),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .unwrap()
        .id
}

#[test]
fn workflow_tree_cap_one_allows_independent_roots_to_run_together() {
    for (cap, root_count) in [(Some(1), 2), (None, 5)] {
        let k = cap.map_or_else(Kit::new, tree_kit);
        k.rt.st().workflow_execution_ready = true;
        if cap.is_none() {
            assert_eq!(k.core.config().max_workflow_parallel, 4);
        }
        let specs = tree_specs();
        let roots: Vec<_> = (0..root_count)
            .map(|_| {
                let parent = k.item("run", "执行", None);
                plan(&k, &parent, &specs);
                parent
            })
            .collect();
        for parent in &roots {
            assert_eq!(unit(&k, parent, "a").status, "running");
            assert_eq!(
                graph(&k, parent)
                    .units
                    .iter()
                    .filter(|u| u.active())
                    .count(),
                1
            );
        }
    }
}

#[test]
fn workflow_tree_cap_is_shared_by_sibling_and_deep_graphs_and_completion_wakes_them() {
    let k = tree_kit(1);
    let root = k.item("run", "执行", None);
    let first = ordinary_fork(&k, &root);
    let sibling = ordinary_fork(&k, &root);
    let middle = ordinary_fork(&k, &root);
    let deep = ordinary_fork(&k, &ordinary_fork(&k, &middle));
    let specs = tree_specs();
    for parent in [&first, &sibling, &deep] {
        plan(&k, parent, &specs);
    }
    assert!(k.db().store().workflows().get(&root).unwrap().is_none());
    assert_eq!(unit(&k, &first, "a").status, "running");
    assert_eq!(unit(&k, &sibling, "a").status, "pending");
    assert_eq!(unit(&k, &deep, "a").status, "pending");

    for id in ["a", "review", "integrate"] {
        finish(&k, &first, id);
    }
    let waiting = [&sibling, &deep];
    let next = waiting
        .iter()
        .find(|parent| unit(&k, parent, "a").status == "running")
        .expect("releasing the tree's slot wakes a waiting graph");
    assert_eq!(
        waiting
            .iter()
            .flat_map(|parent| graph(&k, parent).units)
            .filter(|u| u.active())
            .count(),
        1,
        "sibling and deep descendants share the same reservation"
    );
    for id in ["a", "review", "integrate"] {
        finish(&k, next, id);
    }
    let last = waiting.iter().find(|parent| parent != &next).unwrap();
    assert_eq!(unit(&k, last, "a").status, "running");
}

#[test]
fn workflow_tree_allows_graphs_below_graphless_parents_but_preserves_nested_graph_rejection() {
    let k = tree_kit(1);
    let root = k.item("run", "执行", None);
    let specs = tree_specs();
    plan(&k, &root, &specs);
    let middle = ordinary_fork(&k, &root);
    let ordinary = ordinary_fork(&k, &middle);
    plan(&k, &ordinary, &specs);
    assert_eq!(unit(&k, &ordinary, "a").status, "pending");

    let child = unit(&k, &root, "a").item_id.unwrap();
    for parent in [&middle, &child] {
        let err = k.core.workflow_change(
            parent,
            json!({"action":"plan", "reason":"不能嵌套", "expected_revision":0, "units":specs}),
            "me",
        );
        assert!(
            err.is_err(),
            "an immediate child of a graph owner must not own a nested graph"
        );
        assert!(k.db().store().workflows().get(parent).unwrap().is_none());
    }
}

#[test]
fn workflow_tree_cap_does_not_replace_the_graph_local_parallel_cap() {
    let k = tree_kit(2);
    let root = k.item("run", "执行", None);
    let mut specs = tree_specs();
    let mut b = specs[0].clone();
    b.id = "b".into();
    b.title = "b".into();
    specs.insert(1, b);
    specs[2].depends_on.push("b".into());
    k.core
        .workflow_change(
            &root,
            json!({"action":"plan", "reason":"图内串行", "expected_revision":0,
                "units":specs, "max_parallel":1}),
            "me",
        )
        .unwrap();
    let middle = ordinary_fork(&k, &root);
    let sibling_graph = ordinary_fork(&k, &middle);
    plan(&k, &sibling_graph, &tree_specs());

    assert_eq!(graph(&k, &root).max_parallel, 1);
    assert_eq!(unit(&k, &root, "a").status, "running");
    assert_eq!(unit(&k, &root, "b").status, "pending");
    assert_eq!(unit(&k, &sibling_graph, "a").status, "running");
    finish(&k, &root, "a");
    assert_eq!(unit(&k, &root, "b").status, "running");
    assert_eq!(
        graph(&k, &root).units.iter().filter(|u| u.active()).count(),
        1
    );
}

#[test]
fn workflow_tree_root_uses_persisted_ancestry_without_a_depth_limit() {
    let k = tree_kit(1);
    let root = k.item("run", "执行", None);
    for parent in [json!({}), json!({"item":null})] {
        k.db()
            .exec(
                "UPDATE items SET parent=? WHERE id=?",
                &[&parent.to_string(), &root],
            )
            .unwrap();
        let t = k.core.tx();
        assert_eq!(
            t.workflow_root(&t.item(&root).unwrap()).unwrap(),
            (crate::repo::WS.into(), root.clone())
        );
    }
    let mut leaf = root.clone();
    for _ in 0..40 {
        leaf = ordinary_fork(&k, &leaf);
    }
    let another_root = k.item("run", "执行", None);
    {
        let t = k.core.tx();
        assert_eq!(
            t.workflow_root(&t.item(&leaf).unwrap()).unwrap(),
            (crate::repo::WS.into(), root.clone())
        );
    }
    k.db()
        .exec(
            "UPDATE items SET parent=? WHERE id=?",
            &[&json!({"item":another_root}).to_string(), &root],
        )
        .unwrap();
    let t = k.core.tx();
    assert_eq!(
        t.workflow_root(&t.item(&leaf).unwrap()).unwrap(),
        (crate::repo::WS.into(), another_root)
    );
}

fn assert_invalid_workflow_tree_is_isolated(invalid: &str) {
    let k = tree_kit(1);
    k.rt.st().workflow_execution_ready = false;
    let ancestor = k.item("run", "执行", None);
    let corrupt = ordinary_fork(&k, &ancestor);
    let specs = tree_specs();
    plan(&k, &corrupt, &specs);
    match invalid {
        "cycle" | "missing" => {
            let parent = if invalid == "cycle" {
                corrupt.as_str()
            } else {
                "missing-item"
            };
            k.db()
                .exec(
                    "UPDATE items SET parent=? WHERE id=?",
                    &[&json!({"item":parent}).to_string(), &ancestor],
                )
                .unwrap();
        }
        "workspace" => {
            k.db().exec("INSERT INTO workspaces(id,name,created,item_prefix) VALUES('other','Other',1,'OT')", &[]).unwrap();
            k.db()
                .exec(
                    "UPDATE items SET workspace_id='other' WHERE id=?",
                    &[&ancestor],
                )
                .unwrap();
        }
        _ => panic!("unknown invalid ancestry fixture"),
    }
    {
        let t = k.core.tx();
        assert!(
            t.workflow_root(&t.item(&corrupt).unwrap()).is_err(),
            "{invalid}"
        );
    }
    k.rt.st().workflow_execution_ready = true;
    let independent = k.item("run", "执行", None);
    plan(&k, &independent, &specs);
    k.core.lifecycle_tick().unwrap();
    assert_eq!(
        unit(&k, &corrupt, "a").status,
        "pending",
        "{invalid} ancestry must not dispatch"
    );
    assert_eq!(
        unit(&k, &independent, "a").status,
        "running",
        "{invalid} ancestry must not block another root"
    );
}

#[test]
fn workflow_tree_cycle_is_rejected_without_blocking_independent_roots() {
    assert_invalid_workflow_tree_is_isolated("cycle");
}

#[test]
fn workflow_tree_missing_parent_is_rejected_without_blocking_independent_roots() {
    assert_invalid_workflow_tree_is_isolated("missing");
}

#[test]
fn workflow_tree_cross_workspace_parent_is_rejected_without_blocking_independent_roots() {
    assert_invalid_workflow_tree_is_isolated("workspace");
}

#[test]
fn workflow_tree_paused_graph_reserves_capacity_only_until_its_turn_is_settled() {
    let k = tree_kit(1);
    let root = k.item("run", "执行", None);
    let paused = ordinary_fork(&k, &root);
    let waiting = ordinary_fork(&k, &root);
    let specs = tree_specs();
    plan(&k, &paused, &specs);
    let sid = unit(&k, &paused, "a").session_id.unwrap();
    change(&k, &paused, "pause", None).unwrap();
    plan(&k, &waiting, &specs);
    assert_eq!(
        unit(&k, &waiting, "a").status,
        "pending",
        "an unconfirmed stop retains its reservation"
    );

    k.rt.end_turn(&k.core, &sid, "cancelled", None);
    k.core.lifecycle_tick().unwrap();
    assert!(graph(&k, &paused).paused);
    assert_eq!(
        unit(&k, &paused, "a").status,
        "running",
        "the paused graph retains its resumable logical status"
    );
    assert_eq!(
        unit(&k, &waiting, "a").status,
        "running",
        "a settled paused unit no longer occupies the tree's capacity"
    );

    change(&k, &paused, "resume", None).unwrap();
    assert!(!graph(&k, &paused).paused);
    assert!(
        !k.rt.st().turning.contains(&sid),
        "resuming an idle logical running unit must wait for tree capacity"
    );
    assert_eq!(unit(&k, &waiting, "a").status, "running");
    for id in ["a", "review", "integrate"] {
        finish(&k, &waiting, id);
    }
    assert_eq!(
        unit(&k, &paused, "a").session_id.as_deref(),
        Some(sid.as_str())
    );
    assert!(
        k.rt.st().turning.contains(&sid),
        "releasing the occupied slot resumes the paused unit's original session"
    );
}

/// The coordinator's contract: after building or retrying its graph it ends its turn, and Steve
/// wakes it with what happened. A unit reads its upstream from its opening, not with a get.
#[test]
fn the_coordinator_contract_ends_the_turn_and_a_unit_needs_no_get_for_its_upstream() {
    let (k, parent, specs) = setup(true);
    let coord_sid = k.session(&parent, "idle");
    let coord = k.db().session(&coord_sid).unwrap().unwrap();
    let t = k.core.tx();
    let item = t.item(&parent).unwrap();
    let bot = k
        .db()
        .bot(item.bot_id.as_deref().unwrap())
        .unwrap()
        .unwrap();
    let contract = t.protocol(&item, &bot, &coord).unwrap();
    assert!(
        contract.contains("After you build or retry the graph, end your turn")
            && contract.contains(crate::workflow::SYSTEM_NOTICE)
            && contract.contains("known_revision"),
        "{contract}"
    );
    assert!(
        contract.contains("then retry that unit: its review and everything downstream rerun"),
        "a retry already reruns the review: {contract}"
    );
    drop(t);
    plan(&k, &parent, &specs);
    let a = unit(&k, &parent, "a");
    let s = k
        .db()
        .session(a.session_id.as_deref().unwrap())
        .unwrap()
        .unwrap();
    let t = k.core.tx();
    let child = t.item(a.item_id.as_deref().unwrap()).unwrap();
    let dev = k.db().bot("dev").unwrap().unwrap();
    let contract = t.protocol(&child, &dev, &s).unwrap();
    assert!(
        contract.contains("upstream results are in the opening message")
            && contract.contains("This session can read only its own item"),
        "{contract}"
    );
    assert!(
        !contract.contains("Read your upstream artifacts with steve_workflow action=get"),
        "{contract}"
    );
}

#[test]
fn plan_and_retry_say_next_and_a_get_of_a_known_revision_is_only_unchanged() {
    let (k, parent, specs) = setup(true);
    let coord_sid = k.session(&parent, "idle");
    let coord = k.db().session(&coord_sid).unwrap().unwrap();
    let out = k
        .core
        .tool_workflow(
            &coord,
            json!({"action":"plan","reason":"拆分独立工作","expected_revision":0,"units":specs}),
        )
        .unwrap();
    assert!(
        out["next"].as_str().unwrap().contains("现在结束本回合"),
        "{out}"
    );
    let rev = graph(&k, &parent).revision;
    assert_eq!(
        k.core
            .tool_workflow(&coord, json!({"action":"get","known_revision":rev}))
            .unwrap(),
        json!({"unchanged": true, "revision": rev})
    );
    let full = k
        .core
        .tool_workflow(&coord, json!({"action":"get","known_revision":rev - 1}))
        .unwrap();
    assert_eq!(full["revision"], rev);
    assert!(full["units"].is_array());
    assert!(full.get("next").is_none(), "a read says no next step");
    assert!(
        k.core
            .tool_workflow(&coord, json!({"action":"get","known_revision":"latest"}))
            .is_err()
    );

    // a unit reads its parent as parent_item (never an item_id it would pass back), and the same
    // known revision answers it too
    let a = unit(&k, &parent, "a");
    let s = k
        .db()
        .session(a.session_id.as_deref().unwrap())
        .unwrap()
        .unwrap();
    let context = k.core.tool_workflow(&s, json!({"action":"get"})).unwrap();
    assert_eq!(context["parent_item"], parent);
    assert!(context.get("item_id").is_none(), "{context}");
    assert!(
        context["note"]
            .as_str()
            .unwrap()
            .contains("本会话只能读取本单元的事项"),
        "{context}"
    );
    let rev = graph(&k, &parent).revision;
    assert_eq!(
        k.core
            .tool_workflow(&s, json!({"action":"get","known_revision":rev}))
            .unwrap()["unchanged"],
        true
    );

    k.rt.end_turn(
        &k.core,
        a.session_id.as_deref().unwrap(),
        "error",
        Some("检查失败"),
    );
    let rev = graph(&k, &parent).revision;
    let out = k
        .core
        .tool_workflow(
            &coord,
            json!({"action":"retry","unit_id":"a","reason":"修正后重试","expected_revision":rev}),
        )
        .unwrap();
    assert!(
        out["next"].as_str().unwrap().contains("现在结束本回合"),
        "{out}"
    );
}

#[test]
fn the_coordinator_is_woken_with_the_units_status_and_a_bounded_summary() {
    let (k, parent, specs) = setup(true);
    plan(&k, &parent, &specs);
    let coord = k.session(&parent, "idle");
    finish(&k, &parent, "a");
    let woken = k.rt.prompts_of(&coord);
    let wake = woken.last().unwrap();
    assert!(
        wake.starts_with(crate::workflow::SYSTEM_NOTICE),
        "marked as Steve's: {wake}"
    );
    assert!(
        wake.contains("协作工作「a」（a）已通过，第 1/3 次尝试"),
        "{wake}"
    );
    assert!(wake.contains("摘要：a 已验证"), "{wake}");
    let g = graph(&k, &parent);
    assert!(
        wake.contains(&format!(
            "工作图 revision {}：已通过 1，执行中 1",
            g.revision
        )),
        "{wake}"
    );
    assert!(wake.contains("直接结束回合"), "nothing to do: {wake}");
    k.rt.end_turn(&k.core, &coord, "end_turn", None);

    let long = "很长的失败原因".repeat(200);
    let b = unit(&k, &parent, "b").session_id.unwrap();
    k.rt.end_turn(&k.core, &b, "error", Some(&long));
    let woken = k.rt.prompts_of(&coord);
    let wake = woken.last().unwrap();
    assert!(
        wake.contains("协作工作「b」（b）未通过，第 1/3 次尝试"),
        "{wake}"
    );
    let summary = wake
        .lines()
        .find(|l| l.starts_with("摘要："))
        .unwrap()
        .trim_start_matches("摘要：");
    let kept: String = long
        .chars()
        .take(crate::workflow::WAKE_SUMMARY_CHARS)
        .collect();
    assert!(summary.contains(&kept), "{summary}");
    assert!(
        summary.chars().count() < long.chars().count(),
        "bounded: {summary}"
    );
    assert!(wake.contains("再 retry 该单元"), "{wake}");
}

/// A retry reruns the unit and its downstream; only those must be settled. Independent work goes on.
#[test]
fn a_retry_waits_only_for_the_units_it_reruns() {
    let (k, parent, specs) = setup(true);
    plan(&k, &parent, &specs);
    let sa = unit(&k, &parent, "a").session_id.unwrap();
    let sb = unit(&k, &parent, "b").session_id.unwrap();
    // a reports its failure, its turn not yet over: a itself is not settled
    let s = k.db().session(&sa).unwrap().unwrap();
    k.core
        .tool_workflow(&s, json!({"action":"fail","reason":"缺少回归"}))
        .unwrap();
    let err = change(&k, &parent, "retry", Some("a"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("「a」仍在执行"), "{err}");
    k.rt.end_turn(&k.core, &sa, "end_turn", None);
    // b, independent of a, still runs: the retry does not wait for it, nor touch it
    assert_eq!(unit(&k, &parent, "b").status, "running");
    change(&k, &parent, "retry", Some("a")).unwrap();
    let a = unit(&k, &parent, "a");
    assert_eq!((a.status.as_str(), a.attempt), ("running", 2));
    let b = unit(&k, &parent, "b");
    assert_eq!(
        (b.status.as_str(), b.session_id.as_deref()),
        ("running", Some(sb.as_str()))
    );
    // a downstream unit that runs holds the retry, named
    finish(&k, &parent, "a");
    finish(&k, &parent, "b");
    assert_eq!(unit(&k, &parent, "review").status, "running");
    let err = change(&k, &parent, "retry", Some("a"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("「review」仍在执行"), "{err}");
}
