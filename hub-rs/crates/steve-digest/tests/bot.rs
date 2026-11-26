//! Tool contracts on an isolated database; no harness, model, network or live installation.
#[path = "support/digest_env.rs"]
mod digest_env;
use digest_env::TestEnv;
use serde_json::{Value, json};
use steve_digest::{ConvRef, changes, queue, tools, window};
use steve_modules::contacts::mcp::McpCx;
use steve_modules::testkit::{ME, OTHER, WS};
use steve_store::StoreError;
use steve_store::digest::{Segments, TodoFilter, Todos};
use steve_store::testkit::digest as fixture;

const NOW: i64 = 1_800_000_000_000;
fn cx() -> McpCx {
    McpCx {
        ws: WS.into(),
        user: ME.into(),
        bot_id: "steve".into(),
        item_id: None,
        session_id: Some("digest-test".into()),
    }
}
fn env() -> TestEnv {
    build(3, false)
}
fn build(count: i64, large: bool) -> TestEnv {
    let e = TestEnv::new(|_| {});
    e.clock.set(NOW);
    e.store()
        .write(|w| {
            fixture::bot_space(w)?;
            for n in 1..=count {
                let text = if large {
                    "deployment details ".repeat(50)
                } else {
                    format!("Owner commitment {n}: before Friday")
                };
                fixture::seed_message(
                    w,
                    fixture::ChannelInput {
                        id: &format!("m{n}"),
                        space: "s1",
                        at: NOW - 60_000 + n,
                        reply: None,
                        thread: Some("digest-topic"),
                        text: &text,
                        mentions: &json!([]),
                    },
                )?;
            }
            fixture::set_batching(
                w,
                WS,
                &json!({"packets": 1, "chunk_tokens": if large {600} else {6000}}),
                NOW,
            )
        })
        .unwrap();
    window::cut_now(e.store(), WS, &ConvRef::new("space", "s1"), NOW).unwrap();
    e
}
fn call(e: &TestEnv, name: &str, args: Value) -> Result<Value, String> {
    tools::call_sync(&e.modules, &cx(), name, &args)
        .map_err(|e| e.to_string())
        .map(|s| serde_json::from_str(&s).unwrap())
}
fn lease(e: &TestEnv) -> Value {
    call(e, tools::BATCH, json!({})).unwrap()["packets"][0].clone()
}
fn chunk(e: &TestEnv, l: &Value) -> Value {
    let p = call(
        e,
        tools::READ,
        json!({"packet": l["packet"], "batch": l["batch"], "chunk": 1}),
    )
    .unwrap();
    json!({"packet": l["packet"], "batch": l["batch"], "chunk": p["chunks"][0]["n"], "segments": [{"first": 1, "last": 3, "summary": "Owner discussed commitments", "tags": ["promise"], "new_matter": "Follow up"}]})
}
#[test]
fn batch_resumes_and_read_returns_deterministic_chunk_text() {
    let e = env();
    let l = lease(&e);
    assert!(l["packet"].as_str().unwrap().starts_with("dpk"));
    let r = call(&e, tools::BATCH, json!({})).unwrap();
    assert_eq!(r["resumed"], true);
    assert_eq!(r["packets"][0]["packet"], l["packet"]);
    let r = call(
        &e,
        tools::READ,
        json!({"packet": l["packet"], "batch": l["batch"], "chunk": 1}),
    )
    .unwrap();
    assert!(r["chunks"][0]["text"].as_str().unwrap().contains("[1]"));
    assert_eq!(r["facts"]["messages"][0]["id"], "m1");
}
#[test]
fn chunk_and_finish_are_atomic_and_report_is_undoable() {
    let e = env();
    let l = lease(&e);
    let premature = json!({"packet": l["packet"], "batch": l["batch"], "summary": "too soon", "expected_version": 0});
    assert!(call(&e, tools::FINISH, premature).is_err());
    assert!(
        e.store()
            .read(|r| r.summaries().latest(WS, "space:s1"))
            .unwrap()
            .is_none()
    );
    let write = chunk(&e, &l);
    let saved = call(&e, tools::CHUNK, write.clone()).unwrap();
    assert_eq!(saved["done"], true);
    assert_eq!(call(&e, tools::CHUNK, write).unwrap()["already_done"], true);
    assert!(call(&e, tools::FINISH, json!({"packet": l["packet"], "batch": l["batch"], "summary": "All three commitments", "expected_version": 1})).is_err());
    call(&e, tools::FINISH, json!({"packet": l["packet"], "batch": l["batch"], "summary": "All three commitments", "expected_version": 0})).unwrap();
    let report = call(&e, tools::REPORT, json!({})).unwrap();
    assert_eq!(report["changes"].as_array().unwrap().len(), 2);
    let id = report["changes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["op"] == "summary")
        .unwrap()["id"]
        .as_str()
        .unwrap();
    changes::undo(&e.modules, WS, ME, id).unwrap();
    assert!(
        e.store()
            .read(|r| r.summaries().latest(WS, "space:s1"))
            .unwrap()
            .is_none()
    );
}
#[test]
fn invalid_late_write_rolls_back_segments_and_new_matters() {
    let e = env();
    let l = lease(&e);
    let mut write = chunk(&e, &l);
    write["todos"] = json!([{"text": "bad deadline", "positions": [1], "due": NOW + 1000, "due_quote": "invented words"}]);
    assert!(call(&e, tools::CHUNK, write).is_err());
    let counts = fixture::save_counts(e.store()).unwrap();
    assert_eq!(counts, (0, 0, 0));
}
#[test]
fn stale_and_other_run_cannot_read_or_write() {
    let e = env();
    let l = lease(&e);
    let args = json!({"packet": l["packet"], "batch": l["batch"]});
    let mut other = cx();
    other.session_id = Some("different-run".into());
    assert!(tools::call_sync(&e.modules, &other, tools::READ, &args).is_err());
    e.clock.set(NOW + 121 * 60_000);
    assert!(call(&e, tools::READ, args).is_err());
    assert!(call(&e, tools::CHUNK, chunk_before_expiry(&l)).is_err());
}
fn chunk_before_expiry(l: &Value) -> Value {
    json!({"packet": l["packet"], "batch": l["batch"], "chunk": 1, "segments": []})
}
#[test]
fn retry_retains_chunks_yield_does_not_exhaust_attempt_budget() {
    let e = env();
    let l = lease(&e);
    call(&e, tools::CHUNK, chunk(&e, &l)).unwrap();
    call(
        &e,
        tools::CONTROL,
        json!({"packet": l["packet"], "batch": l["batch"], "action": "yield"}),
    )
    .unwrap();
    let p = e
        .store()
        .read(|r| r.digest_packets().get(WS, l["packet"].as_str().unwrap()))
        .unwrap()
        .unwrap();
    assert_eq!(p.attempts, 0);
    assert_eq!(p.state, "queued");
    assert_eq!(
        e.store()
            .read(|r| r.digest_packets().chunks(&p.id))
            .unwrap()[0]
            .state,
        "done"
    );
    let next = lease(&e);
    assert_ne!(next["batch"], l["batch"]);
    call(&e, tools::FINISH, json!({"packet": next["packet"], "batch": next["batch"], "summary": "Resumed window", "expected_version": 0})).unwrap();
}
#[test]
fn service_and_nonowner_are_not_implicitly_the_owner() {
    let e = env();
    for user in [OTHER, "svc:default"] {
        let mut c = cx();
        c.user = user.into();
        assert!(tools::call_sync(&e.modules, &c, tools::BATCH, &json!({})).is_err());
    }
}
#[test]
fn positions_must_be_real_and_all_chunk_positions_must_be_accounted_for() {
    let e = env();
    let l = lease(&e);
    let mut write = chunk(&e, &l);
    write["segments"][0]["last"] = json!(4);
    assert!(call(&e, tools::CHUNK, write).is_err());
    assert!(call(&e, tools::CHUNK, chunk_before_expiry(&l)).is_err());
}
#[test]
fn associations_cannot_be_added_to_summary_only_packet() {
    let e = env();
    fixture::summary_depth(e.store()).unwrap();
    let l = lease(&e);
    assert!(call(&e, tools::CHUNK, chunk(&e, &l)).is_err());
}

#[test]
fn todo_deadline_uses_source_quote_and_memory_ingests_silently_via_unified_pipeline() {
    let e = env();
    let l = lease(&e);
    let mut write = chunk(&e, &l);
    write["todos"] = json!([{"text": "Follow up", "positions": [1], "due": NOW + 86_400_000, "due_quote": "before Friday", "matter_at": 1}]);
    write["memories"] = json!([{"first": 1, "last": 1, "text": "Commitment to follow up before Friday", "kind": "event", "matter_at": 1}]);
    let out = call(&e, tools::CHUNK, write).unwrap();
    let id = out["memories"][0]["id"].as_str().unwrap();
    let status = out["memories"][0]["status"].as_str().unwrap();
    assert!(
        id.starts_with("obs_"),
        "first implicit episode buffers: {out}"
    );
    assert_eq!(status, "buffered");
    let (source, quote, open_confirm_cards) = e
        .store()
        .read::<_, steve_store::StoreError>(|r| {
            let todo = Todos(r).list(WS, ME, &TodoFilter::default())?.remove(0);
            let open = steve_store::DecisionRows(r).open_confirmations(WS, "mem")?;
            Ok((todo.source, todo.fields.due_quote.unwrap(), open))
        })
        .unwrap();
    assert_eq!(
        (source.as_str(), quote.as_str()),
        ("steve", "before Friday")
    );
    assert_eq!(open_confirm_cards, 0, "zero Inbox confirmation cards");
    let report = call(&e, tools::REPORT, json!({})).unwrap();
    for op in ["segment", "todo", "mem_new"] {
        assert!(
            report["changes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v["op"] == op),
            "missing op {op} in {report}"
        );
    }
}

#[test]
fn finish_retains_deterministic_alert_rollup_and_user_classification() {
    let e = env();
    let l = lease(&e);
    let cv = ConvRef::new("space", "s1");
    e.store()
        .write(|w| {
            w.summaries().append(
                WS,
                &cv.key(),
                steve_store::digest::NewSummary {
                    text: None,
                    rules: Some(&json!({"firing": 7, "recovered": 6})),
                    packet: None,
                    writer: "steve",
                    now: NOW,
                },
            )?;
            steve_digest::profile::apply(
                w,
                WS,
                &cv,
                &serde_json::from_value(json!({"kind": "project", "kind_source": "you"})).unwrap(),
                NOW,
            )
            .unwrap();
            Ok::<_, StoreError>(())
        })
        .unwrap();
    call(&e, tools::CHUNK, chunk(&e, &l)).unwrap();
    call(&e, tools::FINISH, json!({"packet": l["packet"], "batch": l["batch"], "expected_version": 1, "summary": "One unresolved incident", "kind": "feed"})).unwrap();
    let saved = e
        .store()
        .read(|r| r.summaries().latest(WS, &cv.key()))
        .unwrap()
        .unwrap();
    assert_eq!(saved.version, 2);
    assert_eq!(saved.rules, json!({"firing": 7, "recovered": 6}));
    let profile = e
        .store()
        .read(|r| r.digest_profiles().get(WS, &cv.key()))
        .unwrap()
        .unwrap();
    assert_eq!(profile.kind.as_deref(), Some("project"));
}

#[test]
fn large_window_resumes_in_another_session_and_chain_reuses_module_matter_id() {
    let e = build(20, true);
    let l = lease(&e);
    let meta = call(
        &e,
        tools::READ,
        json!({"packet": l["packet"], "batch": l["batch"]}),
    )
    .unwrap();
    let chunks = meta["chunks"].as_array().unwrap();
    assert!(chunks.len() > 1);
    assert!(meta["facts"]["messages"].as_array().unwrap().is_empty());
    let ch = &chunks[0];
    let write = |l: &Value, ch: &Value| json!({"packet": l["packet"], "batch": l["batch"], "chunk": ch["n"], "segments": [{"first": ch["first_pos"], "last": ch["last_pos"], "summary": "Deployment discussion", "tags": ["fact"], "new_matter": "Deployment"}]});
    let first = call(&e, tools::CHUNK, write(&l, ch)).unwrap();
    let matter = first["segments"][0]["matter"].clone();
    assert!(matter.as_str().unwrap().starts_with("mtr"));
    call(
        &e,
        tools::CONTROL,
        json!({"packet": l["packet"], "batch": l["batch"], "action": "yield"}),
    )
    .unwrap();
    let mut next = cx();
    next.session_id = Some("digest-next-session".into());
    let run = |name: &str, args: Value| -> Value {
        serde_json::from_str(&tools::call_sync(&e.modules, &next, name, &args).unwrap()).unwrap()
    };
    let l = run(tools::BATCH, json!({}))["packets"][0].clone();
    let progress = run(
        tools::READ,
        json!({"packet": l["packet"], "batch": l["batch"], "chunk": chunks[1]["n"]}),
    );
    assert_eq!(progress["chunks"][0]["state"], "done");
    assert!(progress["chunks"][0].get("text").is_none());
    for ch in chunks.iter().skip(1) {
        let mut overlap = write(&l, ch);
        overlap["segments"][0]["first"] = json!(ch["first_pos"].as_i64().unwrap() - 1);
        assert!(tools::call_sync(&e.modules, &next, tools::CHUNK, &overlap).is_err());
        let out = run(tools::CHUNK, write(&l, ch));
        assert_eq!(out["segments"][0]["matter"], matter);
        assert_eq!(out["segments"][0]["opened"], false);
    }
    run(
        tools::FINISH,
        json!({"packet": l["packet"], "batch": l["batch"], "expected_version": 0, "summary": "Combined deployment discussion"}),
    );
    let count = fixture::save_counts(e.store()).unwrap().1;
    assert_eq!(count, 1);
}

#[test]
fn single_user_memory_links_keep_range_validation_and_existing_confirmation() {
    use steve_store::base::mems::{self, NewMem};
    let e = env();
    let seed = |owner: &str| {
        e.store()
            .write(|tx| {
                mems::create(
                    tx,
                    WS,
                    &NewMem {
                        text: "A confirmed source fact".into(),
                        created_by: owner.into(),
                        status: "confirmed".into(),
                        ..Default::default()
                    },
                )
            })
            .unwrap()
            .id
    };
    let visible = seed(ME);
    let private = seed(OTHER);
    let l = lease(&e);
    fixture::set_packet_candidates(
        e.store(),
        l["packet"].as_str().unwrap(),
        &json!([{"type":"mem","id":private}]),
    )
    .unwrap();
    let read = call(
        &e,
        tools::READ,
        json!({"packet":l["packet"],"batch":l["batch"],"chunk":1}),
    )
    .unwrap();
    assert!(
        read["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .any(|candidate| candidate["id"] == private)
    );
    let mut write = chunk(&e, &l);
    write["memories"] = json!([{"id":visible, "first":1,"last":2}, {"text":"The source fact changed", "supersedes":visible, "first":2,"last":3}, {"id":private, "first":1,"last":3}]);
    let out = call(&e, tools::CHUNK, write).unwrap();
    assert_eq!(out["memories"][0]["id"], visible);
    assert_eq!(out["memories"][0]["indexed"], true);
    assert_eq!(out["memories"][2]["id"], private);
    assert_eq!(out["memories"][2]["indexed"], true);
    assert!(
        e.store()
            .read::<_, steve_store::StoreError>(|r| Ok(r
                .memory_index()
                .get(WS, &private)?
                .about
                .contains(&steve_store::base::ObjRef::new("space", "s1"))))
            .unwrap()
    );
    let id = out["memories"][1]["id"].as_str().unwrap();
    let status = out["memories"][1]["status"].as_str().unwrap();
    assert!(
        id.starts_with("obs_"),
        "new digest memory routes via unified ingest: {out}"
    );
    assert_eq!(status, "buffered");
    let viewer = steve_store::base::Viewer::bare(WS, ME);
    let (old, open_cards) = e
        .store()
        .read::<_, steve_store::StoreError>(|r| {
            let mut index = r.memory_index();
            Ok((
                index.visible_status(&viewer, &visible)?.unwrap(),
                steve_store::DecisionRows(r).open_confirmations(WS, "mem")?,
            ))
        })
        .unwrap();
    assert_eq!(old, "confirmed");
    assert_eq!(open_cards, 0);
}

#[test]
fn one_run_batches_conversations_and_confirms_the_same_cross_conversation_matter() {
    let e = env();
    fixture::team_followup(e.store(), NOW - 10_000).unwrap();
    window::cut_now(e.store(), WS, &ConvRef::new("space", "s2"), NOW).unwrap();
    let leased = call(&e, tools::BATCH, json!({})).unwrap();
    let packets = leased["packets"].as_array().unwrap();
    assert_eq!(packets.len(), 2);
    let first = packets.iter().find(|p| p["conv"] == "space:s1").unwrap();
    let saved = call(&e, tools::CHUNK, chunk(&e, first)).unwrap();
    let matter = saved["segments"][0]["matter"].as_str().unwrap();
    let read = call(&e, tools::MATTER, json!({"id": matter})).unwrap();
    assert_eq!(read["id"], matter);
    let second = packets.iter().find(|p| p["conv"] == "space:s2").unwrap();
    call(&e, tools::CHUNK, json!({"packet": second["packet"], "batch": second["batch"], "chunk": 1, "segments": [{"first":1,"last":1,"summary":"Other team confirmed follow up","tags":["fact"],"matter":matter}]})).unwrap();
    let conversations = e
        .store()
        .read::<_, steve_store::StoreError>(|r| Ok(Segments::new(r).conversations(matter)?.len()))
        .unwrap();
    assert_eq!(conversations, 2);
}

#[test]
fn visibility_filter_does_not_spend_an_attempt_or_lease_hidden_packets() {
    let e = env();
    let ids = e
        .store()
        .write(|tx| {
            tx.scope(|w| {
                w.digest_packets().lease_matching(
                    WS,
                    NOW,
                    &queue::Batching::default(),
                    5,
                    |_, _| Ok(false),
                )
            })
        })
        .unwrap()
        .1;
    assert!(ids.is_empty());
    let p = e
        .store()
        .read(|r| r.digest_packets().list(WS, None, None, 5))
        .unwrap()
        .remove(0);
    assert_eq!(p.state, "queued");
    assert_eq!(p.attempts, 0);
    assert_eq!(
        call(&e, tools::BATCH, json!({})).unwrap()["packets"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn private_sources_do_not_starve_readable_packets_after_the_first_five_hundred() {
    let e = env();
    let ids = e
        .store()
        .write(|w| {
            let original = w.digest_packets().list(WS, None, None, 1)?.remove(0);
            fixture::duplicate_hidden_packets(w, &original.id, 501)?;
            w.digest_packets()
                .lease_matching(WS, NOW, &queue::Batching::default(), 1, |_, p| {
                    Ok(p.id == original.id)
                })
        })
        .unwrap()
        .1;
    assert_eq!(ids.len(), 1);
    assert!(!ids[0].starts_with("hidden-"));
}
