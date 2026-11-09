//! The routing eval: 22 real messages of the main conversation plus 20 synthetic ones
//! (`tests/route_cases.json`), each with the kinds it may end up as after the guards.
//!
//! - `recorded_router_outputs_pass_the_bar` replays the model replies recorded from the last real run
//!   (`tests/route_outputs.json`) through `parse_route` and `decide`: CI checks parsing and guards on
//!   fixed outputs.
//! - `route_eval_real_model` (ignored) asks the real fast tier; it needs `claude` and the gateway.
//!
//! ```text
//! STEVE_ROUTE_EVAL_MODELS=claude-haiku-5-5,claude-sonnet-5-5 STEVE_ROUTE_EVAL_SAVE=1 \
//!   cargo test -p steve-core route_eval_real_model -- --ignored --nocapture
//! ```
//!
//! The bar: at least 90% of the cases hit, and no question becomes an item.
//!
//! The ahead eval (`tests/ahead_cases.json`, outputs in `tests/ahead_outputs.json`): messages said while
//! Steve is still answering, each marked whether it may go ahead of the answer. Its bar: no message that
//! needs the answer goes ahead. `ahead_eval_real_model` (ignored) asks the real fast tier:
//!
//! ```text
//! STEVE_ROUTE_EVAL_SAVE=1 cargo test -p steve-core ahead_eval_real_model -- --ignored --nocapture
//! ```

use super::decide::{DecideCtx, decide, for_main, now_while_answering, without_router};
use crate::gateway::{self, ClaudeCli, FocusItem, ModelGateway, RouteCtx, Said, Tier};
use crate::repo::{Bot, Project, Tag};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::time::Duration;

const CASES: &str = include_str!("tests/route_cases.json");
const OUTPUTS: &str = include_str!("tests/route_outputs.json");
const AHEAD: &str = include_str!("tests/ahead_cases.json");
const AHEAD_OUTPUTS: &str = include_str!("tests/ahead_outputs.json");

#[derive(Deserialize)]
struct Cases {
    ctx: Ctx,
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Ctx {
    bots: Vec<Value>,
    projects: Vec<Value>,
    tags: Vec<Value>,
    steve_project: Option<String>,
}

#[derive(Deserialize, Clone)]
struct Case {
    id: String,
    text: String,
    files: Vec<String>,
    history: Vec<Value>,
    expect: Vec<String>,
    question: bool,
}

fn s(v: &Value, k: &str) -> String {
    v[k].as_str().unwrap_or_default().to_string()
}

fn route_ctx(c: &Ctx) -> RouteCtx {
    RouteCtx {
        bots: c
            .bots
            .iter()
            .map(|b| Bot {
                id: s(b, "id"),
                workspace_id: "default".into(),
                name: s(b, "id"),
                role: s(b, "role"),
                main: s(b, "id") == "steve",
                harness: "claude-code".into(),
                model: None,
                reasoning_effort: None,
                rules: String::new(),
                accept_mode: "inherit".into(),
                perm_mode: "default".into(),
                enabled: true,
                revision: 1,
                updated: 0,
                persona: String::new(),
                proactive: 1,
                avatar: Default::default(),
                default_project: None,
                prompt: None,
                device: None,
            })
            .collect(),
        projects: c
            .projects
            .iter()
            .map(|p| Project {
                id: s(p, "id"),
                workspace_id: "default".into(),
                name: s(p, "name"),
                description: s(p, "description"),
                device_id: "hub".into(),
                path: "/x".into(),
                kind: "git".into(),
                worktree: true,
                default_bot: p["default_bot"].as_str().map(String::from),
                release: None,
                archived: false,
                created: 0,
                updated: 0,
            })
            .collect(),
        tags: c
            .tags
            .iter()
            .map(|t| Tag {
                id: s(t, "id"),
                workspace_id: "default".into(),
                name: s(t, "name"),
                cat: s(t, "cat"),
                description: s(t, "description"),
                created_by: "Steve".into(),
                created: 0,
            })
            .collect(),
        steve_project: c.steve_project.clone(),
        items: (1..=20).map(|n| format!("ST-{n}")).collect(),
        ..Default::default()
    }
}

/// The message as the lane hands it to the router (attachments noted).
fn routed(c: &Case) -> String {
    if c.files.is_empty() {
        c.text.clone()
    } else {
        format!(
            "{}\n（附带 {} 个附件：{}）",
            c.text,
            c.files.len(),
            c.files.join("、")
        )
    }
}

fn history(c: &Case) -> Vec<Said> {
    c.history
        .iter()
        .map(|h| Said {
            author: s(h, "author"),
            text: s(h, "text"),
        })
        .collect()
}

/// One scored case.
struct Scored {
    id: String,
    text: String,
    model_kind: String,
    kind: String,
    guard: Option<&'static str>,
    hit: bool,
    question_to_item: bool,
}

fn score(ctx: &RouteCtx, c: &Case, raw: Option<&str>) -> Scored {
    let named = false;
    let r = raw
        .and_then(|raw| gateway::parse_route(ctx, &routed(c), raw, false))
        .unwrap_or_else(|| gateway::Route::fallback(&c.text, ctx.steve_project.clone()));
    let d = decide(&r, &c.text, DecideCtx { named_bot: named });
    Scored {
        id: c.id.clone(),
        text: crate::util::js_head(&c.text, 30).replace('\n', " "),
        model_kind: r.kind.clone(),
        hit: c.expect.contains(&d.kind),
        question_to_item: c.question && d.kind == "item",
        kind: d.kind,
        guard: d.guard,
    }
}

fn report(all: &[Scored]) -> (f64, usize) {
    let hits = all.iter().filter(|s| s.hit).count();
    let bad = all.iter().filter(|s| s.question_to_item).count();
    for s in all {
        println!(
            "{} {:<22} model={:<10} kind={:<10} guard={:<4} {}",
            if s.hit { "✓" } else { "✗" },
            s.id,
            s.model_kind,
            s.kind,
            s.guard.unwrap_or("-"),
            s.text
        );
    }
    let rate = hits as f64 / all.len() as f64;
    println!(
        "hits {hits}/{} = {:.1}%; questions made into items: {bad}",
        all.len(),
        rate * 100.0
    );
    (rate, bad)
}

#[test]
fn the_cases_are_the_22_real_messages_and_20_synthetic() {
    let c: Cases = serde_json::from_str(CASES).unwrap();
    assert_eq!(c.cases.len(), 42);
    assert_eq!(
        c.cases.iter().filter(|x| !x.id.starts_with("syn_")).count(),
        22
    );
    let a1 = c
        .cases
        .iter()
        .find(|x| x.text == "现在steve的知识系统是怎么运作的？给我讲一下")
        .unwrap();
    assert_eq!(a1.expect, vec!["research"]);
    for x in &c.cases {
        for k in &x.expect {
            assert!(gateway::KINDS.contains(&k.as_str()), "{}: {k}", x.id);
        }
    }
}

#[test]
fn recorded_router_outputs_pass_the_bar() {
    let c: Cases = serde_json::from_str(CASES).unwrap();
    let out: BTreeMap<String, Value> = serde_json::from_str(OUTPUTS).unwrap();
    let ctx = route_ctx(&c.ctx);
    let all: Vec<Scored> = c
        .cases
        .iter()
        .filter(|x| !x.id.starts_with("syn_") || out.contains_key(&x.id))
        .map(|x| score(&ctx, x, out.get(&x.id).and_then(|v| v["raw"].as_str())))
        .collect();
    assert!(all.len() >= 22);
    let (rate, bad) = report(&all);
    assert_eq!(bad, 0, "a question became an item");
    assert!(rate >= 0.9, "hit rate {rate}");
}

/// The bad router of 11-01 (everything an item, sure of itself): the guards keep the plain questions
/// out of items, and the knowledge-system question becomes research.
#[test]
fn an_item_happy_router_is_held_back_by_the_guards() {
    let c: Cases = serde_json::from_str(CASES).unwrap();
    let ctx = route_ctx(&c.ctx);
    let raw = r#"{"kind":"item","confidence":0.9,"signals":["change"],"title":"t","goal":"g","bot":"steve","project":null}"#;
    for x in c.cases.iter().filter(|x| x.question) {
        let r = gateway::parse_route(&ctx, &routed(x), raw, false).unwrap();
        let d = decide(&r, &x.text, DecideCtx::default());
        if super::decide::pure_question(&x.text) {
            assert_eq!(d.kind, "research", "{}", x.text);
        }
    }
    let r = gateway::parse_route(
        &ctx,
        "现在steve的知识系统是怎么运作的？给我讲一下",
        raw,
        false,
    )
    .unwrap();
    let d = decide(
        &r,
        "现在steve的知识系统是怎么运作的？给我讲一下",
        DecideCtx::default(),
    );
    assert_eq!((d.kind.as_str(), d.guard), ("research", Some("4")));
}

#[derive(Deserialize)]
struct Ahead {
    focus: Vec<Value>,
    cases: Vec<AheadCase>,
}

#[derive(Deserialize, Clone)]
struct AheadCase {
    id: String,
    pending: Vec<String>,
    text: String,
    now: bool,
}

/// The router's context for an ahead case: route_cases' workspace, the open items, what is being answered.
fn ahead_ctx(a: &Ahead, x: &AheadCase) -> RouteCtx {
    let c: Cases = serde_json::from_str(CASES).unwrap();
    RouteCtx {
        focus: a
            .focus
            .iter()
            .map(|f| FocusItem {
                id: s(f, "id"),
                title: s(f, "title"),
                status: s(f, "status"),
                origin: s(f, "origin"),
            })
            .collect(),
        pending: x.pending.clone(),
        ..route_ctx(&c.ctx)
    }
}

/// The conversation before an ahead case: the questions being answered, no answers yet.
fn ahead_history(x: &AheadCase) -> Vec<Said> {
    x.pending
        .iter()
        .map(|q| Said {
            author: "me".into(),
            text: q.clone(),
        })
        .collect()
}

/// Whether an ahead case goes ahead with this router reply, as the lane decides it ([`Core::ahead`]):
/// a plain question waits without the router.
fn goes_ahead(ctx: &RouteCtx, x: &AheadCase, raw: Option<&str>) -> bool {
    if without_router(&x.text, false, false, &[]).is_some() {
        return false;
    }
    let Some(r) = raw.and_then(|raw| gateway::parse_route(ctx, &x.text, raw, false)) else {
        return false;
    };
    let d = decide(
        &r,
        &x.text,
        DecideCtx {
            named_bot: x.text.starts_with("@dev"),
        },
    );
    now_while_answering(&r, &for_main(d, &r))
}

/// Scores the ahead cases: (messages that needed the answer but went ahead, share of standalone ones
/// that went ahead).
fn ahead_report(a: &Ahead, out: &BTreeMap<String, Value>) -> (usize, f64) {
    let (mut wrong, mut now, mut standalone) = (0, 0, 0);
    for x in &a.cases {
        let raw = out.get(&x.id).and_then(|v| v["raw"].as_str());
        let went = goes_ahead(&ahead_ctx(a, x), x, raw);
        wrong += usize::from(went && !x.now);
        standalone += usize::from(x.now);
        now += usize::from(went && x.now);
        println!(
            "{} {:<18} {} {}",
            if went == x.now { "✓" } else { "✗" },
            x.id,
            if went { "now " } else { "wait" },
            raw.map(|r| crate::util::js_head(r, 90).to_string())
                .unwrap_or_default()
        );
    }
    let rate = now as f64 / standalone.max(1) as f64;
    println!(
        "went ahead wrongly: {wrong}; standalone that went ahead: {now}/{standalone} = {:.0}%",
        rate * 100.0
    );
    (wrong, rate)
}

#[test]
fn recorded_ahead_outputs_never_go_ahead_of_an_answer_they_need() {
    let a: Ahead = serde_json::from_str(AHEAD).unwrap();
    let out: BTreeMap<String, Value> = serde_json::from_str(AHEAD_OUTPUTS).unwrap();
    assert_eq!(out.len(), a.cases.len());
    let (wrong, rate) = ahead_report(&a, &out);
    assert_eq!(wrong, 0, "a message went ahead of the answer it needs");
    assert!(rate >= AHEAD_BAR, "standalone ahead {rate}");
}

/// The share of standalone messages that must go ahead of the answer.
const AHEAD_BAR: f64 = 0.7;

/// The real fast tier as the Hub calls it: models from STEVE_ROUTE_EVAL_MODELS through 设置 · 模型 (purpose
/// `route`), `claude` from CLAUDE_CODE_EXECUTABLE. The directory lives as long as the gateway is used.
fn real_gateway() -> (std::sync::Arc<ClaudeCli>, tempfile::TempDir) {
    let models: Vec<String> = std::env::var("STEVE_ROUTE_EVAL_MODELS")
        .unwrap_or_else(|_| "claude-haiku-5-5,claude-sonnet-5-5".into())
        .split(',')
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty())
        .collect();
    assert!(
        models.iter().all(|m| !m.starts_with("kiro/")),
        "the kiro route has no upstream"
    );
    let dir = tempfile::tempdir().unwrap();
    // the models come through 设置 · 模型 as in the Hub: purpose `route`, the eval's list as its default
    let store = steve_store::Store::open(
        dir.path().join("steve.sqlite3"),
        steve_store::FnPublisher(|_| {}),
    )
    .unwrap();
    store.ensure_workspace().unwrap();
    let resolver = crate::models::Models::new(
        store,
        crate::models::Defaults {
            fast: models.clone(),
            smart: models.clone(),
            ..Default::default()
        },
    );
    let gw = std::sync::Arc::new(ClaudeCli {
        one: crate::LocalPrint {
            codex: None,
            executable: std::env::var("CLAUDE_CODE_EXECUTABLE")
                .unwrap_or_else(|_| "/opt/homebrew/bin/claude".into())
                .into(),
            cwd: dir.path().to_path_buf(),
        },
        fast: models.clone(),
        smart: models,
        models: Some(resolver),
    });
    (gw, dir)
}

/// The real fast tier on every ahead case. Writes `tests/ahead_outputs.json` with STEVE_ROUTE_EVAL_SAVE=1.
#[tokio::test]
#[ignore = "calls the real model"]
async fn ahead_eval_real_model() {
    let a: Ahead = serde_json::from_str(AHEAD).unwrap();
    let gw = real_gateway();
    let mut tasks = vec![];
    let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(6));
    for x in a.cases.clone() {
        let (gw, ctx, sem) = (gw.0.clone(), ahead_ctx(&a, &x), sem.clone());
        tasks.push(tokio::spawn(async move {
            let _p = sem.acquire().await.unwrap();
            if without_router(&x.text, false, false, &[]).is_some() {
                return (x.id.clone(), None);
            }
            let prompt = gateway::route_prompt(&ctx, &x.text, &ahead_history(&x), false);
            let r = gw
                .call(
                    &prompt,
                    Tier::Fast,
                    Duration::from_secs(60),
                    &gateway::ThinkTools::None,
                )
                .await;
            (x.id.clone(), r.ok().map(|p| (p.text, p.model)))
        }));
    }
    let mut out = BTreeMap::new();
    for t in tasks {
        let (id, r) = t.await.unwrap();
        out.insert(
            id,
            serde_json::json!({"raw": r.as_ref().map(|x| x.0.clone()), "model": r.map(|x| x.1)}),
        );
    }
    let (wrong, rate) = ahead_report(&a, &out);
    if std::env::var_os("STEVE_ROUTE_EVAL_SAVE").is_some() {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/thread/tests/ahead_outputs.json");
        std::fs::write(p, serde_json::to_string_pretty(&out).unwrap() + "\n").unwrap();
    }
    assert_eq!(wrong, 0, "a message went ahead of the answer it needs");
    assert!(rate >= AHEAD_BAR, "standalone ahead {rate}");
}

/// The real fast tier on every case. Writes `tests/route_outputs.json` with STEVE_ROUTE_EVAL_SAVE=1.
#[tokio::test]
#[ignore = "calls the real model"]
async fn route_eval_real_model() {
    let c: Cases = serde_json::from_str(CASES).unwrap();
    let ctx = route_ctx(&c.ctx);
    let (gw, _dir) = real_gateway();
    // STEVE_ROUTE_EVAL_PARALLEL=1 runs the cases one after another, as a conversation does (cache measurements)
    let parallel = std::env::var("STEVE_ROUTE_EVAL_PARALLEL")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(6);
    let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(parallel));
    let mut tasks = vec![];
    for x in c.cases.clone() {
        let (gw, ctx, sem) = (gw.clone(), ctx.clone(), sem.clone());
        tasks.push(tokio::spawn(async move {
            let _p = sem.acquire().await.unwrap();
            let prompt = gateway::route_prompt(&ctx, &routed(&x), &history(&x), false);
            let t0 = std::time::Instant::now();
            let r = crate::stats::tagged(
                "route",
                "",
                gw.call(
                    &prompt,
                    Tier::Fast,
                    Duration::from_secs(60),
                    &gateway::ThinkTools::None,
                ),
            )
            .await;
            let ms = t0.elapsed().as_millis() as u64;
            (x.id.clone(), r.ok().map(|p| (p.text, p.model)), ms)
        }));
    }
    let mut out = BTreeMap::new();
    for t in tasks {
        let (id, r, ms) = t.await.unwrap();
        out.insert(
            id,
            serde_json::json!({"raw": r.as_ref().map(|x| x.0.clone()), "model": r.map(|x| x.1), "ms": ms}),
        );
    }
    let all: Vec<Scored> = c
        .cases
        .iter()
        .map(|x| score(&ctx, x, out.get(&x.id).and_then(|v| v["raw"].as_str())))
        .collect();
    let mut models: BTreeMap<String, usize> = BTreeMap::new();
    for v in out.values() {
        *models
            .entry(v["model"].as_str().unwrap_or("failed").to_string())
            .or_default() += 1;
    }
    println!("models: {models:?}");
    let (rate, bad) = report(&all);
    let prod: Vec<Scored> = c
        .cases
        .iter()
        .filter(|x| !x.id.starts_with("syn_"))
        .map(|x| score(&ctx, x, out.get(&x.id).and_then(|v| v["raw"].as_str())))
        .collect();
    println!("-- the 22 real messages --");
    report(&prod);
    if std::env::var_os("STEVE_ROUTE_EVAL_SAVE").is_some() {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/thread/tests/route_outputs.json");
        std::fs::write(p, serde_json::to_string_pretty(&out).unwrap() + "\n").unwrap();
    }
    assert_eq!(bad, 0, "a question became an item");
    assert!(rate >= 0.9, "hit rate {rate}");
}
