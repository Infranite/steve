mod common;
use common::*;
use rusqlite::params;
use serde_json::json;
use steve_store::{
    NoPublisher, Result, Store, WS,
    feed::{self, Content, Filter},
};

fn env() -> T {
    let t = fresh();
    t.store.transaction(|tx| -> Result<()> {
        tx.execute("INSERT INTO workspaces(id,name,created) VALUES('other','Other',0)",[])?;
        for (id,ws) in [("alice",WS),("bob",WS),("eve","other")] {
            tx.execute("INSERT INTO users(id,workspace_id,subject,name,role,created) VALUES(?,?,?,?,'member',0)",params![id,ws,id,id])?;
            tx.execute("INSERT INTO workspace_members(workspace_id,user_id,role,created) VALUES(?,?,'member',0)",params![ws,id])?;
        }
        Ok(())
    }).unwrap();
    t.events.take();
    t
}
fn report(t: &T, ws: &str, run: &str, node: &str) {
    t.store
        .transaction(|tx| {
            feed::publish_report(
                tx,
                ws,
                "flow",
                run,
                node,
                &Content {
                    title: run.into(),
                    markdown: "body".into(),
                    ..Default::default()
                },
                &json!({"text":"body","digest":0}),
                10,
            )
        })
        .unwrap();
}
fn notification(t: &T, id: &str, user: &str, event: &str, body: Option<&str>) {
    t.store.transaction(|tx|->Result<()> {
        tx.execute("INSERT INTO personal_notifications(id,workspace_id,user_id,event_id,delivery_key,mode,body,state,created) VALUES(?,?,?,?,?,'log',?,'held',10)",params![id,WS,user,event,id,body])?;
        Ok(())
    }).unwrap();
}
#[test]
fn stable_deep_paging_filters_counts_and_snapshot_do_not_swallow_new_content() {
    let t = env();
    for i in 0..231 {
        report(&t, WS, &format!("r{i}"), "n");
    }
    notification(&t, "nt1", "alice", "event", Some("personal body"));
    let f = Filter {
        kind: Some("report".into()),
        unread: true,
        ..Default::default()
    };
    let first = t
        .store
        .transaction(|tx| feed::list(tx, WS, "alice", &f, None, 17, 100))
        .unwrap();
    assert_eq!(first.unread_count, 231);
    assert!(first.entries.iter().all(|e| e.body.is_none() && !e.purged));
    let same = t
        .store
        .transaction(|tx| feed::list(tx, WS, "alice", &f, None, 17, 100))
        .unwrap();
    assert_eq!(same.snapshot, first.snapshot);
    report(&t, WS, "concurrent", "n");
    let mut cursor = first.next_cursor;
    let mut seqs = first.entries.iter().map(|e| e.seq).collect::<Vec<_>>();
    while let Some(c) = cursor {
        let page = t
            .store
            .transaction(|tx| feed::list(tx, WS, "alice", &f, Some(&c), 17, 100))
            .unwrap();
        assert_eq!(page.snapshot, first.snapshot);
        assert_eq!(page.unread_count, 232);
        seqs.extend(page.entries.iter().map(|e| e.seq));
        cursor = page.next_cursor;
    }
    assert_eq!(seqs.len(), 231);
    assert!(seqs.windows(2).all(|w| w[0] > w[1]));
    assert_eq!(
        t.store
            .transaction(|tx| feed::read_snapshot(tx, WS, "alice", &f, &first.snapshot, 101))
            .unwrap(),
        231
    );
    let p = t
        .store
        .transaction(|tx| feed::list(tx, WS, "alice", &Filter::default(), None, 100, 102))
        .unwrap();
    assert_eq!(p.unread_count, 2); // new report and personal notification
    let p = t
        .store
        .transaction(|tx| feed::list(tx, WS, "bob", &Filter::default(), None, 100, 102))
        .unwrap();
    assert_eq!(p.unread_count, 232);
    assert_eq!(p.entries.len(), 100);
    assert!(
        t.events.take().is_empty(),
        "no feed IDs or reads on workspace SSE"
    );
}
#[test]
fn live_membership_audience_scope_tokens_and_invalid_id_batch() {
    let t = env();
    report(&t, WS, "run", "n");
    report(&t, "other", "other-run", "n");
    notification(&t, "private", "alice", "event", Some("private"));
    let f = Filter::default();
    let p = t
        .store
        .transaction(|tx| feed::list(tx, WS, "alice", &f, None, 1, 100))
        .unwrap();
    assert!(
        t.store
            .reader(|c| feed::detail(c, WS, "bob", "feed_private"))
            .is_err()
    );
    for (ws, user, filter) in [
        (WS, "bob", f.clone()),
        ("other", "eve", f.clone()),
        (
            WS,
            "alice",
            Filter {
                unread: true,
                ..Default::default()
            },
        ),
    ] {
        assert!(
            t.store
                .transaction(|tx| feed::list(
                    tx,
                    ws,
                    user,
                    &filter,
                    p.next_cursor.as_deref(),
                    1,
                    101
                ))
                .is_err()
        );
        assert!(
            t.store
                .transaction(|tx| feed::read_snapshot(tx, ws, user, &filter, &p.snapshot, 101))
                .is_err()
        );
    }
    assert!(
        t.store
            .transaction(|tx| feed::read_snapshot(tx, WS, "alice", &f, &p.snapshot, 86_400_101))
            .is_err()
    );
    assert!(
        t.store
            .transaction(|tx| feed::list(tx, WS, "alice", &f, Some("bad"), 50, 100))
            .is_err()
    );
    assert!(
        t.store
            .transaction(|tx| feed::list(tx, WS, "alice", &f, None, 0, 100))
            .is_err()
    );
    assert!(
        t.store
            .transaction(|tx| feed::read_ids(
                tx,
                WS,
                "alice",
                &[p.entries[0].id.clone(), "invalid".into()],
                100
            ))
            .is_err()
    );
    assert!(
        t.store
            .reader(|c| feed::detail(c, WS, "alice", &p.entries[0].id))
            .unwrap()
            .read_at
            .is_none()
    );
    t.store
        .transaction(|tx| -> Result<()> {
            tx.execute("DELETE FROM workspace_members WHERE user_id='alice'", [])?;
            Ok(())
        })
        .unwrap();
    assert!(
        t.store
            .reader(|c| feed::detail(c, WS, "alice", &p.entries[0].id))
            .is_err()
    );
    assert!(
        t.store
            .transaction(|tx| feed::read_snapshot(tx, WS, "alice", &f, &p.snapshot, 101))
            .is_err()
    );
    assert!(
        t.store
            .transaction(|tx| feed::read_ids(tx, WS, "alice", &[p.entries[0].id.clone()], 101))
            .is_err()
    );
}
#[test]
fn notification_retention_delete_replay_and_per_user_reads() {
    let t = env();
    notification(
        &t,
        "n1",
        "alice",
        "event",
        Some("Private title\nPrivate body"),
    );
    let e = t
        .store
        .reader(|c| feed::detail(c, WS, "alice", "feed_n1"))
        .unwrap();
    assert_eq!(e.title, "Private title");
    assert_eq!(e.body.unwrap().markdown, "Private title\nPrivate body");
    t.store
        .transaction(|tx| feed::read_ids(tx, WS, "alice", &["feed_n1".into()], 12))
        .unwrap();
    t.store
        .transaction(|tx| -> Result<()> {
            tx.execute(
                "UPDATE personal_notifications SET body=NULL,state='purged' WHERE id='n1'",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    let e = t
        .store
        .reader(|c| feed::detail(c, WS, "alice", "feed_n1"))
        .unwrap();
    assert!(e.purged && e.title.is_empty() && e.excerpt.is_empty() && e.body.is_none());
    assert_eq!(e.read_at, Some(12));
    t.store
        .transaction(|tx| -> Result<()> {
            tx.execute("DELETE FROM personal_notifications WHERE id='n1'", [])?;
            Ok(())
        })
        .unwrap();
    notification(&t, "replay", "alice", "event", Some("must not resurrect"));
    let p = t
        .store
        .transaction(|tx| feed::list(tx, WS, "alice", &Filter::default(), None, 50, 100))
        .unwrap();
    assert_eq!(p.entries.len(), 1);
    assert!(p.entries[0].purged);
    assert_eq!(p.entries[0].read_at, Some(12));
    t.store.reader(|c|->Result<()> {assert_eq!(count(c,"SELECT count(*) FROM feed_entries WHERE content IS NOT NULL OR result IS NOT NULL"),0);Ok(())}).unwrap();
    t.store
        .transaction(|tx| -> Result<()> {
            tx.execute("DELETE FROM workspace_members WHERE user_id='alice'", [])?;
            tx.execute("DELETE FROM users WHERE id='alice'", [])?;
            Ok(())
        })
        .unwrap();
    t.store
        .reader(|c| -> Result<()> {
            assert_eq!(count(c, "SELECT count(*) FROM feed_entries"), 0);
            Ok(())
        })
        .unwrap();
}
#[test]
fn publication_is_idempotent_by_run_and_node_and_transaction_failure_rolls_back() {
    let t = env();
    report(&t, WS, "same-run", "a");
    report(&t, WS, "same-run", "a");
    report(&t, WS, "same-run", "b");
    t.store
        .reader(|c| -> Result<()> {
            assert_eq!(count(c, "SELECT count(*) FROM feed_entries"), 2);
            Ok(())
        })
        .unwrap();
    let original = t
        .store
        .reader(|c| feed::report_result(c, WS, "same-run", "a"))
        .unwrap()
        .unwrap();
    let (loser, inserted) = t
        .store
        .transaction(|tx| {
            feed::publish_report(
                tx,
                WS,
                "flow",
                "same-run",
                "a",
                &Content {
                    markdown: "different writer".into(),
                    ..Default::default()
                },
                &json!({"text":"must not replace"}),
                20,
            )
        })
        .unwrap();
    assert!(!inserted);
    assert_eq!(loser, original);
    let canonical = original["feed_entry_id"].as_str().unwrap();
    assert_eq!(
        t.store
            .reader(|c| feed::detail(c, WS, "alice", canonical))
            .unwrap()
            .body
            .unwrap()
            .markdown,
        "body"
    );
    let res = t.store.transaction(|tx| -> Result<()> {
        feed::publish_report(
            tx,
            WS,
            "flow",
            "rollback",
            "a",
            &Content::default(),
            &json!({"persisted":true}),
            1,
        )?;
        Err(steve_store::StoreError::Conflict("fault injection".into()))
    });
    assert!(res.is_err());
    assert!(
        t.store
            .reader(|c| feed::report_result(c, WS, "rollback", "a"))
            .unwrap()
            .is_none()
    );
}
#[test]
fn migration_imports_only_existing_notifications_and_moves_seen_to_feed_reads() {
    let t = env();
    let path = t.store.path().to_path_buf();
    t.store.transaction(|tx|->Result<()> {
        tx.execute_batch("DROP TRIGGER notification_feed_insert; DROP TABLE feed_reads; DROP TABLE feed_windows; DROP TABLE feed_entries;")?;
        tx.execute("INSERT INTO personal_notifications(id,workspace_id,user_id,event_id,delivery_key,mode,body,state,seen_at,created) VALUES('old',?,'alice','event','key','log','original','held',42,10)",[WS])?;
        tx.execute("UPDATE schema_version SET v=72",[])?;Ok(())
    }).unwrap();
    drop(t.store);
    let s = Store::open(path, NoPublisher).unwrap();
    let e = s
        .reader(|c| feed::detail(c, WS, "alice", "feed_old"))
        .unwrap();
    assert_eq!(e.read_at, Some(42));
    s.reader(|c| -> Result<()> {
        assert_eq!(count(c, "SELECT count(*) FROM feed_entries"), 1);
        assert_eq!(
            count(
                c,
                "SELECT count(*) FROM personal_notifications WHERE seen_at IS NOT NULL"
            ),
            0
        );
        Ok(())
    })
    .unwrap();
}
