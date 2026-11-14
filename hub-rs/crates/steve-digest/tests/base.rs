//! Deterministic intake and storage contracts. No network, live home, harness or model.
use rusqlite::params;
use serde_json::{Value, json};
use steve_digest::{
    ConvRef, candidates, changes, conv, facts, matter, memindex, profile, queue, todo, window,
};
use steve_modules::testkit::{ME, OTHER, TestEnv, WS, WS2, call, caller};
use steve_store::{
    StoreError,
    base::mems::{self, NewMem},
};

const NOW: i64 = 1_800_000_000_000;
fn cv(id: &str) -> ConvRef {
    ConvRef::new("space", id)
}
fn env() -> TestEnv {
    let e = TestEnv::new(|_| {});
    e.store().with_conn(|c| {
        c.execute_batch("INSERT INTO spaces (id, workspace_id, system_id, kind, external_id, name, created, updated)
            VALUES ('s1', 'default', 'test', 'group', 'g1', 'Project', 1, 1), ('s2', 'default', 'test', 'dm', 'g2', 'DM', 1, 1),
                   ('foreign', 'w2', 'test', 'group', 'g3', 'Foreign', 1, 1);
            INSERT INTO parties (id, workspace_id, type, name, user_id, created_by, created, updated)
            VALUES ('me', 'default', 'person', 'Owner', 'usr_dev', 'usr_dev', 1, 1),
                   ('alice', 'default', 'person', 'Alice', NULL, 'usr_dev', 1, 1),
                   ('bob', 'default', 'person', 'Bob', NULL, 'usr_dev', 1, 1);
            INSERT INTO accounts (id, workspace_id, system_id, family, party_id, kind, handle, handle_display, source, created, updated)
            VALUES ('owner_acc', 'default', 'test', 'other', 'me', 'personal', 'owner', 'Owner', 'me', 1, 1);")?;
        Ok(())
    }).unwrap();
    e
}
fn msg(
    e: &TestEnv,
    space: &str,
    id: &str,
    at: i64,
    reply: Option<&str>,
    thread: Option<&str>,
    text: &str,
) {
    e.store().transaction(|tx| {
        tx.execute("INSERT INTO chan_messages (id, workspace_id, space_id, external_id, direction, sender_party_id,
            text, mentions, reply_to, thread_key, created) VALUES (?, 'default', ?, ?, 'in', 'alice', ?, ?, ?, ?, ?)",
            params![id, space, id, text, json!([{"account_id": "owner_acc", "name": "Owner"}]).to_string(), reply, thread, at])?;
        Ok::<_, StoreError>(())
    }).unwrap();
}
fn patch(e: &TestEnv, space: &str, p: Value) {
    let p: profile::Patch = serde_json::from_value(p).unwrap();
    e.store()
        .transaction(|tx| {
            assert!(
                profile::apply(tx, WS, &cv(space), &p, NOW)
                    .unwrap()
                    .is_some()
            );
            Ok::<_, StoreError>(())
        })
        .unwrap();
}
fn packets(e: &TestEnv) -> Vec<queue::Packet> {
    e.store()
        .reader(|c| queue::list(c, WS, None, None, 500))
        .unwrap()
}
fn segments(e: &TestEnv, mat: &str, id: &str, space: &str, chains: Value) {
    e.store().transaction(|tx| {
        tx.execute("INSERT INTO digest_segments (id, workspace_id, packet_id, conv, first_pos, last_pos, first_id, last_id,
            first_at, last_at, summary, matter_id, parties, chains, writer, created)
            VALUES (?, 'default', 'fixture', ?, 1, 1, 'cm1', 'cm1', ?, ?, 'deploy', ?, '[\"alice\",\"bob\"]', ?, 'fixture', ?)",
            params![id, cv(space).key(), NOW - 100, NOW - 100, mat, chains.to_string(), NOW])?;
        matter::recount(tx, mat, NOW)?;
        matter::reindex(tx, mat)?;
        Ok::<_, StoreError>(())
    }).unwrap();
}

#[test]
fn defaults_overrides_null_reset_and_revision_are_independent() {
    let e = env();
    for kind in profile::KINDS {
        assert_eq!(profile::defaults(kind).window, 50);
    }
    patch(
        &e,
        "s1",
        json!({"window": 500, "kind": "project", "priority": 3}),
    );
    e.store()
        .reader(|c| {
            let s1 = conv::info(c, WS, &cv("s1"))?.unwrap();
            let s2 = conv::info(c, WS, &cv("s2"))?.unwrap();
            let p = profile::profile(c, WS, &s1)?;
            assert_eq!(p.settings.window, 500);
            assert_eq!(profile::profile(c, WS, &s2)?.settings.window, 50);
            Ok::<_, StoreError>(())
        })
        .unwrap();
    patch(&e, "s1", json!({"window": null, "kind": null}));
    e.store()
        .transaction(|tx| {
            let p = profile::profile(tx, WS, &conv::info(tx, WS, &cv("s1"))?.unwrap())?;
            assert_eq!(p.kind, "team");
            assert_eq!(p.settings.window, 50);
            assert!(!p.own.contains(&"window"));
            let stale = serde_json::from_value(json!({"revision": 1, "window": 50})).unwrap();
            assert!(
                profile::apply(tx, WS, &cv("s1"), &stale, NOW)
                    .unwrap()
                    .is_none()
            );
            let invalid = serde_json::from_value(json!({"window": 0})).unwrap();
            assert!(profile::apply(tx, WS, &cv("s1"), &invalid, NOW).is_err());
            Ok::<_, StoreError>(())
        })
        .unwrap();
}

#[test]
fn windows_are_per_conversation_atomic_idempotent_and_accept_late_messages() {
    let e = env();
    patch(&e, "s1", json!({"window": 5}));
    patch(&e, "s2", json!({"window": 5}));
    for n in 1..=6 {
        msg(&e, "s1", &format!("m{n}"), NOW, None, None, "deploy deploy");
    }
    msg(&e, "s2", "dm1", NOW, None, None, "hi");
    let r = window::tick(e.store(), WS, NOW).unwrap();
    assert_eq!(r.windows, 1);
    assert_eq!(packets(&e)[0].count, 5);
    assert_eq!(window::tick(e.store(), WS, NOW).unwrap().windows, 0);
    // A later arrival with the same timestamp but a lower id, and an older backfilled message.
    msg(&e, "s1", "a0", NOW, None, None, "late same timestamp");
    msg(
        &e,
        "s1",
        "old",
        NOW - 10 * window::DAY_MS,
        None,
        None,
        "stored history",
    );
    let r = window::cut_now(e.store(), WS, &cv("s1"), NOW).unwrap();
    assert_eq!(r.windows, 1);
    let ps = packets(&e);
    assert_eq!(ps.iter().map(|p| p.count).sum::<i64>(), 8);
    assert_eq!(
        window::cut_now(e.store(), WS, &cv("s1"), NOW)
            .unwrap()
            .windows,
        0
    );
    assert_eq!(
        window::tick(e.store(), WS, NOW + 31 * 60_000)
            .unwrap()
            .windows,
        1
    );
    assert!(e.gateway.calls.lock().unwrap().is_empty());
    e.store()
        .reader(|c| {
            assert_eq!(window::cursor(c, WS, &cv("s1"))?.unwrap().windows, 2);
            assert_eq!(window::cursor(c, WS, &cv("s2"))?.unwrap().windows, 1);
            Ok::<_, StoreError>(())
        })
        .unwrap();
}

#[test]
fn quiet_max_wait_pause_record_and_daily_budget() {
    let e = env();
    patch(&e, "s1", json!({"depth": "paused"}));
    msg(&e, "s1", "m1", NOW, None, None, "hello");
    assert_eq!(
        window::cut_now(e.store(), WS, &cv("s1"), NOW)
            .unwrap()
            .windows,
        0
    );
    patch(
        &e,
        "s1",
        json!({"depth": "full", "max_wait_ms": 600_000, "daily_budget": 1}),
    );
    msg(&e, "s1", "m2", NOW + 599_999, None, None, "still active");
    let r = window::tick(e.store(), WS, NOW + 600_001).unwrap();
    assert_eq!(r.windows, 1);
    assert_eq!(packets(&e)[0].trigger, "max_wait");
    msg(&e, "s1", "m3", NOW + 700_000, None, None, "deferred");
    assert_eq!(
        window::tick(e.store(), WS, NOW + 3_000_000)
            .unwrap()
            .deferred,
        1
    );
    assert_eq!(
        window::tick(e.store(), WS, NOW + window::DAY_MS)
            .unwrap()
            .windows,
        1
    );
    patch(&e, "s1", json!({"depth": "record"}));
    msg(
        &e,
        "s1",
        "m4",
        NOW + window::DAY_MS,
        None,
        None,
        "record only",
    );
    let r = window::tick(e.store(), WS, NOW + window::DAY_MS).unwrap();
    assert_eq!((r.recorded, r.windows), (1, 0));
    patch(&e, "s1", json!({"depth": "full"}));
    assert_eq!(
        window::cut_now(e.store(), WS, &cv("s1"), NOW + window::DAY_MS)
            .unwrap()
            .windows,
        0
    );
}

#[test]
fn large_windows_chunk_and_keep_deterministic_reply_roots_and_facts() {
    let e = env();
    patch(&e, "s1", json!({"window": 500}));
    for n in 0..500 {
        let id = format!("m{n:04}");
        let reply = (n > 0).then(|| format!("m{:04}", n - 1));
        msg(
            &e,
            "s1",
            &id,
            NOW + n,
            reply.as_deref(),
            None,
            &"deploy ".repeat(50),
        );
    }
    assert_eq!(window::tick(e.store(), WS, NOW + 500).unwrap().windows, 1);
    let ps = packets(&e);
    assert_eq!(ps[0].count, 500);
    let cs = e.store().reader(|c| queue::chunks(c, &ps[0].id)).unwrap();
    assert!(cs.len() > 1);
    assert_eq!(cs[0].first_pos, 1);
    assert_eq!(cs.last().unwrap().last_pos, 500);
    assert!(cs.iter().all(|c| c.tokens <= 6000), "{cs:?}");
    assert_eq!(
        ps[0].facts["me"]["mentioned"].as_array().unwrap().len(),
        500
    );
    msg(
        &e,
        "s1",
        "m0500",
        NOW + 500,
        Some("m0499"),
        None,
        "next window",
    );
    window::cut_now(e.store(), WS, &cv("s1"), NOW + 501).unwrap();
    let ps = packets(&e);
    let second = ps.iter().find(|p| p.seq == 2).unwrap();
    assert_eq!(second.facts["messages"][0]["chain"], "r:m0000");
}

#[test]
fn queue_fences_stale_leases_retries_only_unfinished_chunks_and_counts_crashes() {
    let e = env();
    msg(&e, "s1", "m1", NOW, None, None, "hello");
    window::cut_now(e.store(), WS, &cv("s1"), NOW).unwrap();
    let id = packets(&e)[0].id.clone();
    let b = queue::Batching {
        max_attempts: 2,
        lease_min: 1,
        ..Default::default()
    };
    // The workspace policy is also used when recovering expired leases.
    e.store().transaction(|tx| {
        tx.execute("INSERT INTO settings (workspace_id, key, value, updated) VALUES (?, 'digest', ?, ?)", params![WS, json!({"max_attempts": 2, "lease_min": 1}).to_string(), NOW])?;
        Ok::<_, StoreError>(())
    }).unwrap();
    let first = e
        .store()
        .transaction(|tx| {
            assert_eq!(queue::due(tx, WS, NOW, &b)?, None);
            assert!(queue::due(tx, WS, NOW + b.every_ms(), &b)?.is_some());
            queue::lease(tx, WS, NOW, &b, 5)?;
            Ok::<_, StoreError>(queue::get(tx, WS, &id)?.unwrap())
        })
        .unwrap();
    e.store()
        .transaction(|tx| {
            assert!(queue::packet_done(tx, &first, true, None, &b, NOW + 1).is_err());
            queue::chunk_done(tx, &first, 1, true, None, NOW + 1)?;
            assert_eq!(
                queue::packet_done(tx, &first, false, Some("interrupted"), &b, NOW + 1)?,
                "queued"
            );
            assert_eq!(queue::chunks(tx, &id)?[0].state, "done");
            queue::lease(tx, WS, NOW + 2, &b, 5)?;
            assert!(queue::chunk_done(tx, &first, 1, false, None, NOW + 3).is_err());
            let second = queue::get(tx, WS, &id)?.unwrap();
            assert_eq!(second.attempts, 2);
            assert!(!queue::chunk_done(tx, &second, 1, false, None, NOW + 3)?);
            queue::release_expired(tx, WS, NOW + 60_002)?;
            assert_eq!(queue::get(tx, WS, &id)?.unwrap().state, "failed");
            Ok::<_, StoreError>(())
        })
        .unwrap();
}

#[test]
fn matter_timeline_split_merge_and_not_same_survive_merge_aliases() {
    let e = env();
    let (a, b, c) = e
        .store()
        .transaction(|tx| {
            Ok::<_, matter::Refused>((
                matter::create(tx, WS, "Deploy", ME, NOW)?,
                matter::create(tx, WS, "Incident", ME, NOW)?,
                matter::create(tx, WS, "Other", ME, NOW)?,
            ))
        })
        .unwrap();
    segments(&e, &a, "seg1", "s1", json!(["t:topic"]));
    segments(&e, &a, "seg2", "s2", json!(["t:topic2"]));
    e.store()
        .transaction(|tx| {
            assert_eq!(
                matter::get(tx, WS, &a)?.unwrap()["timeline"]
                    .as_array()
                    .unwrap()
                    .len(),
                2
            );
            let split = matter::split(tx, WS, &a, &["seg2".into()], "Split", ME, NOW)?;
            assert!(matter::said_not_same(tx, WS, &a, &split)?);
            assert!(matches!(
                matter::merge(tx, WS, &split, &a, ME, false, NOW),
                Err(matter::Refused::NotSame(_, _))
            ));
            matter::not_same(tx, WS, &a, &c, ME, NOW)?;
            matter::merge(tx, WS, &a, &b, ME, false, NOW)?;
            assert!(matter::said_not_same(tx, WS, &b, &c)?);
            assert_eq!(matter::resolve(tx, WS, &a)?.unwrap().0, b);
            assert!(matter::get(tx, WS2, &a)?.is_none());
            Ok::<_, matter::Refused>(())
        })
        .unwrap();
}

#[test]
fn deterministic_candidates_use_topics_people_fts_and_workspace() {
    let e = env();
    let mat = e
        .store()
        .transaction(|tx| matter::create(tx, WS, "deploy deployment", ME, NOW))
        .unwrap();
    let foreign = e
        .store()
        .transaction(|tx| matter::create(tx, WS2, "deploy deployment", ME, NOW))
        .unwrap();
    segments(&e, &mat, "seg1", "s1", json!(["t:topic"]));
    msg(
        &e,
        "s1",
        "m1",
        NOW,
        None,
        Some("topic"),
        "deploy deploy deployment deployment",
    );
    e.store()
        .reader(|c| {
            let info = conv::info(c, WS, &cv("s1"))?.unwrap();
            let p = profile::profile(c, WS, &info)?;
            let msgs = conv::read_after(c, WS, &cv("s1"), 0, "", 10)?;
            let built = facts::build(c, WS, &info, &p, 1, "manual", &[], &msgs)?;
            let found = candidates::find(c, WS, &cv("s1"), &built, NOW)?;
            let candidate = found.iter().find(|x| x.id == mat).unwrap();
            assert!(candidate.reasons.contains(&"same_thread"));
            assert!(candidate.reasons.contains(&"fulltext"));
            assert!(!found.iter().any(|x| x.id == foreign));
            // A single overlapping person in a group is not enough.
            assert!(!candidate.reasons.contains(&"same_people"));
            Ok::<_, StoreError>(())
        })
        .unwrap();
}

#[test]
fn memory_index_validates_ids_ranges_and_unlinks_only_what_it_added() {
    let e = env();
    msg(&e, "s1", "m1", NOW, None, None, "deadline tomorrow");
    let mem = e
        .store()
        .transaction(|tx| {
            mems::create(
                tx,
                WS,
                &NewMem {
                    text: "fact".into(),
                    created_by: ME.into(),
                    ..Default::default()
                },
            )
        })
        .unwrap();
    let range = memindex::Range {
        conv: cv("s1").key(),
        first_id: "m1".into(),
        last_id: "m1".into(),
        first_at: NOW,
        last_at: NOW,
    };
    e.store()
        .transaction(|tx| {
            let a = memindex::link(
                tx,
                WS,
                &mem.id,
                &cv("s1"),
                None,
                &["alice".into()],
                Some(&range),
                None,
            )?;
            let repeated = memindex::link(
                tx,
                WS,
                &mem.id,
                &cv("s1"),
                None,
                &["alice".into()],
                Some(&range),
                None,
            )?;
            assert!(repeated.about.is_empty());
            assert!(repeated.range.is_none());
            assert_eq!(
                memindex::index(tx, WS, &mem.id)?["ranges"]
                    .as_array()
                    .unwrap()
                    .len(),
                1
            );
            assert!(memindex::link(tx, WS2, &mem.id, &cv("s1"), None, &[], None, None).is_err());
            let bad = memindex::Range {
                first_at: NOW - 1,
                ..range.clone()
            };
            assert!(
                memindex::link(tx, WS, &mem.id, &cv("s1"), None, &[], Some(&bad), None).is_err()
            );
            memindex::unlink(tx, WS, &repeated)?;
            assert_eq!(
                memindex::index(tx, WS, &mem.id)?["ranges"]
                    .as_array()
                    .unwrap()
                    .len(),
                1
            );
            memindex::unlink(tx, WS, &a)?;
            assert!(
                memindex::index(tx, WS, &mem.id)?["ranges"]
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
            Ok::<_, StoreError>(())
        })
        .unwrap();
}

#[test]
fn personal_todo_deadlines_need_source_words_and_handoff_needs_real_item() {
    let e = env();
    msg(&e, "s1", "m1", NOW, None, None, "I will finish tomorrow");
    e.store()
        .transaction(|tx| {
            let mut t = todo::NewTodo {
                ws: WS,
                owner: ME,
                text: "finish",
                matter: None,
                due: Some(NOW + window::DAY_MS),
                due_quote: None,
                source: "steve",
                writer: "steve",
                packet: None,
                refs: vec![(cv("s1"), "m1".into())],
                parties: vec!["alice".into()],
            };
            assert!(todo::create(tx, &t, NOW).is_err());
            t.due_quote = Some("next week");
            assert!(todo::create(tx, &t, NOW).is_err());
            t.due_quote = Some("tomorrow");
            let id = todo::create(tx, &t, NOW)?;
            assert_eq!(todo::list(tx, WS, ME, &[], 50)?.len(), 1);
            assert!(todo::list(tx, WS, OTHER, &[], 50)?.is_empty());
            assert!(todo::set_state(tx, WS, &id, "handed", Some("made-up"), NOW).is_err());
            todo::set_state(tx, WS, &id, "done", None, NOW + 1)?;
            assert_eq!(todo::current(tx, WS, &id)?.unwrap().0, "done");
            Ok::<_, StoreError>(())
        })
        .unwrap();
}

#[tokio::test]
async fn rest_owner_scope_foreign_ids_profile_resets_and_undo() {
    let e = env();
    let app = steve_digest::routes().with_state(e.modules.clone());
    let me = caller(WS, ME);
    assert_eq!(
        call(&app, None, "GET", "/digest/packets", None).await.0,
        401
    );
    assert_eq!(
        call(
            &app,
            Some(&caller(WS, OTHER)),
            "GET",
            "/digest/packets",
            None
        )
        .await
        .0,
        403
    );
    assert_eq!(
        call(
            &app,
            Some(&me),
            "GET",
            "/digest/conversations/space:foreign",
            None
        )
        .await
        .0,
        404
    );
    assert_eq!(
        call(
            &app,
            Some(&me),
            "PATCH",
            "/digest/conversations/space:s1",
            Some(&json!({"window": 500}))
        )
        .await
        .0,
        200
    );
    assert_eq!(
        call(
            &app,
            Some(&me),
            "PATCH",
            "/digest/conversations/space:s1",
            Some(&json!({"window": null}))
        )
        .await
        .0,
        200
    );
    let (status, body) = call(
        &app,
        Some(&me),
        "POST",
        "/digest/todos",
        Some(&json!({"text": "manual"})),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let id = body["id"].as_str().unwrap();
    let cs = e
        .store()
        .reader(|c| changes::list(c, WS, None, None, 50))
        .unwrap();
    changes::undo(&e.modules, WS, ME, cs[0]["id"].as_str().unwrap()).unwrap();
    assert!(
        e.store()
            .reader(|c| todo::get(c, WS, id))
            .unwrap()
            .is_none()
    );
    assert!(changes::undo(&e.modules, WS, ME, cs[0]["id"].as_str().unwrap()).is_err());
}

#[test]
fn queued_packets_and_receipts_survive_reopening_store_and_rollback_together() {
    let e = env();
    msg(&e, "s1", "m1", NOW, None, None, "persisted");
    let failed: Result<(), StoreError> = e.store().transaction(|tx| {
        let mut report = window::Report::default();
        window::run_conv(
            tx,
            WS,
            &cv("s1"),
            NOW,
            &queue::Batching::default(),
            true,
            &mut report,
        )?;
        Err(StoreError::Hook("rollback fixture".into()))
    });
    assert!(failed.is_err());
    assert!(packets(&e).is_empty());
    assert_eq!(
        window::cut_now(e.store(), WS, &cv("s1"), NOW)
            .unwrap()
            .packets
            .len(),
        1
    );
    let reopened = steve_store::Store::open(
        e.dir.path().join("steve.sqlite3"),
        steve_store::FnPublisher(|_| {}),
    )
    .unwrap();
    assert_eq!(
        reopened
            .reader(|c| queue::list(c, WS, None, None, 50))
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        window::cut_now(&reopened, WS, &cv("s1"), NOW)
            .unwrap()
            .windows,
        0
    );
}

#[test]
fn mail_threads_use_the_same_stored_message_window_contract() {
    let e = env();
    e.store().with_conn(|c| {
        c.execute_batch("INSERT INTO connections (id, workspace_id, account_id, adapter, auth, created_by, created, updated)
            VALUES ('mailconn', 'default', 'owner_acc', 'fixture', 'none', 'usr_dev', 1, 1);
            INSERT INTO mail_threads (id, workspace_id, connection_id, thread_key, subject, last_at, created_by, created, updated)
            VALUES ('mt1', 'default', 'mailconn', 'thread', 'Contract', 1, 'usr_dev', 1, 1);
            INSERT INTO mail_messages (id, workspace_id, connection_id, thread_id, ext_id, message_id, direction, from_addr,
                from_party_id, from_name, body_new, date, created, to_addrs)
            VALUES ('mail1', 'default', 'mailconn', 'mt1', 'ext1', '<root>', 'in', 'alice@example.com',
                'alice', 'Alice', 'first', 10, 10, '[{\"account_id\":\"owner_acc\",\"name\":\"Owner\"}]'),
                ('mail2', 'default', 'mailconn', 'mt1', 'ext2', '<reply>', 'in', 'alice@example.com',
                'alice', 'Alice', 'second', 10, 10, '[]');
            UPDATE mail_messages SET in_reply_to = '<root>', refs = '[\"<root>\"]' WHERE id = 'mail2';
            INSERT INTO mail_attachments (id, workspace_id, message_id, part, name, mime, size, created)
            VALUES ('attachment', 'default', 'mail2', '1.2', 'contract.pdf', 'application/pdf', 1024, 10);")?;
        Ok(())
    }).unwrap();
    let mail = ConvRef::new("mail_thread", "mt1");
    let r = window::cut_now(e.store(), WS, &mail, NOW).unwrap();
    assert_eq!(r.windows, 1);
    let p = &packets(&e)[0];
    assert_eq!(p.conv, "mail_thread:mt1");
    assert_eq!(p.facts["conv"]["kind"], "mail");
    assert_eq!(p.facts["messages"][1]["reply_to"], 1);
    assert_eq!(p.facts["refs"][0]["key"], "attachment");
    assert_eq!(p.facts["me"]["mentioned"], json!([1]));
    assert_eq!(
        window::cut_now(e.store(), WS, &mail, NOW).unwrap().windows,
        0
    );
}

#[test]
fn alert_rules_roll_up_resolved_messages_and_queue_only_anomalies() {
    let e = env();
    patch(&e, "s1", json!({"kind": "feed"}));
    e.store().with_conn(|c| {
        c.execute_batch("INSERT INTO accounts (id, workspace_id, system_id, family, party_id, kind, handle, handle_display, source, created, updated)
            VALUES ('bot_acc', 'default', 'test', 'other', 'bob', 'bot', 'bot', 'Bot', 'me', 1, 1);")?;
        Ok(())
    }).unwrap();
    msg(
        &e,
        "s1",
        "alert1",
        NOW - 2,
        None,
        None,
        "[FIRING] disk usage 95%",
    );
    msg(
        &e,
        "s1",
        "alert2",
        NOW - 1,
        None,
        None,
        "[RESOLVED] disk usage 50%",
    );
    e.store().transaction(|tx| {
        tx.execute("UPDATE chan_messages SET sender_account_id = 'bot_acc', sender_party_id = 'bob', mentions = '[]'", [])?;
        Ok::<_, StoreError>(())
    }).unwrap();
    let r = window::cut_now(e.store(), WS, &cv("s1"), NOW).unwrap();
    assert_eq!((r.rolled, r.packets.len()), (1, 0));
    msg(
        &e,
        "s1",
        "alert3",
        NOW,
        None,
        None,
        "[FIRING] disk usage 99%",
    );
    e.store().transaction(|tx| {
        tx.execute("UPDATE chan_messages SET sender_account_id = 'bot_acc', sender_party_id = 'bob', mentions = '[]' WHERE id = 'alert3'", [])?;
        Ok::<_, StoreError>(())
    }).unwrap();
    assert_eq!(
        window::cut_now(e.store(), WS, &cv("s1"), NOW)
            .unwrap()
            .packets
            .len(),
        1
    );
    assert_eq!(packets(&e)[0].kind, "alerts");
    let rollup = e
        .store()
        .reader(|c| steve_digest::alerts::rollup(c, WS, &cv("s1")))
        .unwrap();
    assert_eq!(rollup[0]["fired"], 2);
    assert_eq!(rollup[0]["recovered"], 1);
    assert_eq!(rollup[0]["open"], true);
}

#[test]
fn private_sources_do_not_enter_owner_digest_and_initial_floor_does_not_slide() {
    let e = env();
    e.store().with_conn(|c| {
        c.execute_batch("INSERT INTO parties (id, workspace_id, type, name, user_id, created_by, created, updated)
            VALUES ('other_person', 'default', 'person', 'Other', 'usr_other', 'usr_other', 1, 1);
            INSERT INTO accounts (id, workspace_id, system_id, family, party_id, kind, handle, handle_display, source, created, updated)
            VALUES ('other_acc', 'default', 'test', 'other', 'other_person', 'personal', 'other', 'Other', 'me', 1, 1);
            UPDATE spaces SET account_id = 'other_acc' WHERE id = 's2';")?;
        Ok(())
    }).unwrap();
    msg(
        &e,
        "s2",
        "private",
        NOW,
        None,
        None,
        "not the owner's messages",
    );
    assert_eq!(
        window::cut_now(e.store(), WS, &cv("s2"), NOW)
            .unwrap()
            .windows,
        0
    );
    assert!(
        e.store()
            .reader(|c| conv::info(c, WS, &cv("s2")))
            .unwrap()
            .is_none()
    );
    msg(
        &e,
        "s1",
        "waiting",
        NOW - 60_000,
        None,
        None,
        "wait while paused",
    );
    patch(&e, "s1", json!({"depth": "paused"}));
    let b = queue::Batching {
        backlog_days: 1,
        ..Default::default()
    };
    e.store()
        .transaction(|tx| {
            window::run_conv(
                tx,
                WS,
                &cv("s1"),
                NOW,
                &b,
                false,
                &mut window::Report::default(),
            )
        })
        .unwrap();
    patch(&e, "s1", json!({"depth": "full"}));
    let mut report = window::Report::default();
    e.store()
        .transaction(|tx| {
            window::run_conv(
                tx,
                WS,
                &cv("s1"),
                NOW + 3 * window::DAY_MS,
                &b,
                false,
                &mut report,
            )
        })
        .unwrap();
    assert_eq!(report.windows, 1);
    assert_eq!(packets(&e)[0].first_id, "waiting");
}

#[test]
fn segment_positions_are_validated_and_manual_reassignment_prevents_undo() {
    let e = env();
    msg(&e, "s1", "source1", NOW - 2, None, Some("topic"), "one");
    msg(&e, "s1", "source2", NOW - 1, None, Some("topic"), "two");
    window::cut_now(e.store(), WS, &cv("s1"), NOW).unwrap();
    let p = &packets(&e)[0];
    let (first, second) = e
        .store()
        .transaction(|tx| {
            Ok::<_, matter::Refused>((
                matter::create(tx, WS, "First", ME, NOW)?,
                matter::create(tx, WS, "Second", ME, NOW)?,
            ))
        })
        .unwrap();
    let id = e
        .store()
        .transaction(|tx| {
            matter::segment(
                tx,
                WS,
                &matter::NewSegment {
                    packet: &p.id,
                    chunk: 1,
                    first: 1,
                    last: 2,
                    summary: "segment",
                    tags: &[],
                    matter: Some(&first),
                },
                ME,
                NOW,
            )
        })
        .unwrap();
    assert!(
        e.store()
            .transaction(|tx| matter::segment(
                tx,
                WS,
                &matter::NewSegment {
                    packet: &p.id,
                    chunk: 1,
                    first: 0,
                    last: 2,
                    summary: "bad",
                    tags: &[],
                    matter: Some(&first),
                },
                ME,
                NOW
            ))
            .is_err()
    );
    let change = e
        .store()
        .reader(|c| changes::list(c, WS, None, Some(&p.id), 10))
        .unwrap()[0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    e.store()
        .transaction(|tx| matter::assign(tx, WS, &id, Some(&second), ME, NOW + 1))
        .unwrap();
    assert!(changes::undo(&e.modules, WS, ME, &change).is_err());
    assert!(e.gateway.calls.lock().unwrap().is_empty());
}

#[test]
fn failed_fifo_packet_can_be_explicitly_retried_without_resetting_receipts() {
    let e = env();
    patch(&e, "s1", json!({"window": 5}));
    for n in 0..10 {
        msg(&e, "s1", &format!("retry{n}"), NOW, None, None, "work");
    }
    window::cut_now(e.store(), WS, &cv("s1"), NOW).unwrap();
    let b = queue::Batching {
        max_attempts: 1,
        ..Default::default()
    };
    let first = e
        .store()
        .transaction(|tx| {
            let (_, ids) = queue::lease(tx, WS, NOW, &b, 10)?;
            assert_eq!(ids.len(), 1);
            let p = queue::get(tx, WS, &ids[0])?.unwrap();
            assert_eq!(
                queue::packet_done(tx, &p, false, Some("fixture"), &b, NOW)?,
                "failed"
            );
            assert!(queue::lease(tx, WS, NOW, &b, 10)?.1.is_empty());
            assert!(queue::retry(tx, WS, &p.id, NOW)?);
            assert_eq!(queue::lease(tx, WS, NOW, &b, 10)?.1, vec![p.id.clone()]);
            Ok::<_, StoreError>(p.id)
        })
        .unwrap();
    assert!(!first.is_empty());
    assert_eq!(
        window::cut_now(e.store(), WS, &cv("s1"), NOW)
            .unwrap()
            .windows,
        0
    );
}
