//! K3 acceptance (updated for Memory Card 2 file-as-truth silent writes):
//! - Item completion extraction yields at most `MAX_PER_ORIGIN` filtered memories via unified
//!   ingestion, opening ZERO Inbox confirmation cards (`decisions.subject_type = 'mem'`).
//! - `steve_remember` writes silently to `$STEVE_HOME/context/` with `ConfidenceTier::Verified`
//!   and ZERO Inbox confirmation cards.
//! - Main-thread chat days stay searchable under `scope=records`, extract once per day, make 0
//!   model calls on empty days, and emit zero 03:40 `kept_notice` interruptions.
//! - Shared-workspace private nickname isolation holds.

use axum::Router;
use serde_json::{Value, json};
use steve_knowledge::Knowledge;
use steve_knowledge::candidates::{self, MAX_PER_ORIGIN, MemDecisions};
use steve_knowledge::extract::PROMPT_HEAD;
use steve_knowledge::ingest::IngestOutcome;
use steve_knowledge::retrieve::{Purpose, Query, retrieve};
use steve_knowledge::sources::folder::FolderPolicy;
use steve_knowledge::sources::{chat, items};
use steve_modules::contacts::mcp::McpCx;
use steve_modules::testkit::{ME, OTHER, TestEnv, WS, call, caller};
use steve_store::base::{Level, Viewer};
use steve_store::knowledge::{ConfidenceTier, MemoryEntries};
use steve_store::testkit::knowledge as fixture;

#[test]
fn private_address_is_not_injected_into_shared_workspace_chat() {
    let h = setup();
    h.env
        .store()
        .write(|tx| -> steve_modules::ApiResult<()> {
            let who = caller(WS, ME);
            let me = steve_modules::contacts::service::ensure_base(tx, &who)?;
            steve_store::Attributes(tx).insert_value(
                WS,
                &steve_modules::contacts::repo::NewValue {
                    party_id: me.id,
                    key: "call".into(),
                    label: "Internal name".into(),
                    value: "Private nickname".into(),
                    source: "me".into(),
                    source_label: "Manual".into(),
                    source_ref: None,
                    level: Level::Public,
                    status: "confirmed".into(),
                    decision_id: None,
                },
            )?;
            Ok(())
        })
        .unwrap();
    let context = || {
        h.env
            .store()
            .read(|c| {
                let viewer = Viewer::bare(WS, ME);
                Ok::<_, steve_store::StoreError>(
                    steve_knowledge::chat_context(
                        c,
                        &h.kb,
                        &viewer,
                        "unrelated question",
                        steve_store::now(),
                    )
                    .unwrap(),
                )
            })
            .unwrap()
    };
    assert!(!context().is_some_and(|k| k.ctx.text.contains("Private nickname")));
    fixture::only_member(h.env.store(), WS, ME).unwrap();
    assert!(context().is_some_and(|k| k.ctx.text.contains("Private nickname")));
}

struct Env {
    env: TestEnv,
    kb: Knowledge,
    app: Router,
}

fn setup() -> Env {
    let env = TestEnv::new(|r| {
        r.decision(candidates::SUBJECT, Box::new(MemDecisions));
    });
    let kb = Knowledge::new(|_| {});
    kb.set_folder_policy(FolderPolicy::new(
        env.dir.path(),
        env.dir.path().join("steve_home"),
    ));
    let app = steve_knowledge::routes(kb.clone()).with_state(env.modules.clone());
    fixture::seed_delivery(env.store()).expect("seed");
    Env { env, kb, app }
}

impl Env {
    async fn me(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        call(
            &self.app,
            Some(&caller(WS, ME)),
            method,
            path,
            body.as_ref(),
        )
        .await
    }
    fn open_confirms(&self) -> i64 {
        self.env
            .store()
            .read(|r| steve_store::DecisionRows(r).open_confirmations(WS, "mem"))
            .unwrap()
    }
}

fn item_json() -> String {
    let items = vec![
        json!({"changes_future_behavior":true,"kind":"preference","scope":"person","text":"Sarah 喜欢简短的邮件，不超过五行","about":["Sarah Chen"],"level":"conclusion","evidence":"Sarah 一直喜欢简短的邮件，别超过五行","basis":"explicit","dup_of":null,"conflicts":null,"why":"以后给 Sarah 发信保持简洁","when":"给 Sarah 写邮件时"}),
        json!({"changes_future_behavior":true,"kind":"rule","scope":"organization","text":"给 Acme 的邮件一律用英文","about":["Acme"],"level":"public","evidence":["Acme 的邮件一律用英文"],"basis":"explicit","dup_of":null,"conflicts":null,"why":"以后给 Acme 写邮件统一英文","when":"给 Acme 写邮件时"}),
        json!({"changes_future_behavior":true,"kind":"fact","scope":"organization","text":"重复的一条","about":["Acme"],"evidence":"五行以内","dup_of":"mm_whatever","when":"x","why":"y"}),
        json!({"changes_future_behavior":false,"kind":"preference","scope":"person","text":"用户希望续签邮件抄送 Sarah","about":[],"evidence":"给 Acme 写续签邮件，抄送 Sarah","basis":"explicit","when":"本次","why":"一次性"}),
        json!({"changes_future_behavior":true,"kind":"fact","scope":"organization","text":"没有原话的一条","about":["Acme"],"when":"x","why":"y"}),
        json!({"changes_future_behavior":true,"kind":"fact","scope":"organization","text":"没有关于对象的孤立事实","about":[],"evidence":"五行以内","when":"x","why":"y"}),
        json!({"changes_future_behavior":true,"kind":"fact","scope":"person","text":"王五负责 Acme 的采购","about":["王五"],"evidence":"五行以内","when":"x","why":"y"}),
        json!({"changes_future_behavior":true,"kind":"fact","scope":"person","text":"关于 Sarah 的长期合作窗口","about":["Sarah","王五"],"level":"reasoning","evidence":"Sarah 一直喜欢简短的邮件","when":"项目合作","why":"影响后续联系对象"}),
        json!({"changes_future_behavior":true,"kind":"fact","scope":"organization","text":"额外的第 0 条","about":["Acme"],"evidence":"五行以内","when":"合作","why":"测试上限"}),
    ];
    format!("```json\n{}\n```", json!({ "items": items }))
}

#[tokio::test]
async fn item_completion_yields_at_most_three_filtered_memories_with_zero_confirm_cards() {
    let e = setup();
    e.env.gateway.script(PROMPT_HEAD, item_json());
    let got = items::finished(&e.env.modules, &e.kb, "ST-12")
        .await
        .unwrap();

    assert_eq!(got.len(), MAX_PER_ORIGIN, "{got:#?}");
    // Explicit owner rules/preferences wrote silently; implicit fact buffered.
    assert!(matches!(got[0], IngestOutcome::Written { .. }));
    assert!(matches!(got[1], IngestOutcome::Written { .. }));
    assert!(matches!(got[2], IngestOutcome::Buffered { .. }));
    // ZERO Inbox confirmation cards opened!
    assert_eq!(e.open_confirms(), 0);
    // Disclosure ceiling enforced: even `public` is raised to `reasoning`.
    if let IngestOutcome::Written { ref entry, .. } = got[1] {
        assert_eq!(entry.level, Level::Reasoning);
    }

    // Prompt carries owner sections and tags `[AGENT_SUMMARY]` separately.
    let p = e.env.gateway.last_prompt().unwrap();
    for s in [
        "ST-12 交付时",
        "Goal v2: 给 Acme 写续签邮件，抄送 Sarah",
        "[OWNER_SUPPLEMENT]",
        "Sarah 一直喜欢简短的邮件",
        "[OWNER_ANSWER]",
        "Acme 的邮件一律用英文",
        "[AGENT_SUMMARY]",
        "- Sarah Chen (contact)",
        "- Acme (org)",
    ] {
        assert!(p.contains(s), "missing {s} in\n{p}");
    }

    // Items are NOT indexed into knowledge recall; they are searchable under records instead.
    let viewer = Viewer::bare(WS, OTHER);
    let hits = e
        .env
        .store()
        .read(|c| {
            retrieve(
                c,
                &Query::new(
                    &steve_knowledge::port::SqliteStore,
                    &viewer,
                    "续签邮件",
                    Purpose::Search,
                ),
            )
        })
        .unwrap();
    assert!(
        !hits
            .iter()
            .any(|h| h.obj_type == "item" || h.source_kind == "items"),
        "items must not enter knowledge index: {hits:#?}"
    );
    // Silently written memory_entry IS immediately retrievable without a confirmation card!
    assert!(
        hits.iter()
            .any(|h| h.obj_type == "memory_entry" && h.text.contains("Acme")),
        "expected silently written memory_entry in hits: {hits:#?}"
    );
    let (s, v) = e
        .me(
            "GET",
            "/kb/search?q=%E7%BB%AD%E7%AD%BE%E9%82%AE%E4%BB%B6&scope=records",
            None,
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert!(v["hits"].as_array().unwrap().is_empty());
    assert!(
        v["records"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["record_kind"] == "item" && r["href"] == "#/items/ST-12"),
        "{v}"
    );

    // Once per completion: the same completion again does not call the model.
    let calls = e.env.gateway.prompts.lock().unwrap().len();
    assert!(
        items::finished(&e.env.modules, &e.kb, "ST-12")
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(e.env.gateway.prompts.lock().unwrap().len(), calls);
}

#[tokio::test]
async fn steve_remember_writes_silently_to_context_without_confirm_cards() {
    let e = setup();
    let cx = McpCx {
        ws: WS.into(),
        user: ME.into(),
        bot_id: "bot_dev".into(),
        item_id: Some("ST-12".into()),
        session_id: Some("ses_1".into()),
    };
    let m = &e.env.modules;
    let r = candidates::remember(
        m,
        &e.kb,
        &cx,
        &json!({"text": "Acme 每年三月续签", "kind": "fact", "about": "Acme"}),
    );
    assert!(r.is_ok(), "{r:?}");
    assert!(r.unwrap().contains("Saved silently"));
    assert_eq!(e.open_confirms(), 0);

    let entries = e
        .env
        .store()
        .read(|r| MemoryEntries(r).list_active_ws(WS))
        .unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].subject_type, "party");
    assert_eq!(entries[0].subject_id, "org_acme");
    assert_eq!(entries[0].confidence_tier, ConfidenceTier::Verified);
    assert_eq!(entries[0].level, Level::Reasoning);

    let err = candidates::remember(
        m,
        &e.kb,
        &cx,
        &json!({"text": "x", "kind": "fact", "about": "王五"}),
    );
    assert!(err.unwrap_err().contains("王五"));
    let err = candidates::remember(m, &e.kb, &cx, &json!({"text": "x", "kind": "summary"}));
    assert!(err.unwrap_err().contains("at kind"));
    assert_eq!(candidates::remember_tool()["name"], "steve_remember");
}

#[tokio::test]
async fn chat_days_are_records_and_extracted_once_silently() {
    let e = setup();
    let m = &e.env.modules;
    let (start, _) = chat::day_bounds("2026-10-31", steve_runtime::tz::Zone::system()).unwrap();
    fixture::seed_day_messages(e.env.store(), start).unwrap();
    // Chat messages are never indexed as knowledge documents; they are found under records.
    let viewer = Viewer::bare(WS, OTHER);
    let hits = e
        .env
        .store()
        .read(|c| {
            retrieve(
                c,
                &Query::new(
                    &steve_knowledge::port::SqliteStore,
                    &viewer,
                    "部署",
                    Purpose::Search,
                ),
            )
        })
        .unwrap();
    assert!(hits.is_empty(), "{hits:#?}");
    let (s, v) = e
        .me("GET", "/kb/search?q=%E9%83%A8%E7%BD%B2&scope=records", None)
        .await;
    assert_eq!(s, 200, "{v}");
    let recs = v["records"].as_array().unwrap();
    assert!(!recs.is_empty(), "{v}");
    assert!(recs.iter().any(|r| {
        r["record_kind"] == "chat"
            && r["text"]
                .as_str()
                .unwrap_or_default()
                .contains("以后周五下午不安排部署")
    }));

    e.env.gateway.script(
        PROMPT_HEAD,
        r#"{"items":[{"changes_future_behavior":true,"kind":"rule","scope":"person","text":"周五下午不安排部署","about":[],"basis":"explicit","when":"release","why":"避免周末线上事故","evidence":"以后周五下午不安排部署"}]}"#,
    );
    let got = chat::extract_day(m, &e.kb, WS, "2026-10-31").await.unwrap();
    assert_eq!(got.len(), 1);
    assert!(got[0].is_written(), "{got:?}");
    // Zero Inbox confirmation cards opened!
    assert_eq!(e.open_confirms(), 0);
    // Repeated extraction of the same day is a no-op.
    assert!(
        chat::extract_day(m, &e.kb, WS, "2026-10-31")
            .await
            .unwrap()
            .is_empty()
    );

    // A day without messages yields nothing and makes 0 model calls.
    fixture::clear_messages(e.env.store()).unwrap();
    let calls_before = e.env.gateway.prompts.lock().unwrap().len();
    assert!(
        chat::extract_day(m, &e.kb, WS, "2026-10-30")
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(e.env.gateway.prompts.lock().unwrap().len(), calls_before);
}
