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
fn a_permission_inside_the_worktree_does_not_wait_for_you() {
    let t = t();
    let (item, sid, cwd) = t.item();
    t.kit
        .db()
        .timeline_add(&item, "me", "me", "改一下 notes", Some(&sid))
        .unwrap();
    // a message of yours queued behind the call: it waits for the turn, the turn for this answer
    t.kit.db().queue_add(&sid, "还有一点", "me", &[]).unwrap();
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
    let a = t.engine.consider(&d).unwrap().expect("answered now");
    assert_eq!(
        (a.outcome.as_str(), a.rule.as_str()),
        ("decided", "tool.worktree")
    );
}

#[test]
fn a_queued_message_counts_as_presence_only_within_the_window() {
    let t = t();
    let (item, sid, _) = t.item();
    t.kit.db().queue_add(&sid, "等一下", "me", &[]).unwrap();
    let card = |t: &T| {
        t.card(
            &item,
            &sid,
            "accept",
            "item",
            "做完了",
            "x\n\n检查：✓ t",
            &["pass", "rework"],
        )
    };
    let d = card(&t);
    assert!(
        t.engine.consider(&d).unwrap().is_none(),
        "a fresh message: waits"
    );
    // the message is still queued, but it is old now: it no longer holds the card
    t.clock.advance(181_000);
    assert_eq!(t.kit.db().queued(&sid).unwrap().len(), 1);
    let a = t.engine.consider(&d).unwrap().expect("decided");
    assert_eq!(a.outcome, "decided");
}

#[test]
fn many_failure_cards_at_once_trip_the_breaker() {
    let t = t();
    let opts = ["continue", "delivered", "look"];
    // stopped agents and other failures, however many, are not a systemic failure
    for _ in 0..4 {
        let (item, sid, _) = t.item();
        let d = t.card(
            &item,
            &sid,
            "fail",
            "item",
            "Agent 停下来了，没有交付也没有提问",
            "",
            &opts,
        );
        t.engine.consider(&d).unwrap();
    }
    // two cards on one item count once
    let (item, sid, _) = t.item();
    for _ in 0..2 {
        let d = t.card(
            &item,
            &sid,
            "fail",
            "item",
            "会话出错：模型通路断了",
            "",
            &opts,
        );
        t.engine.consider(&d).unwrap();
    }
    let mut last = String::new();
    for n in 0..2 {
        let (item, sid, _) = t.item();
        last = t.card(
            &item,
            &sid,
            "fail",
            "item",
            if n == 0 {
                "会话出错：模型通路断了"
            } else {
                "Hub 重启时这个会话正在运行，已经中断"
            },
            "",
            &opts,
        );
        if n == 0 {
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
fn undoing_one_rule_on_two_items_in_an_hour_trips_the_breaker() {
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
fn an_undo_takes_its_item_out_and_only_spreading_undos_pause_everything() {
    let t = t();
    // an acceptance you undid: that item leaves YOLO, nothing else changes
    let (a, sid, _) = t.item();
    let d = t.card(
        &a,
        &sid,
        "accept",
        "item",
        "做完了",
        "x\n\n检查：✓ t",
        &["pass", "rework"],
    );
    t.engine.consider(&d).unwrap().unwrap();
    t.kit.core.undo_decision(&d, "me").unwrap();
    assert!(t.engine.consider(&d).unwrap().is_none());
    assert!(t.paused().is_none());
    assert!(
        !t.yolo.effective(WS, Some(&a)).unwrap().on,
        "the item left YOLO"
    );
    let undo_on_new_item = |rule: &str| {
        let (item, _, _) = t.item();
        let x = t
            .yolo
            .record(&repo::NewAction {
                workspace_id: WS.into(),
                item_id: Some(item.clone()),
                kind: "ask".into(),
                subject_type: "question".into(),
                rule: rule.into(),
                outcome: "decided".into(),
                ..Default::default()
            })
            .unwrap();
        (item, t.yolo.undone(WS, &x.id).unwrap())
    };
    // another rule on a second item: two items, two rules
    let (b, tripped) = undo_on_new_item("ask.answer");
    assert!(!tripped && t.paused().is_none());
    assert!(!t.yolo.effective(WS, Some(&b)).unwrap().on);
    // a third item: undos spread, the workspace pauses
    let (_, tripped) = undo_on_new_item("propose.deps");
    assert!(tripped);
    assert_eq!(t.paused().expect("paused")["code"], "B3");
}

#[test]
fn two_recalls_pause_everything() {
    let t = t();
    let recall = |t: &T| {
        let x = t
            .yolo
            .record(&repo::NewAction {
                workspace_id: WS.into(),
                kind: "approve".into(),
                subject_type: "chan_message".into(),
                rule: "chan.bot".into(),
                outcome: "decided".into(),
                undo: Some(json!({"kind": "recall"})),
                ..Default::default()
            })
            .unwrap();
        t.yolo.undone(WS, &x.id).unwrap()
    };
    assert!(!recall(&t));
    assert!(t.paused().is_none());
    assert!(recall(&t));
    assert_eq!(t.paused().expect("paused")["code"], "B3");
}

fn auto(choice: &str, rule: &str) -> Verdict {
    Verdict::Auto {
        choice: choice.into(),
        text: None,
        delay_s: 0,
        rule: rule.into(),
        reason: String::new(),
        undo: None,
        checks: Value::Null,
    }
}

#[test]
fn a_recheck_that_says_not_now_or_no_does_not_trip_b5() {
    let t = t();
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
    // you typed while it was being judged: the answer waits for the next look
    t.kit
        .db()
        .timeline_add(&item, "me", "me", "等等", Some(&sid))
        .unwrap();
    let dd = t.decision(&d);
    assert!(
        t.engine
            .execute(&dd, &auto("pass", "accept.checks"), None, None)
            .unwrap()
            .is_none()
    );
    assert!(t.paused().is_none());
    assert!(t.actions(&d).is_empty());
    assert_eq!(t.decision(&d).status, "open");
    // a card the recheck leaves to you is recorded as declined
    let (item, sid, _) = t.item();
    let d = t.card(
        &item,
        &sid,
        "approve",
        "action",
        "删除 ~/Documents/x",
        "rm -rf ~/Documents/x",
        &["yes", "no"],
    );
    let dd = t.decision(&d);
    let a = t
        .engine
        .execute(&dd, &auto("yes", "action.local"), None, None)
        .unwrap()
        .unwrap();
    assert_eq!(a.outcome, "declined");
    assert!(t.paused().is_none());
    assert_eq!(t.decision(&d).status, "open");
}

#[test]
fn a_recheck_with_another_answer_trips_b5() {
    let t = t();
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
    let dd = t.decision(&d);
    assert!(
        t.engine
            .execute(&dd, &auto("rework", "accept.rework"), None, None)
            .unwrap()
            .is_none()
    );
    assert_eq!(t.paused().expect("paused")["code"], "B5");
    assert_eq!(t.decision(&d).status, "open");
}

#[test]
fn errors_before_answering_trip_b5_only_when_they_repeat() {
    let t = t();
    let (item, sid, _) = t.item();
    let d = t.card(
        &item,
        &sid,
        "accept",
        "item",
        "做完了",
        "",
        &["pass", "rework"],
    );
    let e = steve_modules::ApiError::Internal("database is locked".into());
    t.engine.fail_hard(&d, &e);
    t.engine.fail_hard(&d, &e);
    assert!(t.paused().is_none());
    // ten minutes later those two no longer count
    t.clock.advance(11 * 60_000);
    t.engine.fail_hard(&d, &e);
    t.engine.fail_hard(&d, &e);
    assert!(t.paused().is_none());
    t.engine.fail_hard(&d, &e);
    assert_eq!(t.paused().expect("paused")["code"], "B5");
}

#[test]
fn an_error_recording_an_answer_already_given_trips_b5_at_once() {
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
    // the ledger cannot settle the action once the card is answered
    rusqlite::Connection::open(t.kit.home().join("data/steve.sqlite3"))
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER yolo_no_decided BEFORE UPDATE OF outcome ON yolo_actions
               WHEN NEW.outcome = 'decided' BEGIN SELECT RAISE(ABORT, 'disk I/O error'); END;",
        )
        .unwrap();
    let a = t.engine.consider(&d).unwrap().expect("an action");
    assert_ne!(a.outcome, "decided", "not settled");
    assert_eq!(
        t.kit.rt.st().answered,
        vec![(d.clone(), Some("once".to_string()))]
    );
    // one error, after the answer: tripped, not counted towards a repeat
    assert_eq!(t.paused().expect("paused")["code"], "B5");
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

/// A Node far away (N0e-E11c): it answers every git question about the items it holds with the
/// same facts, and counts the questions.
struct Remote {
    items: std::collections::HashSet<String>,
    dirty: bool,
    ahead: u32,
    asked: std::sync::Mutex<Vec<crate::node_git::Ask>>,
    offline: bool,
}

#[async_trait::async_trait]
impl crate::node_git::NodeGit for Remote {
    fn on_node(&self, item: &str) -> bool {
        self.items.contains(item)
    }
    async fn facts(
        &self,
        _item: &str,
        q: crate::node_git::Ask,
    ) -> steve_node_api::ApiResult<steve_node_api::GitFacts> {
        use steve_node_api as na;
        self.asked.lock().unwrap().push(q.clone());
        if self.offline {
            return Err(na::ApiError::new(na::ErrorCode::Offline, "设备离线"));
        }
        Ok(na::GitFacts {
            revision: na::Revision::new("g1").unwrap(),
            head: "c0ffee0000000000000000000000000000000000".into(),
            branch: Some("steve/x".into()),
            dirty: self.dirty,
            refs: q
                .refs
                .iter()
                .map(|r| (r.clone(), (r == "main").then(|| "base".to_string())))
                .collect(),
            ranges: q
                .ranges
                .iter()
                .map(|(from, to)| na::RangeFacts {
                    from: from.clone(),
                    to: to.clone(),
                    merge_base: None,
                    ahead: self.ahead,
                    behind: 0,
                    overlap: vec![],
                    conflicts: vec![],
                })
                .collect(),
            reads: vec![],
        })
    }
    async fn apply(
        &self,
        _item: &str,
        _operation: &str,
        _subject: &str,
        _on: &steve_node_api::GitFacts,
        _action: steve_node_api::GitAction,
    ) -> crate::node_git::Applied {
        crate::node_git::Applied::Unknown("not here".into())
    }
}

/// An item on a remote Node whose `cwd` is a checkout on the Hub that git there would call dirty
/// and not ahead: what YOLO decides on is what the Node says.
fn remote_item(t: &T, remote: Remote) -> (String, String, Arc<Remote>) {
    let id = t.kit.item("run", "执行", None);
    let r = crate::git::tests::repo(&t.kit.home().join(&id));
    std::fs::write(r.join("dirty.txt"), "x").unwrap();
    let c = r.to_string_lossy().to_string();
    t.kit
        .db()
        .item_patch(&id, crate::patch![cwd: c.as_str(), branch: "steve/x"])
        .unwrap();
    let s = t.kit.session(&id, "idle");
    let mut remote = remote;
    remote.items.insert(id.clone());
    let remote = Arc::new(remote);
    t.kit.core.set_node_git(remote.clone());
    (id, s, remote)
}

fn far(dirty: bool, ahead: u32, offline: bool) -> Remote {
    Remote {
        items: Default::default(),
        dirty,
        ahead,
        asked: Default::default(),
        offline,
    }
}

fn accept_card(t: &T, item: &str, sid: &str) -> String {
    t.card(
        item,
        sid,
        "accept",
        "item",
        "做完了，请验收",
        "改好了\n\n检查：✓ cargo test",
        &["pass", "rework"],
    )
}

#[test]
fn an_acceptance_on_a_remote_node_reads_its_branch_there_and_no_git_here() {
    let t = t();
    let (item, sid, remote) = remote_item(&t, far(false, 2, false));
    let d = accept_card(&t, &item, &sid);
    let g = t
        .engine
        .gather(&t.decision(&d), &Policy::default())
        .unwrap();
    let judge::Subject::Accept(a) = &g.facts.subject else {
        panic!("{:?}", g.facts.subject)
    };
    // the Hub's checkout is dirty and has nothing new: the Node's word is clean and ahead
    assert_eq!(a.branch, Some((true, true)));
    assert_eq!(
        g.progress["head"],
        "c0ffee0000000000000000000000000000000000"
    );
    let asked = remote.asked.lock().unwrap().clone();
    assert_eq!(asked.len(), 2, "{asked:?}");
    assert_eq!(asked[0].refs, vec!["main", "master"]);
    // only the base the Node has is compared
    assert_eq!(
        asked[1].ranges,
        vec![("main".to_string(), "HEAD".to_string())]
    );
    assert!(asked.iter().all(|q| !q.fetch));
}

#[test]
fn a_dirty_branch_on_the_node_is_left_for_you_and_an_offline_one_too() {
    let t = t();
    let (item, sid, _) = remote_item(&t, far(true, 2, false));
    let d = accept_card(&t, &item, &sid);
    let g = t
        .engine
        .gather(&t.decision(&d), &Policy::default())
        .unwrap();
    let judge::Subject::Accept(a) = &g.facts.subject else {
        panic!()
    };
    assert_eq!(a.branch, Some((false, true)));

    let t = super::engine_tests::t();
    let (item, sid, remote) = remote_item(&t, far(false, 2, true));
    let d = accept_card(&t, &item, &sid);
    let g = t
        .engine
        .gather(&t.decision(&d), &Policy::default())
        .unwrap();
    let judge::Subject::Accept(a) = &g.facts.subject else {
        panic!()
    };
    // can't tell: never clean and ahead, and no head to call progress
    assert_eq!(a.branch, Some((false, false)));
    assert!(g.progress["head"].is_null());
    assert_eq!(remote.asked.lock().unwrap().len(), 1);
}

#[test]
fn other_cards_of_a_node_item_ask_only_for_its_head() {
    let t = t();
    let (item, sid, remote) = remote_item(&t, far(false, 2, false));
    let d = t.card(
        &item,
        &sid,
        "fail",
        "item",
        "Agent 停下来了",
        "",
        &["continue"],
    );
    let g = t
        .engine
        .gather(&t.decision(&d), &Policy::default())
        .unwrap();
    assert_eq!(
        g.progress["head"],
        "c0ffee0000000000000000000000000000000000"
    );
    let asked = remote.asked.lock().unwrap().clone();
    assert_eq!(asked.len(), 1, "no range for a head: {asked:?}");
    // the head alone: no status scan, no refs
    assert!(asked[0].heads_only, "{asked:?}");
    assert!(asked[0].refs.is_empty() && asked[0].ranges.is_empty());
}

/// A permission card of an item on a Node never waits on the Node: its verdict has no use for the
/// progress snapshot, so YOLO asks no git there (consider and execute alike).
#[test]
fn a_permission_of_a_node_item_asks_its_node_nothing() {
    let t = t();
    let (item, sid, remote) = remote_item(&t, far(false, 2, false));
    let input = ToolInput {
        raw: json!({"file_path": "a.txt"}),
        kind: Some("edit".into()),
        locations: vec![],
        own_dir: None,
        own_pids: None,
    };
    let d = t.permission(&item, &sid, input);
    let g = t
        .engine
        .gather(&t.decision(&d), &Policy::default())
        .unwrap();
    assert!(matches!(g.facts.subject, judge::Subject::Tool(_)));
    let _ = t.engine.consider(&d);
    let asked = remote.asked.lock().unwrap().clone();
    assert!(asked.is_empty(), "{asked:?}");
}

#[test]
fn a_kill_on_a_node_item_never_reads_the_hubs_process_table() {
    let t = t();
    let (item, sid, _) = remote_item(&t, far(false, 0, false));
    let mut child = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let pid = child.id();
    // the Hub's runtime would name this process's own pid as the adapter: a `ps` here would find
    // the child among its descendants
    t.kit
        .rt
        .st()
        .agent_pids
        .insert(sid.clone(), std::process::id());
    let input = ToolInput {
        raw: json!({"command": format!("kill {pid}")}),
        kind: Some("execute".into()),
        locations: vec![],
        own_dir: None,
        own_pids: None,
    };
    let d = t.permission(&item, &sid, input);
    let g = t
        .engine
        .gather(&t.decision(&d), &Policy::default())
        .unwrap();
    let _ = child.kill();
    let _ = child.wait();
    let judge::Subject::Tool(tool) = &g.facts.subject else {
        panic!("{:?}", g.facts.subject)
    };
    assert!(tool.own_pids.is_empty(), "{:?}", tool.own_pids);
    assert_eq!(t.kit.rt.st().pid_asks, 0);
}

#[test]
fn a_kill_on_a_hub_item_still_reads_the_hubs_process_table() {
    let t = t();
    let (item, sid, _) = t.item();
    let mut child = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let pid = child.id();
    t.kit
        .rt
        .st()
        .agent_pids
        .insert(sid.clone(), std::process::id());
    let input = ToolInput {
        raw: json!({"command": format!("kill {pid}")}),
        kind: Some("execute".into()),
        locations: vec![],
        own_dir: None,
        own_pids: None,
    };
    let d = t.permission(&item, &sid, input);
    let g = t
        .engine
        .gather(&t.decision(&d), &Policy::default())
        .unwrap();
    let _ = child.kill();
    let _ = child.wait();
    let judge::Subject::Tool(tool) = &g.facts.subject else {
        panic!()
    };
    assert!(tool.own_pids.contains(&pid), "{:?}", tool.own_pids);
    assert_eq!(t.kit.rt.st().pid_asks, 1);
}
