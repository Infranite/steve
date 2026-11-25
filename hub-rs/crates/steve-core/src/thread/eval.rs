//! The routing eval: 22 real messages of the main conversation plus 20 synthetic ones
//! (`tests/route_cases.json`), each with the kinds it may end up as after the guards.
//!
//! - `recorded_router_outputs_pass_the_bar` replays the model replies recorded from the last real run
//!   (`tests/route_outputs.json`) through `parse_route` and `decide`: CI checks parsing and guards on
//!   fixed outputs.
//! The bar: at least 90% of the cases hit, and no question becomes an item.
//!
//! The ahead eval (`tests/ahead_cases.json`, outputs in `tests/ahead_outputs.json`): messages said while
//! Steve is still answering, each marked whether it may go ahead of the answer.
//!
//! The batch eval routes the 20 synthetic cases (no history) in bursts of [`BURST`], one call each
//! ([`gateway::route_many`]), outputs in `tests/batch_outputs.json`. Same bar as one at a time.

use super::decide::{DecideCtx, decide};
use crate::gateway::{self, RouteCtx};
use crate::repo::{Bot, Project, Tag};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;

const CASES: &str = include_str!("tests/route_cases.json");
const OUTPUTS: &str = include_str!("tests/route_outputs.json");
const BATCH_OUTPUTS: &str = include_str!("tests/batch_outputs.json");

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
                specialties: vec![],
                id: s(b, "id"),
                workspace_id: "default".into(),
                owner: None,
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

/// How many synthetic cases the batch eval routes in one call.
const BURST: usize = 5;

/// The synthetic cases in bursts of [`BURST`], each burst's id the ids joined.
fn bursts(c: &Cases) -> Vec<(String, Vec<Case>)> {
    let syn: Vec<Case> = c
        .cases
        .iter()
        .filter(|x| x.id.starts_with("syn_") && x.history.is_empty())
        .cloned()
        .collect();
    syn.chunks(BURST)
        .map(|b| {
            (
                b.iter()
                    .map(|x| x.id.as_str())
                    .collect::<Vec<_>>()
                    .join("+"),
                b.to_vec(),
            )
        })
        .collect()
}

/// Scores the bursts' replies: each case as [`score`] would, from its element of the array.
fn batch_report(ctx: &RouteCtx, c: &Cases, out: &BTreeMap<String, Value>) -> (f64, usize) {
    let mut all = vec![];
    for (id, b) in bursts(c) {
        let texts: Vec<String> = b.iter().map(routed).collect();
        let raw = out.get(&id).and_then(|v| v["raw"].as_str());
        let rs = raw.and_then(|raw| gateway::parse_routes(ctx, &texts, raw));
        assert!(rs.is_some(), "burst {id} unusable: {raw:?}");
        for (x, r) in b.iter().zip(rs.unwrap()) {
            let raw = serde_json::json!({
                "kind": r.kind, "confidence": r.confidence, "signals": r.signals,
                "item": r.item, "title": r.title, "goal": r.goal,
            })
            .to_string();
            all.push(score(ctx, x, Some(&raw)));
        }
    }
    report(&all)
}

#[test]
fn recorded_batch_outputs_pass_the_bar() {
    let c: Cases = serde_json::from_str(CASES).unwrap();
    let out: BTreeMap<String, Value> = serde_json::from_str(BATCH_OUTPUTS).unwrap();
    let ctx = route_ctx(&c.ctx);
    assert_eq!(out.len(), bursts(&c).len());
    let (rate, bad) = batch_report(&ctx, &c, &out);
    assert_eq!(bad, 0, "a question became an item");
    assert!(rate >= 0.9, "hit rate {rate}");
}
