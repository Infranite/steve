//! Status marks written by the item repository (stats.md slice ST3).

mod common;

use common::*;
use steve_store::{ItemPatch, NewBot, NewItem, WS};

fn status_marks(t: &T, item: &str) -> Vec<(Option<String>, Option<String>)> {
    t.store
        .with_conn(|c| {
            Ok(c.prepare("SELECT from_v, to_v FROM item_marks WHERE item_id = ? AND kind = 'status' ORDER BY id")?
                .query_map([item], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?)
        })
        .unwrap()
}

#[test]
fn create_and_each_status_change_leave_one_mark() {
    let t = fresh();
    t.store
        .bots()
        .seed(
            "dev",
            false,
            &NewBot {
                name: "d".into(),
                harness: "claude-code".into(),
                ..Default::default()
            },
            WS,
        )
        .unwrap();
    let new = |status: Option<&str>, kind: Option<&str>| {
        t.store
            .items()
            .create(
                WS,
                &NewItem {
                    title: "x".into(),
                    goal: "g".into(),
                    bot_id: "dev".into(),
                    status: status.map(Into::into),
                    kind: kind.map(Into::into),
                    ..Default::default()
                },
            )
            .unwrap()
            .unwrap()
            .id
    };
    let i = new(Some("wait"), None);
    assert_eq!(status_marks(&t, &i), [(None, Some("wait".into()))]);
    let set = |s: Option<&str>, title: Option<&str>| {
        t.store
            .items()
            .patch(
                &i,
                &ItemPatch {
                    status: s.map(Into::into),
                    title: title.map(Into::into),
                    ..Default::default()
                },
            )
            .unwrap();
    };
    set(Some("wait"), None); // unchanged
    set(None, Some("y")); // no status
    set(Some("run"), None);
    set(Some("done"), Some("z"));
    assert_eq!(
        status_marks(&t, &i),
        [
            (None, Some("wait".into())),
            (Some("wait".into()), Some("run".into())),
            (Some("run".into()), Some("done".into()))
        ]
    );
    // chats have no statistics
    let chat = new(None, Some("chat"));
    assert!(status_marks(&t, &chat).is_empty());
}
