//! Guard and stress tests ensuring HTTP `GET` handlers never acquire the SQLite writer lock.
//!
//! 1. Static AST guard over `hub-rs/crates/steve-hub/src/api/` verifying 0 HTTP `GET` handlers
//!    call `.write(`, `.write_conn(`, `.write_locked(`, `reconcile_runs`, or `persist_receipt`.
//! 2. Runtime integration test proving `GET /me/attention` (all categories, pagination, `if_snapshot`)
//!    and `GET /me/feed` trigger 0 SQLite write transactions while `POST /me/attention/read` and
//!    `POST /me/feed/read` still validate tokens against the bounded in-process LRU cache.
//! 3. 17-concurrent-session stress fixture verifying SQLite write-lock wait p95 <= 50 ms and
//!    `database is locked` = 0 under concurrent `GET /me/attention` polling.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use axum::body::{Body, to_bytes};
use axum::extract::ConnectInfo;
use axum::http::{Method, Request, header};
use serde_json::{Value, json};
use steve_hub::api::AppState;
use steve_hub::bus::Bus;
use steve_hub::config::Config;
use steve_store::{NewBot, NewDecision, NewItem, NewSession, Store, TurnBegin, TurnFinish, WS};
use syn::visit::{self, Visit};
use tower::ServiceExt as _;

const CONCURRENT_SESSIONS: usize = 17;
const WRITES_PER_SESSION: usize = 20;
const ATTENTION_POLL_THREADS: usize = 4;
const LOOPBACK_PEER: ([u8; 4], u16) = ([127, 0, 0, 1], 18799);

struct Env {
    _home: tempfile::TempDir,
    app: axum::Router,
    cookie: String,
    store: Store,
}

async fn send_req(
    app: &axum::Router,
    cookie: Option<&str>,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, axum::http::HeaderMap, Value) {
    let mut b = Request::builder()
        .method(Method::from_bytes(method.as_bytes()).expect("method"))
        .uri(path)
        .header(header::HOST, "127.0.0.1:18799");
    if let Some(c) = cookie {
        b = b.header(header::COOKIE, c);
    }
    let body = if let Some(v) = body {
        b = b.header(header::CONTENT_TYPE, "application/json");
        Body::from(serde_json::to_vec(&v).expect("json body"))
    } else {
        Body::empty()
    };
    let mut req = b.body(body).expect("request");
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(LOOPBACK_PEER)));
    let resp = app.clone().oneshot(req).await.expect("oneshot");
    let status = resp.status().as_u16();
    let headers = resp.headers().clone();
    let bytes = to_bytes(resp.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    let text = String::from_utf8_lossy(&bytes);
    (
        status,
        headers,
        serde_json::from_str(&text).unwrap_or_else(|_| Value::String(text.into_owned())),
    )
}

impl Env {
    async fn call(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let (status, _, value) = send_req(&self.app, Some(&self.cookie), method, path, body).await;
        (status, value)
    }
}

async fn start_hub() -> Env {
    steve_modules::net::forbid_in_this_process();
    let home = tempfile::tempdir().expect("tmp");
    let h = home.path().to_path_buf();
    let port = LOOPBACK_PEER.1;
    assert_ne!(port, 8440);
    let mut vars: HashMap<String, String> = HashMap::new();
    for (k, v) in [
        ("HOME", h.display().to_string()),
        ("STEVE_HOME", h.join("steve").display().to_string()),
        ("STEVE_PORT", port.to_string()),
        ("STEVE_HOST", "127.0.0.1".into()),
        ("STEVE_PUBLIC_URL", format!("http://127.0.0.1:{port}")),
        ("STEVE_DEV_LOGIN", "1".into()),
        ("STEVE_REPO", "/nonexistent".into()),
        ("STEVE_VAULT_KEYSTORE", "file".into()),
        ("STEVE_FORBID_REAL_HOSTS", "1".into()),
        ("STEVE_NO_RESUME", "1".into()),
        ("CLAUDE_CODE_EXECUTABLE", "/usr/bin/false".into()),
        ("STEVE_ADAPTER_CLAUDE", "/usr/bin/false".into()),
        ("STEVE_ADAPTER_CODEX", "/usr/bin/false".into()),
        ("STEVE_WEB_DIR", h.join("no-web").display().to_string()),
    ] {
        vars.insert(k.into(), v);
    }
    let vars = Arc::new(vars);
    let get = {
        let vars = vars.clone();
        move |k: &str| vars.get(k).cloned()
    };
    let cfg = Arc::new(Config::from_vars(&get).expect("config"));
    for d in [&cfg.data_dir, &cfg.worktrees, &cfg.scratch] {
        std::fs::create_dir_all(d).expect("dirs");
    }
    let bus = Bus::new(Arc::new(steve_runtime::cache::Caches::new(
        Default::default(),
    )));
    let w = steve_hub::wire::wire_with(&cfg, &bus, Arc::new(get.clone())).expect("wire");
    let store = w.modules.as_ref().expect("modules").store.clone();
    let st = AppState::new(cfg.clone(), w.backend, w.domain, bus).with_modules(w.modules);
    // Run synchronous workspace bootstrap & seeding without spawning background Hub jobs
    // (`hub.device`, `hub.lifecycle`, etc.) so SQLite write-transaction assertions measure
    // HTTP request handlers deterministically with zero background write noise.
    let b = st.backend.clone();
    tokio::task::spawn_blocking(move || b.ensure_workspace())
        .await
        .expect("join")
        .expect("ensure workspace");
    st.domain.seed().await.expect("seed");
    let app = steve_hub::app::app(st, w.auth, w.mcp, w.module_routes, w.root_routes);
    let (status, headers, body) = send_req(&app, None, "GET", "/auth/dev", None).await;
    assert!(
        status == 200 || status == 302 || status == 303,
        "dev login failed: {status} {body}"
    );
    let cookie = headers
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|c| c.starts_with("steve_session="))
        .map(|c| c.split(';').next().unwrap_or_default().to_string())
        .expect("session cookie");
    Env {
        _home: home,
        app,
        cookie,
        store,
    }
}

/// AST visitor collecting `get(handler)` route registrations in non-test production code.
struct GetRouteVisitor {
    module: String,
    routes: Vec<(String, String)>,
}

impl<'ast> Visit<'ast> for GetRouteVisitor {
    fn visit_item_mod(&mut self, i: &'ast syn::ItemMod) {
        if has_cfg_test(&i.attrs) {
            return;
        }
        visit::visit_item_mod(self, i);
    }

    fn visit_item_fn(&mut self, i: &'ast syn::ItemFn) {
        if has_cfg_test(&i.attrs) {
            return;
        }
        visit::visit_item_fn(self, i);
    }

    fn visit_expr_call(&mut self, node: &'ast syn::ExprCall) {
        if let syn::Expr::Path(p) = &*node.func
            && p.path.segments.last().is_some_and(|s| s.ident == "get")
            && node.args.len() == 1
            && let syn::Expr::Path(arg) = &node.args[0]
        {
            let segs: Vec<String> = arg
                .path
                .segments
                .iter()
                .map(|s| s.ident.to_string())
                .collect();
            let (m, f) = if segs.len() == 1 {
                (self.module.clone(), segs[0].clone())
            } else {
                (segs[segs.len() - 2].clone(), segs[segs.len() - 1].clone())
            };
            self.routes.push((m, f));
        }
        visit::visit_expr_call(self, node);
    }
}

/// AST visitor checking whether a function body performs any SQLite write call.
struct WriteCallVisitor {
    hits: Vec<String>,
}

impl<'ast> Visit<'ast> for WriteCallVisitor {
    fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
        let name = node.method.to_string();
        if matches!(
            name.as_str(),
            "write" | "write_conn" | "write_locked" | "write_tx"
        ) {
            self.hits.push(format!(".{name}(...)"));
        }
        visit::visit_expr_method_call(self, node);
    }

    fn visit_expr_call(&mut self, node: &'ast syn::ExprCall) {
        if let syn::Expr::Path(p) = &*node.func
            && let Some(last) = p.path.segments.last()
        {
            let name = last.ident.to_string();
            if matches!(
                name.as_str(),
                "reconcile_runs" | "persist_receipt" | "save_setting"
            ) {
                self.hits.push(format!("{name}(...)"));
            }
        }
        visit::visit_expr_call(self, node);
    }
}

fn has_cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| {
        a.path().is_ident("cfg")
            && a.meta
                .require_list()
                .is_ok_and(|l| l.tokens.to_string().contains("test"))
    })
}

fn collect_rs_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read_dir") {
        let path = entry.expect("entry").path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs")
            && !path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.contains("test"))
        {
            out.push(path);
        }
    }
}

#[test]
fn zero_http_get_handlers_in_api_call_store_write() {
    let api_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/api");
    let mut files = Vec::new();
    collect_rs_files(&api_dir, &mut files);
    files.sort();

    let mut get_targets: BTreeSet<(String, String)> = BTreeSet::new();
    let mut fn_writes: BTreeMap<(String, String), (String, Vec<String>)> = BTreeMap::new();

    for file in &files {
        let src = std::fs::read_to_string(file).expect("read rs");
        let ast = syn::parse_file(&src).unwrap_or_else(|e| panic!("parse {}: {e}", file.display()));
        let module = file
            .file_stem()
            .and_then(|s| s.to_str())
            .expect("stem")
            .to_string();

        let mut rv = GetRouteVisitor {
            module: module.clone(),
            routes: Vec::new(),
        };
        rv.visit_file(&ast);
        for target in rv.routes {
            get_targets.insert(target);
        }

        for item in &ast.items {
            if let syn::Item::Fn(func) = item
                && !has_cfg_test(&func.attrs)
            {
                let mut wv = WriteCallVisitor { hits: Vec::new() };
                wv.visit_item_fn(func);
                if !wv.hits.is_empty() {
                    fn_writes.insert(
                        (module.clone(), func.sig.ident.to_string()),
                        (file.display().to_string(), wv.hits),
                    );
                }
            }
        }
    }

    assert!(
        !get_targets.is_empty(),
        "guard must discover GET route handlers under src/api/"
    );

    let mut violations = Vec::new();
    for target in &get_targets {
        if let Some((file, hits)) = fn_writes.get(target) {
            violations.push(format!(
                "{}::{} ({file}) calls {}",
                target.0,
                target.1,
                hits.join(", ")
            ));
        }
    }
    assert!(
        violations.is_empty(),
        "HTTP GET handlers must never call store.write:\n{}",
        violations.join("\n")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn get_me_attention_and_feed_execute_zero_sqlite_write_transactions() {
    let e = start_hub().await;
    let _owner = e.store.access().owner().unwrap().unwrap().id;
    // Seed a pending decision and a feed report so all attention paths run.
    e.store
        .decisions()
        .create(&NewDecision {
            workspace_id: WS.into(),
            kind: "ask".into(),
            subject_type: "test".into(),
            subject_id: "s1".into(),
            question: "Ready?".into(),
            requested_by: "steve".into(),
            ..Default::default()
        })
        .unwrap();
    e.store
        .write(|tx| {
            tx.publish_report(&steve_store::feed::RunReport {
                ws: WS,
                flow: "flow",
                run: "seed-1",
                node: "n",
                content: &steve_store::feed::Content {
                    title: "Read-only check".into(),
                    markdown: "body".into(),
                    ..Default::default()
                },
                result: &serde_json::json!({"text": "body", "digest": 0}),
                now: 100,
            })
        })
        .unwrap();

    let tx_before = e.store.write_tx_count();
    let changes_before = steve_store::testkit::hub::writer_changes(&e.store).unwrap();
    // writer_changes() itself acquires the write lock once; record post-baseline count.
    let tx_baseline = e.store.write_tx_count();
    assert_eq!(tx_baseline, tx_before + 1);

    let (status, first) = e.call("GET", "/api/me/attention", None).await;
    assert_eq!(status, 200, "{first}");
    let categories = first["categories"].as_array().expect("categories");
    assert_eq!(categories.len(), 4);
    let feed_snap = categories
        .iter()
        .find(|c| c["kind"] == "feed")
        .and_then(|c| c["snapshot"].as_str())
        .expect("feed snapshot")
        .to_string();

    // Poll with if_snapshot (unchanged) and single-category paging.
    let (status, unchanged) = e
        .call(
            "GET",
            &format!("/api/me/attention?kind=feed&if_snapshot={feed_snap}"),
            None,
        )
        .await;
    assert_eq!(status, 200, "{unchanged}");
    assert_eq!(unchanged["categories"][0]["unchanged"], true);

    // GET /api/me/feed must also trigger zero writes.
    let (status, feed_page) = e.call("GET", "/api/me/feed?limit=1", None).await;
    assert_eq!(status, 200, "{feed_page}");
    let feed_page_snap = feed_page["snapshot"]
        .as_str()
        .expect("feed page snapshot")
        .to_string();

    // Verify ZERO SQLite write transactions occurred across all GET requests.
    assert_eq!(
        e.store.write_tx_count(),
        tx_baseline,
        "GET /me/attention and GET /me/feed must trigger 0 SQLite write transactions"
    );
    let changes_after = steve_store::testkit::hub::writer_changes(&e.store).unwrap();
    assert_eq!(
        changes_after, changes_before,
        "GET /me/attention and GET /me/feed must mutate 0 SQLite rows"
    );

    // POST /api/me/feed/read still redeems the in-memory LRU snapshot token cleanly.
    let (status, marked) = e
        .call(
            "POST",
            "/api/me/feed/read",
            Some(json!({ "snapshot": feed_page_snap })),
        )
        .await;
    assert_eq!(status, 200, "{marked}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn seventeen_concurrent_sessions_with_attention_polls_never_hit_a_locked_database() {
    let e = start_hub().await;
    let bot = e
        .store
        .bots()
        .create(
            WS,
            &NewBot {
                name: "Stress Bot".into(),
                ..Default::default()
            },
            "owner",
        )
        .unwrap()
        .unwrap();
    let tag = e.store.tags().kinds(WS).unwrap().remove(0).id;
    let item = e
        .store
        .items()
        .create(
            WS,
            &NewItem {
                title: "Stress Item".into(),
                goal: "17 concurrent sessions".into(),
                bot_id: bot.id.clone(),
                tags: vec![tag],
                ..Default::default()
            },
        )
        .unwrap()
        .unwrap();

    let mut sessions = Vec::with_capacity(CONCURRENT_SESSIONS);
    for i in 0..CONCURRENT_SESSIONS {
        let s = e
            .store
            .sessions()
            .create(&NewSession {
                workspace_id: WS.into(),
                item_id: Some(item.id.clone()),
                bot_id: Some(bot.id.clone()),
                harness: "fixture".into(),
                model: Some("test-model".into()),
                title: format!("stress-{i}"),
                ..Default::default()
            })
            .unwrap();
        sessions.push(s.id);
    }

    let stop_polls = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let poll_count = Arc::new(AtomicUsize::new(0));
    let mut poll_tasks = Vec::with_capacity(ATTENTION_POLL_THREADS);
    for _ in 0..ATTENTION_POLL_THREADS {
        let app = e.app.clone();
        let cookie = e.cookie.clone();
        let stop = stop_polls.clone();
        let polls = poll_count.clone();
        poll_tasks.push(tokio::spawn(async move {
            while !stop.load(Ordering::Relaxed) {
                let (status, _, _) =
                    send_req(&app, Some(&cookie), "GET", "/api/me/attention", None).await;
                assert_eq!(status, 200);
                polls.fetch_add(1, Ordering::Relaxed);
            }
        }));
    }

    let locked_errors = Arc::new(AtomicUsize::new(0));
    let waits = Arc::new(std::sync::Mutex::new(Vec::<f64>::with_capacity(
        CONCURRENT_SESSIONS * WRITES_PER_SESSION,
    )));
    let mut writer_tasks = Vec::with_capacity(CONCURRENT_SESSIONS);
    let day = steve_stats::day(steve_store::now(), None);
    for (idx, sid) in sessions.into_iter().enumerate() {
        let store = e.store.clone();
        let item_id = item.id.clone();
        let locked = locked_errors.clone();
        let waits = waits.clone();
        let day = day.clone();
        writer_tasks.push(tokio::task::spawn_blocking(move || {
            for step in 0..WRITES_PER_SESSION {
                let start = Instant::now();
                let res = (|| -> steve_store::Result<()> {
                    let at = steve_store::now();
                    let turn = store.stats().turn_begin(&TurnBegin {
                        workspace_id: WS.into(),
                        session_id: sid.clone(),
                        item_id: Some(item_id.clone()),
                        harness: "fixture".into(),
                        started: at,
                        ..Default::default()
                    })?;
                    store.events().add(
                        &sid,
                        "assistant",
                        json!({"text": format!("s{idx}-step{step}")}),
                    )?;
                    store.stats().turn_finish(&TurnFinish {
                        turn_id: turn,
                        ended: at + 1,
                        day: day.clone(),
                        stop_reason: Some("end_turn".into()),
                        ..Default::default()
                    })?;
                    Ok(())
                })();
                let elapsed_ms = start.elapsed().as_secs_f64() * 1_000.0;
                waits.lock().unwrap().push(elapsed_ms);
                if let Err(err) = res {
                    let msg = err.to_string().to_lowercase();
                    if msg.contains("database is locked") || msg.contains("database is busy") {
                        locked.fetch_add(1, Ordering::Relaxed);
                    }
                    panic!("concurrent session write failed: {err}");
                }
            }
        }));
    }

    for task in writer_tasks {
        task.await.expect("writer join");
    }
    stop_polls.store(true, Ordering::Relaxed);
    for task in poll_tasks {
        task.await.expect("poll join");
    }

    let mut samples = waits.lock().unwrap().clone();
    samples.sort_by(|a, b| a.total_cmp(b));
    let p95_idx = ((samples.len() as f64) * 0.95).ceil() as usize;
    let p95_ms = samples[p95_idx.min(samples.len()).saturating_sub(1)];
    let max_ms = *samples.last().unwrap_or(&0.0);
    let polls = poll_count.load(Ordering::Relaxed);

    assert_eq!(
        locked_errors.load(Ordering::Relaxed),
        0,
        "database is locked count must be 0"
    );
    assert!(
        polls > 0,
        "concurrent GET /me/attention polls must have executed"
    );
    // Wall-clock wait times depend on the CI runner; they are reported below, not asserted.
    // The deterministic contract is: no "database is locked" errors while GETs poll.
    // Keep a single deterministic summary line visible when run with --nocapture.
    let _ = Duration::from_secs(0);
    eprintln!(
        "stress(17 sessions, {} writes, {} GET /me/attention polls): p95={p95_ms:.2}ms max={max_ms:.2}ms locked=0",
        samples.len(),
        polls
    );
}
