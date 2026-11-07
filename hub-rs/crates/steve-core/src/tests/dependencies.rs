use serde_json::{Value, json};

use crate::repo::{Session, WS};
use crate::runtime::AgentRuntime;
use crate::testkit::Kit;
use crate::{Core, CoreConfig, ItemAction, patch};

fn setup(k: &Kit) -> (String, String, Session) {
    let item = k.item("run", "执行", None);
    let dep = k.item("run", "执行", None);
    let sid = k.session(&item, "idle");
    let session = k.db().session(&sid).unwrap().unwrap();
    k.rt.prompt(&sid, "开始", "steve", &[]).unwrap();
    (item, dep, session)
}

fn wait(k: &Kit, s: &Session, deps: &[&str], resume: &str) -> Value {
    k.core
        .tool_item_dependencies(
            s,
            json!({
                "action":"wait","deps":deps,"reason":"等待隔离执行就绪","resume":resume
            }),
        )
        .unwrap()
}

fn complete(k: &Kit, id: &str) {
    k.db()
        .exec(
            "UPDATE items SET status='done',phase='closed',outcome='completed' WHERE id=?",
            &[&id],
        )
        .unwrap();
}

#[test]
fn item_page_registers_the_existing_dependency_without_running_a_turn() {
    let k = Kit::new();
    let (item, dep, s) = setup(&k);
    k.rt.end_turn(&k.core, &s.id, "end_turn", None);
    k.core.add_dep(&item, &dep).unwrap();
    let initial = k.core.dependency_get(&item).unwrap();
    assert_eq!(initial["waiting"], false);
    assert_eq!(initial["resume_source"], "workspace");
    assert_eq!(initial["resume_mode"], "automatic");
    let cycle = initial["cycle"].as_i64().unwrap();
    let before = k.rt.prompts_of(&s.id);
    assert!(
        k.core
            .dependency_wait(&item, cycle + 1, "automatic", "isolation")
            .is_err()
    );
    assert!(
        k.db()
            .store()
            .dependency_waits()
            .get(&item)
            .unwrap()
            .is_none()
    );
    let view = k
        .core
        .dependency_wait(&item, cycle, "automatic", "isolation")
        .unwrap();
    assert_eq!(view["waiting"], true);
    assert_eq!(view["resume_source"], "item");
    assert_eq!(view["resume_override"], "automatic");
    let saved = k
        .db()
        .store()
        .dependency_waits()
        .get(&item)
        .unwrap()
        .unwrap();
    assert_eq!(saved.session_id, s.id);
    assert_eq!(saved.reason, "isolation");
    assert_eq!(k.db().deps_of(&item).unwrap(), [dep.clone()]);
    assert_eq!(k.rt.prompts_of(&s.id), before);
    k.core
        .dependency_wait(&item, cycle, "automatic", "isolation")
        .unwrap();
    assert_eq!(
        k.core.dependency_get(&item).unwrap()["wait_state"],
        "waiting"
    );
    assert!(k.db().queued(&s.id).unwrap().is_empty());
    assert!(k.core.item_action(&item, ItemAction::Finish, "me").is_err());
    // A saved policy does not bypass missing collaboration isolation.
    assert!(k.rt.check_workflow_execution(&item).is_err());
    complete(&k, &dep);
    k.core.lifecycle_tick().unwrap();
    k.core.lifecycle_tick().unwrap();
    assert_eq!(
        k.core.dependency_get(&item).unwrap()["wait_state"],
        "started"
    );
    assert_eq!(k.rt.prompts_of(&s.id).len(), before.len() + 1);
}

#[test]
fn busy_turn_yields_to_block_without_watchdog_or_early_delivery() {
    let k = Kit::new();
    let (item, dep, s) = setup(&k);
    k.db()
        .store()
        .diagnostics()
        .capture(WS, &item, "test", None)
        .unwrap();
    k.decision(&item, Some(&s.id), "ask", "question", &["a"]);
    let view = wait(&k, &s, &[&dep], "automatic");
    assert_eq!(view["status"], "block");
    assert_eq!(view["active_turn"], true);
    assert_eq!(view["end_turn"], true);
    assert_eq!(view["resume_source"], "item");
    assert_eq!(view["open_dependencies"], json!([dep]));
    assert!(k.open_kinds(&item).is_empty());
    assert!(k.core.tool_deliver(&s, "不能提前", &[], &[], &[]).is_err());
    assert!(
        k.core
            .tool_publish(&s, "steve/example", "不能提前发布")
            .unwrap_err()
            .to_string()
            .contains("不能请求发布")
    );
    assert!(k.core.item_action(&item, ItemAction::Finish, "me").is_err());
    assert!(
        k.db()
            .exec(
                "UPDATE items SET phase='closed',outcome='completed',status='done' WHERE id=?",
                &[&item]
            )
            .is_err()
    );
    k.rt.end_turn(&k.core, &s.id, "end_turn", None);
    k.core.check_stalls().unwrap();
    assert_eq!(k.status(&item).0, "block");
    assert!(k.open_kinds(&item).is_empty());
    assert_eq!(k.rt.prompts_of(&s.id).len(), 1);
    assert!(k.rt.prompt(&s.id, "抢跑", "steve", &[]).is_err());
    // Read-only diagnosis must explain this real wait without prompting, settling or
    // manufacturing another card. Reuse this scenario instead of duplicating its setup.
    let before = (
        k.status(&item),
        k.timeline_texts(&item),
        k.rt.prompts_of(&s.id),
    );
    let snapshot = k.core.diagnostic_snapshot(&item).unwrap();
    assert_eq!(snapshot["dependencies"]["resume_mode"], "automatic");
    assert_eq!(snapshot["dependencies"]["open_dependencies"], json!([dep]));
    let until = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        let events = k
            .db()
            .store()
            .diagnostics()
            .events(
                WS,
                &item,
                steve_store::diagnostics::Filter {
                    reason_code: Some("dependency_wait".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        if !events["events"].as_array().unwrap().is_empty() {
            break;
        }
        assert!(
            std::time::Instant::now() < until,
            "missing persisted wait evidence: {events}"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(
        (
            k.status(&item),
            k.timeline_texts(&item),
            k.rt.prompts_of(&s.id)
        ),
        before
    );
}

#[test]
fn automatic_override_resumes_exactly_once_after_active_turn_even_if_deps_already_done() {
    let k = Kit::new();
    let (item, dep, s) = setup(&k);
    k.db()
        .settings_set(
            WS,
            json!({"depsAfter":"提醒我开始"}).as_object().unwrap(),
            "me",
        )
        .unwrap();
    complete(&k, &dep);
    wait(&k, &s, &[&dep], "automatic");
    assert!(
        k.db()
            .exec(
                "UPDATE items SET phase='closed',outcome='completed',status='done' WHERE id=?",
                &[&item]
            )
            .is_err(),
        "a completed prerequisite cannot close the parent before the registered wait has been consumed"
    );
    k.core.lifecycle_tick().unwrap();
    assert_eq!(k.rt.prompts_of(&s.id).len(), 1);
    let notes = k.timeline_texts(&item).len();
    wait(&k, &s, &[&dep], "automatic");
    assert_eq!(
        k.timeline_texts(&item).len(),
        notes,
        "repeated registration is idempotent"
    );
    k.rt.end_turn(&k.core, &s.id, "end_turn", None);
    assert_eq!(k.status(&item).0, "run");
    assert_eq!(k.rt.prompts_of(&s.id).len(), 2);
    assert!(k.rt.prompts_of(&s.id)[1].contains("不要重复"));
    k.core.lifecycle_tick().unwrap();
    let retry = wait(&k, &s, &[&dep], "automatic");
    assert_eq!(retry["wait_state"], "started");
    assert_eq!(retry["end_turn"], false);
    assert_eq!(k.rt.prompts_of(&s.id).len(), 2);
    assert_eq!(
        k.db().setting(WS, "depsAfter", "").unwrap(),
        "提醒我开始",
        "item override never alters workspace policy"
    );
}

#[test]
fn bot_membership_allows_wait_and_resume_across_home_workspaces_and_revocation_stops_it() {
    for revoke in [false, true] {
        let k = Kit::new();
        let (item, dep, s) = setup(&k);
        k.db()
            .exec(
                "INSERT INTO workspaces(id,name,created,item_prefix) VALUES('other','Other',1,'OT')",
                &[],
            )
            .unwrap();
        k.db()
            .store()
            .bots()
            .seed(
                "shared",
                false,
                &steve_store::NewBot {
                    name: "Shared worker".into(),
                    harness: "test-adapter".into(),
                    ..Default::default()
                },
                "other",
            )
            .unwrap();
        k.db()
            .store()
            .bots()
            .set_workspaces("shared", &["other".into(), WS.into()])
            .unwrap();
        k.db().item_patch(&item, patch![bot_id:"shared"]).unwrap();
        k.db()
            .exec("UPDATE sessions SET bot_id='shared' WHERE id=?", &[&s.id])
            .unwrap();
        let s = k.db().session(&s.id).unwrap().unwrap();
        assert_eq!(wait(&k, &s, &[&dep], "automatic")["status"], "block");
        if revoke {
            k.db()
                .store()
                .bots()
                .set_workspaces("shared", &["other".into()])
                .unwrap();
        }
        complete(&k, &dep);
        k.rt.end_turn(&k.core, &s.id, "end_turn", None);
        k.core.lifecycle_tick().unwrap();
        assert_eq!(k.rt.prompts_of(&s.id).len(), if revoke { 1 } else { 2 });
        assert_eq!(k.status(&item).0, if revoke { "wait" } else { "run" });
        assert_eq!(
            k.db()
                .store()
                .dependency_waits()
                .get(&item)
                .unwrap()
                .unwrap()
                .state,
            if revoke { "notified" } else { "started" }
        );
    }
}

#[test]
fn notify_and_workspace_modes_prompt_once_and_user_can_start() {
    for mode in ["notify", "workspace"] {
        let k = Kit::new();
        let (item, dep, s) = setup(&k);
        if mode == "workspace" {
            k.db()
                .settings_set(
                    WS,
                    json!({"depsAfter":"提醒我开始"}).as_object().unwrap(),
                    "me",
                )
                .unwrap();
        }
        wait(&k, &s, &[&dep], mode);
        k.rt.end_turn(&k.core, &s.id, "end_turn", None);
        complete(&k, &dep);
        k.core.lifecycle_tick().unwrap();
        k.core.lifecycle_tick().unwrap();
        assert_eq!(k.status(&item).0, "wait");
        assert_eq!(k.open_kinds(&item), vec!["propose"]);
        assert_eq!(k.rt.prompts_of(&s.id).len(), 1);
        let d = k
            .db()
            .decisions_of_item(&item)
            .unwrap()
            .into_iter()
            .find(|d| d.status == "open")
            .unwrap();
        k.core.resolve(&d.id, "yes", "", "me").unwrap();
        assert_eq!(k.status(&item).0, "run");
        assert!(k.rt.prompts_of(&s.id).len() > 1);
    }
}

#[test]
fn invalid_batch_missing_self_cross_workspace_and_cycle_leave_no_partial_changes() {
    for invalid in ["missing", "self", "workspace", "cycle", "workflow"] {
        let k = Kit::new();
        let (item, dep, s) = setup(&k);
        let other = k.item("run", "执行", None);
        let bad = match invalid {
            "missing" => "missing-item".into(),
            "self" => item.clone(),
            "workspace" => {
                k.db()
                    .exec(
                        "UPDATE items SET workspace_id='other-workspace' WHERE id=?",
                        &[&other],
                    )
                    .unwrap();
                other.clone()
            }
            "cycle" => {
                k.db().dep_add(&other, &item).unwrap();
                other.clone()
            }
            "workflow" => {
                k.db()
                    .exec(
                        "UPDATE items SET source='[\"协作\",\"workflow\"]' WHERE id=?",
                        &[&other],
                    )
                    .unwrap();
                other.clone()
            }
            _ => unreachable!(),
        };
        assert!(
            k.core
                .tool_item_dependencies(
                    &s,
                    json!({"action":"wait","deps":[dep,bad],"reason":"bad"})
                )
                .is_err()
        );
        assert!(k.db().deps_of(&item).unwrap().is_empty());
        assert!(
            k.db()
                .store()
                .dependency_waits()
                .get(&item)
                .unwrap()
                .is_none()
        );
        assert_eq!(k.status(&item).0, "run");
    }
}

#[test]
fn retained_dependencies_and_old_sessions_cannot_be_rewritten_by_stale_tokens() {
    let k = Kit::new();
    let (item, dep, s) = setup(&k);
    let old = k.item("run", "执行", None);
    k.db().dep_add(&item, &old).unwrap();
    wait(&k, &s, &[&dep], "automatic");
    assert_eq!(k.db().deps_of(&item).unwrap().len(), 2);
    let newer = k.session(&item, "idle");
    assert!(
        k.core
            .tool_item_dependencies(&s, json!({"action":"get"}))
            .is_err()
    );
    let current = k.db().session(&newer).unwrap().unwrap();
    k.db()
        .exec("UPDATE sessions SET readonly=1 WHERE id=?", &[&newer])
        .unwrap();
    assert!(
        k.core
            .tool_item_dependencies(&current, json!({"action":"get"}))
            .is_err()
    );
}

#[test]
fn owner_bot_and_workflow_attempt_authority_are_rechecked_in_database() {
    for mutate in [
        "bot",
        "workspace",
        "disabled",
        "membership",
        "deleted",
        "source",
        "device",
        "child",
        "ended",
    ] {
        let k = Kit::new();
        let (item, dep, s) = setup(&k);
        match mutate {
            "bot" => {
                k.db().item_patch(&item, patch![bot_id:"steve"]).unwrap();
            }
            "workspace" => {
                k.db()
                    .exec(
                        "UPDATE sessions SET workspace_id='other' WHERE id=?",
                        &[&s.id],
                    )
                    .unwrap();
            }
            "disabled" => {
                k.db()
                    .exec("UPDATE bots SET enabled=0 WHERE id='dev'", &[])
                    .unwrap();
            }
            "membership" => {
                k.db()
                    .exec("DELETE FROM bot_workspaces WHERE bot_id='dev'", &[])
                    .unwrap();
            }
            "deleted" => {
                k.db()
                    .exec("UPDATE bots SET deleted=1 WHERE id='dev'", &[])
                    .unwrap();
            }
            "source" => {
                k.db()
                    .exec("UPDATE sessions SET source='import' WHERE id=?", &[&s.id])
                    .unwrap();
            }
            "device" => {
                k.db()
                    .exec("UPDATE sessions SET device_id='other' WHERE id=?", &[&s.id])
                    .unwrap();
            }
            "child" => {
                k.db()
                    .exec(
                        "UPDATE items SET source='[\"协作\",\"workflow\"]' WHERE id=?",
                        &[&item],
                    )
                    .unwrap();
            }
            "ended" => {
                k.db().session_patch(&s.id, patch![status:"ended"]).unwrap();
            }
            _ => unreachable!(),
        }
        assert!(
            k.core
                .tool_item_dependencies(&s, json!({"action":"wait","deps":[dep],"reason":"bad"}))
                .is_err(),
            "{mutate}"
        );
        assert!(k.db().deps_of(&item).unwrap().is_empty());
    }
}

#[test]
fn interrupted_wait_recovers_durably_without_losing_or_duplicating_wakeup() {
    let k = Kit::new();
    let (item, dep, s) = setup(&k);
    wait(&k, &s, &[&dep], "automatic");
    // Rebuild the Core: no in-memory markers survive. Runtime recovery finds the saved session.
    k.rt.st().turning.clear();
    k.rt.st().attempts.clear();
    let mut cfg = CoreConfig::for_home(k.home());
    cfg.disk.floor = 0;
    cfg.disk.low = 0;
    let reopened =
        steve_store::Store::open(k.db().store().path(), steve_store::NoPublisher).unwrap();
    let core = Core::new(
        crate::repo::Db::new(reopened),
        k.rt.clone(),
        k.gw.clone(),
        cfg,
    );
    core.recover().unwrap();
    core.ready();
    assert_eq!(k.status(&item).0, "block");
    assert_eq!(k.rt.prompts_of(&s.id).len(), 1);
    assert!(k.open_kinds(&item).is_empty());
    complete(&k, &dep);
    core.lifecycle_tick().unwrap();
    assert_eq!(k.rt.prompts_of(&s.id).len(), 2);
    core.lifecycle_tick().unwrap();
    assert_eq!(k.rt.prompts_of(&s.id).len(), 2);
    assert_eq!(
        core.tool_item_dependencies(&s, json!({"action":"get"}))
            .unwrap()["wait_state"],
        "started"
    );
}

#[test]
fn moving_a_deploy_cut_wait_to_node_keeps_it_blocked_until_prerequisites_finish() {
    let k = Kit::new();
    let (item, dep, s) = setup(&k);
    wait(&k, &s, &[&dep], "automatic");
    k.db()
        .event_add(
            &s.id,
            "system",
            json!({"text": "为了升级暂停了这一回合", "drain_cut": true}),
        )
        .unwrap();
    k.rt.st().turning.clear();
    k.rt.st().attempts.clear();

    assert!(!k.core.moved_to_node(&s.id).unwrap());
    assert_eq!(k.status(&item).0, "block");
    assert!(k.db().queued(&s.id).unwrap().is_empty());
    assert!(k.open_kinds(&item).is_empty());
    k.core.ready();
    assert_eq!(k.rt.prompts_of(&s.id).len(), 1);
    assert_eq!(
        k.core
            .tool_item_dependencies(&s, json!({"action":"get"}))
            .unwrap()["wait_state"],
        "waiting"
    );

    complete(&k, &dep);
    k.core.lifecycle_tick().unwrap();
    assert_eq!(k.rt.prompts_of(&s.id).len(), 2);
    k.core.lifecycle_tick().unwrap();
    assert_eq!(k.rt.prompts_of(&s.id).len(), 2);
}

#[test]
fn wakeup_queue_survives_a_restart_after_transaction_before_runtime_flush() {
    let k = Kit::new();
    let (item, dep, s) = setup(&k);
    wait(&k, &s, &[&dep], "automatic");
    k.rt.end_turn(&k.core, &s.id, "end_turn", None);
    complete(&k, &dep);
    k.rt.st().flush_stuck = true;
    k.core.lifecycle_tick().unwrap();
    assert_eq!(k.status(&item).0, "run");
    assert_eq!(k.db().queued(&s.id).unwrap().len(), 1);
    k.core.lifecycle_tick().unwrap();
    assert_eq!(k.db().queued(&s.id).unwrap().len(), 1);
    k.rt.st().flush_stuck = false;
    k.core.recover().unwrap();
    k.core.ready();
    assert_eq!(k.rt.prompts_of(&s.id).len(), 2);
    assert!(k.db().queued(&s.id).unwrap().is_empty());
}

#[test]
fn previous_cycle_wait_does_not_override_a_new_cycle() {
    let k = Kit::new();
    let (item, dep, s) = setup(&k);
    wait(&k, &s, &[&dep], "notify");
    // A new cycle has separate state. Real lifecycle coverage owns the reopen/stop handshake.
    k.db()
        .exec(
            "UPDATE items SET cycle=cycle+1,status='run' WHERE id=?",
            &[&item],
        )
        .unwrap();
    let v = k
        .core
        .tool_item_dependencies(&s, json!({"action":"get"}))
        .unwrap();
    assert_eq!(v["resume_source"], "workspace");
    assert_eq!(v["waiting"], false);
}

#[test]
fn reopened_prerequisite_can_be_waited_on_again_with_the_same_request() {
    let k = Kit::new();
    let (item, dep, s) = setup(&k);
    complete(&k, &dep);
    wait(&k, &s, &[&dep], "automatic");
    k.rt.end_turn(&k.core, &s.id, "end_turn", None);
    assert_eq!(k.status(&item).0, "run");
    k.db()
        .exec(
            "UPDATE items SET phase='active',outcome=NULL,status='run',cycle=cycle+1 WHERE id=?",
            &[&dep],
        )
        .unwrap();
    let v = wait(&k, &s, &[&dep], "automatic");
    assert_eq!(v["status"], "block");
    assert_eq!(v["waiting"], true);
    k.rt.end_turn(&k.core, &s.id, "end_turn", None);
    assert_eq!(k.rt.prompts_of(&s.id).len(), 2);
    complete(&k, &dep);
    k.core.lifecycle_tick().unwrap();
    assert_eq!(k.rt.prompts_of(&s.id).len(), 3);
}

#[test]
fn revocation_during_wait_never_starts_execution_and_fallback_is_explicit() {
    for revoke in ["disabled", "member", "ended"] {
        let k = Kit::new();
        let (item, dep, s) = setup(&k);
        wait(&k, &s, &[&dep], "automatic");
        k.rt.end_turn(&k.core, &s.id, "end_turn", None);
        match revoke {
            "disabled" => {
                k.db()
                    .exec("UPDATE bots SET enabled=0 WHERE id='dev'", &[])
                    .unwrap();
            }
            "member" => {
                k.db()
                    .exec("DELETE FROM bot_workspaces WHERE bot_id='dev'", &[])
                    .unwrap();
            }
            "ended" => {
                k.db().session_patch(&s.id, patch![status:"ended"]).unwrap();
            }
            _ => unreachable!(),
        }
        complete(&k, &dep);
        k.core.lifecycle_tick().unwrap();
        assert_eq!(k.status(&item).0, "wait");
        assert_eq!(k.rt.prompts_of(&s.id).len(), 1);
        let view = k.core.tx().dependency_view(&item).unwrap();
        assert_eq!(view["resume_mode"], "notify");
        assert_eq!(view["resume_override"], "automatic");
        assert_eq!(view["resume_source"], "fallback");
        assert!(
            view["resume_fallback_reason"]
                .as_str()
                .unwrap()
                .contains("提醒开始")
        );
    }
}

#[test]
fn pause_then_resume_consumes_wait_only_when_prerequisites_are_complete() {
    for completed in [false, true] {
        let k = Kit::new();
        let (item, dep, s) = setup(&k);
        wait(&k, &s, &[&dep], "automatic");
        k.rt.end_turn(&k.core, &s.id, "end_turn", None);
        k.core.item_action(&item, ItemAction::Pause, "me").unwrap();
        if completed {
            complete(&k, &dep);
        }
        k.core.lifecycle_tick().unwrap();
        assert_eq!(k.status(&item).0, "pause");
        assert_eq!(k.rt.prompts_of(&s.id).len(), 1);
        k.core.item_action(&item, ItemAction::Resume, "me").unwrap();
        assert_eq!(k.status(&item).0, if completed { "run" } else { "block" });
        assert_eq!(k.rt.prompts_of(&s.id).len(), if completed { 2 } else { 1 });
        if completed {
            k.core.tool_deliver(&s, "已完成", &[], &[], &[]).unwrap();
        }
    }
}

#[test]
fn live_collaboration_must_stop_before_registering_a_wait() {
    let k = Kit::new();
    let (item, dep, s) = setup(&k);
    k.rt.st().workflow_execution_ready = true;
    let spec = |id: &str, bot: &str, kind: &str, deps: Vec<&str>| {
        json!({
            "id":id,"title":id,"goal":id,"bot_id":bot,"kind":kind,
            "depends_on":deps,"resources":[],"required":true
        })
    };
    k.core.workflow_change(&item,json!({
        "action":"plan","reason":"协作","expected_revision":0,"max_parallel":1,"max_attempts":2,
        "units":[spec("work","dev","work",vec![]),spec("review","steve","review",vec!["work"]),
            spec("integration","dev","integrate",vec!["review"])]
    }),"me").unwrap();
    assert!(
        k.core
            .tool_item_dependencies(&s, json!({"action":"wait","deps":[dep],"reason":"先停止"}))
            .is_err()
    );
    assert!(k.db().deps_of(&item).unwrap().is_empty());
    let graph = k.db().store().workflows().get(&item).unwrap().unwrap();
    let child = graph.units.iter().find_map(|u| u.item_id.clone()).unwrap();
    // Even after stop is confirmed, the child cannot become its parent's prerequisite.
    k.core
        .workflow_change(
            &item,
            json!({"action":"pause","reason":"停止","expected_revision":graph.revision}),
            "me",
        )
        .unwrap();
    for u in graph.units {
        if let Some(sid) = u.session_id {
            k.rt.end_turn(&k.core, &sid, "cancelled", None);
        }
    }
    assert!(
        k.core
            .tool_item_dependencies(
                &s,
                json!({"action":"wait","deps":[dep,child],"reason":"隐式环"})
            )
            .is_err()
    );
    assert!(k.db().deps_of(&item).unwrap().is_empty());
    wait(&k, &s, &[&dep], "automatic");
    assert_eq!(k.status(&item).0, "block");
}

#[test]
fn ordinary_dependency_added_after_wakeup_rearms_the_saved_policy() {
    for mode in ["automatic", "notify"] {
        let k = Kit::new();
        let (item, dep, s) = setup(&k);
        complete(&k, &dep);
        wait(&k, &s, &[&dep], mode);
        k.rt.end_turn(&k.core, &s.id, "end_turn", None);
        let next = k.item("run", "执行", None);
        k.core.add_dep(&item, &next).unwrap();
        assert_eq!(k.status(&item).0, "block");
        assert!(k.open_kinds(&item).is_empty());
        assert!(k.core.tx().dependency_waiting(&item).unwrap());
        if mode == "automatic" {
            k.rt.end_turn(&k.core, &s.id, "end_turn", None);
        }
        complete(&k, &next);
        k.core.lifecycle_tick().unwrap();
        if mode == "automatic" {
            assert_eq!(k.status(&item).0, "run");
            assert_eq!(k.rt.prompts_of(&s.id).len(), 3);
        } else {
            assert_eq!(k.status(&item).0, "wait");
            assert_eq!(k.open_kinds(&item), vec!["propose"]);
            assert_eq!(k.rt.prompts_of(&s.id).len(), 1);
        }
    }
}
