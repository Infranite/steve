//! Deterministic intake and storage contracts. No network, live home, harness or model.
#[path = "support/digest_env.rs"]
mod digest_env;
use digest_env::TestEnv;
use serde_json::{Value, json};
use steve_digest::{
    ConvRef, candidates, changes, conv, facts, matter, memindex, profile, queue, todo, window,
};
use steve_modules::testkit::{ME, OTHER, WS, WS2, call, caller};
use steve_store::testkit::digest as fixture;
use steve_store::{
    StoreError,
    base::mems::{self, NewMem},
};

const NOW: i64 = 1_800_000_000_000;

#[test]
fn owner_viewer_resolves_the_workspace_owner() {
    let e = TestEnv::new(|_| {});
    e.store()
        .read(|r| {
            let viewer = conv::owner_viewer(r, WS)?.unwrap();
            assert_eq!(viewer.user, ME);
            assert!(viewer.scopes.is_empty());
            assert!(conv::owner_viewer(r, "missing").unwrap().is_none());
            Ok(())
        })
        .unwrap();
}

fn cv(id: &str) -> ConvRef {
    ConvRef::new("space", id)
}
fn env() -> TestEnv {
    let e = TestEnv::new(|_| {});
    fixture::conversations(e.store()).unwrap();
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
    e.store()
        .write(|w| {
            fixture::seed_message(
                w,
                fixture::ChannelInput {
                    id,
                    space,
                    at,
                    reply,
                    thread,
                    text,
                    mentions: &json!([{"account_id": "owner_acc", "name": "Owner"}]),
                },
            )
        })
        .unwrap();
}
fn patch(e: &TestEnv, space: &str, p: Value) {
    let p: profile::Patch = serde_json::from_value(p).unwrap();
    e.store()
        .write(|w| {
            assert!(
                profile::apply(w, WS, &cv(space), &p, NOW)
                    .unwrap()
                    .is_some()
            );
            Ok::<_, StoreError>(())
        })
        .unwrap();
}
fn packets(e: &TestEnv) -> Vec<queue::Packet> {
    e.store()
        .read(|r| r.digest_packets().list(WS, None, None, 500))
        .unwrap()
}
fn segments(e: &TestEnv, mat: &str, id: &str, space: &str, chains: Value) {
    e.store()
        .write(|w| {
            fixture::seed_segment(w, mat, id, space, &chains, NOW)?;
            matter::recount(w, mat, NOW)?;
            matter::reindex(w, mat)
        })
        .unwrap();
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
    let (one, two) = e
        .store()
        .read(|r| {
            let mut profiles = r.digest_profiles();
            Ok((profiles.get(WS, "space:s1")?, profiles.get(WS, "space:s2")?))
        })
        .unwrap();
    e.store()
        .read(|c| {
            let s1 = conv::info(c, WS, &cv("s1"))?.unwrap();
            let s2 = conv::info(c, WS, &cv("s2"))?.unwrap();
            let p = profile::profile(&s1, one, || conv::guess_kind(c, WS, &s1))?;
            assert_eq!(p.settings.window, 500);
            let p = profile::profile(&s2, two, || conv::guess_kind(c, WS, &s2))?;
            assert_eq!(p.settings.window, 50);
            Ok::<_, StoreError>(())
        })
        .unwrap();
    patch(&e, "s1", json!({"window": null, "kind": null}));
    e.store()
        .write(|w| {
            let info = conv::info(w, WS, &cv("s1"))?.unwrap();
            let stored = w.digest_profiles().get(WS, "space:s1")?;
            let p = profile::profile(&info, stored, || conv::guess_kind(w, WS, &info))?;
            assert_eq!(p.kind, "team");
            assert_eq!(p.settings.window, 50);
            assert!(!p.own.contains(&"window"));
            let stale = serde_json::from_value(json!({"revision": 1, "window": 50})).unwrap();
            assert!(
                profile::apply(w, WS, &cv("s1"), &stale, NOW)
                    .unwrap()
                    .is_none()
            );
            let invalid = serde_json::from_value(json!({"window": 0})).unwrap();
            assert!(profile::apply(w, WS, &cv("s1"), &invalid, NOW).is_err());
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
        .read(|r| {
            assert_eq!(
                r.digest_windows().cursor(WS, "space:s1")?.unwrap().windows,
                2
            );
            assert_eq!(
                r.digest_windows().cursor(WS, "space:s2")?.unwrap().windows,
                1
            );
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
    let cs = e
        .store()
        .read(|r| r.digest_packets().chunks(&ps[0].id))
        .unwrap();
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
    e.store()
        .write(|w| fixture::set_batching(w, WS, &json!({"max_attempts": 2, "lease_min": 1}), NOW))
        .unwrap();
    let first = e
        .store()
        .write(|tx| {
            assert_eq!(tx.scope(|w| w.digest_packets().due(WS, NOW, &b))?, None);
            assert!(
                tx.scope(|w| w.digest_packets().due(WS, NOW + b.every_ms(), &b))?
                    .is_some()
            );
            tx.scope(|w| {
                w.digest_packets()
                    .lease_matching(WS, NOW, &b, 5, |_, _| Ok(true))
            })?;
            Ok::<_, StoreError>(tx.scope(|w| w.digest_packets().get(WS, &id))?.unwrap())
        })
        .unwrap();
    e.store()
        .write(|tx| {
            assert!(
                tx.scope(|w| w
                    .digest_packets()
                    .packet_done(&first, true, None, &b, NOW + 1))
                    .is_err()
            );
            tx.scope(|w| {
                w.digest_packets()
                    .chunk_done(&first, 1, true, None, NOW + 1)
            })?;
            assert_eq!(
                tx.scope(|w| w.digest_packets().packet_done(
                    &first,
                    false,
                    Some("interrupted"),
                    &b,
                    NOW + 1
                ))?,
                "queued"
            );
            assert_eq!(
                tx.scope(|w| w.digest_packets().chunks(&id))?[0].state,
                "done"
            );
            tx.scope(|w| {
                w.digest_packets()
                    .lease_matching(WS, NOW + 2, &b, 5, |_, _| Ok(true))
            })?;
            assert!(
                tx.scope(|w| w
                    .digest_packets()
                    .chunk_done(&first, 1, false, None, NOW + 3))
                    .is_err()
            );
            let second = tx.scope(|w| w.digest_packets().get(WS, &id))?.unwrap();
            assert_eq!(second.attempts, 2);
            assert!(!tx.scope(|w| w.digest_packets().chunk_done(
                &second,
                1,
                false,
                None,
                NOW + 3
            ))?);
            tx.scope(|w| w.digest_packets().release_expired(WS, NOW + 60_002))?;
            assert_eq!(
                tx.scope(|w| w.digest_packets().get(WS, &id))?
                    .unwrap()
                    .state,
                "failed"
            );
            Ok::<_, StoreError>(())
        })
        .unwrap();
}

#[test]
fn matter_timeline_split_merge_and_not_same_survive_merge_aliases() {
    let e = env();
    let (a, b, c) = e
        .store()
        .write(|tx| {
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
        .write(|tx| {
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
        .write(|tx| matter::create(tx, WS, "deploy deployment", ME, NOW))
        .unwrap();
    let foreign = e
        .store()
        .write(|tx| matter::create(tx, WS2, "deploy deployment", ME, NOW))
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
        .read(|c| {
            let info = conv::info(c, WS, &cv("s1"))?.unwrap();
            let p = profile::profile(&info, None, || conv::guess_kind(c, WS, &info))?;
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
        .write(|w| {
            let a = memindex::link(
                w,
                WS,
                &mem.id,
                &cv("s1"),
                None,
                &["alice".into()],
                Some(&range),
                None,
            )?;
            let repeated = memindex::link(
                w,
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
            assert_eq!(w.memory_index().get(WS, &mem.id)?.ranges.len(), 1);
            assert!(memindex::link(w, WS2, &mem.id, &cv("s1"), None, &[], None, None).is_err());
            let bad = memindex::Range {
                first_at: NOW - 1,
                ..range.clone()
            };
            assert!(
                memindex::link(w, WS, &mem.id, &cv("s1"), None, &[], Some(&bad), None).is_err()
            );
            w.memory_index().unlink(WS, &repeated)?;
            assert_eq!(w.memory_index().get(WS, &mem.id)?.ranges.len(), 1);
            w.memory_index().unlink(WS, &a)?;
            assert!(w.memory_index().get(WS, &mem.id)?.ranges.is_empty());
            Ok::<_, StoreError>(())
        })
        .unwrap();
}

#[test]
fn personal_todo_deadlines_need_source_words_and_handoff_needs_real_item() {
    let e = env();
    msg(&e, "s1", "m1", NOW, None, None, "I will finish tomorrow");
    e.store()
        .write(|tx| {
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
                ..Default::default()
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
        .read(|r| r.digest_changes().list(WS, None, None, 50, None))
        .unwrap();
    changes::undo(&e.modules, WS, ME, &cs[0].id).unwrap();
    assert!(e.store().read(|c| todo::get(c, WS, id)).unwrap().is_none());
    assert!(changes::undo(&e.modules, WS, ME, &cs[0].id).is_err());
}

#[test]
fn queued_packets_and_receipts_survive_reopening_store_and_rollback_together() {
    let e = env();
    msg(&e, "s1", "m1", NOW, None, None, "persisted");
    let failed: Result<(), StoreError> = e.store().write(|w| {
        let mut report = window::Report::default();
        window::run_conv(
            w,
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
            .read(|r| r.digest_packets().list(WS, None, None, 50))
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
    fixture::audit(&reopened).unwrap();
}

#[test]
fn mail_threads_use_the_same_stored_message_window_contract() {
    let e = env();
    fixture::mail_thread(e.store()).unwrap();
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
    fixture::alert_sender(e.store()).unwrap();
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
    fixture::change_sender(e.store(), None).unwrap();
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
    fixture::change_sender(e.store(), Some("alert3")).unwrap();
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
        .read(|r| {
            Ok(serde_json::to_value(
                r.digest_alerts().rollup(WS, "space:s1")?,
            )?)
        })
        .unwrap();
    assert_eq!(rollup[0]["fired"], 2);
    assert_eq!(rollup[0]["recovered"], 1);
    assert_eq!(rollup[0]["open"], true);
}

#[test]
fn single_user_digest_reads_other_accounts_and_initial_floor_does_not_slide() {
    let e = env();
    fixture::other_account(e.store()).unwrap();
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
        1
    );
    assert!(
        e.store()
            .read(|c| conv::info(c, WS, &cv("s2")))
            .unwrap()
            .is_some()
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
        .write(|w| {
            window::run_conv(
                w,
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
        .write(|w| {
            window::run_conv(
                w,
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
    assert_eq!(
        packets(&e)
            .iter()
            .find(|packet| packet.conv == cv("s1").key())
            .unwrap()
            .first_id,
        "waiting"
    );
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
        .write(|tx| {
            Ok::<_, matter::Refused>((
                matter::create(tx, WS, "First", ME, NOW)?,
                matter::create(tx, WS, "Second", ME, NOW)?,
            ))
        })
        .unwrap();
    let id = e
        .store()
        .write(|tx| {
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
            .write(|tx| matter::segment(
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
        .read(|r| r.digest_changes().list(WS, None, Some(&p.id), 10, None))
        .unwrap()[0]
        .id
        .clone();
    e.store()
        .write(|tx| matter::assign(tx, WS, &id, Some(&second), ME, NOW + 1))
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
        .write(|tx| {
            let (_, ids) = tx.scope(|w| {
                w.digest_packets()
                    .lease_matching(WS, NOW, &b, 10, |_, _| Ok(true))
            })?;
            assert_eq!(ids.len(), 1);
            let p = tx.scope(|w| w.digest_packets().get(WS, &ids[0]))?.unwrap();
            assert_eq!(
                tx.scope(|w| w
                    .digest_packets()
                    .packet_done(&p, false, Some("fixture"), &b, NOW))?,
                "failed"
            );
            assert!(
                tx.scope(|w| w
                    .digest_packets()
                    .lease_matching(WS, NOW, &b, 10, |_, _| Ok(true)))?
                    .1
                    .is_empty()
            );
            assert!(tx.scope(|w| w.digest_packets().retry(WS, &p.id, NOW))?);
            assert_eq!(
                tx.scope(|w| w
                    .digest_packets()
                    .lease_matching(WS, NOW, &b, 10, |_, _| Ok(true)))?
                    .1,
                vec![p.id.clone()]
            );
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

#[tokio::test]
async fn single_user_matter_detail_includes_other_creators_private_memories() {
    let e = env();
    let app = steve_digest::routes()
        .merge(steve_knowledge::routes(steve_knowledge::Knowledge::new(
            |_| {},
        )))
        .with_state(e.modules.clone());
    let owner = caller(WS, ME);
    let other = caller(WS, OTHER);
    let (status, matter) = call(
        &app,
        Some(&owner),
        "POST",
        "/digest/matters",
        Some(&json!({"title": "Shared matter"})),
    )
    .await;
    assert_eq!(status, 200, "{matter}");
    let id = matter["id"].as_str().unwrap();
    let mut public_id = String::new();
    for (user, visibility, text) in [
        (&other, "private", "Another member's private memory"),
        (&other, "workspace", "A workspace memory"),
        (&owner, "private", "Owner's private memory"),
    ] {
        let (status, memory) = call(
            &app,
            Some(user),
            "POST",
            "/kb/mems",
            Some(&json!({
                "text": text, "visibility": visibility, "about": [{"type": "matter", "id": id}]
            })),
        )
        .await;
        assert_eq!(status, 200, "{memory}");
        if visibility == "workspace" {
            public_id = memory["id"].as_str().unwrap().to_string();
        }
    }
    let (status, detail) = call(
        &app,
        Some(&owner),
        "GET",
        &format!("/digest/matters/{id}"),
        None,
    )
    .await;
    assert_eq!(status, 200, "{detail}");
    let memories = detail["memories"].as_array().unwrap();
    assert_eq!(memories.len(), 3, "{detail}");
    assert!(memories.iter().any(|m| m["id"] == public_id));
    assert!(
        memories
            .iter()
            .any(|m| m["text"] == "Owner's private memory")
    );
    assert!(
        detail
            .to_string()
            .contains("Another member's private memory")
    );
}

#[tokio::test]
async fn memory_index_undo_conflicts_with_later_ranges_without_partial_deletion() {
    let e = env();
    let app = steve_digest::routes().with_state(e.modules.clone());
    let owner = caller(WS, ME);
    msg(&e, "s1", "range1", NOW, None, None, "first");
    msg(&e, "s1", "range2", NOW + 1, None, None, "second");
    let mem = e
        .store()
        .transaction(|tx| {
            mems::create(
                tx,
                WS,
                &NewMem {
                    text: "Indexed fact".into(),
                    created_by: ME.into(),
                    ..Default::default()
                },
            )
        })
        .unwrap();
    let path = format!("/digest/memories/{}/index", mem.id);
    let mut changes = Vec::new();
    for (message, at) in [("range1", NOW), ("range2", NOW + 1)] {
        let (status, body) = call(
            &app,
            Some(&owner),
            "POST",
            &path,
            Some(&json!({
                "conv": "space:s1", "range": {"conv": "space:s1", "first_id": message,
                    "last_id": message, "first_at": at, "last_at": at}
            })),
        )
        .await;
        assert_eq!(status, 200, "{body}");
        changes.push(body["change"].as_str().unwrap().to_string());
    }
    let before = e
        .store()
        .read(|r| Ok(serde_json::to_value(r.memory_index().get(WS, &mem.id)?)?))
        .unwrap();
    let (status, body) = call(
        &app,
        Some(&owner),
        "POST",
        &format!("/digest/changes/{}/undo", changes[0]),
        None,
    )
    .await;
    assert_eq!(status, 409, "{body}");
    assert_eq!(
        e.store()
            .read(|r| Ok(serde_json::to_value(r.memory_index().get(WS, &mem.id)?)?))
            .unwrap(),
        before
    );
    let state: String = e
        .store()
        .read(|r| Ok(r.digest_changes().get(WS, &changes[0])?.unwrap().state))
        .unwrap();
    assert_eq!(state, "applied");
    // Removing the later operation first makes the original insertion safe to undo again.
    for change in changes.iter().rev() {
        let (status, body) = call(
            &app,
            Some(&owner),
            "POST",
            &format!("/digest/changes/{change}/undo"),
            None,
        )
        .await;
        assert_eq!(status, 200, "{body}");
    }
    assert_eq!(
        e.store()
            .read(|r| Ok(serde_json::to_value(r.memory_index().get(WS, &mem.id)?)?))
            .unwrap(),
        json!({"about": [], "ranges": []})
    );
}

#[tokio::test]
async fn memory_index_undo_conflicts_after_matter_merge_without_partial_deletion() {
    let e = env();
    let app = steve_digest::routes().with_state(e.modules.clone());
    let owner = caller(WS, ME);
    let (a, b) = e
        .store()
        .write(|w| {
            Ok::<_, matter::Refused>((
                matter::create(w, WS, "First", ME, NOW)?,
                matter::create(w, WS, "Second", ME, NOW)?,
            ))
        })
        .unwrap();
    let mem = fixture::create_memory(
        e.store(),
        WS,
        &NewMem {
            text: "Fact".into(),
            created_by: ME.into(),
            ..Default::default()
        },
    )
    .unwrap();
    let path = format!("/digest/memories/{}/index", mem.id);
    let (status, body) = call(
        &app,
        Some(&owner),
        "POST",
        &path,
        Some(&json!({"conv": "space:s1", "matter": a})),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let change = body["change"].as_str().unwrap();
    let (status, body) = call(
        &app,
        Some(&owner),
        "POST",
        &format!("/digest/matters/{a}/relate"),
        Some(&json!({"op": "merge", "other": b})),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let before = e
        .store()
        .read(|r| Ok(serde_json::to_value(r.memory_index().get(WS, &mem.id)?)?))
        .unwrap();
    let (status, body) = call(
        &app,
        Some(&owner),
        "POST",
        &format!("/digest/changes/{change}/undo"),
        None,
    )
    .await;
    assert_eq!(status, 409, "{body}");
    assert_eq!(
        e.store()
            .read(|r| Ok(serde_json::to_value(r.memory_index().get(WS, &mem.id)?)?))
            .unwrap(),
        before
    );
    let state: String = e
        .store()
        .read(|r| Ok(r.digest_changes().get(WS, change)?.unwrap().state))
        .unwrap();
    assert_eq!(state, "applied");
}

#[test]
fn intake_consumes_only_new_positions_including_older_backfill() {
    let e = env();
    patch(&e, "s1", json!({"depth": "record"}));
    for n in 0..100 {
        msg(&e, "s1", &format!("history{n}"), NOW, None, None, "history");
    }
    let (_, _, position) = fixture::pending_positions(e.store(), WS).unwrap();
    let position = position.unwrap();
    assert_eq!(window::tick(e.store(), WS, NOW).unwrap().recorded, 100);
    assert!(conv::with_news(e.store(), WS).unwrap().is_empty());
    assert_eq!(fixture::pending_positions(e.store(), WS).unwrap().0, 0);
    // Provider time and id both sort before the consumed messages.
    msg(
        &e,
        "s1",
        "a-late",
        NOW - 10 * window::DAY_MS,
        None,
        None,
        "backfill",
    );
    msg(&e, "s1", "a-same-time", NOW, None, None, "late id");
    let (n, first, _) = fixture::pending_positions(e.store(), WS).unwrap();
    assert_eq!(n, 2);
    assert!(first.unwrap() > position);
    e.store()
        .read(|r| {
            assert_eq!(conv::unread(r, WS, &cv("s1"), 500)?.len(), 2);
            assert_eq!(
                r.digest_windows().pending_stats(WS, "space:s1")?,
                (2, NOW - 10 * window::DAY_MS, NOW)
            );
            Ok(())
        })
        .unwrap();
    assert_eq!(window::tick(e.store(), WS, NOW).unwrap().recorded, 2);
    assert_eq!(window::tick(e.store(), WS, NOW).unwrap().recorded, 0);
    assert_eq!(fixture::receipt_count(e.store(), WS).unwrap(), 102);
}

#[test]
fn inbox_tracks_status_changes_moves_and_deletes_without_replaying_receipts() {
    let e = env();
    msg(&e, "s1", "outbound", NOW, None, None, "draft");
    e.store()
        .write(|c| -> Result<(), steve_store::StoreError> {
            fixture::message_status(c, "outbound", "pending")?;
            assert_eq!(
                e.store()
                    .read(|r| r.digest_windows().pending_stats(WS, "space:s1"))?
                    .0,
                0
            );
            // Same physical source row, newly eligible after an earlier tick saw pending status.
            fixture::message_status(c, "outbound", "sent")?;
            assert_eq!(
                e.store()
                    .read(|r| r.digest_windows().pending_stats(WS, "space:s1"))?
                    .0,
                1
            );
            fixture::move_message(c, "outbound", "s2", NOW - 1)?;
            assert_eq!(
                e.store()
                    .read(|r| r.digest_windows().pending_stats(WS, "space:s1"))?
                    .0,
                0
            );
            assert_eq!(
                e.store()
                    .read(|r| r.digest_windows().pending_stats(WS, "space:s2"))?,
                (1, NOW - 1, NOW - 1)
            );
            Ok::<_, StoreError>(())
        })
        .unwrap();
    assert_eq!(
        window::cut_now(e.store(), WS, &cv("s2"), NOW)
            .unwrap()
            .windows,
        1
    );
    e.store()
        .write(|c| -> Result<(), steve_store::StoreError> {
            fixture::message_status(c, "outbound", "pending")?;
            fixture::message_status(c, "outbound", "sent")?;
            assert_eq!(
                e.store()
                    .read(|r| r.digest_windows().pending_stats(WS, "space:s2"))?
                    .0,
                0
            );
            Ok::<_, StoreError>(())
        })
        .unwrap();
    msg(
        &e,
        "s1",
        "deleted",
        NOW,
        None,
        None,
        "delete before cutting",
    );
    e.store()
        .write(|c| -> Result<(), steve_store::StoreError> {
            fixture::remove_message(c, "deleted")?;
            assert_eq!(
                e.store()
                    .read(|r| r.digest_windows().pending_stats(WS, "space:s1"))?
                    .0,
                0
            );
            Ok::<_, StoreError>(())
        })
        .unwrap();
}

#[test]
fn bounded_initial_lookback_does_not_exclude_later_backfill() {
    let e = env();
    msg(
        &e,
        "s1",
        "excluded-history",
        NOW - 2 * window::DAY_MS,
        None,
        None,
        "outside initial lookback",
    );
    msg(
        &e,
        "s1",
        "recent",
        NOW,
        None,
        None,
        "inside initial lookback",
    );
    let b = queue::Batching {
        backlog_days: 1,
        ..Default::default()
    };
    e.store()
        .write(|w| {
            window::run_conv(
                w,
                WS,
                &cv("s1"),
                NOW,
                &b,
                true,
                &mut window::Report::default(),
            )
        })
        .unwrap();
    msg(
        &e,
        "s1",
        "late-history",
        NOW - 3 * window::DAY_MS,
        None,
        None,
        "inserted after initialization",
    );
    assert_eq!(
        window::cut_now(e.store(), WS, &cv("s1"), NOW)
            .unwrap()
            .windows,
        1
    );
    assert_eq!(
        window::cut_now(e.store(), WS, &cv("s1"), NOW)
            .unwrap()
            .windows,
        0
    );
    let ps = packets(&e);
    assert_eq!(ps.iter().map(|p| p.count).sum::<i64>(), 2);
    assert!(ps.iter().any(|p| p.first_id == "late-history"));
}

#[test]
fn single_user_fulltext_candidates_include_private_memories_but_keep_workspace_and_lifetime() {
    let e = env();
    let ids = e
        .store()
        .transaction(|tx| {
            let mut ids = Vec::new();
            for (ws, level, status, expired) in [
                (WS, steve_store::base::Level::Conclusion, "confirmed", false),
                (
                    WS2,
                    steve_store::base::Level::Conclusion,
                    "confirmed",
                    false,
                ),
                (WS, steve_store::base::Level::Conclusion, "confirmed", true),
                (WS, steve_store::base::Level::Conclusion, "stale", false),
                (WS, steve_store::base::Level::Never, "confirmed", false),
            ] {
                let memory = mems::create(
                    tx,
                    ws,
                    &NewMem {
                        text: "deployment safeguards".into(),
                        created_by: OTHER.into(),
                        ..Default::default()
                    },
                )?;
                tx.write(|w| steve_knowledge::sync::sync_mem(w, &memory.id, NOW))?;
                // Simulate an index that has not yet caught up with source-state changes.
                mems::patch(
                    tx,
                    &memory.id,
                    &mems::MemPatch {
                        level: Some(level),
                        status: Some(status.into()),
                        valid_until: Some(expired.then_some(NOW - 1)),
                        ..Default::default()
                    },
                )?;
                ids.push(memory.id);
            }
            Ok::<_, StoreError>(ids)
        })
        .unwrap();
    msg(
        &e,
        "s1",
        "candidate-window",
        NOW,
        None,
        None,
        "deployment deployment safeguards safeguards",
    );
    e.store()
        .read(|c| {
            let info = conv::info(c, WS, &cv("s1"))?.unwrap();
            let profile = profile::profile(&info, None, || conv::guess_kind(c, WS, &info))?;
            let messages = conv::read_after(c, WS, &cv("s1"), 0, "", 10)?;
            let built = facts::build(c, WS, &info, &profile, 1, "manual", &[], &messages)?;
            let found = candidates::find(c, WS, &cv("s1"), &built, NOW)?;
            let memories: Vec<_> = found
                .iter()
                .filter(|candidate| candidate.kind == "mem")
                .collect();
            assert_eq!(memories.len(), 1, "{found:?}");
            assert_eq!(memories[0].id, ids[0]);
            assert!(memories[0].reasons.contains(&"fulltext"));
            Ok::<_, StoreError>(())
        })
        .unwrap();
}
