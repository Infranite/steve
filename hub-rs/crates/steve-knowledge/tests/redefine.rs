//! Contract tests for the redefined knowledge boundary:
//! 1. Extraction validation: facts without resolved person/org/team/project `about` are dropped;
//!    one-off requests never become preferences; codebase engineering lessons route to project
//!    `AGENTS.md` proposals instead of personal memory candidates.
//! 2. Retrieval separation: `items` and `chat` never enter knowledge recall (`retrieve()` or
//!    default `GET /kb/search`), while remaining searchable under `scope=records` / `scope=all`.
//! 3. Project proposals REST + decision resolution + undo + project subject summary.
//! 4. Deterministic steward trigger gating around the configured threshold setting.

use axum::Router;
use serde_json::{Value, json};
use steve_knowledge::Knowledge;
use steve_knowledge::candidates::{self, MemDecisions};
use steve_knowledge::extract::{KIND_PROPOSAL, PROMPT_HEAD};
use steve_knowledge::proposals::{self, AgentsProposalDecisions};
use steve_knowledge::retrieve::{Purpose, Query, retrieve};
use steve_knowledge::sources::folder::FolderPolicy;
use steve_knowledge::sources::{chat, items};
use steve_knowledge::steward::{self, DEFAULT_THRESHOLD, StewardGate};
use steve_modules::decisions;
use steve_modules::testkit::{ME, TestEnv, WS, call, caller};
use steve_store::base::mems::NewMem;
use steve_store::base::{Level, Viewer};
use steve_store::knowledge::Memories;
use steve_store::testkit::knowledge as fixture;
use steve_store::{DecisionRows, NewProject};

struct Env {
    env: TestEnv,
    kb: Knowledge,
    app: Router,
}

fn setup() -> Env {
    let env = TestEnv::new(|r| {
        r.decision(candidates::SUBJECT, Box::new(MemDecisions));
        r.decision(proposals::SUBJECT, Box::new(AgentsProposalDecisions));
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
}

#[tokio::test]
async fn extraction_enforces_about_and_routes_engineering_to_agents_proposals() {
    let e = setup();
    let project = e
        .env
        .store()
        .projects()
        .create(
            WS,
            &NewProject {
                name: "steve".into(),
                kind: "git".into(),
                path: "/repo/steve".into(),
                ..Default::default()
            },
        )
        .unwrap();
    let _dir_project = e
        .env
        .store()
        .projects()
        .create(
            WS,
            &NewProject {
                name: "notes_dir".into(),
                kind: "dir".into(),
                path: "/docs/notes".into(),
                ..Default::default()
            },
        )
        .unwrap();

    // Script model output with strongly-typed `scope`:
    // - missing `scope` (dropped as invalid_scope)
    // - `organization` fact with empty `about` (dropped as missing_about)
    // - `person` fact with unresolved `about` (dropped as unresolved_about)
    // - `person` fact whose `about` resolves only to an org, not a person (dropped as unresolved_about)
    // - one-off restatement of item goal (dropped as one_off)
    // - `codebase` targeting a plain dir project without a git repo (dropped as unresolved_about)
    // - explicit `agents_proposal` with `scope: "codebase"` about git project `steve` (routed to project_proposals)
    // - Chinese codebase conclusion (`kind: "fact"`, `scope: "codebase"`) about `steve` (routed deterministically to project_proposals)
    // - valid `organization` fact anchored to existing org `Acme` (kept as knowledge candidate)
    let reply = json!({
        "items": [
            {
                "changes_future_behavior": true,
                "kind": "fact",
                "text": "缺 scope 的条目会被丢弃",
                "about": ["Acme"],
                "evidence": "五行以内",
                "basis": "verified",
                "when": "client",
                "why": "test"
            },
            {
                "changes_future_behavior": true,
                "kind": "fact",
                "scope": "organization",
                "text": "没有主语的孤立事实",
                "about": [],
                "evidence": "五行以内",
                "basis": "verified",
                "when": "client",
                "why": "test"
            },
            {
                "changes_future_behavior": true,
                "kind": "fact",
                "scope": "person",
                "text": "王五负责未知业务",
                "about": ["王五"],
                "evidence": "五行以内",
                "basis": "verified",
                "when": "client",
                "why": "test"
            },
            {
                "changes_future_behavior": true,
                "kind": "fact",
                "scope": "person",
                "text": "类型不匹配：写 person 却只指了组织",
                "about": ["Acme"],
                "evidence": "五行以内",
                "basis": "verified",
                "when": "client",
                "why": "test"
            },
            {
                "changes_future_behavior": false,
                "kind": "preference",
                "scope": "person",
                "text": "用户希望给 Acme 写续签邮件并抄送 Sarah",
                "about": [],
                "evidence": "给 Acme 写续签邮件，抄送 Sarah",
                "basis": "explicit",
                "when": "this_task",
                "why": "one-off"
            },
            {
                "changes_future_behavior": true,
                "kind": KIND_PROPOSAL,
                "scope": "codebase",
                "text": "普通目录项目没有代码仓库，不能接 codebase 提议",
                "about": ["notes_dir"],
                "evidence": "五行以内",
                "basis": "verified",
                "why": "test"
            },
            {
                "changes_future_behavior": true,
                "kind": KIND_PROPOSAL,
                "scope": "codebase",
                "text": "输入框由原生 textarea 绘制光标",
                "about": ["steve"],
                "evidence": "写好了续签邮件草稿",
                "basis": "verified",
                "why": "避免重复实现光标叠加层"
            },
            {
                "changes_future_behavior": true,
                "kind": "fact",
                "scope": "codebase",
                "text": "消息列表在流式输出时保持滚动锚定，不要每次重排整段历史",
                "about": ["steve"],
                "evidence": "五行以内",
                "basis": "verified",
                "why": "避免长对话抖动"
            },
            {
                "changes_future_behavior": true,
                "kind": "fact",
                "scope": "organization",
                "text": "Acme 的邮件往来统一使用英文",
                "about": ["Acme"],
                "evidence": "Acme 的邮件一律用英文",
                "basis": "verified",
                "when": "acme_email",
                "why": "以后给 Acme 发信都要遵守"
            }
        ]
    });
    e.env.gateway.script(PROMPT_HEAD, reply.to_string());

    let got = items::finished(&e.env.modules, &e.kb, "ST-12")
        .await
        .unwrap();
    // Only the valid Acme fact enters unified memory ingestion (buffered until corroborated,
    // ZERO Inbox confirmation cards opened for personal memories).
    assert_eq!(got.len(), 1, "{got:#?}");
    assert!(matches!(
        got[0],
        steve_knowledge::ingest::IngestOutcome::Buffered { .. }
            | steve_knowledge::ingest::IngestOutcome::Written { .. }
    ));

    // Pending candidates endpoint is empty — zero Inbox confirmation cards for memories!
    let (s, cands) = e.me("GET", "/kb/candidates", None).await;
    assert_eq!(s, 200, "{cands}");
    assert_eq!(cands.as_array().unwrap().len(), 0);
    let open_mem = e
        .env
        .store()
        .read(|r| DecisionRows(r).open_confirmations(WS, candidates::SUBJECT))
        .unwrap();
    assert_eq!(open_mem, 0);

    // Both engineering conclusions became project AGENTS.md proposals with open decisions.
    let (s, props) = e
        .me(
            "GET",
            &format!("/kb/proposals?project_id={}", project.id),
            None,
        )
        .await;
    assert_eq!(s, 200, "{props}");
    let list = props.as_array().unwrap();
    assert_eq!(list.len(), 2, "{props}");
    assert!(list.iter().all(|p| p["kind"] == "agents_md"));
    assert!(list.iter().all(|p| p["status"] == "pending"));
    assert!(list.iter().all(|p| p["project_id"] == project.id.as_str()));
    assert!(list.iter().all(|p| p["decision_id"].is_string()));

    // Project subject lists pending proposal count & category = "project".
    let (s, subs) = e.me("GET", "/kb/subjects", None).await;
    assert_eq!(s, 200, "{subs}");
    let proj_sub = subs
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["type"] == "project" && x["id"] == project.id.as_str())
        .expect("project subject listed");
    assert_eq!(proj_sub["category"], "project");
    assert_eq!(proj_sub["proposals"], 2);

    // Subject detail includes both pending proposals.
    let (s, detail) = e
        .me("GET", &format!("/kb/subjects/project/{}", project.id), None)
        .await;
    assert_eq!(s, 200, "{detail}");
    assert_eq!(detail["proposals"].as_array().unwrap().len(), 2);

    // Accept proposal 0 via REST, reject duplicate resolve (409), then undo via decision handler.
    let p0_id = list[0]["id"].as_str().unwrap().to_string();
    let p0_dec = list[0]["decision_id"].as_str().unwrap().to_string();
    let (s, resolved) = e
        .me(
            "POST",
            &format!("/kb/proposals/{p0_id}/resolve"),
            Some(json!({"choice": "yes"})),
        )
        .await;
    assert_eq!(s, 200, "{resolved}");
    assert_eq!(resolved["status"], "accepted");

    let (s, _) = e
        .me(
            "POST",
            &format!("/kb/proposals/{p0_id}/resolve"),
            Some(json!({"choice": "no"})),
        )
        .await;
    assert_eq!(s, 409);

    let dec = e
        .env
        .store()
        .read(|r| DecisionRows(r).get(&p0_dec))
        .unwrap()
        .unwrap();
    assert_eq!(dec.status, steve_types::DecisionStatus::Resolved);
    decisions::undo(&e.env.modules, &p0_dec, ME, |_| true)
        .expect("ours")
        .expect("undone");

    let (s, after_undo) = e
        .me(
            "GET",
            &format!("/kb/proposals?project_id={}&status=pending", project.id),
            None,
        )
        .await;
    assert_eq!(s, 200, "{after_undo}");
    assert_eq!(after_undo.as_array().unwrap().len(), 2);

    // Dismiss proposal 1.
    let p1_id = list[1]["id"].as_str().unwrap().to_string();
    let (s, dismissed) = e
        .me(
            "POST",
            &format!("/kb/proposals/{p1_id}/resolve"),
            Some(json!({"choice": "no"})),
        )
        .await;
    assert_eq!(s, 200, "{dismissed}");
    assert_eq!(dismissed["status"], "dismissed");
}

#[tokio::test]
async fn search_keeps_knowledge_recall_separate_from_item_and_chat_records() {
    let e = setup();
    let (start, _) = chat::day_bounds("2026-10-31", steve_runtime::tz::Zone::system()).unwrap();
    fixture::seed_day_messages(e.env.store(), start).unwrap();

    // Seed a knowledge note and a confirmed rule matching "部署" (same term as the chat message)
    // and another note matching "续签" (same term as item ST-12).
    let (s, _) = e
        .me(
            "POST",
            "/kb/notes",
            Some(json!({"title": "部署与续签手册", "body": "季度部署和续签流程文档。"})),
        )
        .await;
    assert_eq!(s, 200);
    let (s, _) = e
        .me(
            "POST",
            "/kb/mems",
            Some(json!({"kind": "rule", "text": "重要部署前必须先做备份"})),
        )
        .await;
    assert_eq!(s, 200);

    // Default `GET /kb/search` (scope=knowledge) returns ONLY knowledge hits; records is empty.
    let (s, v) = e.me("GET", "/kb/search?q=%E9%83%A8%E7%BD%B2", None).await;
    assert_eq!(s, 200, "{v}");
    let hits = v["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 2, "{v}");
    assert!(
        hits.iter()
            .all(|h| h["source_kind"] != "items" && h["source_kind"] != "chat")
    );
    assert!(v["records"].as_array().unwrap().is_empty());

    // `scope=records` returns ONLY chat/item records; knowledge hits is empty.
    let (s, v) = e
        .me("GET", "/kb/search?q=%E9%83%A8%E7%BD%B2&scope=records", None)
        .await;
    assert_eq!(s, 200, "{v}");
    assert!(v["hits"].as_array().unwrap().is_empty());
    let chat_recs = v["records"].as_array().unwrap();
    assert!(chat_recs.iter().any(|r| r["record_kind"] == "chat"), "{v}");

    let (s, v) = e
        .me("GET", "/kb/search?q=%E7%BB%AD%E7%AD%BE&scope=all", None)
        .await;
    assert_eq!(s, 200, "{v}");
    assert!(!v["hits"].as_array().unwrap().is_empty(), "{v}");
    assert!(
        v["records"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["record_kind"] == "item" && r["id"] == "ST-12"),
        "{v}"
    );

    // Knowledge recall (`retrieve`) never mixes items or chat.
    let viewer = Viewer::bare(WS, ME);
    let recalled = e
        .env
        .store()
        .read(|c| {
            retrieve(
                c,
                &Query::new(
                    &steve_knowledge::port::SqliteStore,
                    &viewer,
                    "部署",
                    Purpose::Chat,
                ),
            )
        })
        .unwrap();
    assert!(
        recalled
            .iter()
            .all(|h| h.source_kind != "items" && h.source_kind != "chat")
    );
}

#[tokio::test]
async fn steward_trigger_gates_deterministically_on_configured_threshold() {
    let e = setup();
    let flow_id = "fl_steward";

    // Default threshold is declared (no magic numbers) and out-of-range values are rejected.
    let (s, v) = e.me("GET", "/kb/steward", None).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["threshold"], DEFAULT_THRESHOLD);

    let (s, _) = e
        .me("PUT", "/kb/steward", Some(json!({"threshold": 0})))
        .await;
    assert_eq!(s, 400);
    let (s, _) = e
        .me("PUT", "/kb/steward", Some(json!({"threshold": 101})))
        .await;
    assert_eq!(s, 400);

    // Bind schedule and set threshold = 2.
    let (s, v) = e
        .me(
            "PUT",
            "/kb/steward",
            Some(json!({"schedule_id": flow_id, "threshold": 2})),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["schedule_id"], flow_id);
    assert_eq!(v["threshold"], 2);

    // Non-steward flow is untouched.
    let other = e
        .env
        .store()
        .write(|w| steward::check_schedule_trigger(w, WS, "fl_other", false, 1000))
        .unwrap();
    assert_eq!(other, StewardGate::NotSteward);

    // 0 new candidates/conflicts -> skipped (`nothing_to_tidy`).
    let gate = e
        .env
        .store()
        .write(|w| steward::check_schedule_trigger(w, WS, flow_id, false, 1000))
        .unwrap();
    assert!(
        matches!(
            gate,
            StewardGate::Skip {
                reason: "nothing_to_tidy",
                ..
            }
        ),
        "{gate:?}"
    );

    // Manual trigger always runs even with 0 signals.
    let manual = e
        .env
        .store()
        .write(|w| steward::check_schedule_trigger(w, WS, flow_id, true, 1000))
        .unwrap();
    assert_eq!(manual, StewardGate::Run { reason: "manual" });

    // Seed 1 candidate row directly in `mems` -> total 1 < threshold 2 -> skipped (`below_threshold`).
    let add_cand = |idx: usize| {
        e.env
            .store()
            .write(|tx| {
                Memories(tx).create(
                    WS,
                    &NewMem {
                        kind: "rule".into(),
                        text: format!("候选规则 {idx}"),
                        source_type: "item".into(),
                        source_label: format!("ST-10{idx} 交付时"),
                        level: Level::Reasoning,
                        status: "candidate".into(),
                        writer: "steve".into(),
                        created_by: ME.into(),
                        ..Default::default()
                    },
                )
            })
            .unwrap();
    };
    add_cand(1);

    let below = e
        .env
        .store()
        .write(|w| steward::check_schedule_trigger(w, WS, flow_id, false, 2000))
        .unwrap();
    assert!(
        matches!(
            below,
            StewardGate::Skip {
                reason: "below_threshold",
                threshold: 2,
                ..
            }
        ),
        "{below:?}"
    );

    // Add 2nd candidate -> total 2 >= threshold 2 -> runs and advances `last_run_at`.
    add_cand(2);
    let now = steve_store::now() + 10_000;
    let run = e
        .env
        .store()
        .write(|w| steward::check_schedule_trigger(w, WS, flow_id, false, now))
        .unwrap();
    assert_eq!(
        run,
        StewardGate::Run {
            reason: "threshold"
        }
    );

    // Immediately after running, no new signals exist since `last_run_at`, so next scheduled tick skips.
    let after = e
        .env
        .store()
        .write(|w| steward::check_schedule_trigger(w, WS, flow_id, false, now + 1000))
        .unwrap();
    assert!(
        matches!(
            after,
            StewardGate::Skip {
                reason: "nothing_to_tidy",
                ..
            }
        ),
        "{after:?}"
    );

    let (s, status) = e.me("GET", "/kb/steward", None).await;
    assert_eq!(s, 200, "{status}");
    assert_eq!(status["last_run_at"], now);
    assert_eq!(status["signals"]["total"], 0);
}
