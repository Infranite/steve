//! In-process HTTP integration tests for P1-28 observability endpoints:
//! - `GET /api/stats/breakdown?by=node` & `by=session` + `GET /api/stats/series?metric=cost&group=node|session`
//!   conserve exact totals against `GET /api/stats/summary` (0 error).
//! - `GET /api/vitals` preserves `process.disk_written_bytes`, `adapter_disk_written_bytes`,
//!   `session_vitals`, and `tokens.{total_cost_usd, today_cost_usd}`.
//! - `GET /api/snapshot?kinds=session`, `GET /api/views?kind=session`, and `GET /api/views/page?kind=session&status=ended`
//!   enrich `SessionView` with cumulative `tokens`, `cost_usd`, and live `vitals` for active sessions.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::extract::ConnectInfo;
use axum::http::{Method, Request, header};
use serde_json::{Value, json};
use steve_hub::api::AppState;
use steve_hub::bus::Bus;
use steve_hub::config::Config;
use steve_hub::metrics;
use steve_hub::nodes::vitals;
use steve_node_api::testkit::FakeNode;
use steve_node_api::{
    Node, NodeMeters, NodeVitals, ProcessMeters, SessionId, SessionVitals, Timestamp,
};
use steve_store::gateway_ledger::{CallBegin, CallFinish};
use steve_store::testkit::stats::{bind_session_node, seed_active_node};
use steve_store::{
    ModelUsage, NewBot, NewSession, SessionPatch, Store, Tokens, TurnBegin, TurnFinish, WS,
};
use tower::ServiceExt as _;

const LOOPBACK_PEER: ([u8; 4], u16) = ([127, 0, 0, 1], 18798);

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
) -> (u16, axum::http::HeaderMap, String) {
    let mut b = Request::builder()
        .method(Method::from_bytes(method.as_bytes()).expect("method"))
        .uri(path)
        .header(header::HOST, "127.0.0.1:18798");
    if let Some(c) = cookie {
        b = b.header(header::COOKIE, c);
    }
    let mut req = b.body(Body::empty()).expect("request");
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(LOOPBACK_PEER)));
    let resp = app.clone().oneshot(req).await.expect("oneshot");
    let status = resp.status().as_u16();
    let headers = resp.headers().clone();
    let bytes = to_bytes(resp.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    (
        status,
        headers,
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

impl Env {
    async fn get_json(&self, path: &str) -> Value {
        let (status, _, text) = send_req(&self.app, Some(&self.cookie), "GET", path).await;
        assert_eq!(status, 200, "GET {path} failed ({status}): {text}");
        serde_json::from_str(&text)
            .unwrap_or_else(|e| panic!("invalid JSON for {path}: {e}\n{text}"))
    }
}

/// Howard Hinnant's civil-from-days conversion for UTC `YYYY-MM-DD` without extra test deps.
fn utc_day(ms: i64) -> String {
    let z = ms.div_euclid(86_400_000) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    format!("{year:04}-{m:02}-{d:02}")
}

async fn start_hub() -> Env {
    steve_modules::net::forbid_in_this_process();
    metrics::install();
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
    steve_hub::app::boot(&st).await.expect("boot");
    let app = steve_hub::app::app(st, w.auth, w.mcp, w.module_routes, w.root_routes);
    let (status, headers, body) = send_req(&app, None, "GET", "/auth/dev").await;
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

fn tok(input: i64, output: i64, cache_read: i64) -> Tokens {
    Tokens {
        input: Some(input),
        output: Some(output),
        cache_read: Some(cache_read),
        cache_write: Some(0),
        reasoning: None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stats_node_session_vitals_and_session_views_contract() {
    let env = start_hub().await;
    let s = &env.store;
    let now_ms = steve_store::now();
    let today = utc_day(now_ms);

    s.settings()
        .set(
            WS,
            &serde_json::Map::from_iter([("stats.tz".into(), json!("UTC"))]),
            "test",
        )
        .unwrap();
    s.bots()
        .seed(
            "dev",
            true,
            &NewBot {
                name: "Dev".into(),
                harness: "claude-code".into(),
                model: Some("claude-opus-5-5".into()),
                ..Default::default()
            },
            WS,
        )
        .unwrap();
    seed_active_node(s, "node_gpu", "local", "GPU Box").unwrap();

    // Active session on local Node + ended session on remote Node.
    let s_live = s
        .sessions()
        .create(&NewSession {
            workspace_id: WS.into(),
            bot_id: Some("dev".into()),
            harness: "claude-code".into(),
            model: Some("claude-opus-5-5".into()),
            cwd: ".".into(),
            title: "Active Local Session".into(),
            ..Default::default()
        })
        .unwrap()
        .id;
    let s_ended = s
        .sessions()
        .create(&NewSession {
            workspace_id: WS.into(),
            bot_id: Some("dev".into()),
            harness: "claude-code".into(),
            model: Some("claude-opus-5-5".into()),
            cwd: ".".into(),
            title: "Ended GPU Session".into(),
            ..Default::default()
        })
        .unwrap()
        .id;
    s.sessions()
        .patch(
            &s_live,
            &SessionPatch {
                status: Some("running".into()),
                ..Default::default()
            },
        )
        .unwrap();
    s.sessions()
        .patch(
            &s_ended,
            &SessionPatch {
                status: Some("ended".into()),
                ..Default::default()
            },
        )
        .unwrap();
    bind_session_node(s, &s_live, "local", "running").unwrap();
    bind_session_node(s, &s_ended, "node_gpu", "ended").unwrap();

    // Record turns + a gateway call today.
    for (sid, inp, out) in [(&s_live, 100_000, 20_000), (&s_ended, 200_000, 30_000)] {
        let tid = s
            .stats()
            .turn_begin(&TurnBegin {
                workspace_id: WS.into(),
                session_id: sid.clone(),
                bot_id: Some("dev".into()),
                harness: "claude-code".into(),
                started: now_ms - 5_000,
                ..Default::default()
            })
            .unwrap();
        s.stats()
            .turn_finish(&TurnFinish {
                turn_id: tid,
                ended: now_ms - 1_000,
                day: today.clone(),
                model: "claude-opus-5-5".into(),
                stop_reason: Some("end_turn".into()),
                usage: vec![ModelUsage {
                    model: "claude-opus-5-5".into(),
                    tokens: tok(inp, out, 10_000),
                    ..Default::default()
                }],
                ..Default::default()
            })
            .unwrap();
    }
    s.stats()
        .gateway_begin(&CallBegin {
            id: "gw_http".into(),
            workspace: WS.into(),
            purpose: "route".into(),
            node_id: Some("local".into()),
            started: now_ms - 3_000,
            day: today.clone(),
            ..Default::default()
        })
        .unwrap();
    s.stats()
        .gateway_finish(
            "gw_http",
            &CallFinish {
                ok: true,
                model: "claude-haiku-5-5".into(),
                harness: "claude-code".into(),
                attempts: 1,
                duration_ms: 400,
                failures: vec![],
                usage: vec![ModelUsage {
                    model: "claude-haiku-5-5".into(),
                    tokens: tok(25_000, 5_000, 0),
                    ..Default::default()
                }],
            },
        )
        .unwrap();

    // Feed live vitals for the local Node including `s_live`.
    let fake_local = FakeNode::new("local");
    let proc_written: u64 = 10_485_760;
    let adapter_written: u64 = 52_428_800;
    fake_local.set_vitals(NodeVitals {
        at: Timestamp(now_ms),
        cpus: Some(8),
        load: Some([1.2, 1.0, 0.8]),
        mem_total: Some(16 << 30),
        mem_available: Some(8 << 30),
        roots: vec![],
        sessions: 1,
        adapter_disk_written_bytes: adapter_written,
        session_vitals: vec![SessionVitals {
            session_id: SessionId::new(&s_live).unwrap(),
            cpu_seconds: 14.5,
            resident_bytes: 256 << 20,
            disk_written_bytes: adapter_written,
        }],
        meters: Some(NodeMeters {
            process: Some(ProcessMeters {
                cpu_seconds: 3.5,
                resident_bytes: 64 << 20,
                disk_read_bytes: Some(4096),
                disk_written_bytes: Some(proc_written),
                ..Default::default()
            }),
            ..Default::default()
        }),
    });
    let local_node: Arc<dyn Node> = fake_local;
    vitals::sample_once(Some(&local_node), None).await;

    // 1. HTTP /api/stats/summary vs breakdown(node/session) & series(cost, node/session): exact 0 error.
    let q = format!("range=custom&from={today}&to={today}");
    let summary = env.get_json(&format!("/api/stats/summary?{q}")).await;
    let sum_tokens = summary["usage"]["total"].as_i64().unwrap();
    let sum_cost = summary["spend"]["est"].as_f64().unwrap();
    assert_eq!(sum_tokens, 130_000 + 240_000 + 30_000);
    assert!(sum_cost > 0.0);

    for by in ["node", "session"] {
        let br = env
            .get_json(&format!("/api/stats/breakdown?{q}&by={by}"))
            .await;
        let rows = br["rows"].as_array().unwrap();
        let tok_sum: i64 = rows.iter().map(|r| r["total"].as_i64().unwrap()).sum();
        let cost_sum: f64 = rows.iter().map(|r| r["est"].as_f64().unwrap()).sum();
        assert_eq!(tok_sum, sum_tokens, "HTTP breakdown tokens by={by}");
        assert!(
            (cost_sum - sum_cost).abs() < 1e-12,
            "HTTP breakdown cost by={by}: {cost_sum} vs {sum_cost}"
        );

        let sr = env
            .get_json(&format!("/api/stats/series?{q}&metric=cost&group={by}"))
            .await;
        let ser_sum: f64 = sr["series"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|s| s["values"].as_array().unwrap())
            .filter_map(|v| v.as_f64())
            .sum();
        assert!(
            (ser_sum - sum_cost).abs() < 1e-12,
            "HTTP series cost group={by}: {ser_sum} vs {sum_cost}"
        );
    }

    // 2. HTTP /api/vitals preserves disk writes, session_vitals, and Node cost.
    let vr = env.get_json("/api/vitals").await;
    let local_vit = &vr["nodes"]["local"];
    assert_eq!(
        local_vit["latest"]["process"]["disk_written_bytes"].as_u64(),
        Some(proc_written)
    );
    assert_eq!(
        local_vit["latest"]["adapter_disk_written_bytes"].as_u64(),
        Some(adapter_written)
    );
    let svs = local_vit["latest"]["session_vitals"].as_array().unwrap();
    assert_eq!(svs.len(), 1);
    assert_eq!(svs[0]["session_id"].as_str(), Some(s_live.as_str()));
    assert!(local_vit["tokens"]["total_cost_usd"].as_f64().unwrap() > 0.0);
    assert!(local_vit["tokens"]["today_cost_usd"].as_f64().unwrap() > 0.0);

    // 3. HTTP /api/snapshot, /api/views, and /api/views/page enrich SessionView with tokens, cost_usd, vitals.
    let snap = env.get_json("/api/snapshot?kinds=session").await;
    let live_view = snap["views"]["session"]
        .as_array()
        .expect("snapshot views.session")
        .iter()
        .find(|x| x["id"].as_str() == Some(s_live.as_str()))
        .expect("live session in snapshot");
    assert_eq!(live_view["tokens"].as_i64(), Some(130_000));
    assert!(live_view["cost_usd"].as_f64().unwrap() > 0.0);
    assert_eq!(
        live_view["vitals"]["resident_bytes"].as_u64(),
        Some(256 << 20)
    );
    assert_eq!(
        live_view["vitals"]["disk_written_bytes"].as_u64(),
        Some(adapter_written)
    );

    let by_id = env
        .get_json(&format!("/api/views?kind=session&ids={s_live},{s_ended}"))
        .await;
    let by_id_sessions = by_id["views"].as_array().expect("by_id views");
    assert_eq!(by_id_sessions.len(), 2);
    let ended_by_id = by_id_sessions
        .iter()
        .find(|x| x["id"].as_str() == Some(s_ended.as_str()))
        .unwrap();
    assert_eq!(ended_by_id["tokens"].as_i64(), Some(240_000));
    assert!(ended_by_id["cost_usd"].as_f64().unwrap() > 0.0);
    assert!(
        ended_by_id.get("vitals").is_none(),
        "ended session has no live vitals"
    );

    let page = env
        .get_json("/api/views/page?kind=session&status=ended")
        .await;
    let ended_page = page["views"]
        .as_array()
        .expect("page views")
        .iter()
        .find(|x| x["id"].as_str() == Some(s_ended.as_str()))
        .expect("ended session in page");
    assert_eq!(ended_page["tokens"].as_i64(), Some(240_000));
    assert!(ended_page["cost_usd"].as_f64().unwrap() > 0.0);

    // 4. HTTP /api/system/health supports 15m/1h/24h windows, golden series, validation, and owner gate.
    // Sample a second snapshot after HTTP traffic so `series` has an interval between snapshots.
    vitals::sample_once(Some(&local_node), None).await;
    for win in ["15m", "1h", "24h"] {
        let health = env
            .get_json(&format!("/api/system/health?window={win}"))
            .await;
        assert_eq!(health["window"].as_str(), Some(win));
        assert!(health["sampled_at"].as_i64().unwrap_or(0) > 0);
        assert!(health["checks"].is_array());
        assert!(health["warnings"].is_array());
        let series = health["series"].as_array().expect("health series array");
        assert!(!series.is_empty(), "expected non-empty series for {win}");
        let last = series.last().unwrap();
        assert!(last["at"].as_i64().unwrap_or(0) > 0);
        assert!(last.get("http_p95").is_some());
        assert!(last.get("write_lock_wait_p95").is_some());
        assert!(last.get("hub_cpu").is_some());
        assert!(last.get("hub_memory").is_some());
    }
    let default_health = env.get_json("/api/system/health").await;
    assert_eq!(default_health["window"].as_str(), Some("15m"));

    let (bad_status, _, _) = send_req(
        &env.app,
        Some(&env.cookie),
        "GET",
        "/api/system/health?window=invalid",
    )
    .await;
    assert_eq!(bad_status, 400);

    let member = s
        .users()
        .create("obs_member", "Observer", None, "member")
        .unwrap();
    s.auth_sessions()
        .create(&member, "obs-member-session", 60_000)
        .unwrap();
    let (member_status, _, _) = send_req(
        &env.app,
        Some("steve_session=obs-member-session"),
        "GET",
        "/api/system/health?window=15m",
    )
    .await;
    assert_eq!(member_status, 403);
}
