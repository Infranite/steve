mod common;

use common::*;
use serde_json::{Value, json};
use steve_store::*;

fn setup() -> (T, String, Vec<UnitSpec>) {
    let t = fresh();
    for id in ["writer", "reviewer", "integrator"] {
        t.store
            .bots()
            .seed(
                id,
                false,
                &NewBot {
                    name: id.into(),
                    harness: "fixture".into(),
                    ..Default::default()
                },
                WS,
            )
            .unwrap();
    }
    let parent = t
        .store
        .items()
        .create(
            WS,
            &NewItem {
                title: "协作事项".into(),
                goal: "完成实现、独立评审和集成验收".into(),
                bot_id: "writer".into(),
                ..Default::default()
            },
        )
        .unwrap()
        .unwrap()
        .id;
    let specs = vec![
        unit("a", "writer", "work", &[], &["src/a"]),
        unit("b", "writer", "work", &[], &["src/b"]),
        unit("review", "reviewer", "review", &["a", "b"], &[]),
        unit("integrate", "integrator", "integrate", &["review"], &[]),
    ];
    (t, parent, specs)
}

fn unit(id: &str, bot: &str, kind: &str, deps: &[&str], resources: &[&str]) -> UnitSpec {
    UnitSpec {
        id: id.into(),
        title: format!("工作 {id}"),
        goal: format!("完成 {id} 并验证"),
        bot_id: bot.into(),
        kind: kind.into(),
        depends_on: deps.iter().map(|s| (*s).into()).collect(),
        resources: resources.iter().map(|s| (*s).into()).collect(),
        required: true,
    }
}

fn plan(t: &T, parent: &str, specs: &[UnitSpec]) -> Workflow {
    t.store
        .workflows()
        .replace(parent, 0, "拆分工作", specs, 2, 3)
        .unwrap()
}

fn start(t: &T, parent: &str, id: &str) -> WorkUnit {
    let claimed = t.store.workflows().claim(parent, id).unwrap();
    let session = t
        .store
        .sessions()
        .create(&NewSession {
            workspace_id: WS.into(),
            item_id: claimed.item_id.clone(),
            bot_id: Some(claimed.spec.bot_id.clone()),
            harness: "fixture".into(),
            cwd: t.dir.path().to_string_lossy().into(),
            title: claimed.spec.title.clone(),
            token: format!("token-{id}-{}", claimed.attempt),
            ..Default::default()
        })
        .unwrap();
    t.store
        .workflows()
        .bind_session(parent, id, claimed.item_id.as_deref().unwrap(), &session.id)
        .unwrap()
}

fn finish(t: &T, unit: &WorkUnit, passed: bool) -> Workflow {
    t.store
        .workflows()
        .deliver(
            unit.item_id.as_deref().unwrap(),
            unit.session_id.as_deref().unwrap(),
            "交付检查结果",
            &json!([{"kind":"文件","title":"成果","location":"result.txt"}]),
            &json!([{"name":"实际验证","passed":passed,"operated":true}]),
            passed,
        )
        .unwrap()
}

#[test]
fn explicit_single_session_mode_is_durable_audited_and_replan_revokes_it() {
    let (t, parent, specs) = setup();
    plan(&t, &parent, &specs);
    let a = start(&t, &parent, "a");
    let success = finish(&t, &a, true);
    is_conflict(
        t.store
            .workflows()
            .accept_single_session(&parent, success.revision, "确认", "owner"),
        "暂停",
    );
    let paused = t
        .store
        .workflows()
        .pause(&parent, true, "用户暂停")
        .unwrap();
    is_conflict(
        t.store
            .workflows()
            .accept_single_session(&parent, paused.revision - 1, "确认", "owner"),
        "已更新",
    );
    is_conflict(
        t.store
            .workflows()
            .accept_single_session(&parent, paused.revision, "", "owner"),
        "用户和原因",
    );
    let single = t
        .store
        .workflows()
        .accept_single_session(
            &parent,
            paused.revision,
            "本轮未完成独立评审，改为单会话验收",
            "owner",
        )
        .unwrap();
    assert!(single.single_session() && single.paused);
    assert!(single.blockers().is_empty());
    assert_eq!(
        single.units[0], success.units[0],
        "successful evidence is retained unchanged"
    );
    assert!(
        single.units[1..]
            .iter()
            .all(|u| u.status == "cancelled" && u.spec.required)
    );
    let history = single.history.last().unwrap();
    assert_eq!(history.action, "single_session");
    assert_eq!(history.detail["actor"], "owner");
    assert_eq!(history.detail["before"][2]["status"], "pending");
    assert_eq!(
        single,
        t.store.workflows().get(&parent).unwrap().unwrap(),
        "mode survives re-read"
    );
    assert_eq!(
        single,
        t.store
            .workflows()
            .accept_single_session(&parent, single.revision, "重复确认", "owner")
            .unwrap()
    );
    assert!(
        t.store
            .workflows()
            .pause(&parent, false, "不能直接恢复")
            .is_err()
    );
    assert!(t.store.workflows().claim(&parent, "b").is_err());
    let replanned = t
        .store
        .workflows()
        .replace(&parent, single.revision, "恢复评审要求", &specs, 2, 3)
        .unwrap();
    assert!(!replanned.single_session());
    assert!(!replanned.blockers().is_empty());
}

fn is_conflict<T: std::fmt::Debug>(r: Result<T>, wanted: &str) {
    match r {
        Err(StoreError::Conflict(message)) => {
            assert!(
                message.contains(wanted),
                "{message} does not contain {wanted}"
            );
        }
        other => panic!("expected conflict {wanted}, got {other:?}"),
    }
}

#[test]
fn child_capture_commits_with_claim_and_repeated_claim_does_not_renew_it() {
    let (t, parent, specs) = setup();
    plan(&t, &parent, &specs);
    t.store
        .diagnostics()
        .capture(WS, &parent, "owner", Some(0))
        .unwrap();
    // Fail after the child and capture were inserted, exercising the actual claim transaction.
    t.store
        .with_conn(|c| {
            c.execute_batch(
                "CREATE TRIGGER reject_claim BEFORE UPDATE OF status ON workflow_units
            WHEN NEW.status='dispatching' BEGIN SELECT RAISE(ABORT,'fixture rejects claim'); END",
            )?;
            Ok(())
        })
        .unwrap();
    assert!(t.store.workflows().claim(&parent, "a").is_err());
    t.store
        .with_conn(|c| {
            assert_eq!(count(c, "SELECT count(*) FROM items"), 1);
            assert_eq!(count(c, "SELECT count(*) FROM diagnostic_captures"), 1);
            c.execute_batch("DROP TRIGGER reject_claim")?;
            Ok(())
        })
        .unwrap();
    let observation = |item: &str| diagnostics::Record {
        workspace_id: WS.into(),
        item_id: Some(item.into()),
        kind: "dependency.checked".into(),
        reason_code: "fixture_condition".into(),
        detail: true,
        ..Default::default()
    };
    assert_eq!(
        t.store.diagnostics().record(observation(&parent)),
        diagnostics::RecordStatus::Skipped
    );
    let child = t
        .store
        .workflows()
        .claim(&parent, "a")
        .unwrap()
        .item_id
        .unwrap();
    assert_eq!(
        t.store.diagnostics().record(observation(&child)),
        diagnostics::RecordStatus::Queued
    );
    let view = t.store.diagnostics().explain(WS, &child).unwrap();
    let created = t.store.items().get(&child).unwrap().unwrap().created;
    assert_eq!(view["capture"]["active"], true);
    assert_eq!(view["capture"]["expires_at"], created + 30 * 60_000);
    t.store
        .diagnostics()
        .capture(WS, &child, "owner", Some(0))
        .unwrap();
    assert_eq!(
        t.store
            .workflows()
            .claim(&parent, "a")
            .unwrap()
            .item_id
            .as_deref(),
        Some(child.as_str())
    );
    let stopped = t.store.diagnostics().explain(WS, &child).unwrap();
    assert_eq!(stopped["capture"]["active"], false);
    assert_eq!(stopped["capture"]["stop_reason"], "manual");
    assert_eq!(
        stopped["capture"]["expires_at"],
        view["capture"]["expires_at"]
    );
}

#[test]
fn rejects_cycles_missing_dependencies_and_non_independent_reviews_before_writing() {
    let (t, parent, specs) = setup();
    for (changed, wanted) in [
        (
            {
                let mut s = specs.clone();
                s[0].depends_on = vec!["integrate".into()];
                s
            },
            "环",
        ),
        (
            {
                let mut s = specs.clone();
                s[2].depends_on = vec!["missing".into()];
                s
            },
            "不存在",
        ),
        (
            {
                let mut s = specs.clone();
                s[2].bot_id = "writer".into();
                s
            },
            "自评",
        ),
        (
            {
                let mut s = specs.clone();
                s[2].depends_on = vec!["a".into()];
                s
            },
            "没有独立评审",
        ),
        (
            {
                let mut s = specs.clone();
                s[3].depends_on = vec!["a".into()];
                s
            },
            "覆盖全部",
        ),
        (
            {
                let mut s = specs.clone();
                s[2].required = false;
                s
            },
            "必需",
        ),
        (
            {
                let mut s = specs.clone();
                s[1].id = "a".into();
                s
            },
            "重复",
        ),
    ] {
        is_conflict(
            t.store
                .workflows()
                .replace(&parent, 0, "验证", &changed, 2, 3),
            wanted,
        );
        assert!(t.store.workflows().get(&parent).unwrap().is_none());
    }
    is_conflict(
        t.store
            .workflows()
            .replace(&parent, 0, "验证", &specs, 0, 3),
        "大于零",
    );
    is_conflict(
        t.store
            .workflows()
            .replace(&parent, 0, "验证", &specs, 2, 0),
        "大于零",
    );
}

#[test]
fn independent_work_runs_in_parallel_dependencies_and_final_gate_wait() {
    let (t, parent, specs) = setup();
    let graph = plan(&t, &parent, &specs);
    assert_eq!(graph.revision, 1);
    assert_eq!(graph.blockers().len(), 4);
    is_conflict(t.store.workflows().claim(&parent, "review"), "前置");
    let a = start(&t, &parent, "a");
    let b = start(&t, &parent, "b");
    assert_ne!(a.item_id, b.item_id);
    assert_eq!(a.status, "running");
    assert_eq!(b.status, "running");
    is_conflict(t.store.workflows().claim(&parent, "review"), "并发");
    let first = finish(&t, &a, true);
    assert_eq!(first.blockers().len(), 3);
    is_conflict(t.store.workflows().claim(&parent, "review"), "前置");
    finish(&t, &b, true);
    let review = start(&t, &parent, "review");
    is_conflict(t.store.workflows().claim(&parent, "integrate"), "前置");
    finish(&t, &review, true);
    let integrate = start(&t, &parent, "integrate");
    let complete = finish(&t, &integrate, true);
    assert!(complete.blockers().is_empty());
    // Child success alone does not mutate or close the parent item's lifecycle.
    assert_eq!(
        t.store.items().get(&parent).unwrap().unwrap().phase,
        "active"
    );
    let child = t
        .store
        .items()
        .get(a.item_id.as_deref().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(child.parent.unwrap().item, Some(Some(parent.clone())));
    assert_eq!(child.bot_id.as_deref(), Some("writer"));
}

#[test]
fn claiming_work_preserves_the_requester_and_never_rebinds_an_existing_attempt() {
    for requested in [Some("requester"), None] {
        let (t, parent, specs) = setup();
        t.store
            .transaction::<_, StoreError>(|tx| {
                tx.execute(
                    "UPDATE items SET requested_by=? WHERE id=?",
                    rusqlite::params![requested, parent],
                )?;
                // The Bot owner is not the requester, including for autonomous service work.
                tx.execute("UPDATE bots SET owner='bot-owner'", [])?;
                Ok(())
            })
            .unwrap();
        plan(&t, &parent, &specs);
        let claimed = t.store.workflows().claim(&parent, "a").unwrap();
        let requester = || {
            t.store
                .reader::<_, StoreError>(|c| {
                    Ok(c.query_row(
                        "SELECT requested_by FROM items WHERE id=?",
                        [claimed.item_id.as_deref().unwrap()],
                        |r| r.get::<_, Option<String>>(0),
                    )?)
                })
                .unwrap()
        };
        assert_eq!(requester().as_deref(), requested);
        t.store
            .transaction::<_, StoreError>(|tx| {
                tx.execute(
                    "UPDATE items SET requested_by='someone-else' WHERE id=?",
                    [&parent],
                )?;
                Ok(())
            })
            .unwrap();
        assert_eq!(
            t.store.workflows().claim(&parent, "a").unwrap().item_id,
            claimed.item_id
        );
        assert_eq!(requester().as_deref(), requested);
    }
}

#[test]
fn overlapping_resources_serialize_without_consuming_attempts() {
    let (t, parent, mut specs) = setup();
    specs[1].resources = specs[0].resources.clone();
    plan(&t, &parent, &specs);
    let a = start(&t, &parent, "a");
    is_conflict(t.store.workflows().claim(&parent, "b"), "资源");
    let graph = t.store.workflows().get(&parent).unwrap().unwrap();
    assert_eq!(graph.units[1].attempt, 0);
    assert_eq!(t.store.items().all(WS).unwrap().len(), 2);
    finish(&t, &a, true);
    assert_eq!(start(&t, &parent, "b").attempt, 1);
}

#[test]
fn concurrent_claims_reserve_exactly_one_child_and_respect_capacity() {
    let (t, parent, specs) = setup();
    t.store
        .workflows()
        .replace(&parent, 0, "单并发", &specs, 1, 3)
        .unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
    let handles: Vec<_> = ["a", "b"]
        .into_iter()
        .map(|id| {
            let store = t.store.clone();
            let parent = parent.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                store.workflows().claim(&parent, id)
            })
        })
        .collect();
    barrier.wait();
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    let claimed = results.into_iter().find_map(Result::ok).unwrap();
    assert_eq!(
        t.store
            .workflows()
            .claim(&parent, &claimed.spec.id)
            .unwrap(),
        claimed
    );
    assert_eq!(t.store.items().all(WS).unwrap().len(), 2);
}

#[test]
fn persisted_dispatch_and_session_bindings_recover_without_duplicate_child() {
    let (t, parent, specs) = setup();
    plan(&t, &parent, &specs);
    let first = t.store.workflows().claim(&parent, "a").unwrap();
    let reopened = Store::open(t.store.path(), NoPublisher).unwrap();
    assert_eq!(reopened.workflows().claim(&parent, "a").unwrap(), first);
    let running = start(&t, &parent, "a");
    let reopened = Store::open(t.store.path(), NoPublisher).unwrap();
    assert_eq!(reopened.workflows().claim(&parent, "a").unwrap(), running);
    assert_eq!(reopened.workflows().active().unwrap().len(), 1);
    assert_eq!(reopened.items().all(WS).unwrap().len(), 2);
}

#[test]
fn replan_is_compare_and_swap_and_keeps_completed_or_running_specs_immutable() {
    let (t, parent, mut specs) = setup();
    plan(&t, &parent, &specs);
    let a = start(&t, &parent, "a");
    let graph = t.store.workflows().get(&parent).unwrap().unwrap();
    specs[0].goal = "变更目标".into();
    is_conflict(
        t.store
            .workflows()
            .replace(&parent, 1, "旧计划", &specs, 2, 3),
        "已更新",
    );
    is_conflict(
        t.store
            .workflows()
            .replace(&parent, graph.revision, "更新", &specs, 2, 3),
        "先取消或重置",
    );
    let graph = finish(&t, &a, true);
    is_conflict(
        t.store
            .workflows()
            .replace(&parent, graph.revision, "更新", &specs, 2, 3),
        "先取消或重置",
    );
    let reset = t
        .store
        .workflows()
        .reset(&parent, "a", "新需求使原结果失效")
        .unwrap();
    let changed = t
        .store
        .workflows()
        .replace(&parent, reset.revision, "目标更新", &specs, 3, 4)
        .unwrap();
    assert_eq!(changed.units[0].spec.goal, "变更目标");
    assert_eq!(changed.units[0].attempt, 1);
    assert_eq!(changed.history.last().unwrap().action, "replan");
    let archived = t
        .store
        .workflows()
        .by_child(a.item_id.as_deref().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(archived.0, parent);
    assert_eq!(archived.1.status, "cancelled");
    assert!(!archived.1.outputs.as_array().unwrap().is_empty());
    assert!(
        t.store
            .workflows()
            .deliver(
                a.item_id.as_deref().unwrap(),
                a.session_id.as_deref().unwrap(),
                "旧会话迟到结果",
                &json!([]),
                &json!([{"passed":true}]),
                true,
            )
            .is_err()
    );
}

#[test]
fn reset_invalidates_review_and_integration_and_keeps_their_evidence() {
    let (t, parent, specs) = setup();
    plan(&t, &parent, &specs);
    for id in ["a", "b", "review", "integrate"] {
        finish(&t, &start(&t, &parent, id), true);
    }
    let reset = t
        .store
        .workflows()
        .reset(&parent, "a", "发现实现缺陷")
        .unwrap();
    assert_eq!(
        reset
            .units
            .iter()
            .map(|u| u.status.as_str())
            .collect::<Vec<_>>(),
        ["pending", "succeeded", "pending", "pending"]
    );
    assert_eq!(reset.blockers().len(), 3);
    let invalidated = reset.history.last().unwrap().detail["invalidated"]
        .as_array()
        .unwrap();
    assert_eq!(invalidated.len(), 3);
    assert!(
        invalidated
            .iter()
            .all(|u| !u["outputs"].as_array().unwrap().is_empty())
    );
    assert!(
        invalidated
            .iter()
            .all(|u| !u["checks"].as_array().unwrap().is_empty())
    );
}

#[test]
fn stale_or_other_session_cannot_deliver_and_repeated_delivery_is_idempotent() {
    let (t, parent, specs) = setup();
    plan(&t, &parent, &specs);
    let a = start(&t, &parent, "a");
    is_conflict(
        t.store.workflows().deliver(
            a.item_id.as_deref().unwrap(),
            "other",
            "冒充交付",
            &json!([]),
            &json!([{"passed":true}]),
            true,
        ),
        "当前工作会话",
    );
    is_conflict(
        t.store.workflows().deliver(
            a.item_id.as_deref().unwrap(),
            a.session_id.as_deref().unwrap(),
            "无检查",
            &json!([]),
            &json!([]),
            true,
        ),
        "全部通过",
    );
    is_conflict(
        t.store.workflows().deliver(
            a.item_id.as_deref().unwrap(),
            a.session_id.as_deref().unwrap(),
            "检查失败",
            &json!([]),
            &json!([{"passed":false}]),
            true,
        ),
        "全部通过",
    );
    let first = finish(&t, &a, true);
    let repeated = finish(&t, &a, true);
    assert_eq!(first, repeated);
    is_conflict(
        t.store.workflows().deliver(
            a.item_id.as_deref().unwrap(),
            a.session_id.as_deref().unwrap(),
            "篡改交付",
            &json!([]),
            &json!([{"passed":true}]),
            true,
        ),
        "已经交付",
    );
}

#[test]
fn cancelled_required_work_blocks_acceptance_retry_preserves_budget_and_pausing_stops_claims() {
    let (t, parent, specs) = setup();
    t.store
        .workflows()
        .replace(&parent, 0, "只允许尝试一次", &specs, 2, 1)
        .unwrap();
    let a = start(&t, &parent, "a");
    let paused = t
        .store
        .workflows()
        .pause(&parent, true, "等待确认")
        .unwrap();
    assert!(paused.blockers().contains(&"协作工作已暂停".into()));
    is_conflict(t.store.workflows().claim(&parent, "b"), "暂停");
    assert!(t.store.workflows().active().unwrap().is_empty());
    assert_eq!(t.store.workflows().all_open().unwrap().len(), 1);
    is_conflict(t.store.workflows().reset(&parent, "a", "重试"), "仍在运行");
    t.store
        .workflows()
        .cancel(&parent, "a", "取消执行")
        .unwrap();
    is_conflict(
        t.store.workflows().deliver(
            a.item_id.as_deref().unwrap(),
            a.session_id.as_deref().unwrap(),
            "迟到交付",
            &json!([]),
            &json!([{"passed":true}]),
            true,
        ),
        "失效",
    );
    is_conflict(
        t.store.workflows().reset(&parent, "a", "恢复尝试"),
        "尝试预算",
    );
    let graph = t.store.workflows().get(&parent).unwrap().unwrap();
    t.store
        .workflows()
        .replace(&parent, graph.revision, "增加重试预算", &specs, 2, 2)
        .unwrap();
    t.store.workflows().reset(&parent, "a", "恢复尝试").unwrap();
    t.store
        .workflows()
        .pause(&parent, false, "继续执行")
        .unwrap();
    assert_eq!(t.store.workflows().claim(&parent, "a").unwrap().attempt, 2);
    assert_eq!(
        t.store
            .workflows()
            .get(&parent)
            .unwrap()
            .unwrap()
            .blockers()
            .len(),
        4
    );
}

#[test]
fn only_own_workspace_bots_and_child_sessions_can_participate() {
    let (t, parent, mut specs) = setup();
    specs[0].bot_id = "not-a-member".into();
    is_conflict(
        t.store
            .workflows()
            .replace(&parent, 0, "分配", &specs, 2, 3),
        "工作空间",
    );
    specs[0].bot_id = "writer".into();
    plan(&t, &parent, &specs);
    let a = t.store.workflows().claim(&parent, "a").unwrap();
    let other = t
        .store
        .sessions()
        .create(&NewSession {
            workspace_id: WS.into(),
            item_id: Some(parent.clone()),
            bot_id: Some("writer".into()),
            harness: "fixture".into(),
            cwd: ".".into(),
            token: "other-token".into(),
            ..Default::default()
        })
        .unwrap();
    is_conflict(
        t.store
            .workflows()
            .bind_session(&parent, "a", a.item_id.as_deref().unwrap(), &other.id),
        "不匹配",
    );
    let mut nested = specs.clone();
    nested[0].id = "inner".into();
    nested[2].depends_on = vec!["inner".into(), "b".into()];
    is_conflict(
        t.store
            .workflows()
            .replace(a.item_id.as_deref().unwrap(), 0, "嵌套", &nested, 2, 3),
        "嵌套",
    );
}

#[test]
fn shared_project_directory_is_refused_before_creating_child() {
    let (t, parent, specs) = setup();
    let project = t
        .store
        .projects()
        .create(
            WS,
            &NewProject {
                name: "项目".into(),
                kind: "git".into(),
                path: "project".into(),
                worktree: false,
                ..Default::default()
            },
        )
        .unwrap();
    t.store
        .items()
        .patch(
            &parent,
            &ItemPatch {
                project_id: Some(Some(project.id.clone())),
                ..Default::default()
            },
        )
        .unwrap();
    plan(&t, &parent, &specs);
    is_conflict(t.store.workflows().claim(&parent, "a"), "独立 Git worktree");
    assert_eq!(t.store.items().all(WS).unwrap().len(), 1);
    t.store
        .projects()
        .patch(
            &project.id,
            &ProjectPatch {
                worktree: Some(true),
                ..Default::default()
            },
            "test",
        )
        .unwrap();
    let claimed = t.store.workflows().claim(&parent, "a").unwrap();
    assert_eq!(
        t.store
            .items()
            .get(claimed.item_id.as_deref().unwrap())
            .unwrap()
            .unwrap()
            .project_id,
        Some(project.id)
    );
}

#[test]
fn migration_applies_once_on_v40_and_leaves_existing_items_unchanged() {
    let c = rusqlite::Connection::open_in_memory().unwrap();
    c.execute_batch(
        "CREATE TABLE schema_version(v INTEGER NOT NULL); INSERT INTO schema_version VALUES(40);",
    )
    .unwrap();
    for m in &db::MIGRATIONS[..40] {
        c.execute_batch(m).unwrap();
    }
    c.execute_batch("INSERT INTO items(id,workspace_id,no,title,status,stage,created,updated) VALUES('old','default',1,'旧事项','run','执行',1,1);").unwrap();
    db::migrate(&c).unwrap();
    db::migrate(&c).unwrap();
    assert_eq!(db::compat(&c).unwrap().schema, db::SCHEMA_VERSION);
    let title: String = c
        .query_row("SELECT title FROM items WHERE id='old'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(title, "旧事项");
    let check: Value = json!(
        c.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
            .unwrap()
    );
    assert_eq!(check, json!("ok"));
}

#[test]
fn new_goal_or_cycle_archives_old_results_without_reusing_them() {
    let (t, parent, specs) = setup();
    t.store
        .workflows()
        .replace(&parent, 0, "首次计划", &specs, 2, 1)
        .unwrap();
    for id in ["a", "b", "review", "integrate"] {
        finish(&t, &start(&t, &parent, id), true);
    }
    let graph = t.store.workflows().get(&parent).unwrap().unwrap();
    assert_eq!((graph.cycle, graph.goal_version), (1, 1));
    t.store
        .items()
        .add_goal(&parent, "追加目标，需要重新检查全部结果")
        .unwrap();
    is_conflict(
        t.store
            .workflows()
            .replace(&parent, graph.revision, "新目标", &specs, 2, 1),
        "先暂停",
    );
    is_conflict(t.store.workflows().reset(&parent, "a", "重做"), "尝试预算");
    let paused = t
        .store
        .workflows()
        .pause(&parent, true, "新目标待规划")
        .unwrap();
    let replanned = t
        .store
        .workflows()
        .replace(&parent, paused.revision, "采用新目标", &specs, 2, 2)
        .unwrap();
    assert_eq!((replanned.cycle, replanned.goal_version), (1, 2));
    assert!(
        replanned
            .units
            .iter()
            .all(|u| u.status == "pending" && u.attempt == 0)
    );
    assert!(replanned.paused);
    t.store
        .workflows()
        .pause(&parent, false, "开始新目标")
        .unwrap();
    let a = start(&t, &parent, "a");
    finish(&t, &a, true);
    t.store
        .with_conn(|c| {
            c.execute("UPDATE items SET cycle=cycle+1 WHERE id=?", [&parent])?;
            Ok(())
        })
        .unwrap();
    let old = t.store.workflows().get(&parent).unwrap().unwrap();
    is_conflict(
        t.store
            .workflows()
            .replace(&parent, old.revision, "新周期", &specs, 2, 2),
        "先暂停",
    );
    is_conflict(t.store.workflows().claim(&parent, "b"), "执行周期");
    let paused = t
        .store
        .workflows()
        .pause(&parent, true, "新周期待规划")
        .unwrap();
    let next = t
        .store
        .workflows()
        .replace(&parent, paused.revision, "采用新周期", &specs, 2, 2)
        .unwrap();
    assert_eq!((next.cycle, next.goal_version), (2, 2));
    assert!(
        next.units
            .iter()
            .all(|u| u.status == "pending" && u.attempt == 0)
    );
}

#[test]
fn an_exhausted_downstream_budget_does_not_erase_upstream_results_on_reset() {
    let (t, parent, specs) = setup();
    t.store
        .workflows()
        .replace(&parent, 0, "原始预算", &specs, 2, 2)
        .unwrap();
    for id in ["a", "b", "review", "integrate"] {
        finish(&t, &start(&t, &parent, id), true);
    }
    t.store
        .workflows()
        .reset(&parent, "review", "复审")
        .unwrap();
    finish(&t, &start(&t, &parent, "review"), true);
    let before = t.store.workflows().get(&parent).unwrap().unwrap();
    is_conflict(
        t.store.workflows().reset(&parent, "a", "改动实现"),
        "下游已达到",
    );
    assert_eq!(before, t.store.workflows().get(&parent).unwrap().unwrap());
}

#[test]
fn unit_delivery_artifacts_and_child_checkpoint_commit_or_roll_back_together() {
    let (t, parent, specs) = setup();
    plan(&t, &parent, &specs);
    let a = start(&t, &parent, "a");
    let child = a.item_id.as_deref().unwrap();
    let session = a.session_id.as_deref().unwrap();
    let outputs = json!([{"kind":"文件","title":"成果","location":"result.txt"}]);
    let checks = json!([{"name":"验证","passed":true}]);
    t.events.take();
    let failed: Result<()> = t.store.transaction(|tx| {
        workflows::deliver_in(tx, child, session, "交付", &outputs, &checks, true)?;
        artifact_add_in(
            tx,
            child,
            "文件",
            "成果",
            Some("result.txt"),
            Some("result"),
        )?;
        Err(StoreError::Conflict("模拟写入检查点失败".into()))
    });
    assert!(failed.is_err());
    assert_eq!(
        t.store.workflows().get(&parent).unwrap().unwrap().units[0].status,
        "running"
    );
    assert!(t.store.artifacts().list(child).unwrap().is_empty());
    assert!(t.events.take().is_empty());
    t.store
        .transaction::<_, StoreError>(|tx| {
            workflows::deliver_in(tx, child, session, "交付", &outputs, &checks, true)?;
            artifact_add_in(
                tx,
                child,
                "文件",
                "成果",
                Some("result.txt"),
                Some("result"),
            )?;
            tx.execute("UPDATE items SET phase='closing' WHERE id=?", [child])?;
            Ok(())
        })
        .unwrap();
    assert_eq!(
        t.store.workflows().get(&parent).unwrap().unwrap().units[0].status,
        "succeeded"
    );
    assert_eq!(
        t.store.items().get(child).unwrap().unwrap().phase,
        "closing"
    );
    assert_eq!(t.store.artifacts().list(child).unwrap().len(), 1);
    assert_eq!(t.store.artifacts().roles(child).unwrap().len(), 1);
}
