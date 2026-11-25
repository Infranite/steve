//! Card 1 acceptance tests: memory use-side delivery, priority/budget assembly, multi-member &
//! `Level::Never` zero-leak boundaries, `items`/`chat` exclusion MRR comparison, and
//! `steve_memory_uses_total{consumer, stage}` observability.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use steve_knowledge::Knowledge;
use steve_knowledge::context::{
    self, KnowledgeContext, L1_ME_MAX_CHARS, SESSION_CONTEXT_MAX_TOKENS, estimate_tokens,
};
use steve_knowledge::port::SqliteStore;
use steve_knowledge::retrieve::{Purpose, Query, retrieve_all};
use steve_modules::contacts::service::{NewAttr, add_attr, start as start_contacts};
use steve_modules::context::PromptContext;
use steve_modules::testkit::{ME, OTHER, TestEnv, WS, WS2, caller};
use steve_store::base::mems::NewMem;
use steve_store::base::{Level, ObjRef, Viewer, Visibility};
use steve_store::knowledge::{ChunkMeta, Chunks, IndexChunk, Memories};
use steve_store::testkit::knowledge as fixture;
use steve_store::{Item, Store, StoreError};

fn env() -> (TestEnv, Knowledge) {
    (TestEnv::new(|_| {}), Knowledge::new(|_| {}))
}

fn ensure_me_party(e: &TestEnv, ws: &str, user: &str) -> String {
    start_contacts(&e.modules, &caller(ws, user)).unwrap().1.id
}

fn add_mem(
    store: &Store,
    ws: &str,
    by: &str,
    kind: &str,
    text: &str,
    vis: Visibility,
    level: Level,
    pinned: bool,
    about: Vec<ObjRef>,
) -> String {
    let now = steve_store::now();
    store
        .write(|w| {
            let m = Memories(w).create(
                ws,
                &NewMem {
                    kind: kind.into(),
                    text: text.into(),
                    source_type: "me".into(),
                    visibility: vis,
                    level,
                    status: "confirmed".into(),
                    writer: "me".into(),
                    created_by: by.into(),
                    pinned,
                    about,
                    ..Default::default()
                },
            )?;
            steve_knowledge::sync::sync_mem(w, &m.id, now)?;
            Ok::<_, StoreError>(m.id)
        })
        .unwrap()
}

fn seed_item(store: &Store, ws: &str, id: &str, title: &str, goal: &str, by: Option<&str>) -> Item {
    let sid = format!("ses_{id}");
    fixture::seed_prompt_item(store, ws, id, &sid, title, goal, by).unwrap();
    store.items().get(id).unwrap().unwrap()
}

#[test]
fn coding_and_research_sessions_reach_100_percent_me_core_coverage() {
    let (e, kb) = env();
    let store = e.store();
    let me = ensure_me_party(&e, WS2, ME);
    let core_entries = [
        (
            "rule",
            "Always run cargo check before committing Rust edits.",
            true,
        ),
        (
            "rule",
            "Never modify user-owned CLAUDE.md or AGENTS.md files.",
            false,
        ),
        (
            "preference",
            "Prefer concise Chinese status reports with exact metrics.",
            false,
        ),
        (
            "fact",
            "Primary engineering workstation runs macOS on Apple Silicon.",
            false,
        ),
    ];
    for (kind, text, pinned) in core_entries {
        add_mem(
            store,
            WS2,
            ME,
            kind,
            text,
            Visibility::Private,
            Level::Conclusion,
            pinned,
            vec![ObjRef::new("party", &me)],
        );
    }

    // Coding session whose title/goal shares zero tokens with `me` rules/profile.
    let coding_item = seed_item(
        store,
        WS2,
        "W2-CODE",
        "Optimize SQLite WAL checkpoint",
        "Reduce checkpoint stalls during bulk ingest",
        Some(ME),
    );
    let coding_cx = context::session_context(store, &kb, &coding_item, steve_store::now()).unwrap();
    let coding_hits = core_entries
        .iter()
        .filter(|(_, text, _)| coding_cx.user_context.contains(*text))
        .count();
    assert_eq!(
        coding_hits,
        core_entries.len(),
        "coding session me coverage must be 100%: {}",
        coding_cx.user_context
    );

    // Research / main-thread context on an unrelated query also receives 100% of `me` entries.
    let viewer = Viewer::bare(WS2, ME);
    let research_cx = store
        .read(|c| {
            context::chat_context(
                c,
                &kb,
                &viewer,
                "Compare WAL checkpoint latency under load",
                steve_store::now(),
            )
        })
        .unwrap()
        .expect("research chat context");
    let research_hits = core_entries
        .iter()
        .filter(|(_, text, _)| research_cx.ctx.text.contains(*text))
        .count();
    assert_eq!(
        research_hits,
        core_entries.len(),
        "research/chat me coverage must be 100%: {}",
        research_cx.ctx.text
    );
}

#[test]
fn main_thread_and_session_preserve_facts_and_more_than_five_preferences_in_priority_order() {
    let (e, kb) = env();
    let store = e.store();
    let me = ensure_me_party(&e, WS2, ME);

    let pinned_fact = "PINNED_FACT: Owner uses strict Conventional Commits.";
    add_mem(
        store,
        WS2,
        ME,
        "fact",
        pinned_fact,
        Visibility::Private,
        Level::Conclusion,
        true,
        vec![ObjRef::new("party", &me)],
    );
    let unpinned_rule = "UNPINNED_RULE: Keep thresholds as named constants.";
    add_mem(
        store,
        WS2,
        ME,
        "rule",
        unpinned_rule,
        Visibility::Private,
        Level::Conclusion,
        false,
        vec![ObjRef::new("party", &me)],
    );
    let mut prefs = Vec::new();
    for i in 1..=8 {
        let text = format!("PREF_{i}: Concise preference number {i}.");
        add_mem(
            store,
            WS2,
            ME,
            "preference",
            &text,
            Visibility::Private,
            Level::Conclusion,
            false,
            vec![ObjRef::new("party", &me)],
        );
        prefs.push(text);
    }
    let unpinned_fact = "OWNER_FACT: Maintains an open-source Rust workspace.";
    add_mem(
        store,
        WS2,
        ME,
        "fact",
        unpinned_fact,
        Visibility::Private,
        Level::Conclusion,
        false,
        vec![ObjRef::new("party", &me)],
    );

    let viewer = Viewer::bare(WS2, ME);
    let chat = store
        .read(|c| context::chat_context(c, &kb, &viewer, "unrelated question", steve_store::now()))
        .unwrap()
        .expect("chat context");

    // Pinned fact ranks ahead of unpinned rule; rule ranks ahead of preferences; preferences ahead
    // of unpinned fact.
    let pos_pinned = chat.ctx.text.find(pinned_fact).expect("pinned fact");
    let pos_rule = chat.ctx.text.find(unpinned_rule).expect("unpinned rule");
    let pos_pref = chat.ctx.text.find(&prefs[0]).expect("preference");
    let pos_fact = chat.ctx.text.find(unpinned_fact).expect("unpinned fact");
    assert!(pos_pinned < pos_rule, "pinned must precede unpinned rule");
    assert!(pos_rule < pos_pref, "rule must precede preference");
    assert!(pos_pref < pos_fact, "preference must precede unpinned fact");

    // All 8 preferences (>5) and both facts are loaded within L1_ME_MAX_CHARS.
    for pref in &prefs {
        assert!(chat.ctx.text.contains(pref), "missing {pref}");
    }
    assert!(chat.ctx.text.contains(unpinned_fact));
    assert!(
        chat.hits
            .iter()
            .filter(|h| h.obj_type == "mem")
            .map(|h| h.text.chars().count())
            .sum::<usize>()
            <= L1_ME_MAX_CHARS
    );
}

#[test]
fn session_fixed_personal_context_stays_within_token_budget() {
    let (e, kb) = env();
    let store = e.store();
    let me = ensure_me_party(&e, WS2, ME);

    let pinned_text = "TOP_PINNED_RULE: Never leave legacy bridges in production code.";
    add_mem(
        store,
        WS2,
        ME,
        "rule",
        pinned_text,
        Visibility::Private,
        Level::Conclusion,
        true,
        vec![ObjRef::new("party", &me)],
    );
    for i in 0..30 {
        let filler = format!(
            "规则编号 {i:02}：{}",
            "每个模块边界必须保留强类型错误与结构化观测指标，禁止魔数。".repeat(4)
        );
        add_mem(
            store,
            WS2,
            ME,
            if i < 10 { "rule" } else { "preference" },
            &filler,
            Visibility::Private,
            Level::Conclusion,
            false,
            vec![ObjRef::new("party", &me)],
        );
    }

    let item = seed_item(
        store,
        WS2,
        "W2-BUDGET",
        "Refactor parser",
        "Clean up parser",
        Some(ME),
    );
    let cx = context::session_context(store, &kb, &item, steve_store::now()).unwrap();
    let tokens = estimate_tokens(&cx.user_context);
    assert!(
        tokens <= SESSION_CONTEXT_MAX_TOKENS,
        "user_context tokens {tokens} exceeded budget {SESSION_CONTEXT_MAX_TOKENS}"
    );
    assert!(
        cx.user_context.contains(pinned_text),
        "pinned rule must survive budget truncation"
    );
    eprintln!(
        "[CARD1_METRIC] session_user_context_tokens={tokens} budget={SESSION_CONTEXT_MAX_TOKENS} chars={}",
        cx.user_context.chars().count()
    );
}

#[test]
fn multi_member_workspace_and_level_never_have_zero_leaks() {
    let (e, kb) = env();
    let store = e.store();

    // Multi-member workspace WS has both ME and OTHER.
    let me_ws = ensure_me_party(&e, WS, ME);
    add_attr(
        &e.modules,
        &caller(WS, ME),
        "me",
        NewAttr {
            key: "call".into(),
            label: None,
            value: "秘密昵称".into(),
            level: Some(Level::Conclusion),
        },
    )
    .unwrap();

    let ws_shared = "WORKSPACE_RULE: Shared coding standard visible to all members.";
    let ws_private = "PRIVATE_SECRET: Owner personal note must not leak in multi-member WS.";
    let ws_never_shared = "NEVER_SHARED: Level::Never workspace secret must never inject.";
    let ws_never_private = "NEVER_PRIVATE: Level::Never private secret must never inject.";

    add_mem(
        store,
        WS,
        ME,
        "rule",
        ws_shared,
        Visibility::Workspace,
        Level::Conclusion,
        false,
        vec![ObjRef::new("party", &me_ws)],
    );
    add_mem(
        store,
        WS,
        ME,
        "rule",
        ws_private,
        Visibility::Private,
        Level::Conclusion,
        true,
        vec![ObjRef::new("party", &me_ws)],
    );
    add_mem(
        store,
        WS,
        ME,
        "rule",
        ws_never_shared,
        Visibility::Workspace,
        Level::Never,
        true,
        vec![ObjRef::new("party", &me_ws)],
    );
    add_mem(
        store,
        WS,
        ME,
        "fact",
        ws_never_private,
        Visibility::Private,
        Level::Never,
        true,
        vec![ObjRef::new("party", &me_ws)],
    );

    let item_me = seed_item(
        store,
        WS,
        "ST-MM1",
        "Build service",
        "Ship service",
        Some(ME),
    );
    let item_other = seed_item(
        store,
        WS,
        "ST-MM2",
        "Build service",
        "Ship service",
        Some(OTHER),
    );
    let item_svc = seed_item(store, WS, "ST-MM3", "Build service", "Ship service", None);

    let mut leaks = 0usize;
    for item in [&item_me, &item_other, &item_svc] {
        let cx = context::session_context(store, &kb, item, steve_store::now()).unwrap();
        for forbidden in [ws_private, ws_never_shared, ws_never_private, "秘密昵称"] {
            if cx.text.contains(forbidden) || cx.user_context.contains(forbidden) {
                leaks += 1;
            }
        }
    }
    // ME's multi-member session STILL receives Workspace-visible rules.
    let cx_me = context::session_context(store, &kb, &item_me, steve_store::now()).unwrap();
    assert!(
        cx_me.user_context.contains(ws_shared),
        "{}",
        cx_me.user_context
    );

    // Multi-member main-thread chat checks.
    for who in [ME, OTHER] {
        let viewer = Viewer::bare(WS, who);
        if let Some(chat) = store
            .read(|c| context::chat_context(c, &kb, &viewer, "Build service", steve_store::now()))
            .unwrap()
        {
            for forbidden in [ws_private, ws_never_shared, ws_never_private, "秘密昵称"] {
                if chat.ctx.text.contains(forbidden) {
                    leaks += 1;
                }
            }
        }
    }

    // Single-member WS2: Level::Never still never leaks even to owner; Private never leaks to service.
    let me_ws2 = ensure_me_party(&e, WS2, ME);
    let ws2_never = "WS2_NEVER_TOKEN: Strict never memory in single-member workspace.";
    let ws2_priv = "WS2_PRIVATE_PREF: Owner private preference in single-member workspace.";
    add_mem(
        store,
        WS2,
        ME,
        "rule",
        ws2_never,
        Visibility::Private,
        Level::Never,
        true,
        vec![ObjRef::new("party", &me_ws2)],
    );
    add_mem(
        store,
        WS2,
        ME,
        "preference",
        ws2_priv,
        Visibility::Private,
        Level::Conclusion,
        false,
        vec![ObjRef::new("party", &me_ws2)],
    );
    let single_me = seed_item(store, WS2, "W2-S1", "Refactor", "Refactor code", Some(ME));
    let single_svc = seed_item(store, WS2, "W2-S2", "Refactor", "Refactor code", None);
    let cx_single_me =
        context::session_context(store, &kb, &single_me, steve_store::now()).unwrap();
    assert!(cx_single_me.user_context.contains(ws2_priv));
    if cx_single_me.text.contains(ws2_never) {
        leaks += 1;
    }
    let cx_single_svc =
        context::session_context(store, &kb, &single_svc, steve_store::now()).unwrap();
    if cx_single_svc.text.contains(ws2_priv) || cx_single_svc.text.contains(ws2_never) {
        leaks += 1;
    }

    assert_eq!(leaks, 0, "multi-member and Level::Never leaks must be 0");
}

#[test]
fn excluding_items_and_chat_improves_memory_retrieval_mrr() {
    let (e, _kb) = env();
    let store = e.store();
    let me = ensure_me_party(&e, WS2, ME);
    let now = steve_store::now();

    let cases: [(&str, &str, &str); 10] = [
        (
            "release checklist",
            "Release checklist requires running cut-release lint before tagging.",
            "release checklist release checklist release checklist ticket discussion",
        ),
        (
            "database backup schedule",
            "Database backup schedule runs nightly at 03:00 local time with 14-day retention.",
            "database backup schedule database backup schedule chat ping",
        ),
        (
            "deploy freeze window",
            "Production deploy freeze window applies every Friday afternoon after 15:00.",
            "deploy freeze window deploy freeze window deploy freeze item log",
        ),
        (
            "mail signature format",
            "External mail signature format uses formal title and omits internal nicknames.",
            "mail signature format mail signature format chat thread",
        ),
        (
            "code review convention",
            "Rust code review convention requires zero clippy warnings and typed error enums.",
            "code review convention code review convention work item note",
        ),
        (
            "oncall escalation path",
            "Critical gateway oncall escalation path pages platform team within 10 minutes.",
            "oncall escalation path oncall escalation path chat alert",
        ),
        (
            "vendor contract renewal",
            "Acme vendor contract renewal happens each March with 20 percent floor discount.",
            "vendor contract renewal vendor contract renewal item update",
        ),
        (
            "travel reimbursement limit",
            "International travel reimbursement limit is economy class under 1200 USD.",
            "travel reimbursement limit travel reimbursement limit chat msg",
        ),
        (
            "staging endpoint policy",
            "Staging endpoint policy forbids automated tests from touching port 8440.",
            "staging endpoint policy staging endpoint policy item status",
        ),
        (
            "documentation language",
            "Public documentation language is concise English markdown.",
            "documentation language documentation language chat reply",
        ),
    ];

    let mut target_ids = Vec::new();
    for (idx, (topic, mem_text, noise_text)) in cases.iter().enumerate() {
        let mid = add_mem(
            store,
            WS2,
            ME,
            "rule",
            mem_text,
            Visibility::Private,
            Level::Conclusion,
            false,
            vec![ObjRef::new("party", &me)],
        );
        target_ids.push(mid);

        // Insert 3 noisy `items` and `chat` chunks per topic with denser keyword repetition and
        // newer timestamps, reproducing pre-exclusion index pollution.
        store
            .write(|w| {
                for n in 0..3 {
                    let kind = if n % 2 == 0 { "items" } else { "chat" };
                    let doc_id = format!("kd_noise_{idx}_{n}");
                    let full_noise = format!("{noise_text} {topic} {topic} #{n}");
                    let terms = steve_knowledge::seg::seg(&full_noise);
                    Chunks(w).append(
                        &ChunkMeta {
                            ws: WS2,
                            kind: "chunk",
                            id: &doc_id,
                            visibility: &Visibility::Private,
                            level: Level::Conclusion,
                            created_by: ME,
                            source_kind: kind,
                            label: "noise",
                            updated: now + 100 + n as i64,
                        },
                        topic,
                        IndexChunk {
                            seq: 0,
                            heading: topic,
                            text: full_noise,
                            start: 0,
                            end: 100,
                            terms,
                        },
                    )?;
                }
                Ok::<_, StoreError>(())
            })
            .unwrap();
    }

    let viewer = Viewer::bare(WS2, ME);
    let eval_mrr = |kinds: Option<&[&str]>| -> f64 {
        let mut rr_sum = 0.0;
        for ((topic, _, _), expected_id) in cases.iter().zip(&target_ids) {
            let found = store
                .read(|c| {
                    let mut q = Query::new(&SqliteStore, &viewer, topic, Purpose::Search);
                    q.k = 10;
                    q.kinds = kinds;
                    q.now = now + 200;
                    retrieve_all(c, &q)
                })
                .unwrap();
            if kinds.is_none() {
                assert!(
                    found
                        .hits
                        .iter()
                        .all(|h| h.source_kind != "items" && h.source_kind != "chat"),
                    "default recall must exclude items/chat"
                );
            }
            if let Some(rank) = found
                .hits
                .iter()
                .position(|h| h.obj_type == "mem" && h.obj_id == *expected_id)
            {
                rr_sum += 1.0 / (rank as f64 + 1.0);
            }
        }
        rr_sum / cases.len() as f64
    };

    let mrr_before = eval_mrr(Some(&["mems", "notes", "items", "chat"]));
    let mrr_after = eval_mrr(None);
    eprintln!(
        "[CARD1_METRIC] memory_mrr_before={mrr_before:.4} memory_mrr_after={mrr_after:.4} delta={:+.4}",
        mrr_after - mrr_before
    );
    assert!(
        mrr_after > mrr_before,
        "excluding items/chat must improve memory MRR: before={mrr_before:.4}, after={mrr_after:.4}"
    );
    assert!(
        mrr_after >= 0.99,
        "clean memory recall MRR expected ~1.0, got {mrr_after:.4}"
    );
}

#[derive(Default)]
struct CounterSink {
    counts: Mutex<HashMap<(String, Vec<(String, String)>), u64>>,
}

struct SinkCounter {
    key: (String, Vec<(String, String)>),
    sink: Arc<CounterSink>,
}

impl metrics::CounterFn for SinkCounter {
    fn increment(&self, value: u64) {
        *self
            .sink
            .counts
            .lock()
            .unwrap()
            .entry(self.key.clone())
            .or_default() += value;
    }
    fn absolute(&self, value: u64) {
        self.sink
            .counts
            .lock()
            .unwrap()
            .insert(self.key.clone(), value);
    }
}

struct TestRecorder(Arc<CounterSink>);

impl metrics::Recorder for TestRecorder {
    fn describe_counter(
        &self,
        _: metrics::KeyName,
        _: Option<metrics::Unit>,
        _: metrics::SharedString,
    ) {
    }
    fn describe_gauge(
        &self,
        _: metrics::KeyName,
        _: Option<metrics::Unit>,
        _: metrics::SharedString,
    ) {
    }
    fn describe_histogram(
        &self,
        _: metrics::KeyName,
        _: Option<metrics::Unit>,
        _: metrics::SharedString,
    ) {
    }
    fn register_counter(&self, key: &metrics::Key, _: &metrics::Metadata<'_>) -> metrics::Counter {
        let mut labels: Vec<(String, String)> = key
            .labels()
            .map(|l| (l.key().to_string(), l.value().to_string()))
            .collect();
        labels.sort();
        metrics::Counter::from_arc(Arc::new(SinkCounter {
            key: (key.name().to_string(), labels),
            sink: self.0.clone(),
        }))
    }
    fn register_gauge(&self, _: &metrics::Key, _: &metrics::Metadata<'_>) -> metrics::Gauge {
        metrics::Gauge::noop()
    }
    fn register_histogram(
        &self,
        _: &metrics::Key,
        _: &metrics::Metadata<'_>,
    ) -> metrics::Histogram {
        metrics::Histogram::noop()
    }
}

#[test]
fn memory_uses_metric_distinguishes_injected_and_cited_stages() {
    let sink = Arc::new(CounterSink::default());
    let recorder = TestRecorder(sink.clone());
    let (e, kb) = env();
    let store = e.store();
    let me = ensure_me_party(&e, WS2, ME);
    add_mem(
        store,
        WS2,
        ME,
        "rule",
        "Always check metrics before shipping.",
        Visibility::Private,
        Level::Conclusion,
        true,
        vec![ObjRef::new("party", &me)],
    );
    add_mem(
        store,
        WS2,
        ME,
        "preference",
        "Prefer concise bullet lists.",
        Visibility::Private,
        Level::Conclusion,
        false,
        vec![ObjRef::new("party", &me)],
    );
    let item = seed_item(
        store,
        WS2,
        "W2-METRIC",
        "Ship feature",
        "Ship feature",
        Some(ME),
    );
    fixture::empty_chat_reply(store).unwrap();
    let provider = KnowledgeContext::new(store.clone(), kb);
    let viewer = Viewer::bare(WS2, ME);

    metrics::with_local_recorder(&recorder, || {
        let session_prompt = provider.session(store, WS2, &item).expect("session prompt");
        assert!(!session_prompt.user_context.is_empty());

        let chat_cx = provider
            .chat(store, WS2, &viewer, "how should we ship?")
            .expect("chat ctx");
        let done = provider.chat_done(
            store,
            WS2,
            "msg_1",
            "We always check metrics [1].",
            &chat_cx,
        );
        assert_eq!(done.meta.unwrap()["cites"].as_array().unwrap().len(), 1);
    });

    let counts = sink.counts.lock().unwrap();
    let get = |consumer: &str, stage: &str| -> u64 {
        let mut labels = vec![
            ("consumer".to_string(), consumer.to_string()),
            ("stage".to_string(), stage.to_string()),
        ];
        labels.sort();
        counts
            .get(&("steve_memory_uses_total".to_string(), labels))
            .copied()
            .unwrap_or(0)
    };
    assert_eq!(get("session", "injected"), 2);
    assert_eq!(get("chat", "injected"), 2);
    assert_eq!(get("chat", "cited"), 1);
}
