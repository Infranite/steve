//! Personal conversation closure is independent from channel processing and visibility.
#[path = "support/digest_env.rs"]
mod digest_env;
use digest_env::TestEnv;
use serde_json::{Value, json};
use steve_channels::conversations::{self, ListQuery};
use steve_modules::{
    ApiResult,
    testkit::{ME, OTHER, WS, WS2},
};
use steve_store::base::Viewer;
use steve_store::testkit::channels::inspect;
use steve_store::testkit::channels::seed;

fn setup() -> TestEnv {
    let h = TestEnv::new(|_| {});
    seed::spaces::conversation_accounts(h.store()).unwrap();
    h
}

fn add(h: &TestEnv, ws: &str, id: &str, account: Option<&str>, created: i64) {
    seed::spaces::conversation_with_turn(h.store(), id, ws, account, created).unwrap();
}
fn close(h: &TestEnv, ws: &str, user: &str, conv: &str, closed: bool) -> ApiResult<()> {
    h.store()
        .write(|c| conversations::set_closed(c, ws, &Viewer::bare(ws, user), conv, closed))
}
fn list(h: &TestEnv, ws: &str, user: &str) -> Vec<Value> {
    conversations::list(h.store(), ws, &Viewer::bare(ws, user)).unwrap()
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
    let path = h.dir.path().join("steve.sqlite3");
    let readback = steve_store::Store::open(&path, steve_store::NoPublisher).unwrap();
    assert_eq!(
        conversations::list(&readback, WS, &Viewer::bare(WS, ME)).unwrap()[0]["closed"],
        true
    );
    let state = inspect::spaces::conversation_history(&readback, "shared-space").unwrap();
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
fn single_user_preferences_keep_workspace_and_membership_checks() {
    let h = setup();
    add(&h, WS, "private-space", Some("private"), 1);
    add(&h, WS2, "other-space", None, 1);
    assert_eq!(list(&h, WS, OTHER).len(), 1);
    close(&h, WS, OTHER, "chan:private-space", true).unwrap();
    assert!(close(&h, WS, ME, "chan:other-space", true).is_err());
    assert!(close(&h, WS2, ME, "chan:private-space", true).is_err());
    assert!(close(&h, WS, ME, "steve", true).is_err());
    assert!(close(&h, WS, ME, "chan:missing", true).is_err());
    assert!(conversations::list(h.store(), WS, &Viewer::bare(WS2, ME)).is_err());
    close(&h, WS, ME, "chan:private-space", true).unwrap();
    seed::people::remove_workspace_member(h.store(), WS, ME).unwrap();
    assert!(close(&h, WS, ME, "chan:private-space", false).is_err());
    assert!(conversations::list(h.store(), WS, &Viewer::bare(WS, ME)).is_err());
}

#[test]
fn single_user_pagination_includes_shared_and_personal_conversations() {
    let h = setup();
    for n in 0..215 {
        add(&h, WS, &format!("shared-{n:03}"), Some("shared"), 1);
    }
    // Both personal and shared conversations participate in pagination without omissions.
    for n in 0..205 {
        add(&h, WS, &format!("private-{n:03}"), Some("private"), 100);
    }
    assert_eq!(list(&h, WS, OTHER).len(), 420);
    close(&h, WS, OTHER, "chan:shared-214", true).unwrap();
    let page = |q: ListQuery| {
        conversations::list_page(h.store(), WS, &Viewer::bare(WS, OTHER), &q, 1).unwrap()
    };
    let mut all = Vec::new();
    let mut offset = 0;
    loop {
        let p = page(ListQuery {
            limit: Some(50),
            offset: Some(offset),
            ..Default::default()
        });
        assert_eq!(p.total, 420);
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
    assert_eq!(all.len(), 420);
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
        419
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
        420
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
        assert!(conversations::list_page(h.store(), WS, &Viewer::bare(WS, ME), &q, 1).is_err());
    }
}

fn outgoing(h: &TestEnv, space: &str, id: &str, status: &str, approval: Option<(&str, &str)>) {
    seed::messages::seed_conversation_outbound(h.store(), WS, space, id, status, approval).unwrap();
}

fn replied(h: &TestEnv, user: &str, q: ListQuery) -> conversations::ListPage {
    conversations::list_page(
        h.store(),
        WS,
        &Viewer::bare(WS, user),
        &ListQuery {
            state: Some("replied".into()),
            ..q
        },
        100,
    )
    .unwrap()
}

#[test]
fn replied_requires_a_sent_message_or_a_draft_still_awaiting_approval() {
    let h = setup();
    for (space, status, decision) in [
        ("sent", "sent", None),
        ("draft", "draft", Some(("approve", "open"))),
        ("both", "sent", None),
        ("failed", "failed", Some(("fail", "open"))),
        ("pending", "pending", None),
        ("unknown", "unknown", None),
        ("discarded", "discarded", Some(("approve", "resolved"))),
        ("settled-draft", "draft", Some(("approve", "resolved"))),
        ("unreviewed-draft", "draft", None),
        ("not-approval", "draft", Some(("fail", "open"))),
    ] {
        add(&h, WS, space, Some("shared"), 1);
        outgoing(&h, space, &format!("out-{space}"), status, decision);
    }
    outgoing(
        &h,
        "both",
        "second-draft",
        "draft",
        Some(("approve", "open")),
    );
    add(&h, WS, "only-incoming", Some("shared"), 10);
    add(&h, WS, "internal-answer", Some("shared"), 10);
    // An internal conversation answer did not necessarily leave the channel.
    h.store()
        .messages()
        .add(
            WS,
            "steve",
            "not sent",
            None,
            &json!({}),
            "chan:internal-answer",
        )
        .unwrap();
    close(&h, WS, ME, "chan:sent", true).unwrap();
    seed::messages::seed_incoming_after_sent(h.store(), WS).unwrap();
    let p = replied(&h, ME, ListQuery::default());
    assert_eq!(p.total, 3);
    assert_eq!(
        p.conversations
            .iter()
            .map(|r| r["space_id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["sent", "both", "draft"]
    );
    assert_eq!(p.conversations[0]["last_author"], "me");
    assert_eq!(
        p.conversations[0]["closed"], true,
        "the replied view includes closed conversations too"
    );
    assert!(!p.has_more);
    assert_eq!(p.next_offset, None);
    // Resolving approval removes a draft-only conversation, but not a previously sent one.
    seed::decisions::resolve_all_approvals(h.store()).unwrap();
    assert_eq!(replied(&h, ME, ListQuery::default()).total, 2);
}

#[test]
fn replied_is_filtered_before_pagination_and_combines_with_search_and_channel() {
    let h = setup();
    for n in 0..55 {
        let space = format!("replied-{n:03}");
        add(&h, WS, &space, Some("shared"), n + 10);
        outgoing(&h, &space, &format!("out-{n}"), "sent", None);
    }
    // Newer non-matching conversations must not consume pages of the replied view.
    for n in 0..60 {
        add(&h, WS, &format!("incoming-{n}"), Some("shared"), 1000 + n);
    }
    seed::spaces::connect_reply_fixtures(h.store(), WS, ME).unwrap();
    let p = replied(
        &h,
        ME,
        ListQuery {
            limit: Some(50),
            connection: Some("test-channel".into()),
            ..Default::default()
        },
    );
    assert_eq!(p.total, 55);
    assert_eq!(p.conversations.len(), 50);
    assert_eq!(p.conversations[0]["space_id"], "replied-054");
    assert_eq!(p.next_offset, Some(50));
    let last = replied(
        &h,
        ME,
        ListQuery {
            limit: Some(50),
            offset: p.next_offset,
            ..Default::default()
        },
    );
    assert_eq!(last.total, 55);
    assert_eq!(last.conversations.len(), 5);
    assert_eq!(last.conversations[4]["space_id"], "replied-000");
    assert!(!last.has_more);
    let search = replied(
        &h,
        ME,
        ListQuery {
            q: Some("replied-000".into()),
            connection: Some("test-channel".into()),
            ..Default::default()
        },
    );
    assert_eq!(search.total, 1);
    assert_eq!(search.conversations[0]["space_id"], "replied-000");
    assert_eq!(
        replied(
            &h,
            ME,
            ListQuery {
                connection: Some("another-channel".into()),
                ..Default::default()
            }
        )
        .total,
        0
    );
}

#[test]
fn single_user_replied_reads_include_all_receiving_identities() {
    let h = setup();
    for id in ["visible-sent", "hidden-sent", "hidden-draft"] {
        add(&h, WS, id, Some("shared"), 1);
    }
    add(&h, WS, "private-space", Some("private"), 1);
    outgoing(&h, "visible-sent", "public-out", "sent", None);
    outgoing(&h, "hidden-sent", "private-out", "sent", None);
    outgoing(
        &h,
        "hidden-draft",
        "private-draft",
        "draft",
        Some(("approve", "open")),
    );
    outgoing(&h, "private-space", "own-out", "sent", None);
    seed::spaces::set_private_receive_identity(h.store(), WS, ME).unwrap();
    let other = replied(&h, OTHER, ListQuery::default());
    assert_eq!(other.total, 4);
    assert_eq!(replied(&h, ME, ListQuery::default()).total, 4);
}
