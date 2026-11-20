//! SG43: actual switch windows, draft normalization/apply/undo and persisted fact dispatch.
mod sg43;
use serde_json::json;
use sg43::Fixture;
use std::sync::Arc;
use steve_core::repo::{NewDecision, NewItem, WS};
use steve_core::yolo::{
    Engine, Switch, ToolInput, Yolo,
    judge::{Policy, StallCause, Subject},
    scope::Forced,
};
use steve_modules::FakeClock;
use steve_store::yolo as repo;
fn switch(on: bool, full: bool, preview: bool) -> Switch {
    Switch {
        on,
        full,
        preview,
        include_open: false,
    }
}
fn row(k: &Fixture) -> Option<repo::ScopeRow> {
    k.db()
        .store()
        .reader(|c| repo::scope_get(c, WS, repo::WORKSPACE, WS))
        .unwrap()
}
fn window(k: &Fixture) -> Option<repo::WindowRow> {
    k.db()
        .store()
        .reader(|c| repo::window_current(c, WS))
        .unwrap()
}
fn controller(k: &Fixture) -> Yolo {
    Yolo::with_clock(
        k.db().clone(),
        None,
        k.home(),
        Arc::new(FakeClock::new(steve_store::now() + 10_000)),
    )
}
fn item(k: &Fixture) -> String {
    k.db()
        .item_create(
            WS,
            &NewItem {
                title: "SG43 fixture".into(),
                goal: "fixed goal".into(),
                bot_id: k.db().main_bot().unwrap().id,
                tags: vec![k.db().tag_by_name(WS, "feat").unwrap().unwrap().id],
                ..Default::default()
            },
        )
        .unwrap()
        .id
}
fn card(
    k: &Fixture,
    item: Option<&str>,
    kind: &str,
    ty: &str,
    q: &str,
    draft: &str,
) -> steve_core::repo::Decision {
    k.db()
        .decision_create(NewDecision {
            workspace_id: WS.into(),
            kind: kind.into(),
            subject_type: ty.into(),
            subject_id: item.unwrap_or("sg43-subject").into(),
            item_id: item.map(str::to_owned),
            session_id: None,
            question: q.into(),
            draft: Some(draft.into()),
            options: Some(vec!["yes".into(), "no".into()]),
            requested_by: "dev".into(),
        })
        .unwrap()
}
#[test]
fn sg43_switch_window_transition_and_error_precedence_table() {
    let k = Fixture::new();
    let y = controller(&k);
    assert!(row(&k).is_none());
    y.put(WS, "me", switch(true, false, true)).unwrap();
    assert!(row(&k).is_none());
    assert!(window(&k).is_none());
    for (on, full, mode) in [
        (true, false, "on"),
        (true, true, "full"),
        (true, false, "on"),
        (false, false, "off"),
        (false, false, "off"),
    ] {
        let before = window(&k).map(|w| w.id);
        y.put(WS, "me", switch(on, full, false)).unwrap();
        assert_eq!(row(&k).unwrap().mode, mode);
        let current = window(&k).map(|w| w.id);
        if on && before.is_some() {
            assert_eq!(current, before);
        }
        if !on {
            assert!(current.is_none());
        }
        let revision = row(&k).unwrap().revision;
        y.put(WS, "me", switch(on, full, false)).unwrap();
        assert_eq!(row(&k).unwrap().revision, revision, "same mode is a no-op");
    }
    assert!(y.put(WS, "me", switch(false, false, true)).is_err());
    y.put(WS, "me", switch(true, false, false)).unwrap();
    assert!(y.trip(WS, "fixture", "held for review").unwrap());
    assert!(!y.trip(WS, "fixture", "second reason").unwrap());
    assert!(window(&k).is_none());
    assert!(
        y.put(WS, "me", switch(true, false, true)).is_err(),
        "pause is checked before preview"
    );
    y.resume(WS, "me").unwrap();
    assert!(row(&k).unwrap().paused.is_none());
    assert!(window(&k).is_some());
    y.put(WS, "me", switch(false, false, false)).unwrap();
    let forced = Yolo::with_clock(
        k.db().clone(),
        Some(Forced::Env),
        k.home(),
        Arc::new(FakeClock::new(1_000_000)),
    );
    assert!(forced.put(WS, "me", switch(true, false, true)).is_err());
    assert!(forced.put(WS, "me", switch(false, false, false)).is_ok());
    let actions: Vec<String> = k
        .db()
        .store()
        .reader(|c| {
            let mut s =
                c.prepare("SELECT action FROM audit WHERE action LIKE 'yolo.%' ORDER BY id")?;
            Ok::<_, steve_store::StoreError>(
                s.query_map([], |r| r.get(0))?
                    .collect::<rusqlite::Result<_>>()?,
            )
        })
        .unwrap();
    assert!(actions.iter().any(|s| s == "yolo.mode"));
    assert!(actions.iter().any(|s| s == "yolo.off"));
}
#[tokio::test]
async fn sg43_draft_plan_normalizes_order_and_undoes_only_usable_operations() {
    use steve_modules::{
        Modules, Registry, changes,
        testkit::{FakeGateway, caller},
    };
    let k = Fixture::new();
    let it = item(&k);
    let project = k
        .db()
        .project_create(
            WS,
            "Fixture project",
            "",
            &k.home().join("project").display().to_string(),
            "repo",
            true,
            None,
            None,
        )
        .unwrap()
        .id;
    let mut reg = Registry::new();
    steve_core::yolo::draft::register(&mut reg, None);
    let gw = FakeGateway::new();
    let m = Modules::new(k.db().store().clone(), gw.clone(), reg);
    let who = caller(WS, "me");
    let raw = json!({"ops":[{"op":"set","value":"开","until":"later"},{"op":"yolo.set","on":false},{"op":"yolo.scope","type":"项目","name":"Fixture project","mode":"关"},{"op":"yolo.scope","type":"事项","name":it,"mode":"跟随"},{"op":"yolo.scope","type":"wrong","name":"x","mode":"on"},{"op":"yolo.scope","type":"project","name":"Fixture project","mode":"wrong"},{"op":"yolo.scope","type":"project","name":"unknown","mode":"on"},{"op":"set","value":7},{"op":"unknown"},{"op":"yolo.scope","type":"project","id":project,"mode":"on"}]});
    gw.reply(raw.to_string());
    let p = changes::draft(
        &m,
        &who,
        "yolo",
        None,
        "fixture request",
        Default::default(),
    )
    .await
    .unwrap();
    assert_eq!(p.changes.len(), 11);
    let cs = k
        .db()
        .store()
        .reader(|c| steve_store::base::changesets::get(c, &p.id))
        .unwrap()
        .unwrap();
    assert_eq!(cs.ops,json!([{"op":"yolo.set","on":true},{"op":"yolo.set","on":false},{"op":"yolo.scope","type":"project","id":project,"mode":"off"},{"op":"yolo.scope","type":"item","id":it,"mode":"inherit"},{"op":"yolo.scope","type":"project","id":project,"mode":"on"}]).as_array().unwrap().clone());
    changes::apply(&m, &who, &p.id).unwrap().unwrap();
    assert_eq!(row(&k).unwrap().mode, "off");
    let scope = k
        .db()
        .store()
        .reader(|c| repo::scope_get(c, WS, "project", &project))
        .unwrap()
        .unwrap();
    assert_eq!(scope.mode, "on");
    changes::undo(&m, &who, &p.id).unwrap().unwrap();
    assert!(
        k.db()
            .store()
            .reader(|c| repo::scope_get(c, WS, "project", &project))
            .unwrap()
            .is_none()
    );
}
#[test]
fn sg43_gather_dispatch_progress_checks_stalls_and_armed_tool_input() {
    let k = Fixture::new();
    let it = item(&k);
    let own = k.core.config().scratch.join(&it);
    std::fs::create_dir_all(&own).unwrap();
    k.db().item_patch(&it,json!({"cwd":own,"plan":[{"step":"done step","done":true},{"step":"remaining step","done":false}]}).as_object().unwrap().clone()).unwrap();
    let y = controller(&k);
    let e = Engine::new(k.core.clone(), y);
    let mut policy = Policy::default();
    policy.presence_s = 0;
    let d = card(
        &k,
        Some(&it),
        "accept",
        "item",
        "review",
        "检查：✓ cargo check；✗ cargo test",
    );
    let got = e.gather(&d, &policy).unwrap();
    assert!(!got.facts.present);
    assert_eq!(got.progress["done"], 1);
    assert!(got.progress["head"].is_null());
    let Subject::Accept(a) = got.facts.subject else {
        panic!("not acceptance")
    };
    assert_eq!(a.checks.len(), 2);
    assert!(a.checks[0].passed);
    assert!(!a.checks[1].passed);
    assert_eq!(a.unfinished, ["remaining step"]);
    assert_eq!(a.branch, None);
    for (question, want) in [
        ("Agent 停下来了", "stopped"),
        ("会话出错：fixture offline", "error"),
        ("ordinary", "other"),
    ] {
        let d = card(&k, Some(&it), "fail", "item", question, "");
        let got = e.gather(&d, &policy).unwrap();
        let Subject::Stall(s) = got.facts.subject else {
            panic!("not stall")
        };
        assert!(match want {
            "stopped" => s.cause == StallCause::Stopped,
            "error" => matches!(s.cause, StallCause::Error { restart: false, .. }),
            _ => s.cause == StallCause::Other,
        });
        assert!(s.asked.is_none());
        assert!(s.own_action.is_none());
    }
    let mut d = card(
        &k,
        Some(&it),
        "approve",
        "tool_call",
        "允许执行：fixture",
        r#"{"command":"old"}"#,
    );
    d.options = vec!["once|Allow|allow_once".into()];
    e.stash(
        &d.id,
        ToolInput {
            raw: json!({"command":"echo fixture"}),
            kind: Some("execute".into()),
            locations: vec!["fixture.txt".into()],
            own_dir: Some(own.clone()),
            own_pids: Some(vec![111]),
        },
    );
    e.arm(&d.id);
    let got = e.gather(&d, &policy).unwrap();
    assert!(got.progress.is_null());
    let Subject::Tool(t) = got.facts.subject else {
        panic!("not tool")
    };
    assert_eq!(t.raw["command"], "echo fixture");
    assert_eq!(t.cwd, Some(own));
    assert_eq!(t.own_pids, [111]);
    assert_eq!(t.locations, ["fixture.txt"]);
    assert!(!t.truncated);
    for (kind, ty, question) in [
        ("propose", "item", "requested in chat"),
        ("approve", "release", "release"),
        ("other", "other", "other"),
    ] {
        let d = card(&k, Some(&it), kind, ty, question, "");
        let g = e.gather(&d, &policy).unwrap();
        assert!(g.progress.is_null());
        assert!(!g.facts.present);
        match ty {
            "item" => assert!(matches!(
                g.facts.subject,
                Subject::ProposeCreate { from_chat: true }
            )),
            "release" => assert!(matches!(g.facts.subject, Subject::Unverified(_))),
            _ => {}
        }
    }
}
