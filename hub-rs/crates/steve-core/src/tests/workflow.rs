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
    assert_eq!(unit(&k, &parent, "integrate").status, "pending");
    assert!(k.finish(&parent).is_err());
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
    restarted.lifecycle_tick().unwrap();
    for u in graph(&k, &parent)
        .units
        .iter()
        .filter(|u| u.status == "running")
    {
        assert!(k.rt.st().cancelled.contains(u.session_id.as_ref().unwrap()));
    }
    assert_eq!(k.rt.st().turning.len(), 2);
    assert!(k.finish(&parent).is_err());
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
