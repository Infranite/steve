//! SG33: compact public-contract checks, old implementation first. No external agents or git run.
use async_trait::async_trait;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;
use steve_core::derive::*;
use steve_core::gateway::{ModelGateway, Printed, ThinkTools, Tier};
use steve_core::repo::{Db, FileRef, NewDecision, NewItem, NewSession, Release, Session, WS};
use steve_core::runtime::{AgentRuntime, Answered, Flushed, Prompted, Released, Running, Stop};
use steve_core::work_policy::WorkPolicy;
use steve_core::{Core, CoreConfig, Result, patch};
use steve_node_api as na;
use steve_store::{NewBot, NoPublisher, Store, UnitSpec};

#[derive(Default)]
struct Runtime {
    busy: Mutex<HashSet<String>>,
    holding: AtomicBool,
    ready: AtomicBool,
}
impl AgentRuntime for Runtime {
    fn prompt(&self, _: &str, _: &str, _: &str, _: &[FileRef]) -> Result<Prompted> {
        Ok(Prompted::Queued)
    }
    fn running(&self, s: &str) -> Option<Running> {
        self.is_busy(s).then(|| Running {
            session: s.into(),
            attempt: "fixture".into(),
        })
    }
    fn stop(&self, _: &Running) -> Result<Stop> {
        Ok(Stop::NotRunning)
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
    fn flush(&self, _: &str, _: Option<(&str, &str)>) -> Flushed {
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
                    ..Default::default()
                },
                WS,
            )
            .unwrap();
        let db = Db::new(store);
        let rt = Arc::new(Runtime::default());
        let gw = Arc::new(Gateway::default());
        let mut cfg = CoreConfig::for_home(home.path());
        cfg.port = 18990;
        cfg.no_resume = true;
        cfg.dynamic_workflows = true;
        cfg.gh = "/usr/bin/false".into();
        cfg.takeover_continues = 0;
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
    fn work(&self, item: &str) -> Value {
        self.core.diagnostic_snapshot(item).unwrap()["work"].clone()
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

#[test]
fn dependency_modes_fallback_and_registration_gates_are_preserved() {
    let k = Kit::new();
    let parent = k.item("parent");
    let dep = k.item("dep");
    let s = k.session(&parent);
    let empty = k.core.dependency_get(&parent).unwrap();
    assert_eq!(empty["register_blocker"], "请先添加前置事项");
    k.core.add_dep(&parent, &dep).unwrap();
    for (setting, expected) in [("自动开始", "automatic"), ("提醒开始", "notify")] {
        k.db.settings_set(
            WS,
            &serde_json::Map::from_iter([("depsAfter".into(), json!(setting))]),
            "fixture",
        )
        .unwrap();
        let v = k.core.dependency_get(&parent).unwrap();
        assert_eq!(v["resume_mode"], expected);
        assert_eq!(v["resume_source"], "workspace");
        assert_eq!(v["dependencies"][0]["id"], dep);
        assert_eq!(v["can_register"], true);
        assert_eq!(v["resume_enabled"], false);
    }
    for mode in ["automatic", "notify", "workspace"] {
        k.db.store()
            .dependency_waits()
            .register(&parent, &s.id, &[dep.clone()], mode, "fixture wait")
            .unwrap();
        let v = k.core.dependency_get(&parent).unwrap();
        assert_eq!(v["resume_override"], mode);
        assert_eq!(v["waiting"], true);
        assert_eq!(v["reason"], "fixture wait");
        assert_eq!(
            v["resume_mode"],
            if mode == "automatic" {
                "automatic"
            } else {
                "notify"
            }
        );
    }
    k.db.exec(
        "UPDATE item_dependency_waits SET resume='automatic',state='notified' WHERE item_id=?",
        &[&parent],
    )
    .unwrap();
    k.db.item_patch(&parent, patch![status:"wait"]).unwrap();
    let v = k.core.dependency_get(&parent).unwrap();
    assert_eq!(v["resume_mode"], "notify");
    assert_eq!(v["resume_source"], "fallback");
    assert_eq!(v["waiting"], false);
    // A historical wait must not override this cycle's workspace choice.
    k.db.item_patch(&parent, patch![status:"block"]).unwrap();
    let v = k.core.dependency_get(&parent).unwrap();
    assert_eq!(v["resume_override"], "workspace");
    k.db.exec("UPDATE sessions SET readonly=1 WHERE id=?", &[&s.id])
        .unwrap();
    assert_eq!(
        k.core.dependency_get(&parent).unwrap()["can_register"],
        false
    );
}

fn facts() -> Facts {
    Facts {
        now: 100_000,
        item: ItemState {
            id: "item".into(),
            cycle: 2,
            phase: "active".into(),
            ..Default::default()
        },
        session: Some(SessionState {
            id: "session".into(),
            turn: Some("turn".into()),
            watched: true,
            last_activity: 0,
            ..Default::default()
        }),
        continues_cap: 2,
        stall_after: 10_000,
        ..Default::default()
    }
}
#[test]
fn derived_first_match_preserves_attention_wait_busy_and_judge_order() {
    let p = WorkPolicy::with(&[("settle_debounce".into(), 0)]);
    let mut f = facts();
    f.item.phase = "closed".into();
    f.cards.push(Card {
        id: "card".into(),
        question: "question".into(),
        stop: false,
    });
    f.item.paused = true;
    f.dependency_wait = true;
    assert_eq!(derive(&f, &p).code, "done");
    f.item.phase = "active".into();
    assert_eq!(derive(&f, &p).code, "card");
    f.cards.insert(
        0,
        Card {
            id: "stop".into(),
            stop: true,
            ..Default::default()
        },
    );
    assert_eq!(
        derive(&f, &p).next_step,
        Some(NextStep::Card {
            decision_id: "card".into()
        })
    );
    f.cards.clear();
    assert_eq!(derive(&f, &p).code, "paused");
    f.item.paused = false;
    f.session.as_mut().unwrap().held_by_you = true;
    assert_eq!(derive(&f, &p).code, "your_stop");
    f.session.as_mut().unwrap().held_by_you = false;
    f.merge_declined = true;
    assert_eq!(derive(&f, &p).code, "merge_declined");
    f.merge_declined = false;
    f.waits.push(Wait {
        id: "wait".into(),
        cycle: 2,
        on: "you".into(),
        what: "approval".into(),
        next_check: 99_000,
        ..Default::default()
    });
    assert_eq!(derive(&f, &p).code, "waits_on_you");
    f.waits[0].on = "external".into();
    f.quota_until = Some(100_001);
    assert_eq!(derive(&f, &p).code, "dependency");
    f.dependency_wait = false;
    assert_eq!(derive(&f, &p).code, "quota");
    f.quota_until = Some(f.now);
    assert_eq!(
        derive(&f, &p).next_step,
        Some(NextStep::Due {
            kind: Due::Wait,
            id: Some("wait".into()),
            at: Some(99_000)
        })
    );
    f.waits[0].cap = Some(0);
    f.session.as_mut().unwrap().running = true;
    f.graph_busy = Some("graph".into());
    assert_eq!(derive(&f, &p).code, "running");
    f.session.as_mut().unwrap().running = false;
    assert_eq!(derive(&f, &p).code, "workflow");
    f.graph_busy = None;
    f.judging = Some(Judging {
        id: "judge".into(),
        cycle: 2,
        session_id: "session".into(),
        turn_id: "turn".into(),
        next_check: 99_000,
        failures: 0,
    });
    assert_eq!(
        derive(&f, &p).next_step,
        Some(NextStep::Due {
            kind: Due::Judge,
            id: Some("judge".into()),
            at: Some(f.now)
        })
    );
    f.judging.as_mut().unwrap().turn_id = "stale".into();
    assert_eq!(derive(&f, &p).code, "follow_up");
    f.continues_spent = 2;
    assert_eq!(derive(&f, &p).code, "stopped");
    f.session = None;
    f.item.phase = "ready".into();
    assert_eq!(derive(&f, &p).code, "not_started");
    f.item.phase = "active".into();
    assert_eq!(derive(&f, &p).code, "nobody");
    f.closing_ops.push(OpenOperation {
        id: "closing".into(),
        kind: "hooks.accepted".into(),
        label: "gate".into(),
    });
    assert_eq!(
        derive(&f, &p).next_step,
        Some(NextStep::Due {
            kind: Due::Closing,
            id: Some("closing".into()),
            at: None
        })
    );
}
#[test]
fn derived_grace_and_wait_cap_cycle_boundaries_keep_their_full_next_step() {
    let p = WorkPolicy::with(&[("settle_debounce".into(), 0), ("close_grace".into(), 100)]);
    let mut f = facts();
    f.judgements.push(Judged {
        id: "done".into(),
        cycle: 2,
        session_id: "session".into(),
        turn_id: "turn".into(),
        scope_revision: 0,
        verdict: "done".into(),
        created: 99_950,
    });
    let d = derive(&f, &p);
    assert_eq!((d.row, d.code), (2, "judged_done"));
    assert_eq!(
        d.next_step,
        Some(NextStep::Due {
            kind: Due::Grace,
            id: Some("done".into()),
            at: Some(100_050)
        })
    );
    f.judgements.clear();
    for (cycle, wakes, cap, expected) in [
        (1, 0, None, "follow_up"),
        (2, 1, Some(1), "follow_up"),
        (2, 0, Some(1), "waiting"),
        (2, 99, None, "waiting"),
    ] {
        f.waits = vec![Wait {
            id: "wait".into(),
            cycle,
            on: "time".into(),
            what: "timer".into(),
            next_check: 123_000,
            wakes,
            cap,
        }];
        let d = derive(&f, &p);
        assert_eq!(d.code, expected);
        assert!(check_next_step(&f, &d).is_ok());
    }
}

fn spec(id: &str, deps: &[&str], required: bool) -> UnitSpec {
    UnitSpec {
        id: id.into(),
        title: id.into(),
        goal: format!("work {id}"),
        bot_id: "dev".into(),
        kind: "work".into(),
        depends_on: deps.iter().map(|s| s.to_string()).collect(),
        resources: vec![],
        required,
    }
}
fn with_gates(mut units: Vec<UnitSpec>) -> Vec<UnitSpec> {
    let work = units.iter().map(|u| u.id.clone()).collect();
    let mut review = spec("review", &[], true);
    review.kind = "review".into();
    review.bot_id = "reviewer".into();
    review.depends_on = work;
    let mut integrate = spec("integrate", &["review"], true);
    integrate.kind = "integrate".into();
    units.extend([review, integrate]);
    units
}
#[test]
fn graph_work_preserves_running_user_disabled_queued_and_failed_priority() {
    let k = Kit::new();
    let parent = k.item("parent");
    let running = k.item("running child");
    let user = k.item("user child");
    let rs = k.session(&running);
    let us = k.session(&user);
    let specs = vec![
        spec("failure", &[], true),
        spec("downstream", &["failure"], true),
        spec("running", &[], true),
        spec("user", &[], true),
        spec("queued", &[], false),
    ];
    k.db.store()
        .workflows()
        .replace(&parent, 0, "fixture", &with_gates(specs), 2, 3)
        .unwrap();
    k.db.exec("UPDATE workflow_units SET status='failed',reason='fixture failure' WHERE workflow_id=? AND id='failure'",&[&parent]).unwrap();
    for (unit, item, sid) in [("running", &running, &rs.id), ("user", &user, &us.id)] {
        k.db.exec("UPDATE workflow_units SET status='running',item_id=?,session_id=? WHERE workflow_id=? AND id=?",&[item,sid,&parent,&unit]).unwrap();
    }
    let d =
        k.db.decision_create(NewDecision {
            workspace_id: WS.into(),
            item_id: Some(user.clone()),
            session_id: Some(us.id.clone()),
            kind: "ask".into(),
            subject_type: "question".into(),
            question: "choose".into(),
            ..Default::default()
        })
        .unwrap();
    k.rt.busy.lock().unwrap().insert(rs.id.clone());
    assert_eq!(k.work(&parent)["state"], "running");
    k.rt.busy.lock().unwrap().clear();
    assert_eq!(k.work(&parent)["state"], "waiting_user");
    k.db.exec(
        "UPDATE decisions SET status='resolved' WHERE id=?",
        &[&d.id],
    )
    .unwrap();
    let mut cfg = k.core.config().clone();
    cfg.dynamic_workflows = false;
    let off = Core::new(k.db.clone(), k.rt.clone(), k.gw.clone(), cfg);
    assert_eq!(
        off.diagnostic_snapshot(&parent).unwrap()["work"]["state"],
        "waiting_user"
    );
    assert_eq!(k.work(&parent)["state"], "failed");
    assert!(
        k.work(&parent)["why"]
            .as_str()
            .unwrap()
            .contains("fixture admission denied")
    );
    k.rt.ready.store(true, Ordering::SeqCst);
    assert_eq!(k.work(&parent)["state"], "queued");
    k.db.exec("UPDATE workflow_units SET status='cancelled' WHERE workflow_id=? AND id IN ('running','user','queued')",&[&parent]).unwrap();
    let v = k.work(&parent);
    assert_eq!(v["state"], "failed");
    assert!(v["why"].as_str().unwrap().contains("failure"));
    assert!(v["why"].as_str().unwrap().contains("downstream"));
    // A success whose frozen child has not closed is queued, but not dispatchable.
    k.db.exec("UPDATE workflow_units SET status='succeeded',item_id=NULL,session_id=NULL WHERE workflow_id=?",&[&parent]).unwrap();
    k.db.exec(
        "UPDATE workflow_units SET item_id=? WHERE workflow_id=? AND id='queued'",
        &[&running, &parent],
    )
    .unwrap();
    k.rt.ready.store(false, Ordering::SeqCst);
    assert_eq!(k.work(&parent)["state"], "queued");
    k.db.exec(
        "UPDATE items SET phase='closed',outcome='completed',status='done' WHERE id=?",
        &[&running],
    )
    .unwrap();
    assert_eq!(k.work(&parent)["state"], "idle");
}
#[test]
fn graph_pause_precedes_stale_scope_and_optional_failure_propagates_transitively() {
    let k = Kit::new();
    let parent = k.item("parent");
    let specs = vec![
        spec("optional", &[], false),
        spec("middle", &["optional"], true),
        spec("last", &["middle"], true),
    ];
    k.db.store()
        .workflows()
        .replace(&parent, 0, "fixture", &with_gates(specs), 2, 3)
        .unwrap();
    k.db.exec(
        "UPDATE workflow_units SET status='failed' WHERE workflow_id=? AND id='optional'",
        &[&parent],
    )
    .unwrap();
    let v = k.work(&parent);
    assert_eq!(v["state"], "failed");
    assert!(v["why"].as_str().unwrap().contains("middle"));
    assert!(v["why"].as_str().unwrap().contains("last"));
    k.db.store()
        .workflows()
        .pause_by(&parent, true, "fixture pause", "user")
        .unwrap();
    k.db.add_goal(&parent, "new scope").unwrap();
    assert_eq!(k.work(&parent)["state"], "paused");
    k.db.store()
        .workflows()
        .pause_by(&parent, false, "handoff", "user")
        .unwrap();
    k.db.store()
        .workflows()
        .pause_by(&parent, true, "fixture pause", "agent")
        .unwrap();
    assert_eq!(k.work(&parent)["state"], "paused_by_agent");
    k.db.store()
        .workflows()
        .pause_by(&parent, false, "resume", "user")
        .unwrap();
    assert_eq!(k.work(&parent)["state"], "failed");
}

fn finished(k: &Kit, sid: &str, text: &str) {
    let at = steve_store::now() - 1_000;
    k.db.store().transaction(|tx|{
        tx.execute("INSERT INTO runtime_sessions(session_id,node_id,workspace_ref,node_session_id,config_revision,phase,updated) VALUES (?,'node-sg33','workspace','session','revision','ready',?)",rusqlite::params![sid,at])?;
        tx.execute("INSERT INTO runtime_turns(turn_id,session_id,state,created,updated) VALUES ('turn',?,'finished',?,?)",rusqlite::params![sid,at,at])?;
        tx.execute("INSERT INTO runtime_attempts(attempt_id,turn_id,ordinal,node_id,operation_id,execution,spec,digest,state,created,updated) VALUES ('attempt','turn',0,'node-sg33','operation','{}','{}','digest','finished',?,?)",rusqlite::params![at,at])?;
        tx.execute("INSERT INTO node_streams(node_id,stream,epoch) VALUES ('node-sg33','events','epoch')",[])?;
        let payload=json!({"subject":{"type":"turn","session":sid,"turn":"turn","attempt":"attempt"},"body":{"type":"text_delta"}});
        tx.execute("INSERT INTO node_event_inbox(node_id,stream,epoch,seq,event_id,payload,applied,received) VALUES ('node-sg33','events','epoch',1,'event',?,1,?)",rusqlite::params![payload.to_string(),at])?;
        tx.execute("INSERT INTO events(session_id,ts,kind,data,format,source_node,source_stream,source_epoch,source_seq) VALUES (?,?,'text',?,'steve.v1','node-sg33','events','epoch',1)",rusqlite::params![sid,at,json!({"role":"assistant","text":text}).to_string()])?;
        Ok::<_,steve_store::StoreError>(())
    }).unwrap();
    k.db.store()
        .sessions()
        .stop_ended(sid, Some("attempt"), "end_turn", None, false)
        .unwrap();
    k.db.store()
        .item_facts()
        .set_policy("settle_debounce", 0)
        .unwrap();
}
#[tokio::test(flavor = "multi_thread")]
async fn judge_bundle_keeps_original_request_current_goal_and_only_terminal_attempt_text() {
    let k = Kit::new();
    let item = k.item("report");
    let s = k.session(&item);
    finished(&k, &s.id, "terminal answer");
    let version = k.db.add_goal(&item, "updated scope").unwrap();
    k.db.artifact_add(&item, "doc", "report", Some("result.md"), None)
        .unwrap();
    k.db.event_add(
        &s.id,
        "text",
        json!({"role":"assistant","text":"unattributed later text"}),
    )
    .unwrap();
    k.db.timeline_add(&item, "dev", "sys", "交付，等你验收", Some(&s.id))
        .unwrap();
    assert_eq!(k.core.check_judges().await.unwrap(), 1);
    let records = k.db.store().item_facts().judgements(&item, 1).unwrap();
    assert_eq!(records.len(), 1);
    let b = &records[0].basis["bundle"];
    assert_eq!(b["final_text"], "terminal answer");
    assert_eq!(b["final_text_source"], "attempt");
    assert_eq!(b["delivered_this_turn"], true);
    assert_eq!(b["attempt"], "attempt");
    assert_eq!(b["stop_reason"], "end_turn");
    assert_eq!(b["request"]["text"], "fixture goal");
    assert_eq!(b["criteria"][0]["version"], version);
    assert_eq!(b["criteria"][0]["text"], "updated scope");
    assert_eq!(b["evidence"][0]["ref"], "o1");
    assert_eq!(b["evidence"][0]["detail"], "report (result.md)");
    assert!(k.gw.0.lock().unwrap()[0].contains("terminal answer"));
    assert_eq!(k.core.check_judges().await.unwrap(), 0);
}
#[tokio::test(flavor = "multi_thread")]
async fn another_attempts_stop_never_supplies_the_judges_reply_or_error() {
    let k = Kit::new();
    let item = k.item("report");
    let s = k.session(&item);
    finished(&k, &s.id, "terminal answer");
    k.db.store()
        .sessions()
        .stop_ended(
            &s.id,
            Some("another-attempt"),
            "error",
            Some("other error"),
            false,
        )
        .unwrap();
    assert_eq!(k.core.check_judges().await.unwrap(), 0);
    let j = k.db.store().item_facts().judgements(&item, 1).unwrap();
    let b = &j[0].basis["bundle"];
    assert_eq!(b["final_text_source"], "unknown");
    assert_eq!(b["final_text"], "");
    assert_eq!(b["stop_reason"], "unknown");
    assert!(b["error"].is_null());
    assert!(k.gw.0.lock().unwrap().is_empty());
}

#[derive(Clone)]
struct GitCase {
    ahead: u32,
    dirty: bool,
    range_error: bool,
    main: bool,
    branch: bool,
    on_branch: bool,
    remote: bool,
    refs: Vec<(String, Option<String>)>,
}
impl Default for GitCase {
    fn default() -> Self {
        Self {
            ahead: 1,
            dirty: false,
            range_error: false,
            main: true,
            branch: true,
            on_branch: true,
            remote: true,
            refs: vec![
                ("refs/remotes/origin/HEAD".into(), Some("up".into())),
                ("refs/remotes/origin/main".into(), Some("up".into())),
            ],
        }
    }
}
#[derive(Default)]
struct GitPort {
    cases: Mutex<HashMap<String, GitCase>>,
    calls: Mutex<Vec<(String, steve_core::node_git::Ask)>>,
}
fn git_facts(head: &str) -> na::GitFacts {
    na::GitFacts {
        revision: na::Revision::new("revision").unwrap(),
        head: head.into(),
        branch: Some("steve/fixture".into()),
        dirty: false,
        refs: vec![],
        ranges: vec![],
        reads: vec![],
    }
}
#[async_trait]
impl steve_core::node_git::NodeGit for GitPort {
    fn on_node(&self, _: &str) -> bool {
        true
    }
    async fn facts(&self, item: &str, q: steve_core::node_git::Ask) -> na::ApiResult<na::GitFacts> {
        self.calls.lock().unwrap().push((item.into(), q.clone()));
        let c = self.cases.lock().unwrap()[item].clone();
        let mut f = git_facts(if q.fetch { "fetched" } else { "head" });
        f.dirty = c.dirty;
        if q.fetch {
            f.refs = c.refs;
            return Ok(f);
        }
        if !q.ranges.is_empty() {
            if c.range_error {
                return Err(na::ApiError::new(
                    na::ErrorCode::Offline,
                    "fixture ranges unavailable",
                ));
            }
            f.ranges = q
                .ranges
                .iter()
                .map(|(from, to)| na::RangeFacts {
                    from: from.clone(),
                    to: to.clone(),
                    merge_base: None,
                    ahead: c.ahead,
                    behind: 0,
                    overlap: vec![],
                    conflicts: vec![],
                })
                .collect();
            return Ok(f);
        }
        for read in &q.reads {
            let answer = match read {
                na::GitRead::Fingerprint => {
                    f.branch = c.on_branch.then(|| "steve/fixture".into());
                    f.refs = vec![
                        ("main".into(), c.main.then(|| "base".into())),
                        (
                            "refs/heads/steve/fixture".into(),
                            c.branch.then(|| "head".into()),
                        ),
                    ];
                    na::GitAnswer::Fingerprint {
                        tree: "tree".into(),
                        own: Some(na::OwnChange {
                            base: "base".into(),
                            patch: "patch".into(),
                        }),
                    }
                }
                na::GitRead::RemoteUrl { .. } => na::GitAnswer::RemoteUrl {
                    url: c
                        .remote
                        .then(|| "https://github.com/example/fixture.git".into()),
                },
                na::GitRead::Landed { .. } => na::GitAnswer::Landed { commit: None },
                _ => panic!("unexpected Node read: {read:?}"),
            };
            f.reads.push(na::GitReadFact {
                read: read.clone(),
                observed: f.head.clone(),
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
        panic!("SG33 never executes a Node action")
    }
}
#[test]
fn node_gather_preserves_range_order_and_unknown_vs_no_changes() {
    let k = Kit::new();
    let port = Arc::new(GitPort::default());
    k.core.set_node_git(port.clone());
    for (name, c, allowed, ranges) in [
        ("ahead", GitCase::default(), true, 2),
        (
            "clean",
            GitCase {
                ahead: 0,
                ..Default::default()
            },
            false,
            2,
        ),
        (
            "dirty",
            GitCase {
                ahead: 0,
                dirty: true,
                ..Default::default()
            },
            true,
            2,
        ),
        (
            "unknown",
            GitCase {
                range_error: true,
                ..Default::default()
            },
            true,
            2,
        ),
        (
            "no-main",
            GitCase {
                main: false,
                ..Default::default()
            },
            true,
            0,
        ),
        (
            "no-branch",
            GitCase {
                branch: false,
                on_branch: false,
                ..Default::default()
            },
            false,
            0,
        ),
    ] {
        let item = k.item(name);
        let s = k.session(&item);
        let project = k.project(None, name);
        k.db.item_patch(&item, patch![project_id:project,branch:"steve/fixture"])
            .unwrap();
        port.cases.lock().unwrap().insert(item.clone(), c);
        let result = k.core.tool_publish(&s, "steve/fixture", "fixture");
        assert_eq!(result.is_ok(), allowed, "{name}: {result:?}");
        let calls: Vec<_> = port
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(i, _)| i == &item)
            .map(|(_, q)| q.clone())
            .collect();
        assert_eq!(calls[0].refs, vec!["main", "refs/heads/steve/fixture"]);
        assert_eq!(calls[0].reads, vec![na::GitRead::Fingerprint]);
        if ranges > 0 {
            assert_eq!(
                calls[1].ranges,
                vec![
                    ("main".into(), "refs/heads/steve/fixture".into()),
                    ("main".into(), "HEAD".into())
                ]
            );
        } else {
            assert_eq!(calls.len(), 1);
        }
        assert_eq!(
            k.db.decisions_of_item(&item)
                .unwrap()
                .iter()
                .filter(|d| d.subject_type == "release")
                .count(),
            usize::from(allowed)
        );
    }
}
#[tokio::test(flavor = "multi_thread")]
async fn node_probe_keeps_remote_head_alias_priority_and_early_failures() {
    let k = Kit::new();
    let port = Arc::new(GitPort::default());
    k.core.set_node_git(port.clone());
    let head_main = GitCase::default();
    let master = GitCase {
        refs: vec![("refs/remotes/origin/master".into(), Some("up".into()))],
        ..Default::default()
    };
    for (name, c, expected) in [
        ("head-main", head_main, "origin/main"),
        ("master", master, "origin/master"),
        (
            "missing",
            GitCase {
                refs: vec![],
                ..Default::default()
            },
            "远端没有默认分支",
        ),
        (
            "no-remote",
            GitCase {
                remote: false,
                ..Default::default()
            },
            "没有 origin 远端",
        ),
        (
            "dirty",
            GitCase {
                dirty: true,
                ..Default::default()
            },
            "工作区还有没提交的改动",
        ),
    ] {
        let item = k.item(name);
        let s = k.session(&item);
        let project = k.project(Some(steve_core::upstream::DONE_MERGE), name);
        k.db.item_patch(
            &item,
            patch![project_id:project,branch:"steve/fixture",revision:1],
        )
        .unwrap();
        k.db.exec("UPDATE items SET revision=1 WHERE id=?", &[&item])
            .unwrap();
        k.db.exec("UPDATE sessions SET updated=0 WHERE id=?", &[&s.id])
            .unwrap();
        port.cases.lock().unwrap().insert(item.clone(), c);
        k.db.exec("UPDATE events SET ts=0 WHERE session_id=?", &[&s.id])
            .unwrap();
        k.core.check_stalls().unwrap();
        let waited = tokio::time::timeout(
            Duration::from_secs(3),
            steve_runtime::testkit::eventually_sync("completion probe records its refusal", || {
                k.db.decisions_of_item(&item)
                    .unwrap()
                    .iter()
                    .any(|d| format!("{} {:?}", d.question, d.draft).contains(expected))
            }),
        )
        .await;
        assert!(
            waited.is_ok(),
            "probe {name}: calls={:?}, item={:?}, work={:?}, cards={:?}",
            port.calls.lock().unwrap(),
            k.db.item(&item).unwrap().map(|i| (i.phase, i.status)),
            k.work(&item),
            k.db.decisions_of_item(&item).unwrap()
        );
        let calls: Vec<_> = port
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(i, _)| i == &item)
            .map(|(_, q)| q.clone())
            .collect();
        assert_eq!(
            calls[0].reads,
            vec![na::GitRead::RemoteUrl {
                remote: "origin".into()
            }]
        );
        if name == "head-main" || name == "master" {
            assert!(calls[1].fetch);
            assert_eq!(
                calls[2].reads,
                vec![na::GitRead::Landed {
                    commit: "fetched".into(),
                    onto: if name == "head-main" {
                        "refs/remotes/origin/HEAD"
                    } else {
                        "refs/remotes/origin/master"
                    }
                    .into()
                }]
            );
        }
        k.db.session_patch(&s.id, patch![status:"ended"]).unwrap();
    }
}

#[test]
fn shadow_selects_only_the_live_turn_candidate_and_projects_busy_then_queued() {
    use steve_store::item_facts::NewCandidate;
    let k = Kit::new();
    let item = k.item("candidate projection");
    let s = k.session(&item);
    finished(&k, &s.id, "answer");
    let facts = k.db.store().item_facts();
    let scope = k.db.goals(&item).unwrap().last().unwrap().version;
    for turn in ["stale", "turn"] {
        assert!(
            facts
                .register_candidate(&NewCandidate {
                    item_id: &item,
                    cycle: 1,
                    session_id: &s.id,
                    turn_id: turn,
                    scope_revision: scope,
                    terminal_attempt: Some("attempt"),
                    registered_by: "sweep",
                    next_check: steve_store::now() + 60_000
                })
                .unwrap()
        );
    }
    k.db.exec("UPDATE item_judge_candidates SET created=CASE WHEN turn_id='stale' THEN 2 ELSE 1 END WHERE item_id=?",&[&item]).unwrap();
    assert_eq!(facts.candidates(&item, 1).unwrap()[0].turn_id, "stale");
    let candidate = facts
        .candidates(&item, 1)
        .unwrap()
        .into_iter()
        .find(|c| c.turn_id == "turn")
        .unwrap();
    let v = k.core.shadow_view(&item).unwrap().unwrap();
    assert_eq!(v["code"], "judging");
    assert_eq!(v["next_step"]["id"], candidate.id);
    k.rt.busy.lock().unwrap().insert(s.id.clone());
    assert_eq!(
        k.core.shadow_view(&item).unwrap().unwrap()["code"],
        "running"
    );
    k.rt.busy.lock().unwrap().clear();
    k.db.queue_add(&s.id, "queued input", "fixture", &[])
        .unwrap();
    assert_eq!(
        k.core.shadow_view(&item).unwrap().unwrap()["code"],
        "queued"
    );
    k.db.queue_take(&s.id).unwrap();
    facts
        .candidate_supersede(&candidate.id, "fixture superseded")
        .unwrap();
    assert_ne!(
        k.core.shadow_view(&item).unwrap().unwrap()["code"],
        "judging"
    );
}

#[test]
fn registered_dependency_resume_uses_policy_and_never_creates_a_missing_session() {
    for (mode, setting, disabled, ended, expected) in [
        ("automatic", "提醒开始", false, false, "started"),
        ("notify", "自动开始", false, false, "notified"),
        ("workspace", "自动开始", false, false, "started"),
        ("workspace", "提醒开始", false, false, "notified"),
        ("automatic", "自动开始", true, false, "notified"),
        ("automatic", "自动开始", false, true, "notified"),
    ] {
        let k = Kit::new();
        let parent = k.item("dependent");
        let dep = k.item("prerequisite");
        let session = k.session(&parent);
        k.core.add_dep(&parent, &dep).unwrap();
        k.db.settings_set(
            WS,
            &serde_json::Map::from_iter([("depsAfter".into(), json!(setting))]),
            "fixture",
        )
        .unwrap();
        k.db.store()
            .dependency_waits()
            .register(&parent, &session.id, &[dep.clone()], mode, "fixture wait")
            .unwrap();
        k.db.exec(
            "UPDATE items SET phase='closed',status='done',outcome='completed' WHERE id=?",
            &[&dep],
        )
        .unwrap();
        if disabled {
            k.db.exec("UPDATE bots SET enabled=0 WHERE id='dev'", &[])
                .unwrap();
        }
        if ended {
            k.db.session_patch(&session.id, patch![status:"ended"])
                .unwrap();
        }
        // This Core is backed only by a fake runtime; no process or adapter can start.
        let mut cfg = k.core.config().clone();
        cfg.no_resume = false;
        let core = Core::new(k.db.clone(), k.rt.clone(), k.gw.clone(), cfg);
        core.lifecycle_tick().unwrap();
        core.lifecycle_tick().unwrap();
        assert_eq!(
            k.db.store()
                .dependency_waits()
                .get(&parent)
                .unwrap()
                .unwrap()
                .state,
            expected,
            "{mode}/{setting}/{disabled}/{ended}"
        );
        assert_eq!(k.db.sessions_of_item(&parent).unwrap().len(), 1);
        assert_eq!(
            k.db.queued(&session.id).unwrap().len(),
            usize::from(expected == "started")
        );
        assert_eq!(
            k.db.decisions_of_item(&parent)
                .unwrap()
                .iter()
                .filter(|d| d.status == "open" && d.kind == "propose")
                .count(),
            usize::from(expected == "notified")
        );
    }
}

#[test]
fn shadow_detail_labels_preserve_chinese_and_english_templates() {
    use steve_store::item_facts::NewWait;
    for (code, zh, en) in [
        (
            "card",
            "等你处理：%{question}",
            "Waiting for you: %{question}",
        ),
        ("waits_on_you", "等你：%{what}", "Waiting for you: %{what}"),
        ("waiting", "在等：%{what}", "Waiting: %{what}"),
        (
            "workflow",
            "协作进行中：%{what}",
            "Collaboration at work: %{what}",
        ),
    ] {
        let k = Kit::new();
        let item = k.item("label");
        match code {
            "card" => {
                k.db.decision_create(NewDecision {
                    workspace_id: WS.into(),
                    item_id: Some(item.clone()),
                    kind: "ask".into(),
                    subject_type: "question".into(),
                    question: "SG33 question".into(),
                    ..Default::default()
                })
                .unwrap();
            }
            "workflow" => {
                k.rt.ready.store(true, Ordering::SeqCst);
                k.db.store()
                    .workflows()
                    .replace(
                        &item,
                        0,
                        "fixture",
                        &with_gates(vec![spec("unit", &[], true)]),
                        2,
                        3,
                    )
                    .unwrap();
            }
            _ => {
                k.db.store()
                    .item_facts()
                    .add_wait(&NewWait {
                        item_id: &item,
                        cycle: 1,
                        waits_on: if code == "waiting" { "external" } else { "you" },
                        what: "SG33 input",
                        next_check: steve_store::now() + 60_000,
                        ..Default::default()
                    })
                    .unwrap();
            }
        }
        for (locale, template) in [("zh-CN", zh), ("en", en)] {
            let v = steve_i18n::with(locale, || k.core.shadow_view(&item).unwrap().unwrap());
            assert_eq!(v["code"], code);
            let detail = v["detail"].as_str().unwrap();
            if code != "workflow" {
                assert_eq!(
                    detail,
                    if code == "card" {
                        "SG33 question"
                    } else {
                        "SG33 input"
                    }
                );
            }
            assert_eq!(
                v["label"],
                template
                    .replace("%{question}", detail)
                    .replace("%{what}", detail)
            );
        }
    }
}
