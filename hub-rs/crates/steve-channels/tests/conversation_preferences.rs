//! Personal conversation closure is independent from channel processing and visibility.
use rusqlite::params;
use serde_json::{Value, json};
use steve_channels::conversations::{self, ListQuery};
use steve_modules::{
    ApiResult,
    testkit::{ME, OTHER, TestEnv, WS, WS2},
};
use steve_store::base::Viewer;

fn setup() -> TestEnv {
    let h = TestEnv::new(|_| {});
    h.store().with_conn(|c| {
        c.execute_batch("INSERT INTO parties (id, workspace_id, type, name, created_by, created, updated) VALUES ('org', 'default', 'organization', 'Org', 'usr_dev', 1, 1);
        INSERT INTO parties (id, workspace_id, type, name, user_id, created_by, created, updated) VALUES ('mine', 'default', 'person', 'Me', 'usr_dev', 'usr_dev', 1, 1);
        INSERT INTO accounts (id, workspace_id, system_id, family, party_id, kind, handle, handle_display, source, created, updated) VALUES ('shared', 'default', 'feishu', 'feishu_app', 'org', 'app_bot', 'shared', 'shared', 'connect', 1, 1), ('private', 'default', 'feishu', 'feishu_user', 'mine', 'personal', 'me', 'me', 'connect', 1, 1);")?;
        Ok(())
    }).unwrap();
    h
}

fn add(h: &TestEnv, ws: &str, id: &str, account: Option<&str>, created: i64) {
    h.store().with_conn(|c| {
        c.execute("INSERT INTO spaces (id, workspace_id, system_id, kind, external_id, name, account_id, created, updated) VALUES (?1, ?2, 'feishu', 'group', ?1, ?1, ?3, 1, 1)", params![id, ws, account])?;
        c.execute("INSERT INTO messages (id, workspace_id, space, author, text, created) VALUES (?1, ?2, ?3, 'me', 'preserved message', ?4)", params![format!("msg_{id}"), ws, conversations::conv_space(id), created])?;
        Ok(())
    }).unwrap();
}
fn close(h: &TestEnv, ws: &str, user: &str, conv: &str, closed: bool) -> ApiResult<()> {
    h.store()
        .transaction(|c| conversations::set_closed(c, ws, &Viewer::bare(ws, user), conv, closed))
}
fn list(h: &TestEnv, ws: &str, user: &str) -> Vec<Value> {
    h.store()
        .reader(|c| conversations::list(c, ws, &Viewer::bare(ws, user)))
        .unwrap()
}

#[test]
fn closure_is_persistent_personal_reversible_and_does_not_change_history() {
    let h = setup();
    add(&h, WS, "shared-space", Some("shared"), 1);
    let conv = "chan:shared-space";
    close(&h, WS, ME, conv, true).unwrap();
    close(&h, WS, ME, conv, true).unwrap(); // idempotent
    assert_eq!(list(&h, WS, ME)[0]["closed"], true);
    assert_eq!(list(&h, WS, OTHER)[0]["closed"], false);
    // Independent connection sees durable state, not process memory.
    let c = rusqlite::Connection::open(h.dir.path().join("steve.sqlite3")).unwrap();
    assert_eq!(
        conversations::list(&c, WS, &Viewer::bare(WS, ME)).unwrap()[0]["closed"],
        true
    );
    let state: (String, String) = c.query_row("SELECT s.status, m.text FROM spaces s JOIN messages m ON m.space = 'chan:' || s.id WHERE s.id = 'shared-space'", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert_eq!(state, ("active".into(), "preserved message".into()));
    // New turns do not change a user's explicit closure.
    h.store()
        .messages()
        .add(WS, "steve", "new reply", None, &json!({}), conv)
        .unwrap();
    assert_eq!(list(&h, WS, ME)[0]["closed"], true);
    close(&h, WS, ME, conv, false).unwrap();
    assert_eq!(list(&h, WS, ME)[0]["closed"], false);
    assert_eq!(list(&h, WS, ME)[0]["count"], 2);
}

#[test]
fn hidden_cross_workspace_and_removed_members_cannot_write_preferences() {
    let h = setup();
    add(&h, WS, "private-space", Some("private"), 1);
    add(&h, WS2, "other-space", None, 1);
    assert!(list(&h, WS, OTHER).is_empty());
    assert!(close(&h, WS, OTHER, "chan:private-space", true).is_err());
    assert!(close(&h, WS, ME, "chan:other-space", true).is_err());
    assert!(close(&h, WS2, ME, "chan:private-space", true).is_err());
    assert!(close(&h, WS, ME, "steve", true).is_err());
    assert!(close(&h, WS, ME, "chan:missing", true).is_err());
    assert!(
        h.store()
            .reader(|c| conversations::list(c, WS, &Viewer::bare(WS2, ME)))
            .is_err()
    );
    close(&h, WS, ME, "chan:private-space", true).unwrap();
    h.store()
        .with_conn(|c| {
            c.execute(
                "DELETE FROM workspace_members WHERE workspace_id = ? AND user_id = ?",
                params![WS, ME],
            )?;
            Ok(())
        })
        .unwrap();
    assert!(close(&h, WS, ME, "chan:private-space", false).is_err());
    assert!(
        h.store()
            .reader(|c| conversations::list(c, WS, &Viewer::bare(WS, ME)))
            .is_err()
    );
}

#[test]
fn more_than_200_conversations_are_searchable_and_paginated_after_visibility() {
    let h = setup();
    for n in 0..215 {
        add(&h, WS, &format!("shared-{n:03}"), Some("shared"), 1);
    }
    // Invisible newer conversations must not consume the first page or hide old visible ones.
    for n in 0..205 {
        add(&h, WS, &format!("private-{n:03}"), Some("private"), 100);
    }
    assert_eq!(list(&h, WS, OTHER).len(), 215);
    close(&h, WS, OTHER, "chan:shared-214", true).unwrap();
    let page = |q: ListQuery| {
        h.store()
            .reader(|c| conversations::list_page(c, WS, &Viewer::bare(WS, OTHER), &q))
            .unwrap()
    };
    let mut all = Vec::new();
    let mut offset = 0;
    loop {
        let p = page(ListQuery {
            limit: Some(50),
            offset: Some(offset),
            ..Default::default()
        });
        assert_eq!(p.total, 215);
        all.extend(
            p.conversations
                .into_iter()
                .map(|v| v["space"].as_str().unwrap().to_owned()),
        );
        match p.next_offset {
            Some(n) => {
                assert!(p.has_more);
                offset = n;
            }
            None => {
                assert!(!p.has_more);
                break;
            }
        }
    }
    all.sort();
    all.dedup();
    assert_eq!(all.len(), 215);
    let p = page(ListQuery {
        q: Some("shared-214".into()),
        state: Some("closed".into()),
        limit: Some(50),
        ..Default::default()
    });
    assert_eq!(p.total, 1);
    assert_eq!(p.conversations[0]["closed"], true);
    assert_eq!(
        page(ListQuery {
            state: Some("open".into()),
            ..Default::default()
        })
        .total,
        214
    );
    assert_eq!(
        page(ListQuery {
            q: Some("%".into()),
            ..Default::default()
        })
        .total,
        0
    );
    assert_eq!(
        page(ListQuery {
            q: Some("preserved".into()),
            ..Default::default()
        })
        .total,
        215
    );
    assert_eq!(
        page(ListQuery {
            offset: Some(999),
            limit: Some(50),
            ..Default::default()
        })
        .conversations
        .len(),
        0
    );
}

#[test]
fn invalid_query_is_rejected() {
    let h = setup();
    for q in [
        ListQuery {
            state: Some("garbage".into()),
            ..Default::default()
        },
        ListQuery {
            limit: Some(0),
            ..Default::default()
        },
        ListQuery {
            limit: Some(201),
            ..Default::default()
        },
        ListQuery {
            offset: Some(-1),
            ..Default::default()
        },
    ] {
        assert!(
            h.store()
                .reader(|c| conversations::list_page(c, WS, &Viewer::bare(WS, ME), &q))
                .is_err()
        );
    }
}
