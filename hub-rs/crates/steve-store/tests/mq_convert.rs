//! 058: the 031 lanes into the merge queue. Every state converts (or is left, read only), and running
//! the conversion again changes nothing.

use rusqlite::{Connection, OptionalExtension, params};
use steve_store::{NewDecision, Store, StoreError, mq};

fn store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(dir.path().join("hub.sqlite3"), steve_store::NoPublisher).unwrap();
    s.ensure_workspace().unwrap();
    (dir, s)
}

fn exec(s: &Store, sql: &str, p: impl rusqlite::Params) {
    s.transaction::<_, StoreError>(|tx| {
        tx.execute(sql, p)?;
        Ok(())
    })
    .unwrap();
}

fn item(s: &Store, id: &str, no: i64) {
    exec(
        s,
        "INSERT INTO items (id, workspace_id, no, title, status, stage, created, updated)
         VALUES (?, 'default', ?, 't', 'wait', 'x', 1, 1)",
        params![id, no],
    );
}

fn request(
    s: &Store,
    id: &str,
    item: &str,
    state: &str,
    unit: Option<i64>,
    released_in: Option<i64>,
) {
    exec(
        s,
        "INSERT INTO lane_requests (id, lane_id, workspace_id, item_id, source, dir, branch, head, arrived, unit,
            released_in, state, updated)
         VALUES (?, 'ln_1', 'default', ?, 'item', '/r', ?, ?, ?, ?, ?, ?, 1)",
        params![
            id,
            item,
            format!("steve/{item}"),
            format!("h-{item}"),
            id.len() as i64,
            unit,
            released_in,
            state
        ],
    );
}

fn card(s: &Store, kind: &str, subject_type: &str, subject: &str, item: Option<&str>) -> String {
    s.decisions()
        .create(&NewDecision {
            workspace_id: "default".into(),
            kind: kind.into(),
            subject_type: subject_type.into(),
            subject_id: subject.into(),
            item_id: item.map(Into::into),
            question: "q".into(),
            requested_by: "steve".into(),
            ..Default::default()
        })
        .unwrap()
        .unwrap()
        .id
}

fn one<T: rusqlite::types::FromSql>(
    c: &Connection,
    sql: &str,
    p: impl rusqlite::Params,
) -> Option<T> {
    c.query_row(sql, p, |r| r.get(0)).optional().unwrap()
}

#[test]
fn every_lane_state_converts_once() {
    let (_d, s) = store();
    exec(
        &s,
        "INSERT INTO lanes (id, upstream, branch, window_ms, max_members, created, updated)
         VALUES ('ln_1', 'github.com/o/r', 'main', 600000, 4, 1, 1)",
        [],
    );
    exec(
        &s,
        "INSERT INTO lanes (id, upstream, branch, created, updated) VALUES ('ln_2', 'github.com/o/x', 'main', 2, 2)",
        [],
    );
    for (i, it) in ["ST-1", "ST-2", "ST-3", "ST-4", "ST-5", "ST-6", "ST-7"]
        .iter()
        .enumerate()
    {
        item(&s, it, i as i64 + 1);
    }
    // ST-1 accepted twice: the last passing acceptance is its consent
    let a1 = card(&s, "accept", "item", "ST-1", Some("ST-1"));
    s.decisions()
        .resolve(
            &a1,
            &serde_json::json!({"choice": "pass"}),
            "resolved",
            &serde_json::Value::Null,
        )
        .unwrap();
    request(&s, "lr_q", "ST-1", "queued", None, None);
    request(&s, "lr_c", "ST-2", "collecting", Some(5), None);
    request(&s, "lr_m", "ST-3", "merging", Some(4), None);
    request(&s, "lr_d", "ST-4", "merged", Some(3), Some(5));
    request(&s, "lr_f", "ST-5", "merged", Some(2), Some(2));
    request(&s, "lr_r", "ST-6", "released", Some(1), Some(1));
    request(&s, "lr_l", "ST-7", "left", Some(1), None);
    for (no, st) in [
        (1, "done"),
        (2, "release_failed"),
        (3, "done"),
        (4, "merging"),
        (5, "collecting"),
    ] {
        exec(
            &s,
            "INSERT INTO lane_units (lane_id, no, state, opened) VALUES ('ln_1', ?, ?, 1)",
            params![no, st],
        );
    }
    let approve = card(&s, "approve", "delivery_batch", "lane:ln_1:4", None);
    let fail = card(&s, "fail", "delivery_batch", "lane:ln_1:2", None);

    let done = s
        .transaction::<_, StoreError>(|tx| mq::convert_031(tx, "converted"))
        .unwrap();
    assert_eq!(done.targets, 2);
    assert_eq!(done.queued, 3, "queued, collecting and merging");
    assert_eq!(done.landed, 2);
    assert_eq!(done.failed_trains, 1);
    assert_eq!(done.superseded, std::slice::from_ref(&approve));
    assert_eq!(done.items.len(), 5);

    let t = s
        .reader(|c| mq::target_by(c, "github.com/o/r", "main"))
        .unwrap()
        .unwrap();
    assert_eq!(t.legacy_lane.as_deref(), Some("ln_1"));
    assert_eq!(t.granted_by.as_deref(), Some("migrated-031"));
    assert_eq!(t.next_batch, 5, "MQ numbers continue the lane's");
    let p: serde_json::Value = serde_json::from_str(&t.policy).unwrap();
    assert_eq!(
        p,
        serde_json::json!({"release_window_s": 600, "max_batch": 4})
    );

    let entries = s.reader(|c| mq::entries_live(c, &t.id)).unwrap();
    let by = |item: &str| {
        entries
            .iter()
            .find(|e| e.item_id.as_deref() == Some(item))
            .cloned()
    };
    for it in ["ST-1", "ST-2", "ST-3"] {
        let e = by(it).unwrap();
        assert_eq!(e.state, "queued", "{it}");
        assert_eq!(e.head, format!("h-{it}"));
    }
    assert_eq!(by("ST-1").unwrap().consent.as_deref(), Some(a1.as_str()));
    assert_eq!(by("ST-2").unwrap().consent, None);
    assert!(
        by("ST-6").is_none() && by("ST-7").is_none(),
        "released and left stay behind"
    );

    // ST-4 merged, its release put off: landed in a new open train; ST-5 with its failed one
    let trains = s.reader(|c| mq::trains_live(c, &t.id)).unwrap();
    let state = |no: Option<i64>| {
        trains
            .iter()
            .find(|x| Some(x.no) == no)
            .map(|x| x.state.clone())
    };
    let d4 = by("ST-4").unwrap();
    let d5 = by("ST-5").unwrap();
    assert_eq!((d4.state.as_str(), d5.state.as_str()), ("landed", "landed"));
    assert_eq!(state(d4.train_no).as_deref(), Some("open"));
    assert_eq!(state(d5.train_no).as_deref(), Some("failed"));
    assert_eq!(t.next_train, 2);
    let fail_now: (String, String, String) = s
        .reader(|c| {
            Ok::<_, StoreError>(
                c.query_row(
                    "SELECT subject_type, subject_id, status FROM decisions WHERE id = ?",
                    [&fail],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .unwrap(),
            )
        })
        .unwrap();
    assert_eq!(
        fail_now,
        (
            "mq_train".into(),
            format!("{}:{}", t.id, d5.train_no.unwrap()),
            "open".into()
        ),
        "the failed release's card is kept, now the train's"
    );
    let approve_now: Option<String> = s
        .reader(|c| {
            Ok::<_, StoreError>(one(
                c,
                "SELECT status FROM decisions WHERE id = ?",
                [&approve],
            ))
        })
        .unwrap();
    assert_eq!(approve_now.as_deref(), Some("expired"));

    // again: nothing more
    let count = |s: &Store| -> (i64, i64, i64) {
        s.reader(|c| {
            Ok::<_, StoreError>((
                one(c, "SELECT count(*) FROM mq_targets", []).unwrap(),
                one(c, "SELECT count(*) FROM mq_entries", []).unwrap(),
                one(c, "SELECT count(*) FROM mq_trains", []).unwrap(),
            ))
        })
        .unwrap()
    };
    let before = count(&s);
    let again = s
        .transaction::<_, StoreError>(|tx| mq::convert_031(tx, "converted"))
        .unwrap();
    assert_eq!(again, mq::Converted::default());
    assert_eq!(count(&s), before);
}

#[test]
fn the_rows_round_trip() {
    let (_d, s) = store();
    let t = s
        .transaction::<_, StoreError>(|tx| mq::target_for(tx, "github.com/o/r", "main"))
        .unwrap();
    let again = s
        .transaction::<_, StoreError>(|tx| mq::target_for(tx, "github.com/o/r", "main"))
        .unwrap();
    assert_eq!(t.id, again.id);
    let e = s
        .transaction::<_, StoreError>(|tx| {
            mq::entry_add(
                tx,
                &mq::NewEntry {
                    target_id: &t.id,
                    workspace_id: Some("default"),
                    item_id: Some("ST-1"),
                    cycle: Some(1),
                    key: None,
                    after: &["ST-0".into()],
                    head: "abc",
                    branch: Some("steve/st-1"),
                    consent: Some("dec_1"),
                    node_id: Some("local"),
                },
            )
        })
        .unwrap();
    assert_eq!(
        (e.key.as_str(), e.after.as_str(), e.state.as_str()),
        ("ST-1", r#"["ST-0"]"#, "queued")
    );
    // a second live entry of the same item and cycle is refused
    assert!(
        s.transaction::<_, StoreError>(|tx| {
            mq::entry_add(
                tx,
                &mq::NewEntry {
                    target_id: &t.id,
                    workspace_id: None,
                    item_id: Some("ST-1"),
                    cycle: Some(1),
                    key: None,
                    after: &[],
                    head: "def",
                    branch: None,
                    consent: None,
                    node_id: None,
                },
            )
        })
        .is_err()
    );
    let b = mq::MqBatch {
        target_id: t.id.clone(),
        no: 1,
        parent: None,
        root: 1,
        state: "running".into(),
        step: Some("integrating".into()),
        base: None,
        tip: None,
        members: format!(r#"["{}"]"#, e.id),
        checks: None,
        pr: None,
        outcome: None,
        reruns: 0,
        opened: 1,
        ended: None,
    };
    s.transaction::<_, StoreError>(|tx| mq::batch_put(tx, &b))
        .unwrap();
    assert_eq!(
        s.reader(|c| mq::batches_live(c, &t.id)).unwrap(),
        std::slice::from_ref(&b)
    );
    let landed = mq::MqBatch {
        state: "landed".into(),
        ended: Some(2),
        ..b
    };
    s.transaction::<_, StoreError>(|tx| mq::batch_put(tx, &landed))
        .unwrap();
    assert!(s.reader(|c| mq::batches_live(c, &t.id)).unwrap().is_empty());
    s.transaction::<_, StoreError>(|tx| {
        mq::stat_add(
            tx,
            &t.id,
            "batch_ended",
            Some(1),
            None,
            &serde_json::json!({"members": 1}),
        )
    })
    .unwrap();
    let st = s
        .reader(|c| mq::stats_since(c, Some(&t.id), 0, 10))
        .unwrap();
    assert_eq!(st.len(), 1);
    assert_eq!(st[0].data["members"], 1);
    s.transaction::<_, StoreError>(|tx| {
        mq::step_put(
            tx,
            &mq::MqStep {
                operation_id: "op_1".into(),
                target_id: t.id.clone(),
                batch_no: Some(1),
                train_no: None,
                kind: "push".into(),
                state: "pending".into(),
                result: None,
                created: 0,
                updated: 0,
            },
        )
    })
    .unwrap();
    s.transaction::<_, StoreError>(|tx| {
        mq::step_put(
            tx,
            &mq::MqStep {
                operation_id: "op_1".into(),
                target_id: t.id.clone(),
                batch_no: Some(1),
                train_no: None,
                kind: "push".into(),
                state: "finished".into(),
                result: Some("{}".into()),
                created: 0,
                updated: 0,
            },
        )
    })
    .unwrap();
    let steps = s.reader(|c| mq::steps_of(c, &t.id, 1, false)).unwrap();
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].state, "finished");
}
