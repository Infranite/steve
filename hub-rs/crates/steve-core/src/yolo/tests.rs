//! YOLO Y0 (switch, scopes, breaker, preview against a temp database) and Y1 (the decision table, the
//! tool guard, and property tests that no policy gets past a hard limit).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use proptest::prelude::*;
use serde_json::{Value, json};
use steve_modules::{ApiError, FakeClock};
use steve_store::yolo as repo;

use super::judge::*;
use super::*;
use crate::repo::{NewDecision, NewItem, WS};
use crate::testkit::Kit;

// ── Y0: the switch and the scopes ──

struct T {
    kit: Kit,
    clock: Arc<FakeClock>,
    yolo: Yolo,
}

fn t_with(forced: Option<Forced>) -> T {
    let kit = Kit::new();
    let clock = Arc::new(FakeClock::new(1_000_000));
    let yolo = Yolo::with_clock(kit.db().clone(), forced, kit.home(), clock.clone());
    T { kit, clock, yolo }
}

fn t() -> T {
    t_with(None)
}

fn on() -> Switch {
    Switch {
        full: false,
        on: true,
        preview: false,
        include_open: false,
    }
}
fn off() -> Switch {
    Switch {
        full: false,
        on: false,
        preview: false,
        include_open: false,
    }
}

impl T {
    fn item(&self, project: Option<&str>) -> String {
        let bot = self.kit.db().main_bot().unwrap();
        self.kit
            .db()
            .item_create(
                WS,
                &NewItem {
                    title: "x".into(),
                    goal: "x".into(),
                    bot_id: bot.id,
                    project_id: project.map(String::from),
                    ..Default::default()
                },
            )
            .unwrap()
            .id
    }
    fn project(&self, name: &str, kind: &str, worktree: bool) -> String {
        let dir = self.kit.home().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        self.kit
            .db()
            .project_create(
                WS,
                name,
                "",
                &dir.to_string_lossy(),
                kind,
                worktree,
                None,
                None,
            )
            .unwrap()
            .id
    }
    fn on_for(&self, item: Option<&str>) -> bool {
        self.yolo.effective(WS, item).unwrap().on
    }
    fn audits(&self, action: &str) -> Vec<Value> {
        self.kit
            .db()
            .store()
            .with_conn(|c| {
                let mut st = c.prepare("SELECT actor, target, detail FROM audit WHERE action = ? ORDER BY id")?;
                let rows = st.query_map([action], |r| {
                    Ok(json!({ "actor": r.get::<_, String>(0)?, "target": r.get::<_, String>(1)?,
                               "detail": serde_json::from_str::<Value>(&r.get::<_, String>(2)?).unwrap_or(Value::Null) }))
                })?;
                Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
            })
            .unwrap()
    }
    fn revisions(&self, object_id: &str) -> Vec<(i64, Value)> {
        self.kit
            .db()
            .store()
            .reader(|c| steve_store::base::revisions(c, WS, repo::REVISION_TYPE, object_id))
            .unwrap()
            .into_iter()
            .map(|(v, p, _, _)| (v, p))
            .collect()
    }
    fn notices(&self) -> Vec<(String, String)> {
        self.kit
            .core
            .notifications(WS)
            .unwrap()
            .into_iter()
            .map(|n| (n.level, n.text))
            .collect()
    }
}

#[test]
fn switch_on_and_off_with_window_audit_revision_and_notice() {
    let t = t();
    let s = t.yolo.status(WS).unwrap();
    assert_eq!(
        (s["on"].clone(), s["active"].clone(), s["window"].clone()),
        (json!(false), json!(false), Value::Null)
    );

    let s = t
        .yolo
        .put(
            WS,
            "u1",
            Switch {
                include_open: true,
                ..on()
            },
        )
        .unwrap();
    assert_eq!(s["on"], true);
    assert_eq!(s["active"], true);
    assert_eq!(s["window"]["include_open"], true);
    assert_eq!(s["window"]["started"], 1_000_000);
    let w = s["window"]["id"].as_str().unwrap().to_string();
    assert!(w.starts_with("yw_"));
    // on again: same window, no second audit row
    t.clock.advance(5);
    assert_eq!(
        t.yolo.put(WS, "u1", on()).unwrap()["window"]["id"],
        w.as_str()
    );
    let a = t.audits("yolo.on");
    assert_eq!(a.len(), 1);
    assert_eq!(a[0]["actor"], "u1");
    assert_eq!(a[0]["detail"]["mode"], "on");
    assert_eq!(a[0]["detail"]["object_type"], "workspace");

    t.clock.advance(1000);
    let s = t.yolo.put(WS, "u1", off()).unwrap();
    assert_eq!(
        (s["on"].clone(), s["window"].clone()),
        (json!(false), Value::Null)
    );
    let row = t
        .kit
        .db()
        .store()
        .reader(|c| repo::window_get(c, &w))
        .unwrap()
        .unwrap();
    assert_eq!(row.ended_why.as_deref(), Some("off"));
    assert_eq!(row.ended, Some(1_001_005));
    assert_eq!(t.audits("yolo.off").len(), 1);
    // revisions: created on, then off
    let r = t.revisions(&format!("workspace:{WS}"));
    assert_eq!(r.iter().map(|x| x.0).collect::<Vec<_>>(), vec![2, 1]);
    assert_eq!(r[0].1["before"]["mode"], "on");
    assert_eq!(r[0].1["after"]["mode"], "off");
    assert_eq!(
        t.notices(),
        vec![(
            "info".into(),
            "YOLO 已结束：替你做了 0 件，留给你 0 件".into()
        )]
    );
    // off again changes nothing
    t.yolo.put(WS, "u1", off()).unwrap();
    assert_eq!(t.audits("yolo.off").len(), 1);
}

#[test]
fn switch_body_has_no_until() {
    assert_eq!(
        Switch::parse(&json!({"on": true, "until": 123})),
        Err(ApiError::invalid("until"))
    );
    assert_eq!(
        Switch::parse(&json!({"until": null, "on": "yes"})),
        Err(ApiError::invalid("on"))
    );
    assert_eq!(Switch::parse(&json!({})), Err(ApiError::invalid("on")));
    assert_eq!(
        Switch::parse(&json!({"on": true, "preview": 1})),
        Err(ApiError::invalid("preview"))
    );
    assert_eq!(
        Switch::parse(&json!({"on": true, "preview": true})),
        Ok(Switch {
            full: false,
            on: true,
            preview: true,
            include_open: false
        })
    );
}

#[test]
fn forced_off_refuses_on_and_says_why() {
    for (f, code) in [(Forced::Env, "env"), (Forced::NoResume, "no_resume")] {
        let t = t_with(Some(f));
        let e = t.yolo.put(WS, "u1", on()).unwrap_err();
        assert_eq!(e.status(), 409);
        assert_eq!(e.to_string(), f.message());
        let s = t.yolo.status(WS).unwrap();
        assert_eq!(s["forced_off"], code);
        assert_eq!(s["forced_reason"], f.message());
        assert_eq!(s["active"], false);
        // turning off is always allowed
        t.yolo.put(WS, "u1", off()).unwrap();
    }
    // a database copied with YOLO on still does nothing on a forced-off Hub
    let t = t();
    t.yolo.put(WS, "u1", on()).unwrap();
    let copy = Yolo::new(t.kit.db().clone(), Some(Forced::NoResume), t.kit.home());
    assert!(!copy.effective(WS, None).unwrap().on);
    assert_eq!(copy.status(WS).unwrap()["active"], false);
}

#[test]
fn item_beats_project_beats_workspace_in_the_database() {
    let t = t();
    let wt = t.project("wt", "git", true);
    let real = t.project("real", "dir", false);
    let a = t.item(Some(&wt));
    let b = t.item(Some(&real));
    let scratch = t.item(None);
    assert!(!t.on_for(Some(&a)) && !t.on_for(None));

    t.yolo.put(WS, "u1", on()).unwrap();
    assert!(t.on_for(Some(&a)));
    assert!(
        t.on_for(Some(&scratch)),
        "scratch items follow the workspace"
    );
    assert!(
        t.on_for(None),
        "decisions without an item follow the workspace"
    );
    assert!(
        !t.on_for(Some(&b)),
        "a real-directory project is off until turned on"
    );
    assert_eq!(
        t.yolo.effective(WS, Some(&b)).unwrap().source,
        Source::ProjectDefault
    );

    t.yolo
        .put_scope(WS, "u1", "project", &wt, &json!({"mode": "off"}))
        .unwrap();
    assert!(!t.on_for(Some(&a)));
    t.yolo
        .put_scope(WS, "u1", "item", &a, &json!({"mode": "on"}))
        .unwrap();
    assert!(t.on_for(Some(&a)));
    assert!(
        t.on_for(None),
        "the project row does not reach item-less decisions"
    );

    t.yolo.put(WS, "u1", off()).unwrap();
    t.yolo
        .put_scope(WS, "u1", "project", &real, &json!({"mode": "on"}))
        .unwrap();
    assert!(
        t.on_for(Some(&b)),
        "a project turned on works with the workspace off"
    );
    assert!(!t.on_for(Some(&scratch)));
    t.yolo
        .put_scope(WS, "u1", "item", &b, &json!({"mode": "off"}))
        .unwrap();
    assert!(!t.on_for(Some(&b)));
    t.yolo
        .put_scope(WS, "u1", "item", &b, &json!({"mode": "inherit"}))
        .unwrap();
    assert!(t.on_for(Some(&b)));

    // the defaults show up in the status, explicit rows replace them
    let s = t.yolo.status(WS).unwrap();
    let scopes = s["scopes"].as_array().unwrap();
    assert!(scopes.iter().any(|x| x["object_id"] == real.as_str()
        && x["mode"] == "on"
        && x.get("default").is_none()));
    assert!(
        scopes
            .iter()
            .any(|x| x["object_id"] == a.as_str() && x["mode"] == "on")
    );
    let audits = t.audits("yolo.scope");
    assert_eq!(audits.len(), 5);
    assert_eq!(audits[0]["target"], format!("project:{wt}"));
    assert_eq!(audits[0]["detail"]["mode"], "off");
    let r = t.revisions(&format!("item:{b}"));
    assert_eq!(r.iter().map(|x| x.0).collect::<Vec<_>>(), vec![2, 1]);
    // the same mode again writes nothing
    t.yolo
        .put_scope(WS, "u1", "item", &b, &json!({"mode": "inherit"}))
        .unwrap();
    assert_eq!(t.audits("yolo.scope").len(), 5);
}

#[test]
fn scope_input_and_ownership() {
    let t = t();
    let p = t.project("p", "git", true);
    let i = t.item(Some(&p));
    let other = "ws_other".to_string();
    let e = t
        .yolo
        .put_scope(&other, "u1", "project", &p, &json!({"mode": "on"}))
        .unwrap_err();
    assert_eq!((e.status(), e.to_string()), (404, "没有这个项目".into()));
    let e = t
        .yolo
        .put_scope(&other, "u1", "item", &i, &json!({"mode": "on"}))
        .unwrap_err();
    assert_eq!((e.status(), e.to_string()), (404, "没有这个事项".into()));
    let e = t
        .yolo
        .put_scope(WS, "u1", "item", "ST-404", &json!({"mode": "on"}))
        .unwrap_err();
    assert_eq!(e.status(), 404);
    assert_eq!(
        t.yolo
            .put_scope(WS, "u1", "item", &i, &json!({"mode": "maybe"}))
            .unwrap_err(),
        ApiError::invalid("mode")
    );
    assert_eq!(
        t.yolo
            .put_scope(WS, "u1", "project", &p, &json!({"mode": "on", "until": 5}))
            .unwrap_err(),
        ApiError::invalid("until")
    );
    assert_eq!(
        t.yolo
            .put_scope(WS, "u1", "workspace", WS, &json!({"mode": "on"}))
            .unwrap_err(),
        ApiError::invalid("type")
    );
    assert!(t.audits("yolo.scope").is_empty());
}

#[test]
fn breaker_pauses_until_resumed() {
    let t = t();
    t.yolo.put(WS, "u1", on()).unwrap();
    let w = t.yolo.status(WS).unwrap()["window"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let scheduled = t
        .kit
        .db()
        .store()
        .transaction(|tx| {
            repo::action_add(
                tx,
                &repo::NewAction {
                    workspace_id: WS.into(),
                    window_id: Some(w.clone()),
                    decision_id: Some("dc_1".into()),
                    kind: "approve".into(),
                    subject_type: "release".into(),
                    rule: "release.self".into(),
                    outcome: "scheduled".into(),
                    reason: "x".into(),
                    due_at: Some(5),
                    ..Default::default()
                },
                1,
            )
        })
        .unwrap();
    assert_eq!(t.yolo.status(WS).unwrap()["counts"]["scheduled"], 1);

    assert!(t.yolo.trip(WS, "B1", "连续 2 次发布失败").unwrap());
    assert!(
        !t.yolo.trip(WS, "B3", "again").unwrap(),
        "a second trip while paused does nothing"
    );
    let s = t.yolo.status(WS).unwrap();
    assert_eq!(s["paused"]["code"], "B1");
    assert_eq!(
        (s["on"].clone(), s["active"].clone(), s["window"].clone()),
        (json!(true), json!(false), Value::Null)
    );
    assert!(!t.on_for(None));
    let row = t
        .kit
        .db()
        .store()
        .reader(|c| repo::window_get(c, &w))
        .unwrap()
        .unwrap();
    assert_eq!(row.ended_why.as_deref(), Some("tripped"));
    let a = t
        .kit
        .db()
        .store()
        .reader(|c| repo::action_get(c, &scheduled.id))
        .unwrap()
        .unwrap();
    assert_eq!(a.outcome, "cancelled");
    assert_eq!(
        t.notices(),
        vec![("fail".into(), "YOLO 已暂停：连续 2 次发布失败".into())]
    );
    assert_eq!(t.audits("yolo.trip")[0]["actor"], "yolo");

    let e = t.yolo.put(WS, "u1", on()).unwrap_err();
    assert_eq!(e.status(), 409);
    let s = t.yolo.resume(WS, "u1").unwrap();
    assert_eq!(s["paused"], Value::Null);
    assert_eq!(s["active"], true);
    assert_ne!(
        s["window"]["id"],
        w.as_str(),
        "a new window, counters from zero"
    );
    assert_eq!(s["counts"]["scheduled"], 0);
    assert_eq!(t.yolo.resume(WS, "u1").unwrap_err().status(), 409);
    assert_eq!(t.audits("yolo.resume").len(), 1);
}

#[test]
fn preview_judges_the_open_cards_without_switching_on() {
    let t = t();
    let p = t.project("wt", "git", true);
    let i = t.item(Some(&p));
    let wt = t.kit.home().join("worktrees").join(&i);
    std::fs::create_dir_all(&wt).unwrap();
    t.kit
        .db()
        .item_patch(&i, crate::patch![cwd: wt.to_string_lossy()])
        .unwrap();
    let card = |subject_type: &str, kind: &str, q: &str, draft: Value, options: Vec<&str>| {
        t.kit
            .db()
            .decision_create(NewDecision {
                workspace_id: WS.into(),
                kind: kind.into(),
                subject_type: subject_type.into(),
                subject_id: "x".into(),
                item_id: Some(i.clone()),
                question: q.into(),
                draft: Some(serde_json::to_string_pretty(&draft).unwrap()),
                options: Some(options.into_iter().map(String::from).collect()),
                requested_by: "agent".into(),
                ..Default::default()
            })
            .unwrap()
    };
    let opts = vec![
        "ok|允许|allow_once",
        "always|总是允许|allow_always",
        "no|拒绝|reject_once",
    ];
    let test = card(
        "tool_call",
        "approve",
        "允许执行：cargo test",
        json!({"command": "cargo test"}),
        opts.clone(),
    );
    card(
        "tool_call",
        "approve",
        "允许执行：git push",
        json!({"command": "git push"}),
        opts,
    );
    card("signal", "ask", "新信号", json!(null), vec!["ok"]);
    card(
        "item",
        "accept",
        "交付了",
        json!(null),
        vec!["pass", "rework"],
    );

    let v = t
        .yolo
        .put(
            WS,
            "u1",
            Switch {
                preview: true,
                ..on()
            },
        )
        .unwrap();
    assert_eq!(
        v["would_handle"],
        json!([{ "decision_id": test.id, "question": "允许执行：cargo test",
        "choice": "ok", "rule": "tool.worktree", "delay_s": 0 }])
    );
    assert_eq!(
        (
            v["hard_limited"].clone(),
            v["muted"].clone(),
            v["keep_for_you"].clone()
        ),
        (json!(1), json!(1), json!(1))
    );
    assert_eq!(
        t.yolo.status(WS).unwrap()["on"],
        false,
        "a preview switches nothing on"
    );
    assert!(t.audits("yolo.on").is_empty());
    // with the item turned off, nothing is handled
    t.yolo
        .put_scope(WS, "u1", "item", &i, &json!({"mode": "off"}))
        .unwrap();
    let v = t
        .yolo
        .put(
            WS,
            "u1",
            Switch {
                preview: true,
                ..on()
            },
        )
        .unwrap();
    assert_eq!(
        (v["would_handle"].clone(), v["keep_for_you"].clone()),
        (json!([]), json!(4))
    );
}

#[test]
fn events_are_scoped_to_the_workspace() {
    let t = t();
    t.kit.events.lock().unwrap().clear();
    t.yolo.put(WS, "u1", on()).unwrap();
    let ev = t.kit.events.lock().unwrap().clone();
    assert!(ev.contains(&steve_store::ChangeEvent::Yolo { ws: WS.into() }));
    assert_eq!(
        serde_json::to_value(steve_store::ChangeEvent::YoloAction {
            id: "ya_1".into(),
            decision_id: Some("d".into()),
            ws: WS.into()
        })
        .unwrap(),
        json!({"type": "yolo_action", "id": "ya_1", "decision_id": "d", "ws": WS})
    );
    assert_eq!(
        steve_store::ChangeEvent::Yolo { ws: "w".into() }.ws(),
        Some("w")
    );
}

// ── Y1: the table ──

/// A worktree under a fake STEVE_HOME, a directory next to it, and a symlink from inside to outside.
struct Place {
    _dir: tempfile::TempDir,
    root: PathBuf,
    env: Env,
    cwd: PathBuf,
}

fn place() -> Place {
    let dir = tempfile::tempdir().unwrap();
    let root = limits::canonical(dir.path());
    let steve = root.join("steve");
    let cwd = steve.join("worktrees/ST-1");
    std::fs::create_dir_all(cwd.join("src")).unwrap();
    std::fs::create_dir_all(steve.join("worktrees/ST-2")).unwrap();
    std::fs::create_dir_all(steve.join("data")).unwrap();
    std::fs::create_dir_all(root.join("outside")).unwrap();
    std::os::unix::fs::symlink(root.join("outside"), cwd.join("link")).unwrap();
    std::os::unix::fs::symlink(root.join("nowhere/deep"), cwd.join("dangling")).unwrap();
    std::fs::create_dir_all(steve.join("current/ops")).unwrap();
    std::fs::write(steve.join("current/ops/release.sh"), "#!/bin/sh\n").unwrap();
    std::fs::create_dir_all(root.join("tmp")).unwrap();
    Place {
        env: Env {
            steve_home: steve,
            home: root.clone(),
            // a stand-in for /tmp: the temp dir the test runs in is not scratch space here
            tmp: vec![root.join("tmp")],
        },
        cwd,
        root,
        _dir: dir,
    }
}

const TOOL_OPTIONS: [&str; 3] = [
    "ok|允许|allow_once",
    "always|总是允许|allow_always",
    "no|拒绝|reject_once",
];

fn facts(kind: &str, subject_type: &str, options: &[&str], subject: Subject) -> Facts {
    Facts {
        decision_id: "dc_1".into(),
        kind: kind.into(),
        subject_type: subject_type.into(),
        question: String::new(),
        draft: String::new(),
        options: options.iter().map(|s| s.to_string()).collect(),
        in_scope: true,
        present: false,
        subject,
    }
}

fn tool_in(pl: &Place, kind: Option<&str>, raw: Value) -> Facts {
    let mut f = facts(
        "approve",
        "tool_call",
        &TOOL_OPTIONS,
        Subject::Tool(Tool {
            kind: kind.map(String::from),
            title: "t".into(),
            raw,
            truncated: false,
            locations: vec![],
            options: tool_options(&TOOL_OPTIONS.map(String::from)),
            cwd: Some(pl.cwd.clone()),
            project_dir: Some(pl.root.join("project")),
            branch: Some("steve/st-1".into()),
            own_pids: vec![],
        }),
    );
    f.question = "允许执行：t".into();
    f
}

fn cmd(pl: &Place, c: &str) -> Facts {
    tool_in(
        pl,
        Some("execute"),
        json!({ "command": c, "description": "d" }),
    )
}

fn run(f: &Facts, pl: &Place) -> Verdict {
    judge(f, &Policy::default(), &pl.env)
}

fn choice(v: &Verdict) -> Option<&str> {
    match v {
        Verdict::Auto { choice, .. } => Some(choice),
        _ => None,
    }
}

#[test]
fn tool_calls_inside_the_worktree_are_allowed_once() {
    let pl = place();
    for c in [
        "cargo test",
        "cargo build --release && cargo clippy -- -D warnings",
        "git status && git diff HEAD~1",
        "git add -A && git commit -m 'fix: x'",
        "git rebase main",
        "npm run build",
        "npm test 2>&1 | tail -20",
        "rm -rf target",
        "mkdir -p src/x && touch src/x/a.rs",
        "ls -la src | head",
        "grep -rn foo src",
        "cat src/../Cargo.toml",
        "mv src/a.rs src/b.rs",
        "cd src && ls",
        "python3 -m pytest -q",
        "sed -n 1,20p src/main.rs",
        "find . -name '*.rs'",
        "cargo test > out.txt",
    ] {
        let v = run(&cmd(&pl, c), &pl);
        assert_eq!(choice(&v), Some("ok"), "{c}: {v:?}");
        let Verdict::Auto { checks, rule, .. } = v else {
            unreachable!()
        };
        assert_eq!(rule, "tool.worktree");
        assert_eq!(checks["commands"][0], c);
    }
    for (kind, raw) in [
        (
            "edit",
            json!({ "file_path": pl.cwd.join("src/a.rs"), "old_string": "a", "new_string": "b" }),
        ),
        ("read", json!({ "file_path": pl.cwd.join("src/a.rs") })),
        ("read", json!({ "file_path": "/usr/include/stdio.h" })),
        ("search", json!({ "pattern": "fn main" })),
        (
            "fetch",
            json!({ "url": "https://docs.rs/serde", "prompt": "x" }),
        ),
        ("think", json!({})),
    ] {
        assert_eq!(
            choice(&run(&tool_in(&pl, Some(kind), raw.clone()), &pl)),
            Some("ok"),
            "{kind} {raw}"
        );
    }
}

#[test]
fn the_guard_leaves_the_rest_to_the_user() {
    let pl = place();
    for c in [
        "make deploy-docs",         // limit
        "kill -9 1234",             // never
        "open .",                   // never
        "echo $(whoami)",           // opaque
        "bash -c 'cargo test'",     // shell
        "eval cargo test",          // never
        "sudo cargo test",          // sudo
        "echo $HOME",               // variable
        "frobnicate --all",         // unknown program
        "cargo install ripgrep",    // global
        "npm i -g x",               // global
        "git fetch",                // not on the list
        "git branch -D steve/st-1", // own branch, but -D stays with you
        "git rebase -i HEAD~3",     // not main
        "cat ~/.ssh/id_rsa",        // secrets
        "cat /etc/passwd",          // outside
        "find . -delete",           // find -delete
        "cd /tmp && ls",            // cd out
        "ls 'unterminated",         // unreadable
        "cp src/a.rs ../ST-2/a.rs", // limit
    ] {
        let v = run(&cmd(&pl, c), &pl);
        assert!(matches!(v, Verdict::Decline { .. }), "{c}: {v:?}");
    }
    // other MCP servers' tools, unknown kinds
    let mut f = tool_in(&pl, Some("other"), json!({ "to": "x" }));
    if let Subject::Tool(t) = &mut f.subject {
        t.title = "mcp__mail__send".into();
    }
    assert_eq!(run(&f, &pl).rule(), "tool.guard");
    // edits outside, reads of another .env
    let f = tool_in(
        &pl,
        Some("edit"),
        json!({ "file_path": pl.root.join("outside/a"), "new_string": "x" }),
    );
    assert!(run(&f, &pl).is_hard_limit());
    let f = tool_in(
        &pl,
        Some("read"),
        json!({ "file_path": pl.root.join("project/.env") }),
    );
    assert_eq!(run(&f, &pl).rule(), "tool.guard");
}

#[test]
fn truncated_input_and_missing_worktree_are_never_allowed() {
    let pl = place();
    let mut f = cmd(&pl, "cargo test");
    if let Subject::Tool(t) = &mut f.subject {
        t.truncated = true;
    }
    assert_eq!(judge(&f, &Policy::widest(), &pl.env).rule(), "tool.guard");
    let mut f = cmd(&pl, "cargo test");
    if let Subject::Tool(t) = &mut f.subject {
        t.cwd = None;
    }
    assert_eq!(
        judge(&f, &Policy::widest(), &pl.env).rule(),
        "tool.no_worktree"
    );
    // only allow_always on offer
    let mut f = cmd(&pl, "cargo test");
    f.options = vec!["always|总是允许|allow_always".into()];
    if let Subject::Tool(t) = &mut f.subject {
        t.options = tool_options(&f.options);
    }
    assert!(!judge(&f, &Policy::widest(), &pl.env).is_auto());
}

#[test]
fn from_the_stored_row_a_cut_draft_counts_as_truncated() {
    let pl = place();
    let d = crate::repo::Decision {
        id: "dc".into(),
        workspace_id: WS.into(),
        kind: "approve".into(),
        subject_type: "tool_call".into(),
        subject_id: "x".into(),
        item_id: None,
        session_id: None,
        question: "允许执行：Bash".into(),
        draft: format!("{{\n \"command\": \"echo {}", "a".repeat(2100))[..2000].to_string(),
        options: TOOL_OPTIONS.map(String::from).to_vec(),
        status: "open".into(),
        result: Value::Null,
        requested_by: "agent".into(),
        created: 0,
        resolved: None,
        undo: Value::Null,
    };
    let f = facts_of(&d, subject_from_row(&d, Some(pl.cwd.clone()), None, None));
    assert_eq!(judge(&f, &Policy::widest(), &pl.env).rule(), "tool.guard");
}

fn accept_facts(
    checks: &[(&str, bool)],
    unfinished: &[&str],
    branch: Option<(bool, bool)>,
    manual: bool,
) -> Facts {
    facts(
        "accept",
        "item",
        &["pass", "rework"],
        Subject::Accept(Accept {
            checks: checks
                .iter()
                .map(|(n, p)| CheckRun {
                    name: n.to_string(),
                    passed: *p,
                })
                .collect(),
            unfinished: unfinished.iter().map(|s| s.to_string()).collect(),
            branch,
            explicit_manual: manual,
            no_progress: 0,
        }),
    )
}

fn release_facts(pl: &Place, cmd: &str) -> Release {
    let _ = pl;
    Release {
        batch: false,
        release_kind: "script".into(),
        cmd: cmd.into(),
        branch_ok: true,
        accepted: true,
        releasing: false,
        spacing_wait_s: 0,
        rebase_returns: 0,
    }
}

fn outbound(
    identity: Identity,
    disclosure: Disclosure,
    ai_prefix: bool,
    stage: OutboundStage,
) -> Outbound {
    Outbound {
        identity,
        disclosure,
        external: false,
        customer: false,
        ai_prefix,
        send_option: Some("yes".into()),
        stage,
    }
}

/// One case per row (with the user's decisions): `(name, facts, expected choice or rule)`.
#[test]
fn every_row_of_the_table() {
    let pl = place();
    let own = pl
        .env
        .steve_home
        .join("current/ops/release.sh")
        .to_string_lossy()
        .to_string();
    let yes_no = ["yes", "no"];
    let mut action = facts("approve", "action", &yes_no, Subject::Action);
    action.question = "可以把生成的文件放进 src/gen 吗".into();
    let mut action_out = facts("approve", "action", &yes_no, Subject::Action);
    action_out.question = "可以推送到远端吗".into();
    let mut action_home = facts("approve", "action", &yes_no, Subject::Action);
    action_home.question = "要改 ~/.zshrc".into();
    let stall = |cause: StallCause, no_progress: u32| {
        facts(
            "fail",
            "item",
            &["continue", "delivered", "look", "tosteve"],
            Subject::Stall(Stall {
                cause,
                no_progress,
                attempts: 0,
                asked: None,
                steve_tried: false,
            }),
        )
    };
    let ask = |answer: Option<Answer>, recommended: Option<&str>, repeated: bool| {
        facts(
            "ask",
            "question",
            &["A", "B"],
            Subject::Ask(Ask {
                recommended: recommended.map(String::from),
                why: Some("更简单".into()),
                answer,
                repeated,
                count: 0,
                key: "用 a 还是 b".into(),
                full: false,
            }),
        )
    };
    let ans = |act: &str, confidence: f64, only_lacks_info: bool| Answer {
        act: act.into(),
        answer: "用 A".into(),
        option: Some("A".into()),
        confidence,
        reason: "目标里写了".into(),
        only_lacks_info,
    };
    let mut ext = outbound(Identity::Bot, Disclosure::Pass, false, OutboundStage::Draft);
    ext.external = true;
    let mut self_ext = outbound(
        Identity::SelfAccount,
        Disclosure::Pass,
        true,
        OutboundStage::Draft,
    );
    self_ext.external = true;
    let mut releasing = release_facts(&pl, &own);
    releasing.releasing = true;
    let mut rebased = release_facts(&pl, &own);
    rebased.rebase_returns = 4;

    let cases: Vec<(&str, Facts, &str)> = vec![
        ("tool in worktree", cmd(&pl, "cargo test"), "ok"),
        ("tool push", cmd(&pl, "git push"), "limit.5"),
        ("action local", action, "yes"),
        ("action outward", action_out, "limit.5"),
        ("action outside", action_home, "action.outside"),
        (
            "accept checks pass",
            accept_facts(&[("cargo test", true)], &[], Some((true, true)), false),
            "pass",
        ),
        (
            "accept no checks",
            accept_facts(&[], &[], None, false),
            "rework",
        ),
        (
            "accept failed check",
            accept_facts(&[("build", true), ("test", false)], &[], None, false),
            "rework",
        ),
        (
            "accept unfinished",
            accept_facts(&[("t", true)], &["docs"], None, false),
            "accept.unfinished",
        ),
        (
            "accept dirty branch",
            accept_facts(&[("t", true)], &[], Some((false, true)), false),
            "accept.branch",
        ),
        (
            "accept manual",
            accept_facts(&[("t", true)], &[], None, true),
            "accept.manual",
        ),
        (
            "release own",
            facts(
                "approve",
                "release",
                &yes_no,
                Subject::Release(release_facts(&pl, &own)),
            ),
            "yes",
        ),
        (
            "release elsewhere",
            facts(
                "approve",
                "release",
                &yes_no,
                Subject::Release(release_facts(&pl, "/usr/local/bin/deploy.sh")),
            ),
            "limit.5",
        ),
        (
            "release while releasing",
            facts("approve", "release", &yes_no, Subject::Release(releasing)),
            "release.wait",
        ),
        (
            "release 4th rebase",
            facts("approve", "release", &yes_no, Subject::Release(rebased)),
            "release.rebase",
        ),
        (
            "batch own",
            facts(
                "approve",
                "delivery_batch",
                &yes_no,
                Subject::Release(Release {
                    batch: true,
                    ..release_facts(&pl, &own)
                }),
            ),
            "yes",
        ),
        (
            "batch failed",
            facts("fail", "delivery_batch", &["look"], Subject::BatchFailed),
            "batch.failed",
        ),
        ("stall", stall(StallCause::Stopped, 0), "continue"),
        (
            "stall stuck",
            stall(StallCause::Stopped, 3),
            "continue.stuck",
        ),
        (
            "stall after Steve continued it",
            {
                let mut f = stall(StallCause::Stopped, 0);
                if let Subject::Stall(s) = &mut f.subject {
                    s.steve_tried = true;
                }
                f
            },
            "continue.steve",
        ),
        (
            "session error",
            stall(
                StallCause::Error {
                    message: "boom".into(),
                    restart: false,
                },
                0,
            ),
            "continue",
        ),
        (
            "release failed once",
            stall(
                StallCause::ReleaseFailed {
                    prev: "v1".into(),
                    reason: "health".into(),
                    log: "/l".into(),
                    nth: 1,
                },
                0,
            ),
            "continue",
        ),
        (
            "release failed twice",
            stall(
                StallCause::ReleaseFailed {
                    prev: "v1".into(),
                    reason: "health".into(),
                    log: "/l".into(),
                    nth: 2,
                },
                0,
            ),
            "continue.release",
        ),
        (
            "release refused busy",
            stall(
                StallCause::ReleaseRefused {
                    busy: true,
                    still_busy: true,
                },
                0,
            ),
            "continue.release",
        ),
        (
            "other failure",
            stall(StallCause::Other, 0),
            "continue.other",
        ),
        (
            "ask confident",
            ask(Some(ans("answer", 0.9, false)), None, false),
            "A",
        ),
        (
            "ask low, recommended",
            ask(Some(ans("answer", 0.5, false)), Some("B"), false),
            "B",
        ),
        (
            "ask objected",
            ask(Some(ans("ask_user", 0.9, false)), Some("B"), false),
            "ask.unsure",
        ),
        (
            "ask lacks info, recommended",
            ask(Some(ans("ask_user", 0.2, true)), Some("B"), false),
            "B",
        ),
        (
            "ask nothing",
            ask(Some(ans("answer", 0.2, false)), None, false),
            "ask.unsure",
        ),
        (
            "ask repeated",
            ask(Some(ans("answer", 0.9, false)), Some("A"), true),
            "ask.repeated",
        ),
        (
            "propose deps",
            facts("propose", "item", &yes_no, Subject::ProposeDeps),
            "yes",
        ),
        (
            "propose chat",
            facts(
                "propose",
                "item",
                &yes_no,
                Subject::ProposeCreate { from_chat: true },
            ),
            "yes",
        ),
        (
            "propose channel",
            facts(
                "propose",
                "item",
                &yes_no,
                Subject::ProposeCreate { from_chat: false },
            ),
            "propose.source",
        ),
        (
            "secret normal",
            facts(
                "approve",
                "secret_use",
                &["allow"],
                Subject::SecretUse {
                    template: "api_key".into(),
                    sensitivity: "normal".into(),
                },
            ),
            "secret",
        ),
        (
            "secret high",
            facts(
                "approve",
                "secret_use",
                &["allow"],
                Subject::SecretUse {
                    template: "api_key".into(),
                    sensitivity: "high".into(),
                },
            ),
            "limit.2",
        ),
        (
            "bank card",
            facts(
                "approve",
                "secret_use",
                &["allow"],
                Subject::SecretUse {
                    template: "bank_card".into(),
                    sensitivity: "sensitive".into(),
                },
            ),
            "limit.1",
        ),
        (
            "mail as you, [AI]",
            facts(
                "approve",
                "mail_draft",
                &["yes"],
                Subject::Mail(outbound(
                    Identity::SelfAccount,
                    Disclosure::Pass,
                    true,
                    OutboundStage::Draft,
                )),
            ),
            "yes",
        ),
        (
            "mail without [AI]",
            facts(
                "approve",
                "mail_draft",
                &["yes"],
                Subject::Mail(outbound(
                    Identity::SelfAccount,
                    Disclosure::Pass,
                    false,
                    OutboundStage::Draft,
                )),
            ),
            "limit.3",
        ),
        (
            "mail external",
            facts(
                "approve",
                "mail_draft",
                &["yes"],
                Subject::Mail(self_ext.clone()),
            ),
            "mail.external",
        ),
        (
            "chan self [AI]",
            facts(
                "approve",
                "chan_message",
                &["yes"],
                Subject::Chan(outbound(
                    Identity::SelfAccount,
                    Disclosure::Rewritten,
                    true,
                    OutboundStage::Draft,
                )),
            ),
            "yes",
        ),
        (
            "chan proxy no [AI]",
            facts(
                "approve",
                "chan_message",
                &["yes"],
                Subject::Chan(outbound(
                    Identity::Proxy,
                    Disclosure::Pass,
                    false,
                    OutboundStage::Draft,
                )),
            ),
            "limit.3",
        ),
        (
            "chan self unchecked",
            facts(
                "approve",
                "chan_message",
                &["yes"],
                Subject::Chan(outbound(
                    Identity::SelfAccount,
                    Disclosure::Unchecked,
                    true,
                    OutboundStage::Draft,
                )),
            ),
            "limit.3",
        ),
        (
            "chan bot internal",
            facts(
                "approve",
                "chan_message",
                &["yes"],
                Subject::Chan(outbound(
                    Identity::Bot,
                    Disclosure::Pass,
                    false,
                    OutboundStage::Draft,
                )),
            ),
            "yes",
        ),
        (
            "chan bot external",
            facts("approve", "chan_message", &["yes"], Subject::Chan(ext)),
            "chan.external",
        ),
        (
            "chan leak draft",
            facts(
                "approve",
                "chan_message",
                &["yes"],
                Subject::Chan(outbound(
                    Identity::Bot,
                    Disclosure::Leak,
                    false,
                    OutboundStage::LeakDraft,
                )),
            ),
            "limit.6",
        ),
        (
            "chan blocked",
            facts(
                "fail",
                "chan_message",
                &["discard"],
                Subject::Chan(outbound(
                    Identity::Bot,
                    Disclosure::Blocked,
                    false,
                    OutboundStage::Blocked,
                )),
            ),
            "limit.6",
        ),
        (
            "chan retry",
            facts(
                "fail",
                "chan_message",
                &["retry", "discard"],
                Subject::Chan(Outbound {
                    send_option: Some("retry".into()),
                    ..outbound(
                        Identity::Bot,
                        Disclosure::Pass,
                        false,
                        OutboundStage::SendFailed,
                    )
                }),
            ),
            "retry",
        ),
        (
            "chan handoff",
            facts(
                "ask",
                "chan_message",
                &["yes"],
                Subject::Chan(outbound(
                    Identity::Bot,
                    Disclosure::Pass,
                    false,
                    OutboundStage::Handoff,
                )),
            ),
            "chan.handoff",
        ),
        (
            "github",
            facts("approve", "gh_pr_open", &["yes"], Subject::Github),
            "limit.5",
        ),
        (
            "flow step",
            facts("approve", "flow_step", &["yes"], Subject::FlowStep),
            "flow",
        ),
        (
            "flow over budget",
            facts(
                "fail",
                "flow_run",
                &["continue_once"],
                Subject::FlowRun { over_budget: true },
            ),
            "limit.1",
        ),
        (
            "connection",
            facts("fail", "connection", &["reauth"], Subject::Connection),
            "connection",
        ),
        (
            "muted",
            facts("ask", "signal", &["ok"], Subject::Muted),
            "mute",
        ),
        (
            "unknown",
            facts("approve", "brand_new", &["yes"], Subject::Unknown),
            "unknown",
        ),
    ];
    for (name, f, want) in cases {
        let v = run(&f, &pl);
        let got = choice(&v).unwrap_or(v.rule());
        assert_eq!(got, want, "{name}: {v:?}");
    }
}

#[test]
fn delays_texts_and_undo() {
    let pl = place();
    let own = pl
        .env
        .steve_home
        .join("current/ops/release.sh")
        .to_string_lossy()
        .to_string();
    let v = run(
        &facts(
            "approve",
            "release",
            &["yes", "no"],
            Subject::Release(release_facts(&pl, &own)),
        ),
        &pl,
    );
    assert!(
        matches!(
            v,
            Verdict::Auto {
                delay_s: 180,
                undo: Some(Undo::Rollback),
                ..
            }
        ),
        "{v:?}"
    );
    let v = run(
        &accept_facts(&[("build", true), ("test", false)], &[], None, false),
        &pl,
    );
    let Verdict::Auto { text, .. } = v else {
        panic!()
    };
    assert_eq!(
        text.as_deref(),
        Some("自动验收需要实际运行过的检查全部通过：✗ test。修好、运行检查后重新交付。")
    );
    let Verdict::Auto { text, .. } = run(&accept_facts(&[], &[], None, false), &pl) else {
        panic!()
    };
    assert_eq!(
        text.as_deref(),
        Some("自动验收需要实际运行过的检查全部通过：没有列出检查。修好、运行检查后重新交付。")
    );
    let f = facts(
        "fail",
        "stall",
        &["continue", "look", "drop"],
        Subject::Stall(Stall {
            cause: StallCause::Stopped,
            no_progress: 0,
            attempts: 0,
            asked: None,
            steve_tried: false,
        }),
    );
    let Verdict::Auto { text, .. } = run(&f, &pl) else {
        panic!()
    };
    assert_eq!(text.as_deref(), Some(CONTINUE_STALL));
    let f = facts(
        "fail",
        "item",
        &["continue"],
        Subject::Stall(Stall {
            cause: StallCause::Error {
                message: "x".repeat(300),
                restart: true,
            },
            no_progress: 0,
            attempts: 1,
            asked: None,
            steve_tried: false,
        }),
    );
    let Verdict::Auto { text, delay_s, .. } = run(&f, &pl) else {
        panic!()
    };
    assert_eq!(delay_s, 900);
    assert_eq!(
        text.unwrap(),
        format!("上一回合出错（{}）。从你停下的地方继续。", "x".repeat(200))
    );
    assert_eq!(
        (
            error_backoff_s(0, false),
            error_backoff_s(1, false),
            error_backoff_s(0, true)
        ),
        (60, 300, 300)
    );
    let f = facts(
        "ask",
        "question",
        &["A"],
        Subject::Ask(Ask {
            recommended: None,
            why: None,
            answer: Some(Answer {
                act: "answer".into(),
                answer: "用 A".into(),
                option: None,
                confidence: 0.8,
                reason: "目标".into(),
                only_lacks_info: false,
            }),
            repeated: false,
            count: 0,
            key: String::new(),
            full: false,
        }),
    );
    let Verdict::Auto {
        text, choice, undo, ..
    } = run(&f, &pl)
    else {
        panic!()
    };
    assert_eq!((choice.as_str(), undo), ("用 A", Some(Undo::Reanswer)));
    assert_eq!(
        text.unwrap(),
        "（YOLO：Steve 代你回答）用 A。依据：目标。如果这个假设后来证明不对，在交付摘要里写明。"
    );
    let v = run(
        &facts(
            "approve",
            "chan_message",
            &["yes"],
            Subject::Chan(outbound(
                Identity::Bot,
                Disclosure::Pass,
                false,
                OutboundStage::Draft,
            )),
        ),
        &pl,
    );
    assert!(
        matches!(
            v,
            Verdict::Auto {
                delay_s: 600,
                undo: Some(Undo::Recall),
                ..
            }
        ),
        "{v:?}"
    );
}

#[test]
fn presence_waits_and_scope_comes_first() {
    let pl = place();
    let mut f = accept_facts(&[("t", true)], &[], None, false);
    f.present = true;
    assert!(matches!(run(&f, &pl), Verdict::Wait { delay_s: 180, .. }));
    f.in_scope = false;
    assert_eq!(run(&f, &pl), Verdict::Off);
    // a tool call inside the worktree does not wait for the user: the guard judges it now
    let mut f = cmd(&pl, "cargo test");
    f.present = true;
    assert_eq!(run(&f, &pl).rule(), "tool.worktree");
    // a hard limit is reported even when the user is around
    let mut f = cmd(&pl, "git push");
    f.present = true;
    assert!(run(&f, &pl).is_hard_limit());
}

// ── Y1: hard limits hold under every policy ──

/// Commands that hit a hard limit, with its number. `{cwd}`, `{steve}`, `{root}` are filled in.
const LIMITED: &[(&str, u8)] = &[
    ("git push origin main", 5),
    ("git push --force", 5),
    ("git -C . push", 5),
    ("/usr/bin/git push", 5),
    ("git -c alias.p=push p", 5),
    ("git p", 5),
    ("git remote add o https://example.com/x.git", 5),
    ("git remote set-url origin git@x:y", 5),
    ("git config alias.ship push", 5),
    ("git submodule foreach git push", 5),
    ("gh pr create --fill", 5),
    ("gh pr merge 3 --squash", 5),
    ("gh pr comment 1 -b hi", 5),
    ("gh api -X POST repos/a/b/issues", 5),
    ("gh api repos/a/b/issues -f title=x", 5),
    ("curl -d a=1 https://example.com", 5),
    ("curl -sSd @f https://example.com", 5),
    ("curl -X POST https://example.com", 5),
    ("curl -XPUT https://example.com", 5),
    ("curl --data-binary @f https://example.com", 5),
    ("curl --json '{}' https://example.com", 5),
    ("curl -F f=@a https://example.com", 5),
    ("curl -T a https://example.com", 5),
    ("wget --post-data=a https://example.com", 5),
    ("npm publish", 5),
    ("pnpm publish --access public", 5),
    ("npm run deploy", 5),
    ("cargo publish", 5),
    ("python3 -m twine upload dist/x", 5),
    ("npx vercel deploy", 5),
    ("docker push img", 5),
    ("ssh host ls", 5),
    ("scp a host:b", 5),
    ("rsync -a . host:/x", 5),
    ("bash -c 'git push'", 5),
    ("sh -c \"gh pr create\"", 5),
    ("zsh -c 'cd src && git push'", 5),
    ("find . -name x -exec git push \\;", 5),
    ("cargo test && git push", 5),
    ("echo ok; curl -X DELETE https://example.com/x", 5),
    ("make release", 5),
    ("rm -rf ../x", 4),
    ("rm -rf /tmp/x", 4),
    ("rm -rf ~", 4),
    ("rm -rf {root}/outside", 4),
    ("rm -rf link/x", 4),
    ("rm -rf dangling", 4),
    ("rm -rf $HOME/x", 4),
    ("rm -rf src/../../x", 4),
    ("mv src/a.rs ../b.rs", 4),
    ("cp src/a.rs /etc/hosts", 4),
    ("echo hi > ../x", 4),
    ("cargo test 2> {root}/outside/log", 4),
    ("cd .. && rm -rf ST-2", 4),
    ("cd /tmp; rm -rf x", 4),
    ("xargs rm < list.txt", 4),
    ("find / -name x -delete", 4),
    ("find .. -delete", 4),
    ("find link -delete", 4),
    ("git -C .. clean -fdx", 4),
    ("git branch -D steve/st-99", 4),
    ("git worktree remove ../ST-2", 4),
    ("sed -i '' s/a/b/ ../x", 4),
    ("dd if=/dev/zero of=../x", 4),
    ("tar -xf a.tar -C /tmp", 4),
    ("chmod 777 /etc/x", 4),
    ("tee {root}/outside/x", 4),
    ("rm -rf {steve}/data", 4),
    ("cat {steve}/data/steve.sqlite3", 4),
    ("cat ../ST-2/secret", 4),
    ("cat ~/.steve/data/steve.sqlite3", 4),
    ("curl http://127.0.0.1:8440/api/state", 4),
    ("lsof -i :8440", 4),
    ("launchctl kickstart -k gui/501/dev.steve", 4),
    ("ls ~/Library/LaunchAgents", 4),
    ("{steve}/current/ops/release.sh", 4),
    ("bash ops/release.sh", 4),
    ("eval 'rm -rf ../x'", 4),
    ("bash -c 'rm -rf /'", 4),
    ("timeout 5 sudo rm -rf /var/x", 4),
    ("env FOO=1 nohup rm -rf ../x", 4),
    ("npm run checkout", 1),
    ("echo 续费 > note.txt", 1),
];

/// Ways an agent might dress up a command.
const WRAPS: &[(&str, &str)] = &[
    ("", ""),
    ("FOO=1 ", ""),
    ("env ", ""),
    ("nohup ", ""),
    ("cargo build && ", ""),
    ("true; ", ""),
    ("", " 2>&1"),
    ("", " | tail -5"),
    ("", " || true"),
    ("\n", "\n"),
];

fn fill(c: &str, pl: &Place) -> String {
    c.replace("{cwd}", &pl.cwd.to_string_lossy())
        .replace("{steve}", &pl.env.steve_home.to_string_lossy())
        .replace("{root}", &pl.root.to_string_lossy())
}

fn arb_policy() -> impl Strategy<Value = Policy> {
    (
        any::<[bool; 10]>(),
        0.0f64..1.0,
        proptest::sample::subsequence(WIDE_PROGRAMS.to_vec(), 0..=WIDE_PROGRAMS.len()),
        0u64..1000,
    )
        .prop_map(|(b, conf, extra, delay)| Policy {
            approve_actions: b[0],
            accept: b[1],
            release_self: b[2],
            continue_stalls: b[3],
            answer_asks: b[4],
            ask_min_confidence: conf,
            propose: b[5],
            chan_bot_internal: b[6],
            self_send: b[7],
            self_send_external: b[8],
            extra_programs: extra.into_iter().map(String::from).collect(),
            presence_s: if b[9] { 0 } else { delay },
            release_delay_s: delay,
            propose_chat_delay_s: delay,
            chan_delay_s: delay,
            chan_retry_delay_s: delay,
        })
}

/// Non-command decisions that hit a hard limit, built for a place.
fn limited_subjects(pl: &Place) -> Vec<(&'static str, Facts, u8)> {
    let yes = ["yes", "send", "retry", "allow", "continue_once"];
    let mut money = facts("approve", "action", &yes, Subject::Action);
    money.question = "要给服务器续费吗".into();
    let mut pay = accept_facts(&[("t", true)], &[], None, false);
    pay.draft = "完成了 checkout 页面并 pay 了一笔".into();
    let mut gh_open = facts("approve", "gh_pr_open", &yes, Subject::Github);
    gh_open.question = "开 PR".into();
    let all_ids = ["yes", "send", "retry", "discard"];
    let o = |identity, disclosure, ai, stage| Outbound {
        send_option: Some("yes".into()),
        external: false,
        ..outbound(identity, disclosure, ai, stage)
    };
    vec![
        (
            "bank card",
            facts(
                "approve",
                "secret_use",
                &yes,
                Subject::SecretUse {
                    template: "bank_card".into(),
                    sensitivity: "normal".into(),
                },
            ),
            1,
        ),
        (
            "high vault",
            facts(
                "approve",
                "secret_use",
                &yes,
                Subject::SecretUse {
                    template: "password".into(),
                    sensitivity: "high".into(),
                },
            ),
            2,
        ),
        ("money words", money, 1),
        ("pay in a delivery", pay, 1),
        (
            "flow over budget",
            facts(
                "fail",
                "flow_run",
                &yes,
                Subject::FlowRun { over_budget: true },
            ),
            1,
        ),
        (
            "mail no [AI]",
            facts(
                "approve",
                "mail_draft",
                &all_ids,
                Subject::Mail(o(
                    Identity::SelfAccount,
                    Disclosure::Pass,
                    false,
                    OutboundStage::Draft,
                )),
            ),
            3,
        ),
        (
            "mail [AI] unchecked",
            facts(
                "approve",
                "mail_draft",
                &all_ids,
                Subject::Mail(o(
                    Identity::SelfAccount,
                    Disclosure::Unchecked,
                    true,
                    OutboundStage::Draft,
                )),
            ),
            3,
        ),
        (
            "mail labelled bot",
            facts(
                "approve",
                "mail_draft",
                &all_ids,
                Subject::Mail(o(
                    Identity::Bot,
                    Disclosure::Pass,
                    false,
                    OutboundStage::Draft,
                )),
            ),
            3,
        ),
        (
            "self chan no [AI]",
            facts(
                "approve",
                "chan_message",
                &all_ids,
                Subject::Chan(o(
                    Identity::SelfAccount,
                    Disclosure::Pass,
                    false,
                    OutboundStage::Draft,
                )),
            ),
            3,
        ),
        (
            "self chan retry no [AI]",
            facts(
                "fail",
                "chan_message",
                &all_ids,
                Subject::Chan(o(
                    Identity::SelfAccount,
                    Disclosure::Pass,
                    false,
                    OutboundStage::SendFailed,
                )),
            ),
            3,
        ),
        (
            "proxy no [AI]",
            facts(
                "approve",
                "chan_message",
                &all_ids,
                Subject::Chan(o(
                    Identity::Proxy,
                    Disclosure::Rewritten,
                    false,
                    OutboundStage::Draft,
                )),
            ),
            3,
        ),
        (
            "leak",
            facts(
                "approve",
                "chan_message",
                &all_ids,
                Subject::Chan(o(
                    Identity::Bot,
                    Disclosure::Leak,
                    true,
                    OutboundStage::Draft,
                )),
            ),
            6,
        ),
        (
            "leak draft",
            facts(
                "approve",
                "chan_message",
                &all_ids,
                Subject::Chan(o(
                    Identity::Bot,
                    Disclosure::Pass,
                    true,
                    OutboundStage::LeakDraft,
                )),
            ),
            6,
        ),
        (
            "blocked",
            facts(
                "fail",
                "chan_message",
                &all_ids,
                Subject::Chan(o(
                    Identity::SelfAccount,
                    Disclosure::Blocked,
                    true,
                    OutboundStage::Blocked,
                )),
            ),
            6,
        ),
        (
            "leak mail",
            facts(
                "approve",
                "mail_draft",
                &all_ids,
                Subject::Mail(o(
                    Identity::SelfAccount,
                    Disclosure::Leak,
                    true,
                    OutboundStage::Draft,
                )),
            ),
            6,
        ),
        ("gh open", gh_open, 5),
        (
            "gh merge",
            facts("approve", "gh_pr_merge", &yes, Subject::Github),
            5,
        ),
        (
            "gh comment",
            facts("approve", "gh_comment", &yes, Subject::Github),
            5,
        ),
        (
            "foreign release script",
            facts(
                "approve",
                "release",
                &yes,
                Subject::Release(release_facts(pl, "/usr/local/bin/release.sh")),
            ),
            5,
        ),
        (
            "release script via ..",
            facts(
                "approve",
                "release",
                &yes,
                Subject::Release(release_facts(
                    pl,
                    &format!("{}/../deploy.sh", pl.env.steve_home.display()),
                )),
            ),
            5,
        ),
        (
            "release not a script",
            facts(
                "approve",
                "release",
                &yes,
                Subject::Release(Release {
                    release_kind: "github".into(),
                    ..release_facts(
                        pl,
                        &format!("{}/current/ops/release.sh", pl.env.steve_home.display()),
                    )
                }),
            ),
            5,
        ),
        (
            "batch elsewhere",
            facts(
                "approve",
                "delivery_batch",
                &yes,
                Subject::Release(Release {
                    batch: true,
                    ..release_facts(pl, "relative/release.sh")
                }),
            ),
            5,
        ),
        (
            "edit outside",
            tool_in(
                pl,
                Some("edit"),
                json!({ "file_path": pl.root.join("outside/x"), "new_string": "x" }),
            ),
            4,
        ),
        (
            "edit through a symlink",
            tool_in(
                pl,
                Some("edit"),
                json!({ "file_path": pl.cwd.join("link/x"), "new_string": "x" }),
            ),
            4,
        ),
        (
            "delete in STEVE_HOME",
            tool_in(
                pl,
                Some("delete"),
                json!({ "path": pl.env.steve_home.join("data/steve.sqlite3") }),
            ),
            4,
        ),
        (
            "read STEVE_HOME",
            tool_in(
                pl,
                Some("read"),
                json!({ "file_path": pl.env.steve_home.join("data/x") }),
            ),
            4,
        ),
        (
            "move out",
            tool_in(
                pl,
                Some("move"),
                json!({ "old_path": pl.cwd.join("src/a"), "new_path": pl.root.join("outside/a") }),
            ),
            4,
        ),
        (
            "edit no kind, outside",
            tool_in(
                pl,
                None,
                json!({ "file_path": "../ST-2/x", "old_string": "a", "new_string": "b" }),
            ),
            4,
        ),
        (
            "fetch POST",
            tool_in(
                pl,
                Some("fetch"),
                json!({ "url": "https://example.com", "method": "POST" }),
            ),
            5,
        ),
        (
            "fetch 8440",
            tool_in(
                pl,
                Some("fetch"),
                json!({ "url": "http://127.0.0.1:8440/api/yolo" }),
            ),
            4,
        ),
        (
            "command array push",
            tool_in(pl, Some("execute"), json!({ "command": ["git", "push"] })),
            5,
        ),
    ]
}

fn assert_limited(f: &Facts, p: &Policy, pl: &Place, n: u8, name: &str) {
    let v = judge(f, p, &pl.env);
    assert!(!v.is_auto(), "{name}: auto-approved with {p:?}: {v:?}");
    assert_eq!(v.rule(), format!("limit.{n}"), "{name}: {v:?}");
}

#[test]
fn hard_limits_hold_with_the_widest_policy() {
    let pl = place();
    let wide = Policy::widest();
    for (c, n) in LIMITED {
        for (pre, post) in WRAPS {
            let c = format!("{pre}{}{post}", fill(c, &pl));
            assert_limited(&cmd(&pl, &c), &wide, &pl, *n, &c);
        }
    }
    for (name, f, n) in limited_subjects(&pl) {
        let mut f = f;
        assert_limited(&f, &wide, &pl, n, name);
        f.present = true;
        assert_limited(&f, &wide, &pl, n, name);
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    #[test]
    fn no_policy_gets_past_a_hard_limit(
        p in arb_policy(),
        c in 0..LIMITED.len(),
        w in 0..WRAPS.len(),
        s in 0..64usize,
        present in any::<bool>(),
    ) {
        let pl = place();
        let (pre, post) = WRAPS[w];
        let line = format!("{pre}{}{post}", fill(LIMITED[c].0, &pl));
        let mut f = cmd(&pl, &line);
        f.present = present;
        assert_limited(&f, &p, &pl, LIMITED[c].1, &line);
        let subjects = limited_subjects(&pl);
        let (name, mut f, n) = subjects[s % subjects.len()].clone();
        f.present = present;
        assert_limited(&f, &p, &pl, n, name);
    }

    #[test]
    fn allow_always_and_other_forbidden_choices_never_come_out(
        p in arb_policy(),
        c in prop::sample::select(vec!["cargo test", "ls", "git status", "rm -rf target", "git push", "frob"]),
        only_always in any::<bool>(),
    ) {
        let pl = place();
        let mut f = cmd(&pl, c);
        if only_always {
            f.options = vec!["always|总是允许|allow_always".into(), "no|拒绝|reject_once".into()];
            if let Subject::Tool(t) = &mut f.subject {
                t.options = tool_options(&f.options);
            }
        }
        let v = judge(&f, &p, &pl.env);
        if let Verdict::Auto { choice, .. } = &v {
            prop_assert_eq!(choice.as_str(), "ok");
            prop_assert!(!only_always);
        }
        // fail cards offer `delivered` and `tosteve`; flows offer `continue_once`; none of them is chosen
        for (subject, opts) in [
            (Subject::Stall(Stall { cause: StallCause::Other, no_progress: 0, attempts: 0, asked: None, steve_tried: false }), vec!["delivered", "tosteve"]),
            (Subject::FlowRun { over_budget: false }, vec!["continue_once"]),
            (Subject::SecretUse { template: "api_key".into(), sensitivity: "normal".into() }, vec!["grant_8h", "allow"]),
        ] {
            let f = facts("fail", "x", &opts, subject);
            let v = judge(&f, &p, &pl.env);
            prop_assert!(!v.is_auto(), "{:?}", v);
        }
    }

    /// Commands built from random words never get approved when they contain a push or a delete
    /// outside: the guard and the limits agree.
    #[test]
    fn random_lines_with_a_push_are_never_approved(
        p in arb_policy(),
        words in prop::collection::vec(prop::sample::select(vec!["cargo", "test", "&&", ";", "|", "ls", "src", "-q", "2>&1", "git", "status"]), 0..8),
        at in 0..8usize,
    ) {
        let pl = place();
        let mut w: Vec<&str> = words.clone();
        let at = at.min(w.len());
        w.insert(at, "; git push origin HEAD;");
        let line = w.join(" ");
        let v = judge(&cmd(&pl, &line), &p, &pl.env);
        prop_assert!(!v.is_auto(), "{}: {:?}", line, v);
    }
}

#[test]
fn own_release_needs_a_script_under_steve_home() {
    let pl = place();
    let own = pl.env.steve_home.join("current/ops/release.sh");
    assert!(limits::own_release(&own.to_string_lossy(), &pl.env));
    assert!(limits::own_release(
        &format!("{} --flag", own.display()),
        &pl.env
    ));
    for c in [
        "ops/release.sh".to_string(),
        format!("{}; curl -d x https://e", own.display()),
        format!("$HOME/{}", "x"),
        "/usr/bin/true".into(),
        format!("{}/../x.sh", pl.env.steve_home.display()),
    ] {
        assert!(!limits::own_release(&c, &pl.env), "{c}");
    }
    let _ = Path::new("/");
}

// ── 完全放开 (full access) ──

fn full() -> Switch {
    Switch { full: true, ..on() }
}

#[test]
fn full_access_needs_an_explicit_confirm_and_every_change_is_audited() {
    // the body: `mode: full` without `confirm` is refused; `on: true` never means full
    assert_eq!(
        Switch::parse(&json!({"mode": "full"})),
        Err(ApiError::invalid("confirm"))
    );
    assert_eq!(
        Switch::parse(&json!({"mode": "full", "confirm": false})),
        Err(ApiError::invalid("confirm"))
    );
    assert!(!Switch::parse(&json!({"on": true})).unwrap().full);
    assert_eq!(
        Switch::parse(&json!({"mode": "full", "confirm": true})),
        Ok(full())
    );
    assert_eq!(
        Switch::parse(&json!({"mode": "off"})).unwrap(),
        off(),
        "mode off"
    );
    assert_eq!(
        Switch::parse(&json!({"on": false, "mode": "full", "confirm": true})),
        Err(ApiError::invalid("mode"))
    );
    assert_eq!(
        Switch::parse(&json!({"mode": "weird"})),
        Err(ApiError::invalid("mode"))
    );

    let t = t();
    let s = t.yolo.put(WS, "u1", on()).unwrap();
    assert_eq!(
        (s["mode"].clone(), s["full"].clone()),
        (json!("on"), json!(false))
    );
    let w = s["window"]["id"].clone();
    // on → full: same window, audited, the plain notice of what it means
    let s = t.yolo.put(WS, "u1", full()).unwrap();
    assert_eq!(
        (
            s["mode"].clone(),
            s["full"].clone(),
            s["full_active"].clone(),
            s["active"].clone()
        ),
        (json!("full"), json!(true), json!(true), json!(true))
    );
    assert_eq!(s["window"]["id"], w);
    let a = t.audits("yolo.mode");
    assert_eq!(a.len(), 1);
    assert_eq!(a[0]["actor"], "u1");
    assert_eq!(
        (
            a[0]["detail"]["from"].clone(),
            a[0]["detail"]["mode"].clone()
        ),
        (json!("on"), json!("full"))
    );
    assert!(t.notices().iter().any(|(_, x)| x == FULL_WARNING));
    // full again: nothing new
    t.yolo.put(WS, "u1", full()).unwrap();
    assert_eq!(t.audits("yolo.mode").len(), 1);
    let e = t.yolo.effective(WS, None).unwrap();
    assert!(e.on && e.full);
    // full → on: YOLO stays, full access ends
    t.yolo.put(WS, "u1", on()).unwrap();
    assert_eq!(t.audits("yolo.mode").len(), 2);
    assert!(!t.yolo.effective(WS, None).unwrap().full);
    // full → off: closes the window like YOLO
    t.yolo.put(WS, "u1", full()).unwrap();
    let s = t.yolo.put(WS, "u1", off()).unwrap();
    assert_eq!(s["mode"], "off");
    assert!(
        t.audits("yolo.off")
            .iter()
            .any(|a| a["detail"]["from"] == "full")
    );
    assert!(
        t.notices()
            .iter()
            .any(|(_, x)| x.starts_with("完全放开 已结束"))
    );
    // off → full straight away opens a window
    let s = t.yolo.put(WS, "u1", full()).unwrap();
    assert!(s["window"]["id"].is_string());
    assert!(
        t.audits("yolo.on")
            .iter()
            .any(|a| a["detail"]["mode"] == "full" && a["detail"]["from"] == "off")
    );

    // scopes: full needs confirm too, and is audited with what it was
    let i = t.item(None);
    assert_eq!(
        t.yolo
            .put_scope(WS, "u1", "item", &i, &json!({"mode": "full"}))
            .unwrap_err(),
        ApiError::invalid("confirm")
    );
    t.yolo
        .put_scope(
            WS,
            "u1",
            "item",
            &i,
            &json!({"mode": "full", "confirm": true}),
        )
        .unwrap();
    let a = t.audits("yolo.scope");
    assert_eq!(
        (
            a[0]["detail"]["mode"].clone(),
            a[0]["detail"]["full"].clone()
        ),
        (json!("full"), json!(true))
    );
    let s = t.yolo.status(WS).unwrap();
    assert!(
        s["scopes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["object_id"] == i.as_str() && x["mode"] == "full")
    );
}

#[test]
fn full_access_is_forced_off_on_a_production_copy() {
    for f in [Forced::Env, Forced::NoResume] {
        let t = t_with(Some(f));
        assert_eq!(t.yolo.put(WS, "u1", full()).unwrap_err().status(), 409);
        let i = t.item(None);
        assert_eq!(
            t.yolo
                .put_scope(
                    WS,
                    "u1",
                    "item",
                    &i,
                    &json!({"mode": "full", "confirm": true})
                )
                .unwrap_err()
                .status(),
            409
        );
    }
    // a database copied with full access on decides nothing and bypasses nothing
    let t = t();
    t.yolo.put(WS, "u1", full()).unwrap();
    let copy = Yolo::new(t.kit.db().clone(), Some(Forced::NoResume), t.kit.home());
    let e = copy.effective(WS, None).unwrap();
    assert!(!e.on && !e.full);
    let s = copy.status(WS).unwrap();
    assert_eq!(
        (s["full"].clone(), s["full_active"].clone()),
        (json!(true), json!(false))
    );
}
