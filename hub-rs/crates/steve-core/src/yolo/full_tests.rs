//! 完全放开 (full access) while the harness still asks: a request left over from the mode the turn
//! started in (the switch lands at the turn's end) is answered as full access promises, at once and
//! once, even when ordinary YOLO had left it to the user; Steve's own data, payments, speaking as the
//! user, pushing and what cannot be read stay with the user. Scopes decide the level as they do for
//! YOLO, a read-only session never counts as full access, and stepping back down judges by the guard
//! again.

use serde_json::json;

use super::Switch;
use super::engine::ToolInput;
use super::engine_tests::{T, t};
use crate::repo::WS;

fn set(t: &T, on: bool, full: bool) {
    t.yolo
        .put(
            WS,
            "me",
            Switch {
                on,
                full,
                preview: false,
                include_open: false,
            },
        )
        .unwrap();
}

/// Outside the item's own directory, `STEVE_HOME` and the temporary directories.
const OUTSIDE: &str = "/opt/steve-yolo-test/out/*.txt";

fn exec(command: &str, cwd: &std::path::Path) -> ToolInput {
    ToolInput {
        raw: json!({ "command": command, "cwd": cwd }),
        kind: Some("execute".into()),
        locations: vec![],
        own_dir: None,
        own_pids: Some(vec![]),
    }
}

fn edit(file: &str) -> ToolInput {
    ToolInput {
        raw: json!({ "file_path": file, "content": "x" }),
        kind: Some("edit".into()),
        locations: vec![file.into()],
        own_dir: None,
        own_pids: Some(vec![]),
    }
}

/// (outcome, rule) of the one action `consider` wrote, or why there was none.
fn look(t: &T, d: &str) -> (String, String) {
    match t.engine.consider(d).unwrap() {
        Some(a) => (a.outcome, a.rule),
        None => ("none".into(), String::new()),
    }
}

fn answered(t: &T, d: &str) -> usize {
    t.kit
        .rt
        .st()
        .answered
        .iter()
        .filter(|(id, _)| id == d)
        .count()
}

/// Ordinary development work outside the worktree, which plain YOLO leaves to the user: under full
/// access it is answered `allow_once`, once.
#[test]
fn full_access_answers_ordinary_work_the_harness_still_asks_for() {
    let t = t();
    let (item, sid, cwd) = t.item();
    let bin = "/opt/steve-yolo-test/bin/tool";
    let cmd = format!("cargo build --release && cp target/release/tool {bin}");
    // plain YOLO: a write outside the worktree is a hard limit
    let first = t.permission(&item, &sid, exec(&cmd, &cwd));
    assert_eq!(look(&t, &first), ("declined".into(), "limit.4".into()));
    // full access: the same request again, a new card
    set(&t, true, true);
    let d = t.permission(&item, &sid, exec(&cmd, &cwd));
    assert_eq!(look(&t, &d), ("decided".into(), "tool.full".into()));
    assert_eq!(answered(&t, &d), 1);
    let dd = t.decision(&d);
    assert_eq!(dd.status, "resolved");
    assert_eq!(
        dd.result["optionId"], "once",
        "allow_once, never allow_always"
    );
    // an unknown program, an edit outside, a target behind a variable: as without a prompt
    for input in [
        exec("make -C /opt/steve-yolo-test/other all", &cwd),
        edit("/opt/steve-yolo-test/notes/x.md"),
        exec("rm -rf \"$OUT_DIR/cache\"", &cwd),
    ] {
        let d = t.permission(&item, &sid, input);
        assert_eq!(look(&t, &d), ("decided".into(), "tool.full".into()));
    }
    // the first card still waits: the switch is heard and it goes too
    let acts = t.engine.kick_waiting_tools(WS);
    assert_eq!(acts.len(), 1, "{acts:?}");
    assert_eq!(acts[0].decision_id.as_deref(), Some(first.as_str()));
    assert_eq!(answered(&t, &first), 1);
    // once: looking again (the tick, a second event) does nothing
    assert_eq!(look(&t, &d), ("none".into(), String::new()));
    assert!(t.engine.kick_waiting_tools(WS).is_empty());
    assert_eq!(answered(&t, &d), 1);
    assert_eq!(answered(&t, &first), 1);
}

/// What full access still keeps: Steve's own data (outside the item's own directory), port 8440
/// and launchd, payments, speaking as the user, pushing, root, `pkill`, what cannot be read, and
/// other MCP servers' tools. The item's own directory under `STEVE_HOME` is its own.
#[test]
fn full_access_keeps_steve_data_and_what_it_promises() {
    let t = t();
    set(&t, true, true);
    let (item, sid, cwd) = t.item();
    let home = t.kit.home().to_string_lossy().to_string();
    let data = format!("{home}/data/steve.sqlite3");
    let kept: Vec<(ToolInput, &str)> = vec![
        (exec(&format!("ls {home}"), &cwd), "limit.4"),
        (exec("cat ~/.steve/data/vault.json", &cwd), "limit.4"),
        (edit(&data), "limit.4"),
        (
            exec(&format!("rm -rf {home}/worktrees/ST-9"), &cwd),
            "limit.4",
        ),
        (exec("curl -s localhost:8440/api/state", &cwd), "limit.4"),
        (exec("launchctl kickstart gui/501/steve", &cwd), "limit.4"),
        (exec("git push origin HEAD", &cwd), "limit.5"),
        (exec("gh pr create --fill", &cwd), "limit.5"),
        (exec("lark-cli im send --text hi", &cwd), "limit.3"),
        (exec("npm run pay-invoice", &cwd), "limit.1"),
        (exec("sudo make install", &cwd), "tool.full"),
        (exec("pkill -f node", &cwd), "tool.full"),
        (exec("kill 4242", &cwd), "tool.full"),
        (exec("echo $(git push)", &cwd), "limit.5"),
        (exec("echo $(whoami)", &cwd), "tool.full"),
    ];
    for (input, rule) in kept {
        let what = input.raw.to_string();
        let d = t.permission(&item, &sid, input);
        assert_eq!(look(&t, &d), ("declined".into(), rule.into()), "{what}");
        assert_eq!(t.decision(&d).status, "open", "{what}");
        assert_eq!(answered(&t, &d), 0, "{what}");
    }
    let d = t.card(
        &item,
        &sid,
        "approve",
        "tool_call",
        "允许执行：mcp__lark__send",
        "{}",
        &super::engine_tests::PERM_OPTIONS,
    );
    t.kit.rt.st().permissions.insert(d.clone());
    t.engine.stash(
        &d,
        ToolInput {
            raw: json!({ "text": "hi" }),
            kind: Some("other".into()),
            locations: vec![],
            own_dir: None,
            own_pids: Some(vec![]),
        },
    );
    t.engine.arm(&d);
    assert_eq!(look(&t, &d), ("declined".into(), "tool.full".into()));
    // its own directory under STEVE_HOME is the item's own
    let own = cwd.join("src/main.rs").to_string_lossy().to_string();
    let d = t.permission(&item, &sid, edit(&own));
    assert_eq!(look(&t, &d), ("decided".into(), "tool.full".into()));
}

/// 等待审批时切换: a request ordinary YOLO left to the user while the runner waits for it is looked
/// at again once full access applies, answered once and never asked again; one full access keeps
/// is recorded once more, at its level, and then stays the user's.
#[test]
fn switching_to_full_while_a_request_waits_answers_it_once() {
    let t = t();
    let (item, sid, cwd) = t.item();
    let outside = OUTSIDE.replace('*', "a");
    let d = t.permission(&item, &sid, edit(&outside));
    let steve = t.permission(&item, &sid, exec("ls ~/.steve", &cwd));
    assert_eq!(look(&t, &d).0, "declined");
    assert_eq!(look(&t, &steve).0, "declined");
    // a decline holds while the level stays
    assert!(t.engine.kick_waiting_tools(WS).is_empty());
    set(&t, true, true);
    let acts = t.engine.kick_waiting_tools(WS);
    assert_eq!(acts.len(), 2, "{acts:?}");
    assert_eq!(t.decision(&d).status, "resolved");
    assert_eq!(answered(&t, &d), 1);
    assert_eq!(
        t.decision(&steve).status,
        "open",
        "Steve's data stays yours"
    );
    assert_eq!(answered(&t, &steve), 0);
    // the same switch heard again, the tick, a second event: nothing more
    for _ in 0..3 {
        assert!(t.engine.kick_waiting_tools(WS).is_empty());
        assert!(t.engine.tick().is_empty());
    }
    assert_eq!(answered(&t, &d), 1, "never answered twice");
    let outcomes = |id: &str| {
        t.actions(id)
            .into_iter()
            .map(|a| a.outcome)
            .collect::<Vec<_>>()
    };
    assert_eq!(outcomes(&d), ["declined", "decided"]);
    assert_eq!(outcomes(&steve), ["declined", "declined"]);
}

/// The item's and the project's own settings decide as they do for YOLO: an item set to plain YOLO
/// under a full workspace is judged by the guard; a project set to full under plain YOLO is full.
#[test]
fn scopes_decide_whether_a_request_is_judged_as_full_access() {
    let t = t();
    set(&t, true, true);
    let (item, sid, _) = t.item();
    let outside = OUTSIDE.replace('*', "b");
    t.yolo
        .put_scope(WS, "me", "item", &item, &json!({ "mode": "on" }))
        .unwrap();
    let d = t.permission(&item, &sid, edit(&outside));
    assert_eq!(look(&t, &d), ("declined".into(), "limit.4".into()));
    t.yolo
        .put_scope(WS, "me", "item", &item, &json!({ "mode": "off" }))
        .unwrap();
    let d = t.permission(&item, &sid, edit(&outside));
    assert_eq!(look(&t, &d).0, "none", "off: left alone, nothing recorded");
    // the item asks for full access under a plain-YOLO workspace
    set(&t, true, false);
    t.yolo
        .put_scope(
            WS,
            "me",
            "item",
            &item,
            &json!({ "mode": "full", "confirm": true }),
        )
        .unwrap();
    let d = t.permission(&item, &sid, edit(&outside));
    assert_eq!(look(&t, &d), ("decided".into(), "tool.full".into()));
    // a project set to full, its item inheriting
    let p = t
        .kit
        .db()
        .project_create(WS, "p", "", "/nowhere", "git", true, Some("dev"), None)
        .unwrap()
        .id;
    let (item2, sid2, _) = t.item();
    t.kit
        .db()
        .item_patch(&item2, crate::patch![project_id: p.as_str()])
        .unwrap();
    t.yolo
        .put_scope(
            WS,
            "me",
            "project",
            &p,
            &json!({ "mode": "full", "confirm": true }),
        )
        .unwrap();
    let d = t.permission(&item2, &sid2, edit(&outside));
    assert_eq!(look(&t, &d), ("decided".into(), "tool.full".into()));
}

/// 收紧权限: back from full access to plain YOLO, a new request is the guard's again; a read-only
/// session never bypasses prompts, so its requests are the guard's under full access too.
#[test]
fn stepping_down_or_a_read_only_session_is_judged_by_the_guard() {
    let t = t();
    set(&t, true, true);
    let (item, sid, _) = t.item();
    let outside = OUTSIDE.replace('*', "c");
    set(&t, true, false);
    let d = t.permission(&item, &sid, edit(&outside));
    assert_eq!(look(&t, &d), ("declined".into(), "limit.4".into()));
    // a decline under plain YOLO is not looked at again while YOLO stays plain
    assert_eq!(look(&t, &d).0, "none");
    set(&t, true, true);
    let ro = t.kit.session(&item, "idle");
    rusqlite::Connection::open(t.kit.home().join("data/steve.sqlite3"))
        .unwrap()
        .execute("UPDATE sessions SET readonly = 1 WHERE id = ?", [&ro])
        .unwrap();
    assert_eq!(t.kit.db().session(&ro).unwrap().unwrap().readonly, 1);
    let d = t.permission(&item, &ro, edit(&outside));
    assert_eq!(look(&t, &d), ("declined".into(), "limit.4".into()));
}

/// YOLO off while a tool request waits, then 完全放开 with the default “only new cards” window: the
/// request the runner still waits for is the running turn's, so it is answered once. Cards from
/// before the window that are not such a request — a business card, a tool card no runner waits
/// for, one the user rejected, one the Node expired — stay as they were, and stepping up only to
/// plain YOLO takes none of them.
#[test]
fn switching_from_off_to_full_answers_the_waiting_tool_request_only() {
    let t = t();
    set(&t, false, false);
    let (item, sid, cwd) = t.item();
    let outside = OUTSIDE.replace('*', "off");
    let waiting = t.permission(&item, &sid, exec(&format!("cp a.txt {outside}"), &cwd));
    let business = t.card(
        &item,
        &sid,
        "approve",
        "action",
        "要不要清理构建缓存？",
        "",
        &["yes", "no"],
    );
    // a card nothing waits for (no runner registered it)
    let unbound = t.card(
        &item,
        &sid,
        "approve",
        "tool_call",
        "允许执行：Edit",
        "{}",
        &super::engine_tests::PERM_OPTIONS,
    );
    // on an item of its own: the user's answer counts as their being around, and YOLO waits then
    let (item2, sid2, _) = t.item();
    let rejected = t.permission(&item2, &sid2, edit(&outside));
    t.kit.core.resolve(&rejected, "reject", "", "me").unwrap();
    let expired = t.permission(&item, &sid, edit(&outside));
    t.kit
        .db()
        .decision_resolve(
            &expired,
            json!({"superseded": "这一轮已经结束"}),
            "expired",
            None,
        )
        .unwrap();
    for d in [&waiting, &business, &unbound, &rejected, &expired] {
        assert_eq!(look(&t, d).0, "none", "off: nothing recorded");
    }
    // the window opens after every card above
    t.clock.advance(60_000);
    set(&t, true, false);
    assert!(
        t.engine.kick_waiting_tools(WS).is_empty(),
        "plain YOLO keeps to new cards"
    );
    set(&t, false, false);
    t.clock.advance(60_000);
    set(&t, true, true);
    let acts = t.engine.kick_waiting_tools(WS);
    assert_eq!(acts.len(), 1, "{acts:?}");
    assert_eq!(acts[0].decision_id.as_deref(), Some(waiting.as_str()));
    assert_eq!(
        (acts[0].outcome.as_str(), acts[0].rule.as_str()),
        ("decided", "tool.full")
    );
    assert_eq!(answered(&t, &waiting), 1);
    assert_eq!(t.decision(&waiting).result["optionId"], "once");
    for d in [&business, &unbound] {
        assert_eq!(look(&t, d).0, "none", "{d}: older than the window");
        assert_eq!(t.decision(d).status, "open");
        assert!(t.actions(d).is_empty());
    }
    // the user's own reject is the only answer the runner got; the expired card got none
    assert_eq!(answered(&t, &rejected), 1);
    assert_eq!(answered(&t, &expired), 0);
    for d in [&rejected, &expired] {
        assert!(t.actions(d).is_empty(), "{d}");
        assert_ne!(t.decision(d).result["decided_by"], "yolo");
    }
    // once: the tick, the event heard again
    for _ in 0..3 {
        assert!(t.engine.kick_waiting_tools(WS).is_empty());
        assert!(t.engine.tick().is_empty());
    }
    assert_eq!(answered(&t, &waiting), 1);
    // stepping down while another old request waits: the window holds again
    set(&t, false, false);
    let later = t.permission(&item, &sid, edit(&outside));
    t.clock.advance(60_000);
    set(&t, true, true);
    set(&t, true, false);
    assert_eq!(
        look(&t, &later).0,
        "none",
        "plain YOLO: older than the window"
    );
    assert_eq!(answered(&t, &later), 0);
}
