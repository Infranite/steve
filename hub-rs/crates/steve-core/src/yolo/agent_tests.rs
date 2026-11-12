//! YOLO Y4: the protocol a session starts with (unchanged while YOLO is off, the lines while it
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
- steve_deliver: when the goal is met, with a Chinese summary, the outputs (branch, commits, files) and the checks you actually ran. Tick off finished plan steps with steve_update first; list in `unfinished` the steps you deliberately left open; any step not ticked off is shown to the user as left open. Then end your turn.
- steve_publish: only when the item changed code (commits on your branch), after the user has accepted the item and asked for it to go live. Commit everything on your branch first. Then end your turn. If you changed nothing (an explanation, a review, an answer), don't call it: accepting the item finishes it.
- steve_lane_status: after steve_publish, to see where the release is: the merge-lane batch it is in (MQ-<n>), an approval it waits for and who answers it, a failure and why, the merged commit and released version, and what you should do next. Read-only. Call it instead of asking the user which batch you are in or how the release is going.
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

/// A mode switch that fails is not lost: the next sync tries again, and the note is made once.
#[test]
fn a_failed_mode_switch_is_tried_again_by_the_next_sync() {
    let k = Kit::new();
    let y = yolo(&k);
    switch(&y, false);
    let (_, sid) = started(&k, &k.home().join("ops/release.sh"));
    switch(&y, true);
    k.rt.st().mode_error = true;
    assert!(k.core.yolo_sync(WS).is_empty(), "not switched");
    assert!(k.core.yolo_sync(WS).is_empty(), "still failing");
    assert!(k.rt.st().modes.is_empty());
    k.rt.st().mode_error = false;
    assert_eq!(k.core.yolo_sync(WS), vec![sid.clone()]);
    assert_eq!(k.rt.st().modes, vec![(sid.clone(), "auto".to_string())]);
    assert!(k.core.yolo_sync(WS).is_empty(), "told once");
    assert_eq!(say(&k, &sid, "继续"), format!("{}\n\n继续", agent::NOTE_ON));
}

/// A failed switch, then YOLO off again before any retry: the session never heard of it, so it
/// hears nothing (no stale 已开启 while YOLO is off), and its mode is put back.
#[test]
fn a_failed_switch_turned_back_leaves_no_note() {
    let k = Kit::new();
    let y = yolo(&k);
    switch(&y, false);
    let (_, sid) = started(&k, &k.home().join("ops/release.sh"));
    switch(&y, true);
    k.rt.st().mode_error = true;
    assert!(k.core.yolo_sync(WS).is_empty(), "not switched");
    switch(&y, false);
    assert!(k.core.yolo_sync(WS).is_empty(), "still failing");
    k.rt.st().mode_error = false;
    assert_eq!(
        k.core.yolo_sync(WS),
        vec![sid.clone()],
        "the mode is retried"
    );
    assert!(k.core.yolo_sync(WS).is_empty(), "once");
    assert_eq!(say(&k, &sid, "继续"), "继续");
}

/// A failed switch whose note is used up by a message before the retry: the retry only switches
/// the mode; the session is not told twice.
#[test]
fn a_retried_switch_after_its_note_was_heard_is_not_told_again() {
    let k = Kit::new();
    let y = yolo(&k);
    switch(&y, false);
    let (_, sid) = started(&k, &k.home().join("ops/release.sh"));
    switch(&y, true);
    k.rt.st().mode_error = true;
    assert!(k.core.yolo_sync(WS).is_empty(), "not switched");
    assert_eq!(say(&k, &sid, "继续"), format!("{}\n\n继续", agent::NOTE_ON));
    k.rt.st().mode_error = false;
    assert_eq!(k.core.yolo_sync(WS), vec![sid.clone()]);
    assert_eq!(k.rt.st().modes, vec![(sid.clone(), "auto".to_string())]);
    assert_eq!(say(&k, &sid, "再看看"), "再看看", "told once");
}

/// A mode the backend cannot express is not an error to retry: asked once, the change counts
/// as told, and later syncs leave it alone.
#[test]
fn an_unsupported_mode_is_asked_once() {
    let k = Kit::new();
    let y = yolo(&k);
    switch(&y, false);
    let (_, sid) = started(&k, &k.home().join("ops/release.sh"));
    switch(&y, true);
    k.rt.st().mode_unsupported = true;
    assert_eq!(k.core.yolo_sync(WS), vec![sid.clone()]);
    assert!(k.core.yolo_sync(WS).is_empty());
    assert!(k.core.yolo_sync(WS).is_empty());
    assert_eq!(k.rt.st().modes.len(), 1, "not asked again every tick");
    assert_eq!(say(&k, &sid, "继续"), format!("{}\n\n继续", agent::NOTE_ON));
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
    assert!(p.contains("do not push to any remote and do not change GitHub with gh"));
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

/// #38: an item with its own worktree edits without a card per file whether or not YOLO is on; an
/// item in a real directory keeps the bot's `default`.
#[test]
fn an_own_directory_widens_default_without_yolo_too() {
    let k = Kit::new();
    let y = yolo(&k);
    switch(&y, false);
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
    let i = k.item("run", "执行", Some(&p));
    let s = k.core.start_session(&i, None, None, &[]).unwrap();
    assert_eq!(
        k.core
            .session_perm_mode(&s, Some("default".into()))
            .as_deref(),
        Some("default")
    );
}

/// `GET /yolo/items/{id}`: the level and the layer that decided it, and per session the mode Steve
/// wants next to the one its backend confirmed; full access waiting for a running turn's end says
/// so, with what the harness still asks answered by YOLO, and only a confirmed mode is applied.
#[test]
fn an_items_state_says_what_is_wanted_and_what_the_backend_confirmed() {
    use crate::runtime::ModeState;
    let k = Kit::new();
    let y = yolo(&k);
    switch(&y, true);
    let (item, sid) = started(&k, &k.home().join("ops/release.sh"));
    let state = || k.core.yolo_item_state(WS, &item).unwrap();
    let s = state();
    assert_eq!(
        (s["level"].as_str(), s["source"].as_str()),
        (Some("on"), Some("workspace"))
    );
    assert_eq!(s["sessions"][0]["session_id"], sid.as_str());
    assert_eq!(
        s["sessions"][0]["status"], "not_running",
        "the backend cannot tell"
    );
    // full access while a turn runs in the mode it started with
    full(&y);
    k.rt.st().mode_states.insert(
        sid.clone(),
        ModeState {
            applied: Some("auto".into()),
            desired: Some("full_access".into()),
            busy: true,
            ..Default::default()
        },
    );
    let s = state();
    assert_eq!(s["level"], "full");
    let row = &s["sessions"][0];
    assert_eq!(row["wanted"], "full_access");
    assert_eq!(row["applied"], "auto", "not shown as in force before it is");
    assert_eq!(row["status"], "after_turn");
    assert_eq!(row["residual"], true);
    // confirmed
    k.rt.st().mode_states.insert(
        sid.clone(),
        ModeState {
            applied: Some("full_access".into()),
            ..Default::default()
        },
    );
    let s = state();
    assert_eq!(s["sessions"][0]["status"], "applied");
    assert_eq!(s["sessions"][0]["residual"], false);
    // the item steps down on its own: the item layer decides, the bypass is no longer wanted
    y.put_scope(WS, "me", "item", &item, &serde_json::json!({"mode": "on"}))
        .unwrap();
    let s = state();
    assert_eq!(
        (s["level"].as_str(), s["source"].as_str()),
        (Some("on"), Some("item"))
    );
    assert_eq!(s["layers"]["workspace"], "full");
    assert_eq!(s["layers"]["item"], "on");
    assert_eq!(s["sessions"][0]["wanted"], "auto");
    assert_eq!(s["sessions"][0]["status"], "not_sent");
    // the Node's owner refused full access
    y.put_scope(
        WS,
        "me",
        "item",
        &item,
        &serde_json::json!({"mode": "inherit"}),
    )
    .unwrap();
    k.rt.st().mode_states.insert(
        sid.clone(),
        ModeState {
            applied: Some("auto".into()),
            refused: Some("this node's owner allows at most permission mode auto here".into()),
            ..Default::default()
        },
    );
    let s = state();
    assert_eq!(s["sessions"][0]["status"], "refused");
    assert!(
        s["sessions"][0]["refused"]
            .as_str()
            .unwrap()
            .contains("at most")
    );
    // another workspace's item is not found
    assert!(k.core.yolo_item_state("other", &item).is_err());
}
