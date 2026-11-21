mod common;
use common::*;
use std::time::Duration;
use steve_store_sqlite::retention::{Budget, policies::Kind};

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
fn only_the_core_compaction_guard_can_compress_payloads_and_advance_upto() {
    use serde_json::json;
    use steve_store::{NewSession, WS};
    let t = fresh();
    let session = t
        .store
        .sessions()
        .create(&NewSession {
            workspace_id: WS.into(),
            harness: "fixture".into(),
            cwd: "/tmp".into(),
            ..Default::default()
        })
        .unwrap();
    t.store.events().add(&session.id,"user",json!({"text":"screenshot","images":[{"type":"image","data":"QUJD".repeat(1000),"mimeType":"image/png"}]})).unwrap();
    let upto = t.store.events().last_seq(&session.id).unwrap();
    for kind in steve_store::retention::ALL {
        retain(&t.store, kind, 100 * 86_400_000);
    }
    assert!(
        t.store.events().after(&session.id, 0, 100).unwrap()[0].data["images"][0]
            .get("data")
            .is_some(),
        "no background policy compresses sessions"
    );
    assert_eq!(
        t.store
            .events()
            .compact(&session.id, upto, || None::<()>)
            .unwrap(),
        None
    );
    assert_eq!(
        t.store.events().compacted_upto(&session.id).unwrap(),
        0,
        "cancelled Core guard cannot certify a pass"
    );
    assert_eq!(
        t.store
            .events()
            .compact(&session.id, upto, || Some(()))
            .unwrap(),
        Some(1)
    );
    assert_eq!(t.store.events().compacted_upto(&session.id).unwrap(), upto);
    assert_eq!(
        t.store
            .reader(|c| Ok::<_, steve_store::StoreError>(count(
                c,
                "SELECT count(*) FROM event_payload_refs"
            )))
            .unwrap(),
        0,
        "Core compaction removes stale normalized payload references"
    );
    assert_eq!(
        t.store
            .events()
            .compact(&session.id, upto, || Some(()))
            .unwrap(),
        Some(0)
    );
}
