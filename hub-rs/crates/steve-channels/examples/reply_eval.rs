//! The owner-voice reply eval, offline (`steve_channels::reply_eval`). Reads cases from files the
//! operator keeps outside the repository; writes no Steve database and sends nothing.
//!
//! ```text
//! cargo run -p steve-channels --example reply_eval -- <command> [options]
//!
//! replay   --cases F --out F --owner NAME [--samples N] [--only id,id] [--concurrency N]
//!          [--corpus F --style] [--bot-layer] [--alias A]... [--tz ZONE]
//!          [--route-db PATH [--workspace WS]]
//!          [--harness H] [--models m1,m2] [--effort E] [--dry-run]
//! baseline --cases F --out F
//! check    --cases F --labels F --outputs F [--corpus F] [--owner NAME] [--alias A]...
//!          [--against F] [--live-secs N] [--latency-secs N] [--rows F]
//! judge    --cases F --outputs F --out F --models m1,m2 [--harness H] [--corpus F]
//!          [--owner NAME] [--alias A]... [--concurrency N]
//! gate     --cases F --outputs F --out F [--live-secs N] [--gap-secs N] [--latency-secs N]
//! sweep    --cases F --labels F --outputs F [--live 0,60,…] [--gap 0,30,…] [--latency-secs N]
//! ```
//!
//! `replay` builds the composer's layers for each case at its time ([`reply_eval::layers`]; the
//! rules of writing as you always; with `--style`, your samples and habits from `--corpus` sent
//! before the case, never any case's gold answers; with `--bot-layer`, the ablation: the Bot's name,
//! duty, manner and rules rendered on your own account as before) and asks the model the composer
//! asks, with its second try when the checks of writing as you send a reply back
//! ([`compose::voice_verdict`], [`compose::settle_revised`]): the gateway's smart tier with the purpose `compose`, through
//! Steve's own model resolver and one-shot driver. The route — the main Bot's harness, model and
//! effort, and the `models.compose` override — is read from `--route-db` (opened read-only, as a
//! SQLite URI with `mode=ro`) and given to the resolver over a throwaway store in a temporary
//! directory; `--harness/--models/--effort` override it. `STEVE_MODELS_SMART` / `STEVE_MODELS_FAST`
//! are read as the Hub reads them. The harness runs in a temporary directory with `STEVE_HOME`
//! pointed there unless it is set, so the installed Hub's files are not touched; set
//! `STEVE_ADAPTER_*` when the adapter is not on `PATH`. Output: `{case_id, sample, outcome, reason,
//! text, model}` per line. `--dry-run` prints each prompt's size, how many of your samples it has
//! and which situation's habits (from how many messages) instead of calling a model.
//!
//! `baseline` writes what Steve did then (`steve` of each case) in the same format.
//!
//! `check` prints the rule metrics (all, dev, test) as JSON and a Markdown table; `--against` adds
//! the baseline's column and the cases it got right that this run does not.
//!
//! `gate` replays the presence gate and the merge of one sender's turn
//! ([`steve_channels::presence`], [`reply_eval::apply_gates`]) on answers: the gated answers go to
//! `--out` (for `check`), what each gate changed to stdout. `sweep` does it for every pair of window
//! lengths (seconds; the defaults are the gate's constants) and prints the rule metrics of each, with
//! the cases each pair gets wrong that the ungated answers got right: how the windows were chosen.
//!
//! `judge` asks a different model than the one that drafted (refused otherwise) about understanding,
//! voice and card answerability, and writes one judgment per answer.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use serde_json::{Value, json};
use steve_channels::compose::{self, BotInfo, HISTORY, HISTORY_WIDE};
use steve_channels::deliver::{Generation, Outcome};
use steve_channels::reply_eval::{self as re, Case, CheckParams, Label, Output, OwnerLine};
use steve_channels::spaces::Mode;
use steve_modules::{Gateway, Tier};

type Args = HashMap<String, Vec<String>>;

fn parse_args(raw: &[String]) -> Args {
    let mut a: Args = HashMap::new();
    let mut i = 0;
    while i < raw.len() {
        if let Some(k) = raw[i].strip_prefix("--") {
            let v = raw.get(i + 1).filter(|v| !v.starts_with("--")).cloned();
            i += if v.is_some() { 2 } else { 1 };
            a.entry(k.to_string())
                .or_default()
                .push(v.unwrap_or_else(|| "true".into()));
        } else {
            i += 1;
        }
    }
    a
}

fn one<'a>(a: &'a Args, k: &str) -> Option<&'a str> {
    a.get(k).and_then(|v| v.last()).map(String::as_str)
}

fn need<'a>(a: &'a Args, k: &str) -> &'a str {
    one(a, k).unwrap_or_else(|| fail(&format!("--{k} is required")))
}

fn list(a: &Args, k: &str) -> Vec<String> {
    a.get(k)
        .into_iter()
        .flatten()
        .flat_map(|v| v.split(','))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn fail(msg: &str) -> ! {
    eprintln!("reply_eval: {msg}");
    std::process::exit(2)
}

fn read<T: serde::de::DeserializeOwned>(path: &str) -> Vec<T> {
    let s = std::fs::read_to_string(path).unwrap_or_else(|e| fail(&format!("{path}: {e}")));
    re::read_jsonl(&s).unwrap_or_else(|e| fail(&format!("{path}: {e}")))
}

fn write_jsonl<T: serde::Serialize>(path: &str, rows: &[T]) {
    let mut s = String::new();
    for r in rows {
        s.push_str(&serde_json::to_string(r).expect("serializable"));
        s.push('\n');
    }
    std::fs::write(path, s).unwrap_or_else(|e| fail(&format!("{path}: {e}")));
}

fn main() {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = raw.first().cloned() else {
        fail("a command is required: replay | baseline | check | judge");
    };
    let args = parse_args(&raw[1..]);
    match cmd.as_str() {
        "baseline" => baseline(&args),
        "check" => check(&args),
        "gate" => gate(&args),
        "sweep" => sweep(&args),
        "replay" | "judge" => {
            let scratch = tempfile::tempdir().unwrap_or_else(|e| fail(&format!("tempdir: {e}")));
            if std::env::var_os("STEVE_HOME").is_none() {
                // before any thread exists: the harness's home is the scratch directory
                unsafe { std::env::set_var("STEVE_HOME", scratch.path().join("home")) };
            }
            let rt = tokio::runtime::Runtime::new().expect("runtime");
            rt.block_on(async {
                if cmd == "replay" {
                    replay(&args, scratch.path()).await
                } else {
                    judge(&args, scratch.path()).await
                }
            });
        }
        other => fail(&format!("unknown command {other}")),
    }
}

// ---------------------------------------------------------------- the route

/// What the main Bot and 设置 · 模型 say about the purpose `compose`.
#[derive(Default)]
struct Route {
    bot: Option<BotInfo>,
    harness: Option<String>,
    effort: Option<String>,
    /// `models.compose`, as stored.
    compose: Option<Value>,
}

/// The route from a Steve database, opened read-only.
fn route_from_db(path: &str, ws: &str) -> Route {
    use rusqlite::{OpenFlags, OptionalExtension};
    let uri = format!("file:{path}?mode=ro");
    let c = rusqlite::Connection::open_with_flags(
        &uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .unwrap_or_else(|e| fail(&format!("{path}: {e}")));
    let bot = c
        .query_row(
            "SELECT b.id, b.name, b.role, b.persona, b.rules, coalesce(b.prompt, ''), b.model, b.harness, b.reasoning_effort
               FROM bots b JOIN bot_workspaces w ON w.bot_id = b.id AND w.workspace_id = ?
              WHERE b.main = 1 AND b.deleted = 0 AND b.enabled = 1 ORDER BY b.updated LIMIT 1",
            [ws],
            |r| {
                Ok((
                    BotInfo {
                        id: r.get(0)?,
                        name: r.get(1)?,
                        role: r.get(2)?,
                        persona: r.get(3)?,
                        rules: r.get(4)?,
                        prompt: r.get(5)?,
                        model: r.get::<_, Option<String>>(6)?.filter(|m| !m.is_empty()),
                    },
                    r.get::<_, String>(7)?,
                    r.get::<_, Option<String>>(8)?,
                ))
            },
        )
        .optional()
        .unwrap_or_else(|e| fail(&format!("main Bot: {e}")));
    let compose: Option<String> = c
        .query_row(
            "SELECT value FROM settings WHERE workspace_id = ? AND key = 'models.compose'",
            [ws],
            |r| r.get(0),
        )
        .optional()
        .unwrap_or_else(|e| fail(&format!("settings: {e}")));
    let (bot, harness, effort) = match bot {
        Some((b, h, e)) => (Some(b), Some(h), e.filter(|e| !e.is_empty())),
        None => (None, None, None),
    };
    Route {
        bot,
        harness,
        effort,
        compose: compose.and_then(|v| serde_json::from_str(&v).ok()),
    }
}

fn env_list(k: &str) -> Vec<String> {
    std::env::var(k)
        .unwrap_or_default()
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// The gateway the composer would call: Steve's resolver over a throwaway store holding only the
/// route, and the one-shot driver in `scratch`. Explicit `--harness/--models/--effort` become the
/// purpose's override.
fn gateway(route: &Route, a: &Args, scratch: &Path) -> (Arc<dyn Gateway>, String) {
    let store = steve_store::Store::open(
        scratch.join("route.sqlite3"),
        steve_store::FnPublisher(|_| {}),
    )
    .unwrap_or_else(|e| fail(&format!("route store: {e}")));
    store.ensure_workspace().expect("workspace");
    let ws = steve_store::WS;
    if let Some(b) = &route.bot {
        store
            .bots()
            .seed(
                &b.id,
                true,
                &steve_store::NewBot {
                    name: b.name.clone(),
                    harness: route.harness.clone().unwrap_or_default(),
                    model: b.model.clone(),
                    reasoning_effort: route.effort.clone(),
                    ..Default::default()
                },
                ws,
            )
            .unwrap_or_else(|e| fail(&format!("route bot: {e}")));
    }
    let models = list(a, "models");
    let mut compose = route.compose.clone();
    if one(a, "harness").is_some() || !models.is_empty() || one(a, "effort").is_some() {
        let mut c = compose.clone().unwrap_or_else(|| json!({}));
        let harness = one(a, "harness")
            .map(String::from)
            .or_else(|| c["harness"].as_str().map(String::from))
            .or_else(|| route.harness.clone())
            .unwrap_or_else(|| steve_core::models::Choice::default().harness);
        c["harness"] = json!(harness);
        if !models.is_empty() {
            c["models"] = json!(models);
        }
        if let Some(e) = one(a, "effort") {
            c["effort"] = json!(e);
        }
        compose = Some(c);
    }
    if let Some(c) = compose {
        let mut m = serde_json::Map::new();
        m.insert(steve_core::models::key("compose"), c);
        store
            .settings()
            .set(ws, &m, "reply_eval")
            .unwrap_or_else(|e| fail(&format!("route setting: {e}")));
    }
    let defaults = steve_core::models::Defaults {
        fast: env_list("STEVE_MODELS_FAST"),
        smart: env_list("STEVE_MODELS_SMART"),
        ..Default::default()
    };
    let resolver = steve_core::models::Models::new(store, defaults);
    let shown = resolver.for_call(ws, "compose", steve_core::Tier::Smart);
    let described = shown.describe();
    let inner = Arc::new(steve_core::InProcess {
        one: steve_core::LocalAcp {
            acp: steve_acp::AcpConfig::from_env(),
            cwd: scratch.join("scratch"),
        },
        fast: vec![],
        smart: vec![],
        models: Some(resolver),
    });
    let models: Vec<String> = shown.models.clone();
    (
        Arc::new(steve_core::SharedGateway(inner)),
        if models.is_empty() {
            described
        } else {
            format!("{described} [{}]", models.join(","))
        },
    )
}

// ---------------------------------------------------------------- replay

/// The composer's call and what it does with the answer (`Composer::generate`): once, and once more
/// with the wider history when the model needs more context; then [`compose::after_wide`].
async fn compose_once(
    gw: &dyn Gateway,
    prompt: &str,
    owner: &str,
) -> Result<(Outcome, String, String, String), String> {
    let out = steve_modules::gateway::purpose(
        "compose",
        gw.run_with_images(Tier::Smart, prompt, &[], compose::TIMEOUT),
    )
    .await?;
    let (o, text, _, why) = compose::read_answer(&out.text, Mode::Own, owner)
        .ok_or_else(|| "模型没有给出正文".to_string())?;
    Ok((o, text, why, out.model))
}

/// One case as `Composer::generate` answers it: the call, the wider pass on `need_context`, then the
/// checks of writing as you and their second try.
async fn replay_one(
    gw: &dyn Gateway,
    narrow: &compose::Layers,
    wide: &compose::Layers,
    owner: &str,
) -> Result<compose::Reply, String> {
    let reply = |(o, text, why, model): (Outcome, String, String, String)| compose::Reply {
        text,
        generation: Generation {
            outcome: o,
            why,
            model,
            ..Generation::default()
        },
    };
    let mut used = narrow;
    let mut r = reply(compose_once(gw, &compose::prompt(narrow), owner).await?);
    if r.generation.outcome == Outcome::NeedContext {
        used = wide;
        r = reply(compose_once(gw, &compose::prompt(wide), owner).await?);
        let (o, t) = compose::after_wide(
            Mode::Own,
            r.generation.outcome.clone(),
            &r.text,
            wide.unreadable,
        );
        r.generation.outcome = o;
        r.text = t;
    }
    let style = used
        .voice
        .as_ref()
        .and_then(|v| v.style.as_ref())
        .map(|(_, s)| s);
    let Some(feedback) = compose::voice_verdict(Mode::Own, false, &r, style) else {
        return Ok(r);
    };
    let found: Vec<String> = steve_channels::voice::problems(&r.text, style)
        .iter()
        .map(|p| p.code().to_string())
        .collect();
    let again = compose::Layers {
        revise: Some(feedback),
        ..used.clone()
    };
    let second = reply(compose_once(gw, &compose::prompt(&again), owner).await?);
    let mut out = compose::settle_revised(&r, second, style);
    out.generation.revised = found;
    Ok(out)
}

async fn replay(a: &Args, scratch: &Path) {
    let cases: Vec<Case> = read(need(a, "cases"));
    let out_path = need(a, "out").to_string();
    let owner = need(a, "owner").to_string();
    let samples: usize = one(a, "samples").and_then(|n| n.parse().ok()).unwrap_or(1);
    let only: HashSet<String> = list(a, "only").into_iter().collect();
    let corpus: Vec<OwnerLine> = one(a, "corpus").map(read).unwrap_or_default();
    let style = one(a, "style").is_some();
    if style && corpus.is_empty() {
        fail("--style needs --corpus");
    }
    let excluded = re::gold_ids(&cases);
    let route = match one(a, "route-db") {
        Some(p) => route_from_db(p, one(a, "workspace").unwrap_or(steve_store::WS)),
        None => Route::default(),
    };
    let setting = re::Setting {
        owner: owner.clone(),
        aliases: list(a, "alias"),
        tz: one(a, "tz").map(|t| {
            t.parse::<chrono_tz::Tz>()
                .unwrap_or_else(|_| fail("--tz: an IANA time zone"))
        }),
        bot: route.bot.clone(),
        bot_layer: one(a, "bot-layer").is_some(),
        ..Default::default()
    };
    let todo: Vec<&Case> = cases
        .iter()
        .filter(|c| only.is_empty() || only.contains(&c.case_id))
        .collect();
    let yours = names(a);
    let people = re::people(&cases, &corpus);
    let prompts: Vec<(String, compose::Layers, compose::Layers)> = todo
        .iter()
        .map(|c| {
            let voice = if style {
                re::voice(&corpus, c, &excluded, &yours, &people)
            } else {
                re::bare_voice(c)
            };
            (
                c.case_id.clone(),
                re::layers(c, &setting, HISTORY, voice.clone()),
                re::layers(c, &setting, HISTORY_WIDE, voice),
            )
        })
        .collect();
    if one(a, "dry-run").is_some() {
        for (id, l, _) in &prompts {
            println!(
                "{}",
                json!({
                    "case_id": id,
                    "prompt_chars": compose::prompt(l).chars().count(),
                    "samples": l.voice.as_ref().map_or(0, |v| v.samples.len()),
                    "habits": l.voice.as_ref().and_then(|v| v.style.as_ref()).map(|(s, st)| (s.code(), st.n)),
                })
            );
        }
        return;
    }
    let (gw, described) = gateway(&route, a, scratch);
    eprintln!(
        "reply_eval: compose runs on {described}; {} cases × {samples}",
        prompts.len()
    );
    let sem = Arc::new(tokio::sync::Semaphore::new(
        one(a, "concurrency")
            .and_then(|n| n.parse().ok())
            .unwrap_or(4),
    ));
    let mut tasks = tokio::task::JoinSet::new();
    for (id, narrow, wide) in prompts {
        for sample in 0..samples {
            let (gw, sem, owner) = (gw.clone(), sem.clone(), owner.clone());
            let (id, narrow, wide) = (id.clone(), narrow.clone(), wide.clone());
            tasks.spawn(async move {
                let _permit = sem.acquire_owned().await.expect("semaphore");
                let r = replay_one(gw.as_ref(), &narrow, &wide, &owner).await;
                match r {
                    Ok(r) => Output {
                        case_id: id,
                        sample,
                        outcome: r.generation.outcome.code().into(),
                        reason: r.generation.why,
                        text: r.text,
                        model: r.generation.model,
                        revised: r.generation.revised,
                    },
                    Err(e) => Output {
                        case_id: id,
                        sample,
                        outcome: "error".into(),
                        reason: e,
                        ..Default::default()
                    },
                }
            });
        }
    }
    let mut outs = Vec::new();
    while let Some(r) = tasks.join_next().await {
        let o = r.expect("task");
        eprintln!("reply_eval: {} #{} → {}", o.case_id, o.sample, o.outcome);
        outs.push(o);
    }
    outs.sort_by(|x, y| (&x.case_id, x.sample).cmp(&(&y.case_id, y.sample)));
    write_jsonl(&out_path, &outs);
}

// ---------------------------------------------------------------- baseline and check

fn baseline(a: &Args) {
    let cases: Vec<Case> = read(need(a, "cases"));
    let outs: Vec<Output> = cases.iter().map(re::recorded_output).collect();
    write_jsonl(need(a, "out"), &outs);
}

/// This crate's system texts and the router's.
fn system_texts() -> Vec<String> {
    steve_channels::deliver::SYSTEM_TEXTS
        .iter()
        .chain([steve_core::thread::research::QUEUE_FULL].iter())
        .map(|s| s.to_string())
        .collect()
}

fn names(a: &Args) -> Vec<String> {
    let mut n = list(a, "alias");
    if let Some(o) = one(a, "owner") {
        n.insert(0, o.to_string());
    }
    n
}

fn fmt(v: Option<f64>) -> String {
    v.map(|x| format!("{x:.2}")).unwrap_or_else(|| "–".into())
}

fn table(cols: &[(&str, &re::Metrics)]) -> String {
    type Get = fn(&re::Metrics) -> String;
    let lines: [(&str, Get); 16] = [
        ("answers", |m| m.answers.to_string()),
        ("decision accuracy", |m| fmt(Some(m.decision_accuracy))),
        ("act precision", |m| fmt(m.act_precision)),
        ("act recall (should act)", |m| fmt(m.act_recall)),
        ("act exact (should act)", |m| fmt(m.act_exact)),
        ("presence-skip recall", |m| fmt(m.presence_skip_recall)),
        ("drafts", |m| m.drafts.to_string()),
        ("length ratio (median)", |m| fmt(m.length_ratio_median)),
        ("per-message length ratio (median)", |m| {
            fmt(m.line_length_ratio_median)
        }),
        ("full-stop rate", |m| fmt(m.full_stop_rate)),
        ("question rate", |m| fmt(m.question_rate)),
        ("opening boilerplate rate", |m| fmt(m.opening_rate)),
        ("leaks (system texts)", |m| m.leaks.to_string()),
        ("option lists", |m| m.option_lists.to_string()),
        ("errors", |m| m.errors.to_string()),
        ("unstable cases", |m| m.unstable_cases.to_string()),
    ];
    let mut s = format!(
        "| metric | {} |\n|---|{}\n",
        cols.iter().map(|(n, _)| *n).collect::<Vec<_>>().join(" | "),
        "---|".repeat(cols.len())
    );
    for (name, get) in lines {
        s.push_str(&format!(
            "| {name} | {} |\n",
            cols.iter()
                .map(|(_, m)| get(m))
                .collect::<Vec<_>>()
                .join(" | ")
        ));
    }
    s
}

fn check(a: &Args) {
    let cases: Vec<Case> = read(need(a, "cases"));
    let labels: Vec<Label> = read(need(a, "labels"));
    let outputs: Vec<Output> = read(need(a, "outputs"));
    let corpus: Vec<OwnerLine> = one(a, "corpus").map(read).unwrap_or_default();
    let names = names(a);
    let styles = re::owner_styles(&corpus, &names);
    let p = CheckParams {
        names,
        system: system_texts(),
        live_secs: one(a, "live-secs")
            .and_then(|n| n.parse().ok())
            .unwrap_or(re::LIVE_WINDOW_SECS),
        latency_secs: one(a, "latency-secs").and_then(|n| n.parse().ok()),
    };
    let rows = re::rows(&cases, &labels, &outputs, &styles, &p);
    if let Some(path) = one(a, "rows") {
        write_jsonl(path, &rows);
    }
    let by = |split: Option<&str>, rows: &[re::Row]| {
        re::metrics(rows.iter().filter(|r| split.is_none_or(|s| r.split == s)))
    };
    let mut report = BTreeMap::new();
    for (k, s) in [("all", None), ("dev", Some("dev")), ("test", Some("test"))] {
        report.insert(k, json!(by(s, &rows)));
    }
    let owner: BTreeMap<&str, Value> = styles.iter().map(|(s, st)| (s.code(), json!(st))).collect();
    let mut md = String::new();
    match one(a, "against") {
        Some(path) => {
            let before: Vec<Output> = read(path);
            let base = re::rows(&cases, &labels, &before, &styles, &p);
            for (k, s) in [("all", None), ("dev", Some("dev")), ("test", Some("test"))] {
                let (b, n) = (by(s, &base), by(s, &rows));
                md.push_str(&format!(
                    "**{k}**\n\n{}\n",
                    table(&[("baseline", &b), ("this run", &n)])
                ));
            }
            let broke = re::regressions(&base, &rows);
            let fixed = re::regressions(&rows, &base);
            md.push_str(&format!(
                "Right before, wrong now: {}\n\nWrong before, right now: {}\n",
                if broke.is_empty() {
                    "none".into()
                } else {
                    broke.join(", ")
                },
                if fixed.is_empty() {
                    "none".into()
                } else {
                    fixed.join(", ")
                }
            ));
            report.insert("regressions", json!(broke));
            report.insert("fixed", json!(fixed));
        }
        None => {
            for (k, s) in [("all", None), ("dev", Some("dev")), ("test", Some("test"))] {
                md.push_str(&format!(
                    "**{k}**\n\n{}\n",
                    table(&[("this run", &by(s, &rows))])
                ));
            }
        }
    }
    let wrong: Vec<&str> = rows
        .iter()
        .filter(|r| !r.correct)
        .map(|r| r.case_id.as_str())
        .collect();
    md.push_str(&format!(
        "\nWrong decisions: {}\n",
        if wrong.is_empty() {
            "none".into()
        } else {
            wrong.join(", ")
        }
    ));
    report.insert("owner_style", json!(owner));
    report.insert(
        "params",
        json!({"live_secs": p.live_secs, "latency_secs": p.latency_secs}),
    );
    println!("{}", serde_json::to_string_pretty(&report).expect("json"));
    println!("\n{md}");
}

// ---------------------------------------------------------------- gate, sweep

fn secs(a: &Args, k: &str) -> Option<i64> {
    one(a, k).map(|n| {
        n.parse()
            .unwrap_or_else(|_| fail(&format!("--{k}: a number")))
    })
}

fn gate_params(a: &Args) -> re::GateParams {
    let d = re::GateParams::default();
    re::GateParams {
        live_ms: secs(a, "live-secs").map_or(d.live_ms, |s| s * 1000),
        gap_ms: secs(a, "gap-secs").map_or(d.gap_ms, |s| s * 1000),
        latency_secs: secs(a, "latency-secs").unwrap_or(d.latency_secs),
    }
}

fn gate(a: &Args) {
    let cases: Vec<Case> = read(need(a, "cases"));
    let outputs: Vec<Output> = read(need(a, "outputs"));
    let mut p = gate_params(a);
    if secs(a, "latency-secs").is_none() {
        p.latency_secs = re::median_latency(&cases).unwrap_or(0);
    }
    let (outs, gated) = re::apply_gates(&cases, &outputs, &p);
    write_jsonl(need(a, "out"), &outs);
    for g in gated.iter().filter(|g| g.gate.is_some() || g.closed_later) {
        println!("{}", serde_json::to_string(g).expect("json"));
    }
}

fn sweep(a: &Args) {
    let cases: Vec<Case> = read(need(a, "cases"));
    let labels: Vec<Label> = read(need(a, "labels"));
    let outputs: Vec<Output> = read(need(a, "outputs"));
    let d = re::GateParams::default();
    let nums = |k: &str, def: i64| -> Vec<i64> {
        let v: Vec<i64> = list(a, k)
            .iter()
            .map(|n| {
                n.parse()
                    .unwrap_or_else(|_| fail(&format!("--{k}: numbers")))
            })
            .collect();
        if v.is_empty() { vec![def] } else { v }
    };
    let latency = secs(a, "latency-secs")
        .or_else(|| re::median_latency(&cases))
        .unwrap_or(0);
    let p = CheckParams {
        names: names(a),
        system: system_texts(),
        live_secs: re::LIVE_WINDOW_SECS,
        latency_secs: Some(latency),
    };
    let styles = BTreeMap::new();
    let base = re::rows(&cases, &labels, &outputs, &styles, &p);
    let m = re::metrics(&base);
    println!(
        "| live s | gap s | decision accuracy | act recall | act precision | presence-skip recall | gated | merged | opened, then closed by you | right before, wrong now |\n|---|---|---|---|---|---|---|---|---|---|"
    );
    println!(
        "| (no gate) | | {} | {} | {} | {} | 0 | 0 | | |",
        fmt(Some(m.decision_accuracy)),
        fmt(m.act_recall),
        fmt(m.act_precision),
        fmt(m.presence_skip_recall)
    );
    for live in nums("live", d.live_ms / 1000) {
        for gap in nums("gap", d.gap_ms / 1000) {
            let gp = re::GateParams {
                live_ms: live * 1000,
                gap_ms: gap * 1000,
                latency_secs: latency,
            };
            let (outs, gated) = re::apply_gates(&cases, &outputs, &gp);
            let rows = re::rows(&cases, &labels, &outs, &styles, &p);
            let m = re::metrics(&rows);
            let n = |f: &dyn Fn(&re::Gated) -> bool| gated.iter().filter(|g| f(g)).count();
            let broke = re::regressions(&base, &rows);
            println!(
                "| {live} | {gap} | {} | {} | {} | {} | {} | {} | {} | {} |",
                fmt(Some(m.decision_accuracy)),
                fmt(m.act_recall),
                fmt(m.act_precision),
                fmt(m.presence_skip_recall),
                n(&|g| g.gate.as_deref().is_some_and(|x| x != "merged")),
                n(&|g| g.gate.as_deref() == Some("merged")),
                n(&|g| g.closed_later),
                if broke.is_empty() {
                    "none".into()
                } else {
                    broke.join(", ")
                }
            );
        }
    }
}

// ---------------------------------------------------------------- judge

async fn judge(a: &Args, scratch: &Path) {
    let cases: Vec<Case> = read(need(a, "cases"));
    let outputs: Vec<Output> = read(need(a, "outputs"));
    let out_path = need(a, "out").to_string();
    let models = list(a, "models");
    if models.is_empty() {
        fail("--models is required: the judge's own models");
    }
    let drafters: HashSet<&str> = outputs
        .iter()
        .map(|o| o.model.as_str())
        .filter(|m| !m.is_empty())
        .collect();
    if let Some(m) = models.iter().find(|m| drafters.contains(m.as_str())) {
        fail(&format!(
            "{m} drafted some of these answers; the judge must be another model"
        ));
    }
    let corpus: Vec<OwnerLine> = one(a, "corpus").map(read).unwrap_or_default();
    let styles = re::owner_styles(&corpus, &names(a));
    let (gw, described) = gateway(&Route::default(), a, scratch);
    eprintln!("reply_eval: judging on {described}");
    let by_case: BTreeMap<&str, &Case> = cases.iter().map(|c| (c.case_id.as_str(), c)).collect();
    let sem = Arc::new(tokio::sync::Semaphore::new(
        one(a, "concurrency")
            .and_then(|n| n.parse().ok())
            .unwrap_or(4),
    ));
    let mut tasks = tokio::task::JoinSet::new();
    for o in outputs.iter().filter(|o| o.outcome != "error") {
        let Some(case) = by_case.get(o.case_id.as_str()) else {
            continue;
        };
        let style = re::style_for(&styles, re::situation(case)).map(|(_, s)| s.clone());
        let prompt = re::judge_prompt(case, o, style.as_ref(), HISTORY);
        let (gw, sem, o) = (gw.clone(), sem.clone(), o.clone());
        tasks.spawn(async move {
            let _permit = sem.acquire_owned().await.expect("semaphore");
            let r = steve_modules::gateway::purpose(
                "compose",
                gw.run(Tier::Smart, &prompt, compose::TIMEOUT),
            )
            .await;
            let (j, model, error) = match r {
                Ok(out) => (re::parse_judgment(&out.text), out.model, None),
                Err(e) => (None, String::new(), Some(e)),
            };
            json!({"case_id": o.case_id, "sample": o.sample, "outcome": o.outcome,
                   "judgment": j, "judge_model": model, "error": error})
        });
    }
    let mut rows = Vec::new();
    while let Some(r) = tasks.join_next().await {
        rows.push(r.expect("task"));
    }
    rows.sort_by_key(|r| {
        (
            r["case_id"].as_str().unwrap_or("").to_string(),
            r["sample"].as_u64(),
        )
    });
    let voice: Vec<f64> = rows
        .iter()
        .filter_map(|r| r["judgment"]["voice"].as_f64())
        .collect();
    let rate = |k: &str| {
        let v: Vec<bool> = rows
            .iter()
            .filter_map(|r| r["judgment"][k].as_bool())
            .collect();
        (!v.is_empty()).then(|| v.iter().filter(|x| **x).count() as f64 / v.len() as f64)
    };
    eprintln!(
        "{}",
        json!({"judged": rows.len(), "understanding": rate("understanding"),
               "voice_mean": (!voice.is_empty()).then(|| voice.iter().sum::<f64>() / voice.len() as f64),
               "card_answerable": rate("card_answerable")})
    );
    write_jsonl(&out_path, &rows);
}
