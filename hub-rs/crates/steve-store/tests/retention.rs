mod common;
use common::*;
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use steve_runtime::{
    FakeClock,
    due::{DueSource, due_loop},
    policy::{Backoff, Jitter},
};
use steve_store_sqlite::retention::{Budget, SliceReport, policies::Kind};

#[test]
fn another_writer_completes_between_retention_slices() {
    let t = fresh();
    t.store
        .with_conn(|c| {
            for n in 0..1200 {
                c.execute(
                    "INSERT INTO node_vitals(node_id,at,data) VALUES('n',?,'{}')",
                    [n],
                )?;
            }
            Ok(())
        })
        .unwrap();
    let mut after = None;
    let mut affected = 0;
    loop {
        let page = t
            .store
            .retention_slice(Kind::Vitals, 200_000_000, Budget::default(), after)
            .unwrap();
        assert!(page.inspected <= 500);
        affected += page.affected;
        after = page.next;
        let store = t.store.clone();
        let (done, wait) = std::sync::mpsc::channel();
        let writer = std::thread::spawn(move || {
            store.with_conn(|c| { c.execute("INSERT OR REPLACE INTO settings(workspace_id,key,value,updated) VALUES('default','writer','true',0)",[])?; Ok(()) }).unwrap();
            done.send(()).unwrap();
        });
        wait.recv_timeout(Duration::from_secs(5))
            .expect("writer grant released after each slice");
        writer.join().unwrap();
        if after.is_none() {
            break;
        }
    }
    assert_eq!(affected, 1200);
}

#[tokio::test]
async fn one_due_source_continues_cursors_and_runs_again_on_virtual_time() {
    let clock = Arc::new(FakeClock::new(0));
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let source = Arc::new(steve_store::retention::Source::new(
        100,
        move |kind: Kind, _: i64, after: Option<i64>| {
            let calls = observed.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(SliceReport {
                    affected: 0,
                    inspected: 1,
                    elapsed_ms: 0,
                    overruns: 0,
                    failures: 0,
                    next: if kind == Kind::Vitals && after.unwrap_or(0) < 5 {
                        Some(after.unwrap_or(0) + 1)
                    } else {
                        None
                    },
                })
            }
        },
    ));
    assert_eq!(source.name(), "storage.retention");
    let task = tokio::spawn(due_loop(
        source.clone(),
        clock.clone(),
        Arc::new(tokio::sync::Notify::new()),
        Backoff {
            base: Duration::from_secs(1),
            max: Duration::from_secs(1),
            factor: 1,
            jitter: Jitter::None,
            attempts: None,
            reset_after: None,
        },
    ));
    async fn settle(clock: &FakeClock, at: i64) {
        for _ in 0..1000 {
            if clock.observed(at) {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("due_loop did not reach virtual sleep at {at}");
    }
    settle(&clock, 0).await;
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    clock.advance_to(100);
    settle(&clock, 100).await;
    assert_eq!(
        calls.load(Ordering::SeqCst),
        15,
        "ten policies plus five continuations; four-slice quota resumes next round"
    );
    assert_eq!(source.next_due().await.unwrap(), Some(300_100));
    clock.advance_to(300_100);
    settle(&clock, 300_100).await;
    assert_eq!(
        calls.load(Ordering::SeqCst),
        30,
        "every sweep restarts its cursors"
    );
    task.abort();
    let _ = task.await;
}

#[test]
fn reused_feed_and_attention_capabilities_are_read_only_and_expiry_is_background_work() {
    use steve_store::{Result, WS, attention, feed};
    let t = fresh();
    t.store.with_conn(|c| {
        c.execute_batch("INSERT INTO users(id,workspace_id,subject,name,role,created) VALUES('u','default','u','User','owner',0);
            INSERT INTO workspace_members(workspace_id,user_id,role,created) VALUES('default','u','owner',0);
            INSERT INTO feed_windows(token,workspace_id,user_id,unread,upper_seq,expires) VALUES('expired','default','u',0,0,0);
            INSERT INTO attention_windows(token,workspace_id,user_id,kind,revision,upper_seq,position,expires) VALUES('expired','default','u','feed','old',0,0,0);")?;
        Ok(())
    }).unwrap();
    let read = || {
        t.store
            .transaction(|tx| -> Result<_> {
                let f = feed::list(tx, WS, "u", &feed::Filter::default(), None, 10, 100)?;
                let a = attention::page(tx, WS, "u", "feed", vec![], 0, None, None, 10, 100)?;
                attention::cleanup(tx, 100)?;
                Ok((f, a))
            })
            .unwrap()
    };
    // First issuance persists the existing opaque-capability contract; repeated reads reuse it.
    read();
    t.store
        .with_conn(|c| {
            c.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
            Ok(())
        })
        .unwrap();
    let bytes = std::fs::read(t.store.path()).unwrap();
    let mtime = std::fs::metadata(t.store.path())
        .unwrap()
        .modified()
        .unwrap();
    let changes = t.store.with_conn(|c| Ok(c.total_changes())).unwrap();
    read();
    assert_eq!(
        t.store.with_conn(|c| Ok(c.total_changes())).unwrap(),
        changes,
        "read request issues no INSERT/UPDATE/DELETE for reusable windows"
    );
    assert_eq!(std::fs::read(t.store.path()).unwrap(), bytes);
    assert_eq!(
        std::fs::metadata(t.store.path())
            .unwrap()
            .modified()
            .unwrap(),
        mtime
    );
    for (kind, table) in [
        (Kind::Feed, "feed_windows"),
        (Kind::Attention, "attention_windows"),
    ] {
        assert_eq!(
            t.store
                .reader(|c| Ok::<_, steve_store::StoreError>(count(
                    c,
                    &format!("SELECT count(*) FROM {table} WHERE token='expired'")
                )))
                .unwrap(),
            1,
            "read did not prune expired window"
        );
        assert_eq!(retain(&t.store, kind, 100), 1);
        assert_eq!(
            t.store
                .reader(|c| Ok::<_, steve_store::StoreError>(count(
                    c,
                    &format!("SELECT count(*) FROM {table} WHERE token='expired'")
                )))
                .unwrap(),
            0
        );
    }
}

#[test]
fn background_payload_compaction_uses_keep_sess_and_normalized_payload_refs() {
    use serde_json::json;
    use steve_store::{NewSession, WS};
    let t = fresh();
    t.store.with_conn(|c| {
        c.execute("INSERT INTO items(id,workspace_id,no,title,status,stage,phase,archived,created,updated) VALUES('i','default',1,'I','done','','closed',1,0,0)",[])?;
        c.execute("INSERT INTO settings(workspace_id,key,value,updated) VALUES('default','keepSess','\"90 天\"',0)",[])?;
        Ok(())
    }).unwrap();
    let session = t
        .store
        .sessions()
        .create(&NewSession {
            workspace_id: WS.into(),
            item_id: Some("i".into()),
            harness: "fixture".into(),
            cwd: "/tmp".into(),
            ..Default::default()
        })
        .unwrap();
    t.store.events().add(&session.id,"user",json!({"text":"screenshot","images":[{"type":"image","data":"QUJD".repeat(1000),"mimeType":"image/png"}]})).unwrap();
    let now = 100 * 86_400_000;
    assert_eq!(
        retain(&t.store, Kind::Payloads, now),
        0,
        "active session protected even on an archived item"
    );
    t.store
        .with_conn(|c| {
            c.execute(
                "UPDATE sessions SET status='ended' WHERE id=?",
                [&session.id],
            )?;
            Ok(())
        })
        .unwrap();
    assert_eq!(retain(&t.store, Kind::Payloads, now), 1);
    let ev = t.store.events().after(&session.id, 0, 100).unwrap();
    assert!(ev[0].data["images"][0].get("data").is_none());
    assert_eq!(
        t.store
            .reader(|c| Ok::<_, steve_store::StoreError>(count(
                c,
                "SELECT count(*) FROM event_payload_refs"
            )))
            .unwrap(),
        0,
        "compacted content has no stale payload reference"
    );
    assert_eq!(
        retain(&t.store, Kind::Payloads, now),
        0,
        "repeated slices are idempotent"
    );
    t.store
        .with_conn(|c| {
            c.execute(
                "UPDATE settings SET value='\"永远\"' WHERE key='keepSess'",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    t.store
        .events()
        .add(
            &session.id,
            "user",
            json!({"images":[{"type":"image","data":"QUJD".repeat(1000),"mimeType":"image/png"}]}),
        )
        .unwrap();
    assert_eq!(
        retain(&t.store, Kind::Payloads, now),
        0,
        "forever setting wins on each sweep"
    );
}
