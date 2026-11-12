use serde_json::{Value, json};
use rusqlite::params;
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
    event(1, at - 900_000, "a_current", json!({"type":"attempt_started"}));
    event(2, at - 599_000, "a_current", json!({"type":"message_started","message":"assistant","block":"b","role":"assistant"}));
    assert!(store.workflows().confirmed_activity(&child, &sid, at).unwrap().is_some());
    assert!(store.workflows().confirmed_activity(&child, &sid, at + 1_000).unwrap().is_none(), "600 seconds is stale");
    event(3, at + 1_001, "a_current", json!({"type":"message_started","message":"user","block":"b","role":"user"}));
    event(4, at + 1_002, "a_retired", json!({"type":"tool_changed","tool":{}}));
    assert!(store.workflows().confirmed_activity(&child, &sid, at + 1_003).unwrap().is_none());
    event(5, at + 1_004, "a_current", json!({"type":"tool_changed","tool":{}}));
    assert!(store.workflows().confirmed_activity(&child, &sid, at + 1_005).unwrap().is_some());
    store.with_conn(|c| { c.execute("UPDATE nodes SET last_seen=0 WHERE node_id='n'", [])?; Ok(()) }).unwrap();
    assert!(store.workflows().confirmed_activity(&child, &sid, at + 1_005).unwrap().is_some(), "a connection timestamp is not a heartbeat");
    store.with_conn(|c| { c.execute("UPDATE node_streams SET fault='gap' WHERE node_id='n'", [])?; Ok(()) }).unwrap();
    assert!(store.workflows().confirmed_activity(&child, &sid, at + 1_005).unwrap().is_none(), "a faulted event stream cannot confirm activity");
    store.with_conn(|c| { c.execute("UPDATE node_streams SET fault=NULL WHERE node_id='n'", [])?; Ok(()) }).unwrap();
    store.with_conn(|c| { c.execute("UPDATE runtime_attempts SET cancel_requested=1 WHERE attempt_id='a_current'", [])?; Ok(()) }).unwrap();
    assert!(store.workflows().confirmed_activity(&child, &sid, at + 1_005).unwrap().is_none());
    store.with_conn(|c| { c.execute("UPDATE runtime_attempts SET cancel_requested=0 WHERE attempt_id='a_current'", [])?; Ok(()) }).unwrap();
    event(6, at + 20_000, "a_current", json!({"type":"tool_changed","tool":{}}));
    assert!(store.workflows().confirmed_activity(&child, &sid, at + 10_000).unwrap().is_some(), "future evidence must not replace a valid earlier event");
    store.with_conn(|c| { c.execute("DELETE FROM node_event_inbox WHERE seq=5", [])?; Ok(()) }).unwrap();
    assert!(store.workflows().confirmed_activity(&child, &sid, at + 10_000).unwrap().is_none(), "future evidence cannot confirm activity");
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
            k.db().queue_add(sid, "未结清输入", "me", &[]).unwrap();
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
    let old_session = k.db().session(old_review.session_id.as_deref().unwrap()).unwrap().unwrap();
    change(&k, &parent, "pause", None).unwrap();
    k.db().exec("UPDATE items SET cycle=cycle+1 WHERE id=?", &[&parent]).unwrap();

    let result = k.core.workflow_change(&parent, json!({
        "action":"plan", "reason":"新周期重新验收，旧成果只留作历史",
        "expected_revision":graph(&k,&parent).revision, "units":specs,
        "max_parallel":2, "max_attempts":3
    }), "me").unwrap();
    assert_eq!(result["current_cycle"], true);
    assert_eq!(result["cycle"], 2);
    assert_eq!(result["paused"], true);
    for u in &graph(&k, &parent).units {
        assert_eq!(u.status, "pending");
        assert_eq!(u.attempt, 0);
        assert!(u.item_id.is_none());
    }
    assert_eq!(result["history_attempts_total"], 4);
    assert!(result["history_attempts"].as_array().unwrap().iter().any(|u|
        u["unit_id"] == "review" && u["snapshot"]["status"] == "succeeded"));
    assert!(k.core.tool_deliver(&old_session, "过期结果", &[], &[], &[]).is_err());
    assert!(parent_deliver(&k, &parent).is_err(), "old successes must not accept the new cycle");
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

#[test]
fn restart_finishes_a_retired_failed_attempt_after_its_turn_ends() {
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
    assert!(unit(&k, &parent, "a").item_id.is_none());
    let history = k.core.workflow_get(&parent).unwrap();
    assert_eq!(history["history_attempts_total"], 1);
    assert_eq!(history["history_attempts"][0]["item_id"], child);
    assert_eq!(history["history_attempts"][0]["snapshot"]["session_id"], sid);
    assert_eq!(history["history_attempts"][0]["snapshot"]["status"], "cancelled");
    assert!(history["history_attempts"][0]["snapshot"].get("outputs").is_none(), "history view stays compact");

    k.core.tx().workflow_tick().unwrap();
    assert_eq!(k.phase(&child), "active", "live turn still owns its child");
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

#[test]
fn restart_cancels_a_retired_attempt_without_a_failed_delivery() {
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
    assert_eq!(k.phase(&child), "closed");
    assert_eq!(k.status(&child).0, "drop");
    assert_eq!(
        k.db().item(&child).unwrap().unwrap().outcome.as_deref(),
        Some("cancelled")
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
    let restarted = crate::Core::new(
        k.db().clone(),
        k.rt.clone(),
        k.gw.clone(),
        k.core.config().clone(),
    );
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
