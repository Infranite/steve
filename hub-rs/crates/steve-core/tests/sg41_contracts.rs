//! SG41: public contracts on temporary stores and in-process ports, before extraction.
use async_trait::async_trait;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use steve_core::gateway::{ModelGateway, Printed, ThinkTools, Tier};
use steve_core::repo::{Db, FileRef, NewDecision, NewItem, NewSession, Release, Session, WS};
use steve_core::runtime::{AgentRuntime, Answered, Flushed, Prompted, Released, Running, Stop};

use steve_core::{Core, CoreConfig, Result, patch};
use steve_node_api as na;
use steve_store::{NewBot, NoPublisher, Store, UnitSpec};

#[derive(Default)]
struct Runtime {
    busy: Mutex<HashSet<String>>,
    holding: AtomicBool,
    ready: AtomicBool,
    db: Mutex<Option<Db>>,
    prompts: Mutex<Vec<(String, String, String)>>,
    flushes: Mutex<Vec<String>>,
}
impl AgentRuntime for Runtime {
    fn prompt(&self, sid: &str, text: &str, via: &str, _: &[FileRef]) -> Result<Prompted> {
        self.prompts
            .lock()
            .unwrap()
            .push((sid.into(), text.into(), via.into()));
        self.busy.lock().unwrap().insert(sid.into());
        let db = self.db.lock().unwrap().clone().unwrap();
        db.session_patch(sid, patch![status:"running"])?;
        db.event_add(sid, "user", json!({"text":text,"via":via}))?;
        Ok(Prompted::Sent)
    }
    fn running(&self, s: &str) -> Option<Running> {
        self.is_busy(s).then(|| Running {
            session: s.into(),
            attempt: "fixture".into(),
        })
    }
    fn stop(&self, turn: &Running) -> Result<Stop> {
        self.idle(&turn.session);
        Ok(Stop::Requested)
    }
    fn is_busy(&self, s: &str) -> bool {
        self.busy.lock().unwrap().contains(s)
    }
    fn release(&self, _: &str) -> Result<Released> {
        Ok(Released::Gone)
    }
    fn answer_permission(&self, _: &str, _: Option<&str>) -> Result<Answered> {
        Ok(Answered::Gone)
    }
    fn hold(&self, on: bool) {
        self.holding.store(on, Ordering::SeqCst)
    }
    fn is_holding(&self) -> bool {
        self.holding.load(Ordering::SeqCst)
    }
    fn running_turns(&self) -> usize {
        self.busy.lock().unwrap().len()
    }
    fn flush(&self, sid: &str, _: Option<(&str, &str)>) -> Flushed {
        self.flushes.lock().unwrap().push(sid.into());
        Flushed::Empty
    }
    fn check_workflow_execution(&self, _: &str) -> Result<()> {
        if self.ready.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err(steve_core::CoreError::Msg(
                "fixture admission denied".into(),
            ))
        }
    }
}
impl Runtime {
    fn idle(&self, sid: &str) {
        self.busy.lock().unwrap().remove(sid);
        self.db
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .session_patch(sid, patch![status:"idle"])
            .unwrap();
    }
}
#[derive(Default)]
struct Gateway(Mutex<Vec<String>>);
#[async_trait]
impl ModelGateway for Gateway {
    async fn call(&self, p: &str, _: Tier, _: Duration, _: &ThinkTools) -> Result<Printed> {
        self.0.lock().unwrap().push(p.into());
        Ok(Printed{text:r#"{"verdict":"done","criteria":[{"id":"goal","status":"met","evidence":["final_text"]}],"reason":"fixture"}"#.into(),model:"fixture-model".into(),reported_model:Some("fixture-model".into()),attempts:1,..Default::default()})
    }
}
struct Kit {
    home: tempfile::TempDir,
    db: Db,
    core: Core,
    rt: Arc<Runtime>,
    gw: Arc<Gateway>,
}
impl Kit {
    fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let store = Store::open(home.path().join("fixture.sqlite3"), NoPublisher).unwrap();
        store.ensure_workspace().unwrap();
        store
            .bots()
            .seed(
                "dev",
                false,
                &NewBot {
                    name: "Dev".into(),
                    harness: "claude-code".into(),
                    accept_mode: Some("manual".into()),
                    ..Default::default()
                },
                WS,
            )
            .unwrap();
        store
            .bots()
            .seed(
                "reviewer",
                false,
                &NewBot {
                    name: "Reviewer".into(),
                    harness: "claude-code".into(),
                    accept_mode: Some("manual".into()),
                    ..Default::default()
                },
                WS,
            )
            .unwrap();
        let db = Db::new(store);
        let rt = Arc::new(Runtime::default());
        *rt.db.lock().unwrap() = Some(db.clone());
        let gw = Arc::new(Gateway::default());
        let mut cfg = CoreConfig::for_home(home.path());
        cfg.port = 18990;
        cfg.no_resume = false;
        cfg.dynamic_workflows = true;
        cfg.gh = "/usr/bin/false".into();
        cfg.takeover_continues = 2;
        let core = Core::new(db.clone(), rt.clone(), gw.clone(), cfg);
        Self {
            home,
            db,
            core,
            rt,
            gw,
        }
    }
    fn item(&self, title: &str) -> String {
        let tag = self
            .db
            .kinds(WS)
            .unwrap()
            .into_iter()
            .find(|t| t.name == "feat")
            .unwrap()
            .id;
        self.db
            .item_create(
                WS,
                &NewItem {
                    title: title.into(),
                    goal: "fixture goal".into(),
                    bot_id: "dev".into(),
                    tags: vec![tag],
                    status: Some("run".into()),
                    stage: Some("执行".into()),
                    ..Default::default()
                },
            )
            .unwrap()
            .id
    }
    fn session(&self, item: &str) -> Session {
        let s = self
            .db
            .session_create(&NewSession {
                workspace_id: WS.into(),
                item_id: Some(item.into()),
                bot_id: Some("dev".into()),
                harness: "claude-code".into(),
                model: None,
                reasoning_effort: None,
                cwd: self.home.path().to_string_lossy().into(),
                title: "fixture".into(),
                token: uuid::Uuid::new_v4().to_string(),
            })
            .unwrap();
        self.db.session_patch(&s.id, patch![status:"idle"]).unwrap();
        self.db.session(&s.id).unwrap().unwrap()
    }
    fn project(&self, rule: Option<&str>, suffix: &str) -> String {
        self.db
            .project_create(
                WS,
                suffix,
                "",
                &self.home.path().join(suffix).to_string_lossy(),
                "dir",
                false,
                Some("dev"),
                Some(&Release {
                    kind: "script".into(),
                    cmd: "/usr/bin/false".into(),
                    done_when: rule.map(str::to_owned),
                }),
            )
            .unwrap()
            .id
    }
}

use steve_core::cycle::{COMPLETE, Closing};
use steve_core::{Check, ItemAction, Output};
fn spec(id: &str, kind: &str, bot: &str, deps: &[&str]) -> UnitSpec {
    UnitSpec {
        id: id.into(),
        title: id.into(),
        goal: format!("complete {id}"),
        kind: kind.into(),
        bot_id: bot.into(),
        depends_on: deps.iter().map(|s| s.to_string()).collect(),
        resources: vec![],
        required: true,
    }
}
fn specs() -> Vec<UnitSpec> {
    vec![
        spec("a", "work", "dev", &[]),
        spec("b", "work", "dev", &[]),
        spec("review", "review", "reviewer", &["a", "b"]),
        spec("integrate", "integrate", "dev", &["review"]),
    ]
}
fn graph(k: &Kit, p: &str) -> steve_store::Workflow {
    k.db.store().workflows().get(p).unwrap().unwrap()
}
fn change(k: &Kit, p: &str, action: &str, extra: Value) -> Result<Value> {
    let mut args = extra;
    args["action"] = json!(action);
    args["reason"] = json!("fixture change");
    args["expected_revision"] = json!(graph(k, p).revision);
    k.core.workflow_change(p, args, "human")
}
fn checks() -> Vec<Check> {
    vec![
        Check {
            name: "unit".into(),
            passed: true,
            scope: Some("unit".into()),
            ..Default::default()
        },
        Check {
            name: "unrelated baseline".into(),
            passed: false,
            scope: Some("baseline".into()),
            ..Default::default()
        },
    ]
}
fn outputs() -> Vec<Output> {
    vec![Output {
        kind: "doc".into(),
        title: "result".into(),
        location: Some("result.md".into()),
        role: None,
    }]
}
fn paused_core(k: &Kit) -> Core {
    let mut cfg = k.core.config().clone();
    cfg.no_resume = true;
    Core::new(k.db.clone(), k.rt.clone(), k.gw.clone(), cfg)
}

#[test]
fn completion_commits_one_cycle_and_one_done_fact_without_ticking_unfinished_plan() {
    let k = Kit::new();
    let item = k.item("completed");
    k.db.item_patch(&item,patch![plan:json!([{"step":"unfinished","done":false,"left":false},{"step":"finished","done":true,"left":false}])]).unwrap();
    k.core
        .item_action(&item, ItemAction::Finish, "human")
        .unwrap();
    let closed = k.db.item(&item).unwrap().unwrap();
    assert_eq!(closed.phase, "closed");
    assert_eq!(closed.outcome.as_deref(), Some("completed"));
    assert_eq!(closed.acceptance.as_deref(), Some("human"));
    assert!(!closed.plan[0].done && closed.plan[0].left);
    assert!(closed.plan[1].done && !closed.plan[1].left);
    for _ in 0..2 {
        k.core.lifecycle_tick().unwrap();
    }
    assert_eq!(
        k.db.item(&item).unwrap().unwrap().closed_at,
        closed.closed_at
    );
    k.db.store().reader::<_,steve_store::StoreError>(|c|{let cycles:i64=c.query_row("SELECT COUNT(*) FROM item_cycles WHERE item_id=?",[&item],|r|r.get(0))?;let done:i64=c.query_row("SELECT COUNT(*) FROM outbox_events WHERE topic='work.item.done' AND json_extract(payload,'$.item_id')=?",[&item],|r|r.get(0))?;assert_eq!((cycles,done),(1,1));Ok(())}).unwrap();
}
#[test]
fn cancellation_keeps_effect_reason_and_expires_open_cards() {
    let k = Kit::new();
    let item = k.item("cancelled");
    k.db.decision_create(NewDecision {
        workspace_id: WS.into(),
        item_id: Some(item.clone()),
        kind: "ask".into(),
        subject_type: "question".into(),
        question: "old question".into(),
        ..Default::default()
    })
    .unwrap();
    k.db.exec("INSERT INTO item_operations(id,item_id,cycle,revision,kind,status,label,created,updated) VALUES('effect',?,1,0,'release','succeeded','published',1,1)",&[&item]).unwrap();
    k.core
        .item_action(&item, ItemAction::Drop, "human")
        .unwrap();
    let i = k.db.item(&item).unwrap().unwrap();
    assert_eq!(i.phase, "closed");
    assert_eq!(i.outcome.as_deref(), Some("cancelled"));
    assert!(i.closed_reason.unwrap().contains("已发生"));
    assert!(
        k.db.decisions_of_item(&item)
            .unwrap()
            .iter()
            .all(|d| d.status != "open")
    );
    let v = k.core.diagnostic_snapshot(&item).unwrap();
    assert_eq!(v["lifecycle"]["blockers"], json!([]));
    assert_eq!(v["lifecycle"]["active_work"], 0);
}
#[test]
fn lifecycle_projection_deduplicates_and_sorts_named_blockers_without_counting_build_as_active() {
    let k = Kit::new();
    let item = k.item("projection");
    let dep = k.item("prerequisite");
    k.core.add_dep(&item, &dep).unwrap();
    let s = k.session(&item);
    k.rt.busy.lock().unwrap().insert(s.id.clone());
    for (kind, subject, q) in [
        ("ask", "question", "q"),
        ("approve", "action", "approval"),
        ("accept", "item", "accept"),
        ("approve", "tool_call", "permission"),
        ("approve", "tool_call", "permission"),
        ("fail", "item", "failure"),
    ] {
        k.db.decision_create(NewDecision {
            workspace_id: WS.into(),
            item_id: Some(item.clone()),
            kind: kind.into(),
            subject_type: subject.into(),
            question: q.into(),
            ..Default::default()
        })
        .unwrap();
    }
    let c = Closing {
        kind: COMPLETE.into(),
        policy: "human".into(),
        reason: "fixture".into(),
        ..Default::default()
    };
    k.db.exec(
        "UPDATE items SET phase='closing',closing=? WHERE id=?",
        &[&serde_json::to_string(&c).unwrap(), &item],
    )
    .unwrap();
    for (id, kind, status, label, tag) in [
        ("deploy", "deploy", "pending", "deployment", "deploy-tag"),
        ("build", "release.build", "running", "build", "build-tag"),
        ("unknown", "hooks.accepted", "unknown", "unknown", ""),
        ("running", "fixture", "running", "activity", ""),
    ] {
        k.db.exec("INSERT INTO item_operations(id,item_id,cycle,revision,kind,status,label,required,evidence,created,updated) VALUES(?,?,1,0,?,?,?,1,?,1,1)",&[&id,&item,&kind,&status,&label,&json!({"tag":tag}).to_string()]).unwrap();
    }
    let v = k.core.diagnostic_snapshot(&item).unwrap()["lifecycle"].clone();
    let b = v["blockers"].as_array().unwrap();
    let kinds: Vec<_> = b.iter().map(|r| r["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds.iter().filter(|k| **k == "permission").count(), 1);
    assert_eq!(v["active_work"], 2);
    assert_eq!(kinds[0], "failure");
    assert_eq!(kinds[1], "unknown_effect");
    assert!(
        kinds.iter().position(|s| *s == "permission")
            < kinds.iter().position(|s| *s == "acceptance")
    );
    let operations: Vec<_> = b
        .iter()
        .filter(|r| r["kind"] == "operation")
        .map(|r| r["label"].as_str().unwrap())
        .collect();
    assert_eq!(operations.len(), 2);
    assert!(operations[0].contains("build-tag"));
    assert!(operations[1].contains("deploy-tag"));
}

#[test]
fn automatic_continue_spends_once_then_respects_budget_and_user_stop() {
    let k = Kit::new();
    let item = k.item("continue");
    let s = k.session(&item);
    for n in 1..=2 {
        k.rt.idle(&s.id);
        k.db.exec("UPDATE sessions SET updated=0 WHERE id=?", &[&s.id])
            .unwrap();
        k.db.exec("UPDATE events SET ts=0 WHERE session_id=?", &[&s.id])
            .unwrap();
        k.core.check_stalls().unwrap();
        assert_eq!(k.rt.prompts.lock().unwrap().len(), n);
        k.core.check_stalls().unwrap();
        assert_eq!(k.rt.prompts.lock().unwrap().len(), n);
    }
    k.rt.idle(&s.id);
    k.db.exec("UPDATE sessions SET updated=0 WHERE id=?", &[&s.id])
        .unwrap();
    k.db.exec("UPDATE events SET ts=0 WHERE session_id=?", &[&s.id])
        .unwrap();
    k.core.check_stalls().unwrap();
    assert_eq!(k.rt.prompts.lock().unwrap().len(), 2);
    assert!(
        k.db.decisions_of_item(&item)
            .unwrap()
            .iter()
            .any(|d| d.kind == "fail")
    );
    let other = k.item("your stop");
    let session = k.session(&other);
    k.db.store()
        .sessions()
        .stop_asked(&session.id, Some("fixture"))
        .unwrap();
    k.db.exec("UPDATE sessions SET updated=0 WHERE id=?", &[&session.id])
        .unwrap();
    k.core.check_stalls().unwrap();
    assert_eq!(k.rt.prompts.lock().unwrap().len(), 2);
}
#[test]
fn repeated_refusal_precedes_continue_budget_without_another_prompt() {
    let k = Kit::new();
    let item = k.item("refusal");
    let s = k.session(&item);
    for _ in 0..2 {
        k.db.timeline_add(&item, "steve", "sys", "交付被拒：same reason", Some(&s.id))
            .unwrap();
    }
    k.db.exec("UPDATE sessions SET updated=0 WHERE id=?", &[&s.id])
        .unwrap();
    k.core.check_stalls().unwrap();
    assert!(k.rt.prompts.lock().unwrap().is_empty());
    assert!(
        k.db.decisions_of_item(&item)
            .unwrap()
            .iter()
            .any(|d| d.question.contains("连续 2 次"))
    );
}

#[test]
fn workflow_change_checks_revision_actor_budget_and_single_session_without_partial_updates() {
    let k = Kit::new();
    let core = paused_core(&k);
    let item = k.item("workflow");
    let session = k.session(&item);
    let args = json!({"action":"plan","expected_revision":0,"reason":"fixture","units":specs(),"max_attempts":4});
    assert!(core.tool_workflow(&session, args.clone()).is_err());
    assert!(k.db.store().workflows().get(&item).unwrap().is_none());
    let mut args = args;
    args["max_attempts"] = json!(3);
    core.tool_workflow(&session, args).unwrap();
    let before = graph(&k, &item);
    assert!(
        core.workflow_change(
            &item,
            json!({"action":"pause","expected_revision":0,"reason":"stale"}),
            "human"
        )
        .is_err()
    );
    assert_eq!(graph(&k, &item), before);
    assert!(
        core.tool_workflow(
            &session,
            json!({"action":"single_session","expected_revision":before.revision,"reason":"bot"})
        )
        .is_err()
    );
    core.workflow_change(
        &item,
        json!({"action":"pause","expected_revision":before.revision,"reason":"settle"}),
        "human",
    )
    .unwrap();
    let paused = graph(&k, &item);
    core.workflow_change(
        &item,
        json!({"action":"single_session","expected_revision":paused.revision,"reason":"human"}),
        "human",
    )
    .unwrap();
    let single = graph(&k, &item);
    assert!(single.single_session());
    core.workflow_change(
        &item,
        json!({"action":"single_session","expected_revision":single.revision,"reason":"again"}),
        "human",
    )
    .unwrap();
    assert_eq!(graph(&k, &item).revision, single.revision);
    assert!(
        core.workflow_change(
            &item,
            json!({"action":"resume","expected_revision":single.revision,"reason":"blocked"}),
            "human"
        )
        .is_err()
    );
}
#[test]
fn workflow_view_caps_text_history_and_keeps_runtime_activity_unconfirmed() {
    let k = Kit::new();
    let item = k.item("view");
    k.db.store()
        .workflows()
        .replace(&item, 0, "fixture", &specs(), 2, 3)
        .unwrap();
    let body = "界".repeat(2400);
    k.db.exec("UPDATE workflow_units SET summary=?,reason=?,status='failed' WHERE workflow_id=? AND id='a'",&[&body,&body,&item]).unwrap();
    for n in 0..25 {
        k.db.store()
            .workflows()
            .pause_by(&item, n % 2 == 0, "fixture", "user")
            .unwrap();
    }
    let v = k.core.workflow_get(&item).unwrap();
    assert_eq!(v["history"].as_array().unwrap().len(), 20);
    assert_eq!(v["history_total"], 26);
    assert!(
        v["history"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["detail"].is_null())
    );
    let a = &v["units"][0];
    assert!(
        a["summary"]
            .as_str()
            .unwrap()
            .starts_with(&"界".repeat(2000))
    );
    assert!(a["summary"].as_str().unwrap().chars().count() < 2050);
    assert!(a["reason"].as_str().unwrap().chars().count() < 2050);
    k.db.exec(
        "UPDATE workflow_units SET status='running' WHERE workflow_id=? AND id='a'",
        &[&item],
    )
    .unwrap();
    assert_eq!(
        k.core.workflow_get(&item).unwrap()["units"][0]["activity"]["state"],
        "unconfirmed"
    );
}
#[test]
fn workflow_drive_keeps_resource_reservations_and_resumes_the_same_session() {
    let k = Kit::new();
    k.rt.ready.store(true, Ordering::SeqCst);
    let item = k.item("drive");
    let mut units = specs();
    units[0].resources = vec!["shared".into()];
    units[1].resources = vec!["shared".into()];
    k.core.workflow_change(&item,json!({"action":"plan","expected_revision":0,"reason":"fixture","units":units,"max_parallel":2}),"human").unwrap();
    let g = graph(&k, &item);
    assert_eq!(g.units[0].status, "running");
    assert_eq!(g.units[1].status, "pending");
    let sid = g.units[0].session_id.clone().unwrap();
    k.core.lifecycle_tick().unwrap();
    assert_eq!(k.rt.prompts.lock().unwrap().len(), 1);
    change(&k, &item, "pause", json!({})).unwrap();
    steve_runtime::testkit::eventually_blocking(
        "pause stops the frozen turn outside the core lock",
        || !k.rt.is_busy(&sid),
    );
    change(&k, &item, "resume", json!({})).unwrap();
    assert_eq!(
        graph(&k, &item).units[0].session_id.as_deref(),
        Some(sid.as_str())
    );
    let p = k.rt.prompts.lock().unwrap();
    assert_eq!(p.len(), 2);
    assert_eq!(p[1].2, "workflow.resume");
}
fn claimed(k: &Kit, p: &str, id: &str) -> Session {
    let u = k.db.store().workflows().claim(p, id).unwrap();
    let child = u.item_id.unwrap();
    let s = k.session(&child);
    k.db.store()
        .workflows()
        .bind_session(p, id, &child, &s.id)
        .unwrap();
    s
}
#[test]
fn unit_report_rejects_bad_evidence_and_queued_input_then_commits_once() {
    let k = Kit::new();
    let item = k.item("report");
    k.db.store()
        .workflows()
        .replace(&item, 0, "fixture", &specs(), 2, 3)
        .unwrap();
    let s = claimed(&k, &item, "a");
    let child = s.item_id.clone().unwrap();
    assert!(
        k.core
            .tool_deliver(&s, "", &outputs(), &checks(), &[])
            .is_err()
    );
    assert!(
        k.core
            .tool_deliver(&s, "missing outputs", &[], &checks(), &[])
            .is_err()
    );
    let bad = vec![Check {
        name: "bad".into(),
        passed: false,
        ..Default::default()
    }];
    assert!(
        k.core
            .tool_deliver(&s, "failed unit check", &outputs(), &bad, &[])
            .is_err()
    );
    k.db.queue_add(&s.id, "pending", "human", &[]).unwrap();
    assert!(
        k.core
            .tool_deliver(&s, "queued", &outputs(), &checks(), &[])
            .is_err()
    );
    k.db.queue_take(&s.id).unwrap();
    let before = graph(&k, &item);
    assert_eq!(before.units[0].status, "running");
    assert!(k.db.store().artifacts().list(&child).unwrap().is_empty());
    k.core
        .tool_deliver(&s, "passed", &outputs(), &checks(), &[])
        .unwrap();
    let count = graph(&k, &item).history.len();
    k.core
        .tool_deliver(&s, "passed", &outputs(), &checks(), &[])
        .unwrap();
    assert_eq!(graph(&k, &item).history.len(), count);
    assert_eq!(k.db.store().artifacts().list(&child).unwrap().len(), 1);
    assert!(
        k.core
            .tool_deliver(&s, "different", &outputs(), &checks(), &[])
            .is_err()
    );
    let b = claimed(&k, &item, "b");
    k.core
        .tool_workflow(&b, json!({"action":"fail","reason":"fixture failed"}))
        .unwrap();
    assert_eq!(graph(&k, &item).units[1].status, "failed");
}

#[test]
fn closing_parent_archives_unfinished_children_but_keeps_completed_children() {
    let k = Kit::new();
    let parent = k.item("parent");
    let tags =
        k.db.kinds(WS)
            .unwrap()
            .into_iter()
            .filter(|t| t.name == "feat")
            .map(|t| t.id)
            .collect::<Vec<_>>();
    let child = |title: &str| {
        k.db.item_create(
            WS,
            &NewItem {
                title: title.into(),
                goal: "fixture goal".into(),
                bot_id: "dev".into(),
                tags: tags.clone(),
                status: Some("run".into()),
                parent: Some(steve_store::Parent {
                    item: Some(Some(parent.clone())),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .unwrap()
        .id
    };
    let unfinished = child("unfinished");
    let completed = child("completed");
    k.core
        .item_action(&completed, ItemAction::Finish, "human")
        .unwrap();
    k.core
        .item_action(&parent, ItemAction::Finish, "human")
        .unwrap();
    let a = k.db.item(&unfinished).unwrap().unwrap();
    assert!(a.archived);
    assert_eq!(a.outcome.as_deref(), Some("cancelled"));
    assert!(a.closed_reason.unwrap().starts_with("随父事项 "));
    let b = k.db.item(&completed).unwrap().unwrap();
    assert!(!b.archived);
    assert_eq!(b.outcome.as_deref(), Some("completed"));
}

struct CompletionPort {
    patch: &'static str,
    mode: &'static str,
    calls: Mutex<Vec<steve_core::node_git::Ask>>,
}
fn node_facts() -> na::GitFacts {
    na::GitFacts {
        revision: na::Revision::new("fixture").unwrap(),
        head: "head".into(),
        branch: Some("steve/fixture".into()),
        dirty: false,
        refs: vec![
            ("main".into(), Some("base".into())),
            ("refs/heads/steve/fixture".into(), Some("head".into())),
        ],
        ranges: vec![],
        reads: vec![],
    }
}
#[async_trait]
impl steve_core::node_git::NodeGit for CompletionPort {
    fn on_node(&self, _: &str) -> bool {
        true
    }
    async fn facts(&self, _: &str, q: steve_core::node_git::Ask) -> na::ApiResult<na::GitFacts> {
        self.calls.lock().unwrap().push(q.clone());
        let mut f = node_facts();
        if q.fetch {
            f.refs = vec![("refs/remotes/origin/main".into(), Some("up".into()))];
            return Ok(f);
        }
        for (from, to) in q.ranges {
            f.ranges.push(na::RangeFacts {
                from,
                to,
                merge_base: Some("base".into()),
                ahead: u32::from(self.patch != "-"),
                behind: 0,
                overlap: vec![],
                conflicts: vec![],
            });
        }
        for read in q.reads {
            let answer = match &read {
                na::GitRead::Fingerprint => na::GitAnswer::Fingerprint {
                    tree: "tree".into(),
                    own: Some(na::OwnChange {
                        base: "base".into(),
                        patch: self.patch.into(),
                    }),
                },
                na::GitRead::RemoteUrl { .. } => {
                    if self.mode == "offline" {
                        return Err(na::ApiError::new(na::ErrorCode::Offline, "fixture offline"));
                    }
                    na::GitAnswer::RemoteUrl {
                        url: Some("https://example.invalid/fixture.git".into()),
                    }
                }
                na::GitRead::Landed { .. } => na::GitAnswer::Landed {
                    commit: (self.mode == "met").then(|| "head".into()),
                },
                na::GitRead::IsAncestor { .. } => na::GitAnswer::IsAncestor { yes: true },
                na::GitRead::ChangedPaths { .. } => na::GitAnswer::ChangedPaths {
                    paths: vec![],
                    truncated: false,
                },
                _ => panic!("unexpected completion read {read:?}"),
            };
            f.reads.push(na::GitReadFact {
                read,
                observed: "head".into(),
                answer,
            });
        }
        Ok(f)
    }
    async fn apply(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: &na::GitFacts,
        _: na::GitAction,
    ) -> steve_core::node_git::Applied {
        panic!("completion checks never execute Node git actions")
    }
}
#[tokio::test(flavor = "multi_thread")]
async fn completion_requirement_waives_unchanged_and_records_met_missing_or_unread_once() {
    for (patch, mode, expected) in [
        ("-", "missing", "waived"),
        ("patch", "met", "succeeded"),
        ("patch", "missing", "pending"),
        ("patch", "offline", "pending"),
    ] {
        let k = Kit::new();
        let item = k.item("contract");
        let s = k.session(&item);
        let project = k.project(Some(steve_core::upstream::DONE_MERGE), mode);
        k.db.item_patch(&item, patch![project_id:project,branch:"steve/fixture"])
            .unwrap();
        let port = Arc::new(CompletionPort {
            patch,
            mode,
            calls: Mutex::new(vec![]),
        });
        k.core.set_node_git(port.clone());
        k.core
            .tool_deliver(&s, "fixture delivery", &[], &[], &[])
            .unwrap();
        let accept =
            k.db.decisions_of_item(&item)
                .unwrap()
                .into_iter()
                .find(|d| d.kind == "accept" && d.status == "open")
                .unwrap();
        k.core.resolve(&accept.id, "pass", "", "human").unwrap();
        tokio::time::timeout(
            Duration::from_secs(3),
            steve_runtime::testkit::eventually_sync(
                "completion requirement reaches its durable result",
                || {
                    let ops = k.db.store().items().operations(&item, 1).unwrap();
                    let state = ops
                        .iter()
                        .find(|o| o.kind == "release")
                        .map(|o| o.status.as_str());
                    state == Some(expected)
                        && if expected == "pending" {
                            if mode == "offline" {
                                k.db.decisions_of_item(&item)
                                    .unwrap()
                                    .iter()
                                    .any(|d| d.kind == "fail" && d.status == "open")
                            } else {
                                !k.rt.prompts.lock().unwrap().is_empty()
                            }
                        } else {
                            k.db.item(&item).unwrap().unwrap().phase == "closed"
                        }
                },
            ),
        )
        .await
        .expect("completion requirement fixture timed out");
        let ops = k.db.store().items().operations(&item, 1).unwrap();
        assert_eq!(ops.iter().filter(|o| o.kind == "release").count(), 1);
        let before = (
            k.rt.prompts.lock().unwrap().len(),
            k.db.decisions_of_item(&item).unwrap().len(),
        );
        k.core.lifecycle_tick().unwrap();
        k.core.lifecycle_tick().unwrap();
        assert_eq!(
            (
                k.rt.prompts.lock().unwrap().len(),
                k.db.decisions_of_item(&item).unwrap().len()
            ),
            before
        );
        if patch == "-" {
            assert!(port.calls.lock().unwrap().iter().all(|q| !q.fetch));
        }
    }
}
