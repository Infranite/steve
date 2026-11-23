//! Status marks written by the item repository.

use steve_store::testkit::SqliteFixtures;
mod common;

use common::*;
use steve_store::{ItemPatch, LifecycleWrite, NewBot, NewItem, WS};

fn status_marks(t: &T, item: &str) -> Vec<(Option<String>, Option<String>)> {
    t.store
        .fixture_write(|c| {
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
                    tags: if kind.unwrap_or("task") == "task" {
                        common::task_tags(&t.store, WS)
                    } else {
                        vec![]
                    },
                    title: "x".into(),
                    goal: "g".into(),
                    bot_id: "dev".into(),
                    phase: status.map(|phase| phase.parse().unwrap()),
                    kind: kind.map(Into::into),
                    ..Default::default()
                },
            )
            .unwrap()
            .unwrap()
            .id
    };
    let i = new(Some("ready"), None);
    assert_eq!(status_marks(&t, &i), [(None, Some("ready".into()))]);
    t.store
        .items()
        .patch(
            &i,
            &ItemPatch {
                title: Some("updated".into()),
                ..Default::default()
            },
        )
        .unwrap();
    t.store
        .write(|tx| {
            steve_store::lifecycle_write_in(
                tx,
                &i,
                &LifecycleWrite {
                    phase: steve_types::ItemPhase::Closed,
                    cycle: 1,
                    outcome: Some("completed".into()),
                    ..Default::default()
                },
            )
        })
        .unwrap();
    assert_eq!(
        status_marks(&t, &i),
        [
            (None, Some("ready".into())),
            (Some("ready".into()), Some("completed".into()))
        ]
    );
    // chats have no statistics
    let chat = new(None, Some("chat"));
    assert!(status_marks(&t, &chat).is_empty());
}
