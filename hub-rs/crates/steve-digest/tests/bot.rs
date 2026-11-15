//! Tool contracts on an isolated database; no harness, model, network or live installation.
use rusqlite::params;
use serde_json::{Value, json};
use steve_digest::{ConvRef, changes, queue, summary, tools, window};
use steve_modules::contacts::mcp::McpCx;
use steve_modules::testkit::{ME, OTHER, TestEnv, WS};
use steve_store::StoreError;

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
    e.store().transaction(|tx| {
        tx.execute_batch("INSERT INTO spaces (id, workspace_id, system_id, kind, external_id, name, created, updated)
            VALUES ('s1', 'default', 'test', 'group', 'g1', 'Project', 1, 1);
            INSERT INTO parties (id, workspace_id, type, name, created_by, created, updated)
            VALUES ('alice', 'default', 'person', 'Alice', 'usr_dev', 1, 1);")?;
        for n in 1..=count {
            tx.execute("INSERT INTO chan_messages (id, workspace_id, space_id, external_id, direction, sender_party_id, text, mentions, thread_key, created) VALUES (?, 'default', 's1', ?, 'in', 'alice', ?, '[]', 'digest-topic', ?)", params![format!("m{n}"), format!("m{n}"), if large { "deployment details ".repeat(50) } else { format!("Owner commitment {n}: before Friday") }, NOW - 60_000 + n])?;
        }
        tx.execute("INSERT OR REPLACE INTO settings (workspace_id, key, value, updated) VALUES (?, 'digest', ?, ?)", params![WS, json!({"packets": 1, "chunk_tokens": if large {600} else {6000}}).to_string(), NOW])?;
        Ok::<_, StoreError>(())
    }).unwrap();
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
            .reader(|c| summary::latest(c, WS, &ConvRef::new("space", "s1")))
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
            .reader(|c| summary::latest(c, WS, &ConvRef::new("space", "s1")))
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
    let counts: (i64, i64, i64) = e
        .store()
        .reader(|c| {
            Ok::<_, StoreError>((
                c.query_row("SELECT count(*) FROM digest_segments", [], |r| r.get(0))?,
                c.query_row("SELECT count(*) FROM matters", [], |r| r.get(0))?,
                c.query_row("SELECT count(*) FROM digest_changes", [], |r| r.get(0))?,
            ))
        })
        .unwrap();
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
        .reader(|c| queue::get(c, WS, l["packet"].as_str().unwrap()))
        .unwrap()
        .unwrap();
    assert_eq!(p.attempts, 0);
    assert_eq!(p.state, "queued");
    assert_eq!(
        e.store().reader(|c| queue::chunks(c, &p.id)).unwrap()[0].state,
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
    e.store()
        .transaction(|tx| {
            tx.execute(
                "UPDATE digest_packets SET facts = json_set(facts, '$.conv.depth', 'summary')",
                [],
            )?;
            Ok::<_, StoreError>(())
        })
        .unwrap();
    let l = lease(&e);
    assert!(call(&e, tools::CHUNK, chunk(&e, &l)).is_err());
}

#[test]
fn todo_deadline_uses_source_quote_and_memory_remains_a_confirmable_candidate() {
    let e = env();
    let l = lease(&e);
    let mut write = chunk(&e, &l);
    write["todos"] = json!([{"text": "Follow up", "positions": [1], "due": NOW + 86_400_000, "due_quote": "before Friday", "matter_at": 1}]);
    write["memories"] = json!([{"first": 1, "last": 1, "text": "Commitment to follow up before Friday", "kind": "event", "matter_at": 1}]);
    let out = call(&e, tools::CHUNK, write).unwrap();
    let id = out["memories"][0]["id"].as_str().unwrap();
    let (status, source, quote): (String, String, String) = e
        .store()
        .reader(|c| {
            Ok::<_, StoreError>((
                c.query_row("SELECT status FROM mems WHERE id = ?", [id], |r| r.get(0))?,
                c.query_row("SELECT source FROM todos", [], |r| r.get(0))?,
                c.query_row("SELECT due_quote FROM todos", [], |r| r.get(0))?,
            ))
        })
        .unwrap();
    assert_eq!(
        (status.as_str(), source.as_str(), quote.as_str()),
        ("candidate", "steve", "before Friday")
    );
    let indexed: i64 = e
        .store()
        .reader(|c| {
            Ok::<_, StoreError>(c.query_row(
                "SELECT count(*) FROM mem_ranges WHERE mem_id = ?",
                [id],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(indexed, 1);
    let report = call(&e, tools::REPORT, json!({})).unwrap();
    for op in ["segment", "todo", "mem_new", "mem_link"] {
        assert!(
            report["changes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v["op"] == op)
        );
    }
}

#[test]
fn finish_retains_deterministic_alert_rollup_and_user_classification() {
    let e = env();
    let l = lease(&e);
    let cv = ConvRef::new("space", "s1");
    e.store()
        .transaction(|tx| {
            summary::write(
                tx,
                WS,
                &cv,
                None,
                Some(&json!({"firing": 7, "recovered": 6})),
                None,
                "steve",
                NOW,
            )?;
            steve_digest::profile::apply(
                tx,
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
        .reader(|c| summary::latest(c, WS, &cv))
        .unwrap()
        .unwrap();
    assert_eq!(saved.version, 2);
    assert_eq!(saved.rules, json!({"firing": 7, "recovered": 6}));
    let profile = e
        .store()
        .reader(|c| steve_digest::profile::stored(c, WS, &cv))
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
    let count: i64 = e
        .store()
        .reader(|c| {
            Ok::<_, StoreError>(c.query_row("SELECT count(*) FROM matters", [], |r| r.get(0))?)
        })
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn memory_links_and_replacements_recheck_visibility_and_use_existing_confirmation() {
    use steve_store::base::mems::{self, NewMem};
    let e = env();
    let seed = |owner: &str| {
        e.store()
            .transaction(|tx| {
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
    e.store()
        .transaction(|tx| {
            tx.execute(
                "UPDATE digest_packets SET candidates = ? WHERE id = ?",
                params![
                    json!([{"type":"mem", "id":private}]).to_string(),
                    l["packet"].as_str().unwrap()
                ],
            )?;
            Ok::<_, StoreError>(())
        })
        .unwrap();
    let read = call(
        &e,
        tools::READ,
        json!({"packet":l["packet"],"batch":l["batch"],"chunk":1}),
    )
    .unwrap();
    assert!(read["candidates"].as_array().unwrap().is_empty());
    let mut write = chunk(&e, &l);
    write["memories"] = json!([{"id": private, "first": 1, "last": 2}]);
    assert!(call(&e, tools::CHUNK, write.clone()).is_err());
    write["memories"] = json!([{"id":visible, "first":1,"last":2}, {"text":"The source fact changed", "supersedes":visible, "first":2,"last":3}]);
    let out = call(&e, tools::CHUNK, write).unwrap();
    assert_eq!(out["memories"][0]["id"], visible);
    assert_eq!(out["memories"][0]["indexed"], true);
    let id = out["memories"][1]["id"].as_str().unwrap();
    let (old, new, replacement): (String, String, String) = e
        .store()
        .reader(|c| {
            Ok::<_, StoreError>((
                c.query_row("SELECT status FROM mems WHERE id = ?", [&visible], |r| {
                    r.get(0)
                })?,
                c.query_row("SELECT status FROM mems WHERE id = ?", [id], |r| r.get(0))?,
                c.query_row(
                    "SELECT supersedes FROM mem_candidates WHERE mem_id = ?",
                    [id],
                    |r| r.get(0),
                )?,
            ))
        })
        .unwrap();
    assert_eq!(old, "confirmed");
    assert_eq!(new, "candidate");
    assert_eq!(replacement, visible);
}

#[test]
fn one_run_batches_conversations_and_confirms_the_same_cross_conversation_matter() {
    let e = env();
    e.store().transaction(|tx| {
        tx.execute_batch("INSERT INTO spaces (id, workspace_id, system_id, kind, external_id, name, created, updated) VALUES ('s2','default','test','group','g2','Other team',1,1);
            INSERT INTO chan_messages (id, workspace_id, space_id, external_id, direction, sender_party_id, text, created) VALUES ('m4','default','s2','m4','in','alice','Same follow up',1799999990000);")?;
        Ok::<_, StoreError>(())
    }).unwrap();
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
    let conversations: i64 = e
        .store()
        .reader(|c| {
            Ok::<_, StoreError>(c.query_row(
                "SELECT count(DISTINCT conv) FROM digest_segments WHERE matter_id = ?",
                [matter],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(conversations, 2);
}

#[test]
fn visibility_filter_does_not_spend_an_attempt_or_lease_hidden_packets() {
    let e = env();
    let ids = e
        .store()
        .transaction(|tx| {
            queue::lease_matching(tx, WS, NOW, &queue::Batching::default(), 5, |_| Ok(false))
        })
        .unwrap()
        .1;
    assert!(ids.is_empty());
    let p = e
        .store()
        .reader(|c| queue::list(c, WS, None, None, 5))
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
    let ids = e.store().transaction(|tx| {
        let original = queue::list(tx, WS, None, None, 1)?.remove(0);
        for n in 0..501 {
            tx.execute("INSERT INTO digest_packets(id,workspace_id,conv,seq,kind,trigger,first_id,last_id,first_at,last_at,count,tokens,priority,facts,candidates,created,updated) SELECT ?,workspace_id,?,seq,kind,trigger,first_id,last_id,first_at,last_at,count,tokens,priority+1,facts,candidates,created,updated FROM digest_packets WHERE id=?", rusqlite::params![format!("hidden-{n}"), format!("space:hidden-{n}"), original.id])?;
        }
        queue::lease_matching(tx, WS, NOW, &queue::Batching::default(), 1, |p| Ok(p.id == original.id))
    }).unwrap().1;
    assert_eq!(ids.len(), 1);
    assert!(!ids[0].starts_with("hidden-"));
}
