//! YOLO Y2: the executor against a temp database and the fake runtime — tool permissions, acceptance,
//! automatic continues with back-off, cards the user answers first, undo and the breakers, the
//! policy setting, and the timeline and result of what YOLO answered.

use std::sync::Arc;

use serde_json::{Value, json};
use steve_modules::FakeClock;
use steve_store::yolo as repo;

use super::engine::{Engine, POLICY_KEY, ToolInput};
use super::judge::{CONTINUE_STALL, continue_error, error_backoff_s, rework_text};
use super::*;
use crate::repo::{NewDecision, WS};
use crate::testkit::Kit;

pub(super) struct T {
    pub(super) kit: Kit,
    pub(super) clock: Arc<FakeClock>,
    pub(super) yolo: Yolo,
    pub(super) engine: Engine,
}

pub(super) fn t() -> T {
    let kit = Kit::new();
    // the database stamps rows with the real time; the fake clock starts there
    let clock = Arc::new(FakeClock::new(steve_store::now()));
    let yolo = Yolo::with_clock(kit.db().clone(), None, kit.home(), clock.clone());
    let engine = Engine::new(kit.core.clone(), yolo.clone());
    yolo.put(
        WS,
        "me",
        Switch {
            full: false,
            on: true,
            preview: false,
            include_open: false,
        },
    )
    .unwrap();
    T {
        kit,
        clock,
        yolo,
        engine,
    }
}

pub(super) const PERM_OPTIONS: [&str; 3] = [
    "allow|Always Allow|allow_always",
    "once|Allow|allow_once",
    "reject|Reject|reject_once",
];

impl T {
    /// A running item with its own scratch directory and a live session.
    pub(super) fn item(&self) -> (String, String, std::path::PathBuf) {
        let id = self.kit.item("run", "执行", None);
        let cwd = self.kit.home().join("scratch").join(&id);
        std::fs::create_dir_all(&cwd).unwrap();
        let c = cwd.to_string_lossy().to_string();
        self.kit
            .db()
            .item_patch(&id, crate::patch![cwd: c.as_str()])
            .unwrap();
        let s = self.kit.session(&id, "idle");
        (id, s, cwd)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn card(
        &self,
        item: &str,
        sid: &str,
        kind: &str,
        st: &str,
        q: &str,
        draft: &str,
        opts: &[&str],
    ) -> String {
        self.kit
            .db()
            .decision_create(NewDecision {
                workspace_id: WS.into(),
                kind: kind.into(),
                subject_type: st.into(),
                subject_id: item.into(),
                item_id: Some(item.into()),
                session_id: Some(sid.into()),
                question: q.into(),
                draft: Some(draft.into()),
                options: Some(opts.iter().map(|s| s.to_string()).collect()),
                requested_by: "dev".into(),
            })
            .unwrap()
            .id
    }

    pub(super) fn permission(&self, item: &str, sid: &str, input: ToolInput) -> String {
        let d = self.card(
            item,
            sid,
            "approve",
            "tool_call",
            "允许执行：Edit",
            &serde_json::to_string_pretty(&input.raw).unwrap(),
            &PERM_OPTIONS,
        );
        self.kit.rt.st().permissions.insert(d.clone());
        self.engine.stash(&d, input);
        self.engine.arm(&d);
        d
    }

    pub(super) fn decision(&self, id: &str) -> crate::repo::Decision {
        self.kit.db().decision(id).unwrap().unwrap()
    }

    pub(super) fn actions(&self, decision: &str) -> Vec<repo::ActionRow> {
        self.yolo
            .store()
            .reader(|c| repo::actions_of_decision(c, decision))
            .unwrap()
    }

    pub(super) fn audit_actor(&self, target: &str) -> Vec<String> {
        let c = rusqlite::Connection::open(self.kit.home().join("data/steve.sqlite3")).unwrap();
        let mut st = c
            .prepare("SELECT actor FROM audit WHERE target = ? AND action = 'decision.resolve'")
            .unwrap();
        st.query_map([target], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    pub(super) fn paused(&self) -> Option<Value> {
        self.yolo.status(WS).unwrap()["paused"]
            .clone()
            .as_object()
            .map(|o| Value::Object(o.clone()))
    }
}

#[test]
fn a_tool_call_inside_the_worktree_is_allowed_once_and_recorded() {
    let t = t();
    let (item, sid, cwd) = t.item();
    let file = cwd.join("notes.txt").to_string_lossy().to_string();
    let d = t.permission(
        &item,
        &sid,
        ToolInput {
            raw: json!({"file_path": file, "content": "x"}),
            kind: Some("edit".into()),
            locations: vec![file.clone()],
            own_dir: None,
            own_pids: None,
        },
    );
    let a = t.engine.consider(&d).unwrap().expect("an action");
    assert_eq!(
        (a.outcome.as_str(), a.rule.as_str()),
        ("decided", "tool.worktree")
    );
    assert_eq!(
        a.choice.as_deref(),
        Some("once"),
        "allow_once, never allow_always"
    );
    assert_eq!(
        t.kit.rt.st().answered,
        vec![(d.clone(), Some("once".to_string()))]
    );
    let dd = t.decision(&d);
    assert_eq!(dd.status, "resolved");
    assert_eq!(dd.result["optionId"], "once");
    assert_eq!(dd.result["decided_by"], "yolo");
    assert_eq!(dd.result["yolo_action"], a.id.as_str());
    assert_eq!(dd.result["yolo_rule"], "tool.worktree");
    assert_eq!(t.audit_actor(&d), vec!["yolo"]);
    // once: a second look does nothing
    assert!(t.engine.consider(&d).unwrap().is_none());
    assert_eq!(t.actions(&d).len(), 1);
}

#[test]
fn a_delete_outside_the_worktree_stays_for_the_user() {
    let t = t();
    let (item, sid, _) = t.item();
    let outside = t
        .kit
        .home()
        .join("elsewhere.txt")
        .to_string_lossy()
        .to_string();
    let d = t.permission(
        &item,
        &sid,
        ToolInput {
            raw: json!({"file_path": outside}),
            kind: Some("delete".into()),
            locations: vec![outside.clone()],
            own_dir: None,
            own_pids: None,
        },
    );
    let a = t
        .engine
        .consider(&d)
        .unwrap()
        .expect("declined is recorded");
    assert_eq!(a.outcome, "declined");
    assert!(a.reason.starts_with("硬限制"), "{}", a.reason);
    assert_eq!(t.decision(&d).status, "open");
    assert!(t.kit.rt.st().answered.is_empty());
    // recorded once
    assert!(t.engine.consider(&d).unwrap().is_none());
    assert_eq!(t.actions(&d).len(), 1);
}

#[test]
fn a_permission_the_runner_does_not_wait_for_is_left_alone() {
    let t = t();
    let (item, sid, cwd) = t.item();
    let d = t.card(
        &item,
        &sid,
        "approve",
        "tool_call",
        "允许执行：Edit",
        "{}",
        &PERM_OPTIONS,
    );
    let file = cwd.join("a").to_string_lossy().to_string();
    t.engine.stash(
        &d,
        ToolInput {
            raw: json!({"file_path": file}),
            kind: Some("edit".into()),
            locations: vec![],
            own_dir: None,
            own_pids: None,
        },
    );
    // stashed, not armed: the runner has not registered it yet
    assert!(t.engine.consider(&d).unwrap().is_none());
    assert!(t.actions(&d).is_empty());
}

#[test]
fn an_accept_with_passing_checks_passes_and_without_checks_is_reworked() {
    let t = t();
    let (item, sid, _) = t.item();
    let d = t.card(
        &item,
        &sid,
        "accept",
        "item",
        "做完了，请验收",
        "改好了\n\n检查：✓ cargo test；✓ cargo clippy",
        &["pass", "rework"],
    );
    // the card answers a delivery: the result is frozen and waits for this acceptance
    t.kit
        .core
        .tx()
        .lc_request_complete(
            &item,
            "bot",
            crate::cycle::Policy::Human,
            false,
            "做完了，请验收",
            Some(d.clone()),
            vec![],
        )
        .unwrap();
    let a = t.engine.consider(&d).unwrap().unwrap();
    assert_eq!(
        (a.outcome.as_str(), a.rule.as_str()),
        ("decided", "accept.checks")
    );
    let dd = t.decision(&d);
    assert_eq!(dd.result["choice"], "pass");
    assert_eq!(dd.result["decided_by"], "yolo");
    assert_eq!(t.audit_actor(&d), vec!["yolo"]);
    assert_eq!(t.kit.status(&item).0, "done");
    let tl = t.kit.db().timeline(&item).unwrap();
    let line = tl
        .iter()
        .find(|r| r.text == "YOLO：验收通过")
        .expect("timeline");
    assert_eq!((line.actor.as_str(), line.kind.as_str()), ("steve", "sys"));
    assert!(
        !tl.iter().any(|r| r.kind == "me"),
        "never as if you said it"
    );

    // no checks listed: back to the agent with the fixed text
    let (item, sid, _) = t.item();
    let d = t.card(
        &item,
        &sid,
        "accept",
        "item",
        "做完了，请验收",
        "改好了",
        &["pass", "rework"],
    );
    let a = t.engine.consider(&d).unwrap().unwrap();
    assert_eq!(
        (a.outcome.as_str(), a.rule.as_str()),
        ("decided", "accept.rework")
    );
    let want = format!(
        "验收没有通过，返工意见：{}\n改完再用 steve_deliver 交付。",
        rework_text(&[])
    );
    assert_eq!(t.kit.rt.prompts_of(&sid).last(), Some(&want));
}

#[test]
fn the_policy_setting_narrows_what_yolo_does() {
    let t = t();
    let mut p = serde_json::Map::new();
    p.insert(POLICY_KEY.into(), json!({"accept": false}));
    t.kit.db().settings_set(WS, &p, "me").unwrap();
    let (item, sid, _) = t.item();
    let d = t.card(
        &item,
        &sid,
        "accept",
        "item",
        "做完了",
        "x\n\n检查：✓ t",
        &["pass", "rework"],
    );
    let a = t.engine.consider(&d).unwrap().unwrap();
    assert_eq!(
        (a.outcome.as_str(), a.rule.as_str()),
        ("declined", "accept.off")
    );
    assert_eq!(t.decision(&d).status, "open");
}

#[test]
fn a_failed_session_continues_with_back_off_and_after_three_tries_stays_for_you() {
    let t = t();
    let (item, sid, _) = t.item();
    let opts = ["continue", "delivered", "look", "tosteve"];
    for n in 0..3u32 {
        let d = t.card(&item, &sid, "fail", "item", "会话出错：boom", "", &opts);
        let a = t.engine.consider(&d).unwrap().unwrap();
        assert_eq!(
            (a.outcome.as_str(), a.rule.as_str()),
            ("scheduled", "continue.error")
        );
        let wait = error_backoff_s(n, false) as i64 * 1000;
        assert_eq!(a.due_at, Some(t.clock.now() + wait), "back-off {n}");
        // not yet
        assert!(
            t.engine
                .tick()
                .iter()
                .all(|x| x.decision_id.as_deref() != Some(d.as_str()))
        );
        assert_eq!(t.decision(&d).status, "open");
        t.clock.advance(wait + 1);
        let done = t.engine.tick();
        let x = done
            .iter()
            .find(|x| x.decision_id.as_deref() == Some(d.as_str()))
            .expect("ran when due");
        assert_eq!(x.outcome, "decided");
        assert_eq!(t.decision(&d).result["decided_by"], "yolo");
        assert_eq!(
            t.kit.rt.prompts_of(&sid).last(),
            Some(&continue_error("boom"))
        );
        // the turn ends with the same error
        t.kit.rt.st().turning.remove(&sid);
    }
    // the fourth card: three continues without progress
    let d = t.card(&item, &sid, "fail", "item", "会话出错：boom", "", &opts);
    let a = t.engine.consider(&d).unwrap().unwrap();
    assert_eq!(
        (a.outcome.as_str(), a.rule.as_str()),
        ("declined", "continue.stuck")
    );
    assert_eq!(t.decision(&d).status, "open");
    let l = t.yolo.layers(WS, Some(&item)).unwrap();
    assert_eq!(l.item, Some(Mode::Off), "the item leaves YOLO");
    assert!(!t.yolo.effective(WS, Some(&item)).unwrap().on);
    let ns = t.kit.core.notifications(WS).unwrap();
    assert!(
        ns.iter()
            .any(|n| n.text.contains("连续 3 次自动继续没有进展")),
        "{:?}",
        ns.iter().map(|n| &n.text).collect::<Vec<_>>()
    );
}

#[test]
fn a_stopped_agent_is_continued_at_once() {
    let t = t();
    let (item, sid, _) = t.item();
    let d = t.card(
        &item,
        &sid,
        "fail",
        "item",
        "Agent 停下来了，没有交付也没有提问",
        "",
        &["continue", "delivered", "look"],
    );
    let a = t.engine.consider(&d).unwrap().unwrap();
    assert_eq!(
        (a.outcome.as_str(), a.rule.as_str()),
        ("decided", "continue.stall")
    );
    assert_eq!(
        t.kit.rt.prompts_of(&sid).last().map(String::as_str),
        Some(CONTINUE_STALL)
    );
    assert!(
        t.kit
            .timeline_texts(&item)
            .contains(&"YOLO：让它继续".to_string())
    );
}

#[test]
fn a_card_you_answer_first_cancels_the_scheduled_answer_and_restart_runs_it_once() {
    let t = t();
    let (item, sid, _) = t.item();
    let opts = ["continue", "delivered", "look"];
    let d = t.card(&item, &sid, "fail", "item", "会话出错：x", "", &opts);
    let a = t.engine.consider(&d).unwrap().unwrap();
    assert_eq!(a.outcome, "scheduled");
    t.kit.core.resolve(&d, "look", "", "me").unwrap();
    t.engine.consider(&d).unwrap();
    let rows = t.actions(&d);
    assert_eq!(rows[0].outcome, "cancelled");

    // a scheduled action survives a restart (a new executor) and runs once
    let (item, sid, _) = t.item();
    let d = t.card(&item, &sid, "fail", "item", "会话出错：y", "", &opts);
    t.engine.consider(&d).unwrap();
    let again = Engine::new(t.kit.core.clone(), t.yolo.clone());
    t.clock.advance(61_000);
    assert_eq!(
        again
            .tick()
            .iter()
            .filter(|x| x.outcome == "decided")
            .count(),
        1
    );
    assert!(
        again
            .tick()
            .iter()
            .all(|x| x.decision_id.as_deref() != Some(d.as_str()))
    );
    assert_eq!(
        t.kit
            .rt
            .prompts_of(&sid)
            .iter()
            .filter(|p| p.contains("上一回合出错"))
            .count(),
        1
    );
}

#[test]
fn you_being_around_makes_yolo_wait() {
    let t = t();
    let (item, sid, _) = t.item();
    t.kit
        .db()
        .timeline_add(&item, "me", "me", "我看一下", Some(&sid))
        .unwrap();
    let d = t.card(
        &item,
        &sid,
        "accept",
        "item",
        "做完了",
        "x\n\n检查：✓ t",
        &["pass", "rework"],
    );
    assert!(t.engine.consider(&d).unwrap().is_none(), "waits for you");
    assert_eq!(t.decision(&d).status, "open");
    t.clock.advance(181_000);
    let done = t.engine.tick();
    assert!(
        done.iter()
            .any(|x| x.decision_id.as_deref() == Some(d.as_str()) && x.outcome == "decided")
    );
}

#[test]
fn many_failure_cards_at_once_trip_the_breaker() {
    let t = t();
    let opts = ["continue", "delivered", "look"];
    let mut last = String::new();
    for n in 0..5 {
        let (item, sid, _) = t.item();
        last = t.card(
            &item,
            &sid,
            "fail",
            "item",
            "会话出错：模型通路断了",
            "",
            &opts,
        );
        if n < 4 {
            t.engine.consider(&last).unwrap();
        }
    }
    assert!(t.paused().is_none());
    assert!(t.engine.consider(&last).unwrap().is_none());
    let p = t.paused().expect("paused");
    assert_eq!(p["code"], "B2");
    // what was scheduled is cancelled, and one notice went out
    let w = t.yolo.store().reader(|c| repo::windows(c, WS, 1)).unwrap();
    assert_eq!(w[0].ended_why.as_deref(), Some("tripped"));
    let n = t
        .yolo
        .store()
        .reader(|c| repo::counts(c, &w[0].id))
        .unwrap();
    assert_eq!(n.scheduled, 0);
    let ns = t.kit.core.notifications(WS).unwrap();
    assert_eq!(
        ns.iter()
            .filter(|n| n.text.starts_with("YOLO 已暂停"))
            .count(),
        1
    );
    // paused: nothing more is done
    let (item, sid, _) = t.item();
    let d = t.card(
        &item,
        &sid,
        "accept",
        "item",
        "做完了",
        "x\n\n检查：✓ t",
        &["pass", "rework"],
    );
    assert!(t.engine.consider(&d).unwrap().is_none());
}

#[test]
fn undoing_yolo_twice_in_an_hour_trips_the_breaker() {
    let t = t();
    for n in 0..2 {
        let (item, sid, _) = t.item();
        let d = t.card(
            &item,
            &sid,
            "accept",
            "item",
            "做完了",
            "x\n\n检查：✓ t",
            &["pass", "rework"],
        );
        t.engine.consider(&d).unwrap().unwrap();
        t.kit.core.undo_decision(&d, "me").unwrap();
        assert_eq!(t.decision(&d).status, "open");
        // the undo is noticed; the card stays yours
        assert!(t.engine.consider(&d).unwrap().is_none());
        assert!(t.actions(&d)[0].undone.is_some());
        assert_eq!(t.decision(&d).status, "open");
        if n == 0 {
            assert!(t.paused().is_none());
        }
    }
    assert_eq!(t.paused().expect("paused")["code"], "B3");
}

#[test]
fn nothing_happens_while_switched_off() {
    let t = t();
    t.yolo
        .put(
            WS,
            "me",
            Switch {
                full: false,
                on: false,
                preview: false,
                include_open: false,
            },
        )
        .unwrap();
    let (item, sid, _) = t.item();
    let d = t.card(
        &item,
        &sid,
        "accept",
        "item",
        "做完了",
        "x\n\n检查：✓ t",
        &["pass", "rework"],
    );
    assert!(t.engine.consider(&d).unwrap().is_none());
    assert!(t.actions(&d).is_empty());
}

/// A Node session: the item's cwd says nothing about where the Node put its worktree (here it
/// points elsewhere), the Node's directory under `STEVE_HOME/data/node` is reported as `own_dir`.
fn node_item(t: &T) -> (String, String, std::path::PathBuf) {
    let id = t.kit.item("run", "执行", None);
    let elsewhere = t.kit.home().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let c = elsewhere.to_string_lossy().to_string();
    t.kit
        .db()
        .item_patch(&id, crate::patch![cwd: c.as_str()])
        .unwrap();
    let s = t.kit.session(&id, "idle");
    let dir = t.kit.home().join("data/node/worktrees/w1");
    std::fs::create_dir_all(&dir).unwrap();
    (id, s, limits::canonical(&dir))
}

fn node_edit(file: &str, own_dir: Option<std::path::PathBuf>) -> ToolInput {
    ToolInput {
        raw: json!({"file_path": file}),
        kind: Some("edit".into()),
        locations: vec![file.into()],
        own_dir,
        own_pids: Some(vec![]),
    }
}

fn node_exec(command: &str, cwd: &std::path::Path, own_pids: Vec<u32>) -> ToolInput {
    ToolInput {
        raw: json!({"command": command, "cwd": cwd}),
        kind: Some("execute".into()),
        locations: vec![],
        own_dir: Some(cwd.to_path_buf()),
        own_pids: Some(own_pids),
    }
}

#[test]
fn a_node_worktree_is_the_items_own_directory_when_the_node_says_so() {
    let t = t();
    let (item, sid, dir) = node_item(&t);
    let file = dir.join("src/x.rs").to_string_lossy().to_string();
    let d = t.permission(&item, &sid, node_edit(&file, Some(dir.clone())));
    let a = t.engine.consider(&d).unwrap().expect("an action");
    assert_eq!(
        (a.outcome.as_str(), a.rule.as_str()),
        ("decided", "tool.worktree"),
        "{}",
        a.reason
    );
    // without the Node's word the same path is Steve's data, never inside
    let d = t.permission(&item, &sid, node_edit(&file, None));
    let a = t.engine.consider(&d).unwrap();
    assert!(a.as_ref().is_none_or(|a| a.outcome != "decided"), "{a:?}");
    assert_eq!(t.decision(&d).status, "open");
}

#[test]
fn a_node_session_kills_only_what_the_node_says_its_harness_started() {
    let t = t();
    let (item, sid, dir) = node_item(&t);
    let mut child = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let pid = child.id();
    // the Hub's own child, but the Node did not report it: left to the user (no `ps` here)
    let d = t.permission(&item, &sid, node_exec(&format!("kill {pid}"), &dir, vec![]));
    let a = t.engine.consider(&d).unwrap();
    assert!(a.as_ref().is_none_or(|a| a.outcome != "decided"), "{a:?}");
    assert_eq!(t.decision(&d).status, "open");
    // reported by the Node: its own
    let d = t.permission(
        &item,
        &sid,
        node_exec(&format!("kill {pid}"), &dir, vec![pid]),
    );
    let a = t.engine.consider(&d).unwrap().expect("an action");
    let _ = child.kill();
    let _ = child.wait();
    assert_eq!(a.outcome, "decided", "{}", a.reason);
}

/// 审批过期: a card the Node expired is not answered by YOLO even though it was armed.
#[test]
fn an_expired_node_card_is_never_answered() {
    let t = t();
    let (item, sid, dir) = node_item(&t);
    let file = dir.join("a.txt").to_string_lossy().to_string();
    let d = t.permission(&item, &sid, node_edit(&file, Some(dir.clone())));
    t.kit
        .db()
        .decision_resolve(&d, json!({"superseded": "这一轮已经结束"}), "expired", None)
        .unwrap();
    assert!(t.engine.consider(&d).unwrap().is_none());
    assert!(t.kit.rt.st().answered.is_empty());
    assert!(t.engine.tool_input(&d).is_none(), "its input is let go");
}

/// After a Hub restart nothing is stashed or armed for an old card: YOLO leaves it alone.
#[test]
fn a_node_card_from_before_a_restart_is_left_alone() {
    let t = t();
    let (item, sid, _) = node_item(&t);
    let d = t.card(
        &item,
        &sid,
        "approve",
        "tool_call",
        "允许执行：Edit",
        "{}",
        &PERM_OPTIONS,
    );
    t.kit.rt.st().permissions.insert(d.clone());
    assert!(t.engine.consider(&d).unwrap().is_none());
    assert!(t.actions(&d).is_empty());
    assert!(t.kit.rt.st().answered.is_empty());
}
