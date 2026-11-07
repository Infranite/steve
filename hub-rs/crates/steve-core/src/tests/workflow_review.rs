//! Independent review regressions: paused queues, abandoned attempts and optional reservations.

use serde_json::json;
use steve_store::{UnitSpec, WorkUnit, Workflow};

use crate::testkit::Kit;
use crate::{AgentRuntime, Check, Flushed, Output};

fn setup(optional: bool) -> (Kit, String) {
    let k = Kit::new();
    k.rt.st().workflow_execution_ready = true;
    let parent = k.item("run", "执行", None);
    let spec = |id: &str, bot: &str, kind: &str, deps: &[&str], required: bool| UnitSpec {
        id: id.into(),
        title: id.into(),
        goal: format!("验证 {id}"),
        bot_id: bot.into(),
        kind: kind.into(),
        depends_on: deps.iter().map(|s| (*s).into()).collect(),
        resources: vec![],
        required,
    };
    let mut units = vec![
        spec("work", "dev", "work", &[], true),
        spec("review", "steve", "review", &["work"], true),
        spec("integrate", "dev", "integrate", &["review"], true),
    ];
    if optional {
        units.push(spec("extra", "dev", "work", &[], false));
    }
    k.core
        .workflow_change(
            &parent,
            json!({"action":"plan","reason":"评审回归","expected_revision":0,
                "units":units,"max_parallel":2,"max_attempts":3}),
            "me",
        )
        .unwrap();
    (k, parent)
}

fn graph(k: &Kit, parent: &str) -> Workflow {
    k.db().store().workflows().get(parent).unwrap().unwrap()
}

fn unit(k: &Kit, parent: &str, id: &str) -> WorkUnit {
    graph(k, parent)
        .units
        .into_iter()
        .find(|u| u.spec.id == id)
        .unwrap()
}

fn change(k: &Kit, parent: &str, action: &str, id: Option<&str>) {
    k.core
        .workflow_change(
            parent,
            json!({"action":action,"reason":"评审回归","unit_id":id,
                "expected_revision":graph(k,parent).revision}),
            "me",
        )
        .unwrap();
}

fn finish(k: &Kit, parent: &str, id: &str) {
    let sid = unit(k, parent, id).session_id.unwrap();
    let s = k.db().session(&sid).unwrap().unwrap();
    k.core
        .tool_deliver(
            &s,
            "已验证",
            &[Output {
                kind: "文件".into(),
                title: id.into(),
                location: Some(format!("{id}.txt")),
                role: None,
            }],
            &[Check {
                name: "已操作并检查".into(),
                passed: true,
                operated: true,
            }],
            &[],
        )
        .unwrap();
    k.rt.end_turn(&k.core, &sid, "end_turn", None);
}

#[test]
fn paused_persisted_queue_stays_held_through_turn_end_and_restart() {
    let (k, parent) = setup(false);
    let sid = unit(&k, &parent, "work").session_id.unwrap();
    k.rt.prompt(&sid, "暂停前排队的补充", "me", &[]).unwrap();
    let before = k.rt.prompts_of(&sid).len();
    change(&k, &parent, "pause", None);
    k.rt.end_turn(&k.core, &sid, "cancelled", None);
    assert_eq!(k.rt.prompts_of(&sid).len(), before);
    assert_eq!(k.db().queued(&sid).unwrap().len(), 1);
    assert_eq!(k.rt.flush(&sid, None), Flushed::Held);
    k.core.recover().unwrap();
    k.core.ready();
    k.core.lifecycle_tick().unwrap();
    assert_eq!(k.rt.prompts_of(&sid).len(), before);
    assert_eq!(k.db().queued(&sid).unwrap().len(), 1);
    change(&k, &parent, "resume", None);
    k.core.tx().flush_queued().unwrap();
    assert_eq!(k.rt.prompts_of(&sid).len(), before + 1);
    assert!(k.db().queued(&sid).unwrap().is_empty());
}

#[test]
fn cancelling_clears_the_queue_and_retry_cannot_revive_the_old_session() {
    let (k, parent) = setup(false);
    let old = unit(&k, &parent, "work").session_id.unwrap();
    k.rt.prompt(&old, "旧尝试的排队补充", "me", &[]).unwrap();
    change(&k, &parent, "cancel", Some("work"));
    assert!(k.db().queued(&old).unwrap().is_empty());
    k.rt.end_turn(&k.core, &old, "cancelled", None);
    let count = k.rt.prompts_of(&old).len();
    change(&k, &parent, "retry", Some("work"));
    let new = unit(&k, &parent, "work").session_id.unwrap();
    assert_ne!(old, new);
    let old_session = k.db().session(&old).unwrap().unwrap();
    assert!(
        k.core
            .tool_workflow(&old_session, json!({"action":"get"}))
            .is_err(),
        "a retired token must not read the new attempt's upstream results",
    );
    assert!(k.rt.prompt(&old, "迟到的补充", "me", &[]).is_err());
    assert_eq!(k.rt.flush(&old, Some(("请继续", "me"))), Flushed::Held);
    assert_eq!(k.rt.prompts_of(&old).len(), count);
    assert!(k.rt.running(&new).is_some());
}

#[test]
fn optional_claim_without_a_live_session_still_prevents_parent_completion() {
    let (k, parent) = setup(true);
    finish(&k, &parent, "work");
    finish(&k, &parent, "review");
    finish(&k, &parent, "integrate");
    let optional = unit(&k, &parent, "extra");
    k.rt.end_turn(
        &k.core,
        optional.session_id.as_deref().unwrap(),
        "error",
        Some("模拟准备中断"),
    );
    // The durable state at a crash between claim and session creation: a reserved optional unit,
    // no active process and no current session. Completion must not throw the reservation away.
    k.db()
        .store()
        .transaction::<_, steve_store::StoreError>(|tx| {
            tx.execute(
                "UPDATE workflow_units SET status='dispatching',session_id=NULL
                 WHERE workflow_id=? AND id='extra'",
                [&parent],
            )?;
            Ok(())
        })
        .unwrap();
    assert!(
        k.core.workflow_get(&parent).unwrap()["blockers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v.as_str().unwrap().contains("extra"))
    );
    assert!(k.finish(&parent).is_err());
    assert_ne!(k.phase(&parent), "closed");
}

#[test]
fn replan_cannot_forget_a_cancelled_attempt_until_its_execution_has_settled() {
    for unsettled in ["running", "uncertain", "queued"] {
        let (k, parent) = setup(true);
        let old = unit(&k, &parent, "extra");
        let sid = old.session_id.as_deref().unwrap();
        change(&k, &parent, "cancel", Some("extra"));
        assert_eq!(unit(&k, &parent, "extra").status, "cancelled");
        // Cancellation is an intent; an ended session does not prove its Node execution stopped.
        assert_eq!(k.db().session(sid).unwrap().unwrap().status, "ended");
        if unsettled != "running" {
            let mut runtime = k.rt.st();
            runtime.turning.remove(sid);
            runtime.attempts.remove(sid);
            if unsettled == "uncertain" {
                runtime.uncertain.insert(sid.into());
            }
        }
        if unsettled == "queued" {
            k.db().queue_add(sid, "尚未结清的输入", "me", &[]).unwrap();
        }
        let before = graph(&k, &parent);
        for remove in [false, true] {
            let specs: Vec<_> = before
                .units
                .iter()
                .filter(|u| !remove || u.spec.id != "extra")
                .map(|u| {
                    let mut spec = u.spec.clone();
                    if spec.id == "extra" {
                        spec.title = "改派后的额外工作".into();
                    }
                    spec
                })
                .collect();
            let error = k
                .core
                .workflow_change(
                    &parent,
                    json!({"action":"plan","reason":"替换已取消工作",
                        "expected_revision":before.revision,"units":specs}),
                    "me",
                )
                .unwrap_err();
            assert!(
                error.to_string().contains("仍未确认结束"),
                "{unsettled}: a replan must retain the old attempt until cancellation settles: {error}",
            );
            assert_eq!(graph(&k, &parent), before);
        }
        {
            let mut runtime = k.rt.st();
            runtime.turning.remove(sid);
            runtime.attempts.remove(sid);
            runtime.uncertain.remove(sid);
        }
        k.db().queue_take(sid).unwrap();
        let remaining: Vec<_> = before
            .units
            .iter()
            .filter(|u| u.spec.id != "extra")
            .map(|u| u.spec.clone())
            .collect();
        k.core
            .workflow_change(
                &parent,
                json!({"action":"plan","reason":"已确认旧执行停止",
                    "expected_revision":before.revision,"units":remaining}),
                "me",
            )
            .unwrap();
        assert!(
            graph(&k, &parent)
                .units
                .iter()
                .all(|u| u.spec.id != "extra")
        );
        assert!(
            k.db()
                .store()
                .workflows()
                .by_child(old.item_id.as_deref().unwrap())
                .unwrap()
                .is_some(),
            "the settled attempt remains available for audit",
        );
    }
}
