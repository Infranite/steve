use super::*;
use rusqlite::params;
use std::sync::{Mutex, MutexGuard};
use steve_store::{Item, ItemPatch, NewBot, NewDecision, NewItem, NewSession, PlanStep};

// A Store opens a pooled set of connections. Bound these fixtures within this test binary so the
// regression suite also runs with macOS's default per-process file descriptor allowance.
static DATABASE_FIXTURE: Mutex<()> = Mutex::new(());

struct Fixture {
    store: Store,
    _dir: tempfile::TempDir,
    _guard: MutexGuard<'static, ()>,
}

impl Fixture {
    fn new() -> Self {
        let guard = DATABASE_FIXTURE.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let store =
            Store::open(dir.path().join("items.sqlite3"), steve_store::NoPublisher).unwrap();
        store.ensure_workspace().unwrap();
        store
            .with_conn(|c| {
                c.execute(
                    "INSERT INTO workspaces (id,name,created,item_prefix) VALUES ('other','Other',1,'OT')",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        for ws in ["default", "other"] {
            store
                .bots()
                .seed(
                    &format!("bot_{ws}"),
                    false,
                    &NewBot {
                        name: format!("Worker {ws}"),
                        harness: "test-adapter".into(),
                        ..Default::default()
                    },
                    ws,
                )
                .unwrap();
        }
        Self {
            store,
            _dir: dir,
            _guard: guard,
        }
    }

    fn item(&self, ws: &str, title: &str) -> Item {
        self.store
            .items()
            .create(
                ws,
                &NewItem {
                    title: title.into(),
                    goal: format!("goal: {title}"),
                    bot_id: format!("bot_{ws}"),
                    ..Default::default()
                },
            )
            .unwrap()
            .unwrap()
    }

    fn session(&self, item: &Item) -> Session {
        self.store
            .sessions()
            .create(&NewSession {
                workspace_id: item.workspace_id.clone(),
                item_id: Some(item.id.clone()),
                bot_id: item.bot_id.clone(),
                harness: "test-adapter".into(),
                cwd: self._dir.path().to_string_lossy().into_owned(),
                token: steve_store::uid("test_token"),
                ..Default::default()
            })
            .unwrap()
    }

    fn origin(&self, item: &Item, space: &str) -> String {
        let message = self
            .store
            .messages()
            .add(
                &item.workspace_id,
                "user",
                "please work on this",
                Some(&item.id),
                &Value::Null,
                space,
            )
            .unwrap();
        self.store
            .thread()
            .item_add(&item.id, &item.workspace_id, &message, None, false)
            .unwrap();
        message
    }

    fn private_progress(&self, item: &Item) {
        self.store
            .items()
            .patch(
                &item.id,
                &ItemPatch {
                    now: Some("private-now-marker".into()),
                    plan: Some(vec![PlanStep {
                        step: "private-plan-marker".into(),
                        done: false,
                        left: false,
                    }]),
                    ..Default::default()
                },
            )
            .unwrap();
        self.store
            .decisions()
            .create(&NewDecision {
                workspace_id: item.workspace_id.clone(),
                kind: "accept".into(),
                subject_type: "item".into(),
                subject_id: item.id.clone(),
                item_id: Some(item.id.clone()),
                question: "accept?".into(),
                draft: Some("private-delivery-marker".into()),
                requested_by: "test".into(),
                ..Default::default()
            })
            .unwrap();
    }
}

#[test]
fn main_origin_reads_current_workspace_but_not_another_workspace() {
    let f = Fixture::new();
    let own = f.item("default", "management");
    let candidate = f.item("default", "P1 isolation candidate");
    let foreign = f.item("other", "P1 foreign candidate");
    f.origin(&own, steve_core::thread::MAIN);
    f.private_progress(&candidate);
    let s = f.session(&own);
    // Use sessions.create's real source spelling, never a hand-constructed trusted session.
    assert_eq!(s.source, "Steve");
    let (_, view) = scope(&f.store, &s).unwrap();
    let result = research_mcp::items_search(&f.store, "default", &view, "P1").unwrap();
    assert!(result.contains(&candidate.id), "{result}");
    assert!(!result.contains(&foreign.id), "{result}");
    assert!(!result.contains(&foreign.title), "{result}");
    let detail = research_mcp::item_read(&f.store, "default", &view, &candidate.id).unwrap();
    assert!(detail.contains("private-now-marker"), "{detail}");
    assert!(detail.contains("private-plan-marker"), "{detail}");
    assert!(detail.contains("private-delivery-marker"), "{detail}");
    let hidden = research_mcp::item_read(&f.store, "default", &view, &foreign.id);
    assert!(hidden.is_err());
}

#[test]
fn bot_membership_grants_only_the_current_items_workspace_even_with_a_different_home() {
    let f = Fixture::new();
    f.store
        .bots()
        .set_workspaces("bot_other", &["other".into(), "default".into()])
        .unwrap();
    let own = f
        .store
        .items()
        .create(
            "default",
            &NewItem {
                title: "shared bot management".into(),
                bot_id: "bot_other".into(),
                ..Default::default()
            },
        )
        .unwrap()
        .unwrap();
    let candidate = f.item("default", "P1 current workspace");
    let foreign = f.item("other", "P1 bot home workspace");
    f.origin(&own, steve_core::thread::MAIN);
    let s = f.session(&own);
    let (current, view) = scope(&f.store, &s).unwrap();
    let result = research_mcp::items_search(&f.store, &current.workspace_id, &view, "P1").unwrap();
    assert!(result.contains(&candidate.id), "{result}");
    assert!(!result.contains(&foreign.id), "{result}");
    assert!(research_mcp::item_read(&f.store, &current.workspace_id, &view, &foreign.id).is_err());
    f.store
        .bots()
        .set_workspaces("bot_other", &["other".into()])
        .unwrap();
    assert!(scope(&f.store, &s).is_err());
}

#[test]
fn unknown_origin_reveals_only_its_own_safe_projection() {
    let f = Fixture::new();
    let own = f.item("default", "P1 own");
    let hidden = f.item("default", "P1 hidden");
    f.private_progress(&own);
    let (_, view) = scope(&f.store, &f.session(&own)).unwrap();
    let result = research_mcp::items_search(&f.store, "default", &view, "P1").unwrap();
    assert!(result.contains(&own.id), "{result}");
    assert!(!result.contains(&hidden.id), "{result}");
    let detail = research_mcp::item_read(&f.store, "default", &view, &own.id).unwrap();
    assert!(detail.contains("goal: P1 own"));
    for marker in [
        "private-now-marker",
        "private-plan-marker",
        "private-delivery-marker",
    ] {
        assert!(!detail.contains(marker), "{detail}");
    }
    assert!(research_mcp::item_read(&f.store, "default", &view, &hidden.id).is_err());
}

#[test]
fn outside_conversation_reads_only_its_items_without_private_progress() {
    let f = Fixture::new();
    let own = f.item("default", "P1 own");
    let same_chat = f.item("default", "P1 sibling");
    let hidden = f.item("default", "P1 private");
    f.origin(&own, "chan:group-a");
    f.origin(&same_chat, "chan:group-a");
    f.origin(&hidden, steve_core::thread::MAIN);
    f.private_progress(&same_chat);
    let (_, view) = scope(&f.store, &f.session(&own)).unwrap();
    let result = research_mcp::items_search(&f.store, "default", &view, "P1").unwrap();
    assert!(result.contains(&same_chat.id), "{result}");
    assert!(!result.contains(&hidden.id), "{result}");
    assert!(!result.contains("private-now-marker"), "{result}");
    let detail = research_mcp::item_read(&f.store, "default", &view, &same_chat.id).unwrap();
    assert!(detail.contains(&same_chat.title));
    assert!(detail.contains("goal: P1 sibling"));
    for marker in [
        "private-now-marker",
        "private-plan-marker",
        "private-delivery-marker",
    ] {
        assert!(!detail.contains(marker), "{detail}");
    }
    assert!(research_mcp::item_read(&f.store, "default", &view, &hidden.id).is_err());
}

#[test]
fn source_message_workspace_cannot_grant_another_workspaces_main_scope() {
    let f = Fixture::new();
    let own = f.item("default", "P1 own");
    let hidden = f.item("default", "P1 hidden");
    let foreign = f.item("other", "foreign main origin");
    let message = f.origin(&foreign, steve_core::thread::MAIN);
    f.store
        .thread()
        .item_add(&own.id, "default", &message, None, false)
        .unwrap();
    let (_, view) = scope(&f.store, &f.session(&own)).unwrap();
    assert!(view.sees(&own.id));
    assert!(!view.sees(&hidden.id));
}

#[test]
fn changing_source_scope_revokes_cached_main_access_and_owner_marker_is_not_enough() {
    let f = Fixture::new();
    let own = f.item("default", "P1 own");
    let hidden = f.item("default", "P1 hidden");
    let origin = f.origin(&own, steve_core::thread::MAIN);
    let s = f.session(&own);
    assert!(scope(&f.store, &s).unwrap().1.sees(&hidden.id));
    f.store
        .with_conn(|c| {
            c.execute(
                "UPDATE messages SET space='chan:unverified',meta=? WHERE id=?",
                params![
                    json!({"chan": {"owner_only": true, "connection": "unverified"}}).to_string(),
                    origin
                ],
            )?;
            Ok(())
        })
        .unwrap();
    let (_, view) = scope(&f.store, &s).unwrap();
    assert!(view.sees(&own.id));
    assert!(!view.sees(&hidden.id));
}

#[test]
fn workflow_child_does_not_inherit_a_main_origin() {
    let f = Fixture::new();
    let own = f.item("default", "P1 child");
    let parent = f.item("default", "P1 parent");
    f.origin(&own, steve_core::thread::MAIN);
    f.store
        .with_conn(|c| {
            c.execute(
                "UPDATE items SET source=? WHERE id=?",
                params![json!(["collaboration", "workflow"]).to_string(), own.id],
            )?;
            Ok(())
        })
        .unwrap();
    let (_, view) = scope(&f.store, &f.session(&own)).unwrap();
    assert!(view.sees(&own.id));
    assert!(!view.sees(&parent.id));
}

#[test]
fn stale_identity_and_non_agent_sessions_are_rejected_from_persisted_state() {
    for mutation in [
        "UPDATE sessions SET readonly=1 WHERE id=?",
        "UPDATE sessions SET source='Imported session' WHERE id=?",
        "UPDATE sessions SET status='ended' WHERE id=?",
        "UPDATE sessions SET workspace_id='other' WHERE id=?",
        "UPDATE sessions SET bot_id='bot_other' WHERE id=?",
        "UPDATE sessions SET device_id='external-device' WHERE id=?",
    ] {
        let f = Fixture::new();
        let own = f.item("default", "current");
        let s = f.session(&own);
        f.store
            .with_conn(|c| {
                c.execute(mutation, [&s.id])?;
                Ok(())
            })
            .unwrap();
        assert!(scope(&f.store, &s).is_err(), "{mutation}");
    }
}

#[test]
fn previous_session_cannot_reuse_its_identity_after_replacement() {
    let f = Fixture::new();
    let own = f.item("default", "current");
    let old = f.session(&own);
    let current = f.session(&own);
    assert!(scope(&f.store, &old).is_err());
    assert!(scope(&f.store, &current).is_ok());
}

#[test]
fn dependency_management_exception_is_only_for_a_current_waiting_parent() {
    let f = Fixture::new();
    let own = f.item("default", "current");
    let dep = f.item("default", "prerequisite");
    let s = f.session(&own);
    f.store
        .with_conn(|c| {
            c.execute("UPDATE items SET status='run' WHERE id=?", [&own.id])?;
            c.execute(
                "INSERT INTO item_deps(item_id,dep_id,created) VALUES(?,?,1)",
                params![own.id, dep.id],
            )?;
            Ok(())
        })
        .unwrap();
    assert!(dependency_management_allowed(&f.store, &s).unwrap());
    for (mutation, restore) in [
        ("status='pause'", "status='run'"),
        ("archived=1", "archived=0"),
        ("source='[\"work\",\"workflow\"]'", "source='[\"\",\"\"]'"),
    ] {
        f.store
            .with_conn(|c| {
                c.execute(
                    &format!("UPDATE items SET {mutation} WHERE id=?"),
                    [&own.id],
                )?;
                Ok(())
            })
            .unwrap();
        assert!(
            !dependency_management_allowed(&f.store, &s).unwrap(),
            "{mutation}"
        );
        f.store
            .with_conn(|c| {
                c.execute(&format!("UPDATE items SET {restore} WHERE id=?"), [&own.id])?;
                Ok(())
            })
            .unwrap();
    }
    let replacement = f.session(&own);
    assert!(!dependency_management_allowed(&f.store, &s).unwrap());
    assert!(dependency_management_allowed(&f.store, &replacement).unwrap());
    f.store
        .with_conn(|c| {
            c.execute(
                "UPDATE sessions SET workspace_id='other' WHERE id=?",
                [&replacement.id],
            )?;
            Ok(())
        })
        .unwrap();
    assert!(!dependency_management_allowed(&f.store, &replacement).unwrap());
}

#[test]
fn disabled_or_removed_bot_no_longer_has_item_search_authority() {
    for mutation in [
        "UPDATE bots SET enabled=0 WHERE id='bot_default'",
        "UPDATE bots SET deleted=1 WHERE id='bot_default'",
        "DELETE FROM bot_workspaces WHERE bot_id='bot_default' AND workspace_id='default'",
    ] {
        let f = Fixture::new();
        let own = f.item("default", "current");
        let s = f.session(&own);
        f.origin(&own, steve_core::thread::MAIN);
        f.store
            .with_conn(|c| {
                c.execute(mutation, [])?;
                Ok(())
            })
            .unwrap();
        assert!(scope(&f.store, &s).is_err(), "{mutation}");
    }
}

#[test]
fn hidden_dependencies_are_removed_from_both_readback_arrays() {
    let view = ItemScope::Conversation(["ST-visible".into()].into_iter().collect());
    let mut snapshot = json!({
        "status": "block",
        "dependencies": [
            {"id": "ST-visible", "title": "visible"},
            {"id": "ST-hidden", "title": "secret-title-marker", "status": "run"}
        ],
        "open_dependencies": ["ST-visible", "ST-hidden"],
        "resume": "automatic",
        "reason": "wait for ST-hidden, secret-title-marker"
    });
    redact_dependencies(&mut snapshot, &view);
    assert_eq!(snapshot["hidden_dependencies"], 1);
    assert_eq!(snapshot["open_dependencies"], json!(["ST-visible"]));
    assert_eq!(snapshot["dependencies"].as_array().unwrap().len(), 1);
    let text = snapshot.to_string();
    assert!(!text.contains("ST-hidden"), "{text}");
    assert!(!text.contains("secret-title-marker"), "{text}");
    assert_eq!(snapshot["status"], "block");
    assert_eq!(snapshot["resume"], "automatic");
}

#[test]
fn tool_arguments_cannot_override_session_scope_or_write_unrecognized_state() {
    for (name, valid) in [
        (research_mcp::ITEMS_SEARCH, json!({"query": "P1"})),
        (research_mcp::ITEM_READ, json!({"id": "ST-1"})),
        (DEPENDENCIES, json!({"action": "get"})),
        (
            DEPENDENCIES,
            json!({"action": "wait", "deps": ["ST-1"], "reason": "await isolation", "resume": "automatic"}),
        ),
    ] {
        assert!(validate_args(name, &valid).is_ok(), "{name}: {valid}");
        for key in [
            "item",
            "item_id",
            "workspace",
            "workspace_id",
            "principal",
            "session_id",
            "status",
        ] {
            let mut invalid = valid.clone();
            invalid[key] = json!("injected");
            assert!(validate_args(name, &invalid).is_err(), "{name}: {invalid}");
        }
    }
    for args in [
        json!(null),
        json!({"action": "done"}),
        json!({"action": "get", "resume": "automatic"}),
        json!({"action": "wait", "deps": ["ST-1"]}),
        json!({"action": "wait", "deps": [false], "reason": "wait"}),
        json!({"action": "wait", "deps": [" "], "reason": "wait"}),
        json!({"action": "wait", "deps": ["ST-1"], "reason": "wait", "resume": true}),
    ] {
        assert!(validate_args(DEPENDENCIES, &args).is_err(), "{args}");
    }
}
