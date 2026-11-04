//! YOLO Y4: the protocol a session starts with (unchanged while YOLO is off, the §4.1 lines while it
//! is on, the release sentence only for Steve's own release), the harness mode, and what a running
//! session hears when YOLO changes under it.

use std::sync::Arc;

use steve_modules::FakeClock;

use super::agent::{self, perm_mode};
use super::{Switch, Yolo};
use crate::git::tests::repo;
use crate::repo::{Release, WS};
use crate::testkit::Kit;

/// The protocol's tool section before YOLO existed, byte for byte.
const BEFORE: &str = "How to work with Steve — use the steve MCP tools:
- steve_update: call it first with a short plan, and again whenever your step or plan changes. Write `now` in Chinese.
- steve_ask: when you need a decision from the user. Never ask questions in plain text. After calling it, end your turn.
- steve_request_approval: before anything with outside effects (deploy, release, push, send, pay, delete). After calling it, end your turn.
- steve_deliver: when the goal is met, with a Chinese summary, the outputs (branch, commits, files) and the checks you actually ran. Tick off finished plan steps with steve_update first; list in `unfinished` the steps you deliberately left open. A delivery with other open steps is refused. Then end your turn.
- steve_publish: only when the item changed code (commits on your branch), after the user has accepted the item and asked for it to go live. Commit everything on your branch first. Then end your turn. If you changed nothing (an explanation, a review, an answer), don't call it: accepting the item finishes it.
Do the work yourself; do not stop to ask for confirmation you don't need. Reply to the user in Chinese.";

fn yolo(k: &Kit) -> Yolo {
    let y = Yolo::with_clock(
        k.db().clone(),
        None,
        k.home(),
        Arc::new(FakeClock::new(steve_store::now())),
    );
    k.core.set_yolo(y.clone());
    y
}

fn switch(y: &Yolo, on: bool) {
    y.put(
        WS,
        "me",
        Switch {
            full: false,
            on,
            preview: false,
            include_open: false,
        },
    )
    .unwrap();
}

/// A git project with worktrees and a release script at `script`; an item in it with a session.
fn started(k: &Kit, script: &std::path::Path) -> (String, String) {
    let r = repo(&k.home().join(crate::util::uid("src")));
    let rel = Release {
        kind: "script".into(),
        cmd: script.to_string_lossy().into(),
        done_when: None,
    };
    let p = k
        .db()
        .project_create(
            WS,
            &crate::util::uid("Steve"),
            "",
            &r.to_string_lossy(),
            "git",
            true,
            Some("dev"),
            Some(&rel),
        )
        .unwrap()
        .id;
    let i = k.item("run", "执行", Some(&p));
    let s = k.core.start_session(&i, Some("开始"), None, &[]).unwrap();
    (i, s.id)
}

/// Ends the running turn, sends `text` as the user, and returns what the session got.
fn say(k: &Kit, sid: &str, text: &str) -> String {
    k.rt.end_turn(&k.core, sid, "end_turn", None);
    k.core.session_message(sid, text, false, &[]).unwrap();
    k.rt.prompts_of(sid).last().unwrap().clone()
}

fn first_prompt(k: &Kit, sid: &str) -> String {
    k.rt.prompts_of(sid)[0].clone()
}

#[test]
fn the_protocol_is_unchanged_while_off() {
    let k = Kit::new();
    let (_, s1) = started(&k, &k.home().join("ops/release.sh"));
    let without = first_prompt(&k, &s1);
    assert!(without.contains(BEFORE), "{without}");
    let y = yolo(&k);
    switch(&y, false);
    let (_, s2) = started(&k, &k.home().join("ops/release.sh"));
    let off = first_prompt(&k, &s2);
    // only the item and paths differ between the two sessions
    let tools = |p: &str| p[p.find("How to work with Steve").unwrap()..].to_string();
    assert_eq!(tools(&off), tools(&without));
    assert!(!off.contains("YOLO"));
}

#[test]
fn the_protocol_says_yolo_while_on_and_releases_only_for_steve_itself() {
    let k = Kit::new();
    let y = yolo(&k);
    switch(&y, true);
    let (_, sid) = started(&k, &k.home().join("ops/release.sh"));
    let p = first_prompt(&k, &sid);
    for l in [
        agent::UPDATE,
        agent::ON,
        agent::ASK,
        agent::APPROVAL,
        agent::DELIVER,
    ] {
        assert!(p.contains(l), "{l}");
    }
    assert!(p.contains(&format!("Then end your turn. If you changed nothing (an explanation, a review, an answer), don't call it: accepting the item finishes it.{}", agent::PUBLISH)));
    assert!(!p.contains("- steve_ask: when you need a decision from the user."));

    // a release script outside STEVE_HOME: no sentence about YOLO releasing
    let k = Kit::new();
    let y = yolo(&k);
    switch(&y, true);
    let elsewhere = tempfile::tempdir().unwrap();
    let (_, sid) = started(&k, &elsewhere.path().join("deploy.sh"));
    let p = first_prompt(&k, &sid);
    assert!(p.contains(agent::ON));
    assert!(!p.contains(agent::PUBLISH));
}

#[test]
fn the_mode_widens_only_default_and_only_with_an_own_directory() {
    let k = Kit::new();
    let y = yolo(&k);
    switch(&y, true);
    let (_, sid) = started(&k, &k.home().join("ops/release.sh"));
    let s = k.db().session(&sid).unwrap().unwrap();
    assert_eq!(
        k.core
            .session_perm_mode(&s, Some("default".into()))
            .as_deref(),
        Some("acceptEdits")
    );
    assert_eq!(
        k.core.session_perm_mode(&s, Some("plan".into())).as_deref(),
        Some("plan")
    );
    assert_eq!(k.core.session_perm_mode(&s, None), None);
    // an item working in a real directory keeps the bot's mode
    let dir = k.home().join("real");
    std::fs::create_dir_all(&dir).unwrap();
    let p = k
        .db()
        .project_create(
            WS,
            "真实目录",
            "",
            &dir.to_string_lossy(),
            "dir",
            false,
            Some("dev"),
            None,
        )
        .unwrap()
        .id;
    y.put_scope(WS, "me", "project", &p, &serde_json::json!({"mode": "on"}))
        .unwrap();
    let i = k.item("run", "执行", Some(&p));
    let s = k.core.start_session(&i, None, None, &[]).unwrap();
    assert_eq!(
        k.core
            .session_perm_mode(&s, Some("default".into()))
            .as_deref(),
        Some("default")
    );
    assert_eq!(
        perm_mode(Some("default"), false).as_deref(),
        Some("default")
    );
}

#[test]
fn a_running_session_hears_of_a_switch_with_its_next_message_and_its_mode_changes() {
    let k = Kit::new();
    let y = yolo(&k);
    switch(&y, false);
    let (item, sid) = started(&k, &k.home().join("ops/release.sh"));
    switch(&y, true);
    assert_eq!(k.core.yolo_sync(WS), vec![sid.clone()]);
    // the dev bot runs in `auto`, which YOLO leaves as it is
    assert_eq!(k.rt.st().modes, vec![(sid.clone(), "auto".to_string())]);
    assert!(k.core.yolo_sync(WS).is_empty(), "told once");
    assert_eq!(say(&k, &sid, "继续"), format!("{}\n\n继续", agent::NOTE_ON));
    assert_eq!(say(&k, &sid, "再看看"), "再看看");

    // the item taken out: the next message says YOLO is over
    y.put_scope(WS, "me", "item", &item, &serde_json::json!({"mode": "off"}))
        .unwrap();
    k.core.yolo_sync(WS);
    assert_eq!(say(&k, &sid, "好"), format!("{}\n\n好", agent::NOTE_OFF));
}

fn full(y: &Yolo) {
    y.put(
        WS,
        "me",
        Switch {
            full: true,
            on: true,
            preview: false,
            include_open: false,
        },
    )
    .unwrap();
}

#[test]
fn full_access_bypasses_prompts_and_says_what_still_holds() {
    let k = Kit::new();
    let y = yolo(&k);
    full(&y);
    let (_, sid) = started(&k, &k.home().join("ops/release.sh"));
    let s = k.db().session(&sid).unwrap().unwrap();
    // whatever the bot's mode: the harness asks nothing (steve-acp maps it to Codex full-access)
    for bot in [Some("default"), Some("plan"), Some("auto"), None] {
        assert_eq!(
            k.core
                .session_perm_mode(&s, bot.map(String::from))
                .as_deref(),
            Some(agent::BYPASS)
        );
    }
    let p = first_prompt(&k, &sid);
    assert!(p.contains(agent::ON), "full access is YOLO too");
    for l in agent::full_lines(&y.env().steve_home.to_string_lossy()) {
        assert!(p.contains(&l), "{l}\n{p}");
    }
    assert!(p.contains("never use pkill or killall"));
    assert!(p.contains("do not push to any remote unless the item explicitly asks"));
    assert!(p.contains(&format!("do not touch {}", y.env().steve_home.display())));
    // plain YOLO: no full-access lines, no bypass
    let k = Kit::new();
    let y = yolo(&k);
    switch(&y, true);
    let (_, sid) = started(&k, &k.home().join("ops/release.sh"));
    assert!(!first_prompt(&k, &sid).contains("Full access is on"));
    let s = k.db().session(&sid).unwrap().unwrap();
    assert_ne!(
        k.core
            .session_perm_mode(&s, Some("default".into()))
            .as_deref(),
        Some(agent::BYPASS)
    );
}

#[test]
fn switching_full_access_mid_run_changes_the_mode_in_place_and_off_restores_the_bot() {
    let k = Kit::new();
    let y = yolo(&k);
    switch(&y, true);
    let (item, sid) = started(&k, &k.home().join("ops/release.sh"));
    k.core.yolo_sync(WS);
    k.rt.st().modes.clear();
    // on → full: bypass at once, the note with the next message
    full(&y);
    assert_eq!(k.core.yolo_sync(WS), vec![sid.clone()]);
    assert_eq!(
        k.rt.st().modes,
        vec![(sid.clone(), agent::BYPASS.to_string())]
    );
    assert_eq!(
        say(&k, &sid, "继续"),
        format!("{}\n\n继续", agent::NOTE_FULL)
    );
    // the item steps down to plain YOLO: the dev bot's own `auto` again
    y.put_scope(WS, "me", "item", &item, &serde_json::json!({"mode": "on"}))
        .unwrap();
    k.core.yolo_sync(WS);
    assert_eq!(k.rt.st().modes.last().unwrap().1, "auto");
    assert_eq!(
        say(&k, &sid, "好"),
        format!("{}\n\n好", agent::NOTE_FULL_OFF)
    );
    // the item asks for full access on its own, then everything goes off
    y.put_scope(
        WS,
        "me",
        "item",
        &item,
        &serde_json::json!({"mode": "full", "confirm": true}),
    )
    .unwrap();
    k.core.yolo_sync(WS);
    assert_eq!(k.rt.st().modes.last().unwrap().1, agent::BYPASS);
    y.put_scope(WS, "me", "item", &item, &serde_json::json!({"mode": "off"}))
        .unwrap();
    k.core.yolo_sync(WS);
    assert_eq!(k.rt.st().modes.last().unwrap().1, "auto", "the bot's mode");
    assert_eq!(say(&k, &sid, "停"), format!("{}\n\n停", agent::NOTE_OFF));
}
