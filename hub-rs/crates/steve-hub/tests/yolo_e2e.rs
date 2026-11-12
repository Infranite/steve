//! YOLO Y2 end to end on the wired Hub: a temp STEVE_HOME (fresh, so YOLO is not forced off), the
//! scripted ACP agent of steve-acp (`fake-acp.js`) as the claude-code adapter on the local Node, YOLO switched on over
//! the API. A tool call inside the item's own directory is answered `allow_once` by YOLO and logged
//! with `decided_by: "yolo"`; a delete outside it stays for the user. No real agent or host.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use steve_hub::api::AppState;
use steve_hub::bus::Bus;
use steve_hub::config::Config;

struct Env {
    _home: tempfile::TempDir,
    base: String,
    http: reqwest::Client,
    cookie: String,
    log: PathBuf,
    db: PathBuf,
}

impl Env {
    async fn call(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let m = reqwest::Method::from_bytes(method.as_bytes()).expect("method");
        let mut req = self
            .http
            .request(m, format!("{}{path}", self.base))
            .header("cookie", &self.cookie);
        if let Some(b) = body {
            req = req.json(&b);
        }
        let r = req.send().await.expect("request");
        let status = r.status().as_u16();
        let text = r.text().await.unwrap_or_default();
        (
            status,
            serde_json::from_str(&text).unwrap_or(Value::String(text)),
        )
    }

    async fn session(&self, id: &str) -> Value {
        let (s, v) = self.call("GET", &format!("/api/sessions/{id}"), None).await;
        assert_eq!(s, 200, "{v}");
        v
    }
}

async fn until<F: std::future::Future<Output = bool>>(secs: u64, mut f: impl FnMut() -> F) -> bool {
    let end = tokio::time::Instant::now() + Duration::from_secs(secs);
    while tokio::time::Instant::now() < end {
        if f().await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

async fn start() -> Env {
    steve_modules::net::forbid_in_this_process();
    let home = tempfile::tempdir().expect("tmp");
    let h = home.path().to_path_buf();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let port = listener.local_addr().expect("addr").port();
    assert_ne!(port, 8440);
    let fake = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../steve-acp/tests/fake-acp.js");
    let log = h.join("acp.log");
    // the adapter inherits the process env: the log path goes there
    // SAFETY: this file has one test, so one Hub per process sets it, before any adapter starts
    unsafe { std::env::set_var("FAKE_ACP_LOG", &log) };
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
        // a fresh temp home, not a production copy: YOLO may be on
        ("STEVE_NO_RESUME", "0".into()),
        // the local Node runs the sessions (the Hub runs none itself)
        ("STEVE_NODE_MODE", "local".into()),
        ("CLAUDE_CODE_EXECUTABLE", "/usr/bin/false".into()),
        ("STEVE_ADAPTER_CLAUDE", fake.display().to_string()),
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
    let bus = Bus::new();
    let w = steve_hub::wire::wire_with(&cfg, &bus, Arc::new(get.clone())).expect("wire");
    let st = AppState::new(cfg.clone(), w.backend, w.domain, bus).with_modules(w.modules);
    steve_hub::app::boot(&st).await.expect("boot");
    for job in w.jobs {
        tokio::spawn(job);
    }
    let app = steve_hub::app::app(st, w.auth, w.mcp, w.module_routes, w.root_routes);
    tokio::spawn(async move {
        let _ = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await;
    });
    let base = format!("http://127.0.0.1:{port}");
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("client");
    let r = http
        .get(format!("{base}/auth/dev"))
        .send()
        .await
        .expect("dev login");
    let cookie = r
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|c| c.starts_with("steve_session="))
        .map(|c| c.split(';').next().unwrap_or_default().to_string())
        .expect("session cookie");
    Env {
        _home: home,
        base,
        http,
        cookie,
        log,
        db: cfg.db_path(),
    }
}

/// An item whose first turn runs `goal` on the fake agent; returns its session id.
async fn item(e: &Env, goal: &str) -> String {
    let (_, state) = e.call("GET", "/api/state", None).await;
    let bot = state["bots"]
        .as_array()
        .and_then(|bs| bs.iter().find(|b| b["harness"] == "claude-code"))
        .and_then(|b| b["id"].as_str())
        .expect("a claude-code bot")
        .to_string();
    let (s, i) = e
        .call(
            "POST",
            "/api/items",
            Some(json!({"goal": goal, "title": "YOLO 测试", "bot": bot, "project": null})),
        )
        .await;
    assert_eq!(s, 200, "{i}");
    let iid = i["id"].as_str().expect("item id").to_string();
    let (_, ss) = e.call("GET", "/api/sessions", None).await;
    let sid = ss
        .as_array()
        .and_then(|ss| ss.iter().find(|s| s["item_id"] == iid.as_str()))
        .and_then(|s| s["id"].as_str())
        .unwrap_or_else(|| panic!("no session of {iid} in {ss}"))
        .to_string();
    assert!(
        until(10, || async { e.session(&sid).await["busy"] == true }).await,
        "the first turn runs"
    );
    sid
}

fn rows(db: &PathBuf, sql: &str, p: &[&dyn rusqlite::ToSql]) -> Vec<Vec<Value>> {
    let c = rusqlite::Connection::open(db).expect("db");
    c.busy_timeout(Duration::from_secs(5)).expect("timeout");
    let mut st = c.prepare(sql).expect("prepare");
    let n = st.column_count();
    st.query_map(p, |r| {
        Ok((0..n)
            .map(|i| match r.get_ref(i).expect("col") {
                rusqlite::types::ValueRef::Null => Value::Null,
                rusqlite::types::ValueRef::Integer(x) => json!(x),
                rusqlite::types::ValueRef::Real(x) => json!(x),
                rusqlite::types::ValueRef::Text(t) => json!(String::from_utf8_lossy(t)),
                rusqlite::types::ValueRef::Blob(_) => Value::Null,
            })
            .collect())
    })
    .expect("query")
    .collect::<rusqlite::Result<_>>()
    .expect("rows")
}

async fn texts(e: &Env, sid: &str) -> String {
    let (_, v) = e
        .call("GET", &format!("/api/sessions/{sid}/events"), None)
        .await;
    v.to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn yolo_answers_tool_calls_inside_the_worktree_and_leaves_the_rest() {
    let e = start().await;
    let (s, v) = e.call("PUT", "/api/yolo", Some(json!({"on": true}))).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["active"], true, "{v}");

    // inside: answered at once with the allow_once option
    let inside = item(&e, "写笔记 @permtool edit notes.txt").await;
    assert!(
        until(20, || async {
            texts(&e, &inside).await.contains("perm:selected:once")
        })
        .await,
        "the agent got allow_once: {}",
        texts(&e, &inside).await
    );
    let r = rows(
        &e.db,
        "SELECT d.status, d.result, a.outcome, a.rule, a.choice FROM decisions d JOIN yolo_actions a ON a.decision_id = d.id
          WHERE d.subject_type = 'tool_call' AND d.session_id = ?",
        &[&inside],
    );
    assert_eq!(r.len(), 1, "{r:?}");
    assert_eq!(r[0][0], "resolved");
    let result: Value = serde_json::from_str(r[0][1].as_str().unwrap()).unwrap();
    assert_eq!(result["decided_by"], "yolo", "{result}");
    // the Node's answer is Steve's own (it picks the adapter's allow_once option itself)
    assert_eq!(result["optionId"], "allow_once");
    assert_eq!(
        (r[0][2].clone(), r[0][3].clone(), r[0][4].clone()),
        (
            json!("decided"),
            json!("tool.worktree"),
            json!("allow_once")
        )
    );
    let who = rows(
        &e.db,
        "SELECT a.actor FROM audit a JOIN decisions d ON d.id = a.target
          WHERE a.action = 'decision.resolve' AND d.subject_type = 'tool_call' AND d.session_id = ?",
        &[&inside],
    );
    assert_eq!(who, vec![vec![json!("yolo")]]);

    // outside: a delete stays with you, the reason recorded once
    let outside = item(&e, "清理 @permtool delete ../../../outside.txt").await;
    assert!(
        until(20, || async {
            !rows(
                &e.db,
                "SELECT a.outcome FROM decisions d JOIN yolo_actions a ON a.decision_id = d.id
                  WHERE d.subject_type = 'tool_call' AND d.session_id = ?",
                &[&outside],
            )
            .is_empty()
        })
        .await,
        "YOLO looked at it"
    );
    tokio::time::sleep(Duration::from_millis(500)).await;
    let r = rows(
        &e.db,
        "SELECT d.status, a.outcome, a.reason FROM decisions d JOIN yolo_actions a ON a.decision_id = d.id
          WHERE d.subject_type = 'tool_call' AND d.session_id = ?",
        &[&outside],
    );
    assert_eq!(r.len(), 1, "{r:?}");
    assert_eq!(
        (r[0][0].clone(), r[0][1].clone()),
        (json!("open"), json!("declined"))
    );
    // the Node reports a delete as an action YOLO does not judge: it stays with you, said why
    assert!(r[0][2].as_str().unwrap().contains("留给你"), "{r:?}");
    assert!(!texts(&e, &outside).await.contains("perm:selected"));

    // #38: switching off leaves a live session of an item with its own directory in acceptEdits
    // (its mode is the same with YOLO on or off): no session/set_mode
    let set_modes = || {
        std::fs::read_to_string(&e.log)
            .unwrap_or_default()
            .lines()
            .filter(|l| l.contains("\"session/set_mode\""))
            .count()
    };
    let before = set_modes();
    let (s, v) = e.call("PUT", "/api/yolo", Some(json!({"on": false}))).await;
    assert_eq!(s, 200, "{v}");
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(set_modes(), before, "no mode switch for YOLO off");

    // 完全放开: only with an explicit confirm
    let (s, v) = e
        .call("PUT", "/api/yolo", Some(json!({"mode": "full"})))
        .await;
    assert_eq!(s, 400, "{v}");
    let bypass_modes = || {
        std::fs::read_to_string(&e.log)
            .unwrap_or_default()
            .lines()
            .filter(|l| l.contains("\"session/set_mode\"") && l.contains("\"bypassPermissions\""))
            .count()
    };
    assert_eq!(
        bypass_modes(),
        0,
        "never bypass before the user switched it on"
    );
    let (s, v) = e
        .call(
            "PUT",
            "/api/yolo",
            Some(json!({"mode": "full", "confirm": true})),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(
        (v["mode"].clone(), v["full_active"].clone()),
        (json!("full"), json!(true))
    );
    // running sessions switch in place
    assert!(
        until(10, || async { bypass_modes() > 0 }).await,
        "a live session got bypassPermissions"
    );
    // a new session starts in bypass mode: the harness asks nothing, so no card and no YOLO row
    let free = item(&e, "清理 @permtool delete ../../../outside2.txt").await;
    assert!(
        until(20, || async {
            texts(&e, &free).await.contains("perm:bypassed:")
        })
        .await,
        "the agent ran without asking: {}",
        texts(&e, &free).await
    );
    let r = rows(
        &e.db,
        "SELECT id FROM decisions WHERE subject_type = 'tool_call' AND session_id = ?",
        &[&free],
    );
    assert!(
        r.is_empty(),
        "no permission decisions under full access: {r:?}"
    );
    let audit = rows(
        &e.db,
        "SELECT action, json_extract(detail, '$.mode') FROM audit WHERE action LIKE 'yolo.%' ORDER BY rowid",
        &[],
    );
    assert!(
        audit.contains(&vec![json!("yolo.on"), json!("full")]),
        "{audit:?}"
    );

    // back to plain YOLO: the live sessions leave bypass
    let modes_before = set_modes();
    let (s, v) = e
        .call("PUT", "/api/yolo", Some(json!({"mode": "on"})))
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["full"], false);
    assert!(
        until(10, || async { set_modes() > modes_before }).await,
        "switched back in place"
    );
    let last_free_mode = std::fs::read_to_string(&e.log)
        .unwrap_or_default()
        .lines()
        .rfind(|l| l.contains("\"session/set_mode\""))
        .map(String::from)
        .unwrap_or_default();
    assert!(
        !last_free_mode.contains("bypassPermissions"),
        "{last_free_mode}"
    );

    // The harness's own permission mode decides what it runs without asking; Steve does not
    // second-guess it. A command it reports as run (plain ACP `tool_call` updates, no permission
    // request) runs on and the turn ends normally, even one naming launchctl: a read-only
    // `launchctl print` over ssh on another host used to stop turns.
    let cancels = || {
        std::fs::read_to_string(&e.log)
            .unwrap_or_default()
            .lines()
            .filter(|l| l.contains("\"session/cancel\""))
            .count()
    };
    let cancels_before = cancels();
    let ssh = "ssh -o BatchMode=yes macbook '/bin/launchctl print gui/501'";
    let unasked = item(&e, &format!("看看 @run {ssh} @say after")).await;
    assert!(
        until(30, || async {
            let t = texts(&e, &unasked).await;
            t.contains("ran:ssh -o BatchMode=yes macbook") && t.contains("after")
        })
        .await,
        "the command ran and the turn went on: {}",
        texts(&e, &unasked).await
    );
    // `busy` describes the current turn: takeover may already have started another one
    // by the time we poll. Check the durable completion of the turn exercised here.
    assert!(
        until(30, || async {
            texts(&e, &unasked)
                .await
                .contains("\"stopReason\":\"end_turn\"")
        })
        .await,
        "the turn completed normally; session: {}; events: {}",
        e.session(&unasked).await,
        texts(&e, &unasked).await
    );
    let t = texts(&e, &unasked).await;
    assert!(!t.contains("\"stopReason\":\"cancelled\""), "{t}");
    assert!(!t.contains("停下了这一轮"), "{t}");
    assert_eq!(cancels(), cancels_before, "no session/cancel");

    // what the harness does ask about is still answered by YOLO, and a push is not approved:
    // it stays with the user as a card
    let push = item(&e, "推送 @permexec git push origin main").await;
    assert!(
        until(20, || async {
            !rows(
                &e.db,
                "SELECT a.outcome FROM decisions d JOIN yolo_actions a ON a.decision_id = d.id
                  WHERE d.subject_type = 'tool_call' AND d.session_id = ?",
                &[&push],
            )
            .is_empty()
        })
        .await,
        "YOLO looked at it"
    );
    tokio::time::sleep(Duration::from_millis(500)).await;
    let r = rows(
        &e.db,
        "SELECT d.status, a.outcome FROM decisions d JOIN yolo_actions a ON a.decision_id = d.id
          WHERE d.subject_type = 'tool_call' AND d.session_id = ?",
        &[&push],
    );
    assert_eq!(r.len(), 1, "{r:?}");
    assert_eq!(r[0][0], json!("open"), "the card waits for the user: {r:?}");
    assert_ne!(r[0][1], json!("decided"), "{r:?}");
    assert!(!texts(&e, &push).await.contains("perm:selected"));

    // 等待审批时切换: plain YOLO leaves a write outside the worktree to you; switching to full
    // access while the harness waits answers that very request once (the tool is not run again,
    // the turn goes on), the page says the harness still runs in its old mode until the turn ends,
    // and only once the Node confirmed the new mode does it read as applied
    let waiting = item(
        &e,
        "复制 @permexec cp notes.txt /opt/steve-e2e/notes.txt @say mid @sleep 4000 @say after",
    )
    .await;
    let acts = |sid: String| {
        let db = e.db.clone();
        move || {
            rows(
                &db,
                "SELECT d.status, a.outcome, a.rule FROM decisions d JOIN yolo_actions a ON a.decision_id = d.id
                  WHERE d.subject_type = 'tool_call' AND d.session_id = ? ORDER BY a.rowid",
                &[&sid],
            )
        }
    };
    let waiting_acts = acts(waiting.clone());
    assert!(
        until(20, || async { !waiting_acts().is_empty() }).await,
        "YOLO looked at it"
    );
    assert_eq!(
        waiting_acts()[0][1],
        json!("declined"),
        "{:?}",
        waiting_acts()
    );
    let iid = e.session(&waiting).await["item_id"]
        .as_str()
        .expect("item")
        .to_string();
    let state = || async {
        let (s, v) = e.call("GET", &format!("/api/yolo/items/{iid}"), None).await;
        assert_eq!(s, 200, "{v}");
        v
    };
    assert_eq!(state().await["level"], "on");
    let (s, v) = e
        .call(
            "PUT",
            "/api/yolo",
            Some(json!({"mode": "full", "confirm": true})),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert!(
        until(20, || async {
            texts(&e, &waiting).await.contains("perm:selected:")
        })
        .await,
        "the waiting request was answered: {}",
        texts(&e, &waiting).await
    );
    let r = waiting_acts();
    assert_eq!(r.len(), 2, "{r:?}");
    assert_eq!(
        (r[1][0].clone(), r[1][1].clone(), r[1][2].clone()),
        (json!("resolved"), json!("decided"), json!("tool.full"))
    );
    let cards = rows(
        &e.db,
        "SELECT id FROM decisions WHERE subject_type = 'tool_call' AND session_id = ?",
        &[&waiting],
    );
    assert_eq!(cards.len(), 1, "one card, answered once: {cards:?}");
    let v = state().await;
    assert_eq!(
        (v["level"].clone(), v["source"].clone()),
        (json!("full"), json!("workspace")),
        "{v}"
    );
    let row = &v["sessions"][0];
    assert_eq!(row["wanted"], "full_access", "{v}");
    assert_ne!(
        row["applied"], "full_access",
        "not in force while the turn runs: {v}"
    );
    assert_eq!(row["status"], "after_turn", "{v}");
    assert_eq!(row["residual"], true, "{v}");
    assert!(
        until(30, || async {
            let v = state().await;
            v["sessions"][0]["status"] == "applied" && v["sessions"][0]["applied"] == "full_access"
        })
        .await,
        "confirmed by the Node after the turn: {}",
        state().await
    );
    let t = texts(&e, &waiting).await;
    assert_eq!(
        t.matches("perm:selected:").count(),
        1,
        "the tool was not asked again: {t}"
    );
    assert!(t.contains("after"), "{t}");

    // 关 → 完全放开: while YOLO is off a request waits and nothing is recorded; switching to full
    // access with the default “new cards only” window answers that very request once (it belongs to
    // the running turn), while one that would stop processes by name inside `find -exec` stays yours
    let (s, v) = e
        .call("PUT", "/api/yolo", Some(json!({"mode": "off"})))
        .await;
    assert_eq!(s, 200, "{v}");
    let off_ok = item(
        &e,
        "复制 @permexec cp notes.txt /opt/steve-e2e/off.txt @say after-off",
    )
    .await;
    let off_kill = item(
        &e,
        "停掉 @permexec find . -name x.pid -exec killall node ; @say after-kill",
    )
    .await;
    let open_cards = |sid: String| {
        let db = e.db.clone();
        move || {
            rows(
                &db,
                "SELECT id FROM decisions WHERE subject_type = 'tool_call' AND status = 'open' AND session_id = ?",
                &[&sid],
            )
        }
    };
    let (ok_open, kill_open) = (open_cards(off_ok.clone()), open_cards(off_kill.clone()));
    assert!(
        until(20, || async {
            ok_open().len() == 1 && kill_open().len() == 1
        })
        .await,
        "both requests wait for you"
    );
    let (ok_acts, kill_acts) = (acts(off_ok.clone()), acts(off_kill.clone()));
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        ok_acts().is_empty() && kill_acts().is_empty(),
        "off: YOLO records nothing"
    );
    let (s, v) = e
        .call(
            "PUT",
            "/api/yolo",
            Some(json!({"mode": "full", "confirm": true})),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert!(
        until(20, || async {
            texts(&e, &off_ok).await.contains("perm:selected:")
        })
        .await,
        "the waiting request was answered and the turn went on: {}",
        texts(&e, &off_ok).await
    );
    let r = ok_acts();
    assert_eq!(r.len(), 1, "{r:?}");
    assert_eq!(
        (r[0][0].clone(), r[0][1].clone(), r[0][2].clone()),
        (json!("resolved"), json!("decided"), json!("tool.full"))
    );
    assert_eq!(
        texts(&e, &off_ok).await.matches("perm:selected:").count(),
        1
    );
    assert!(
        until(20, || async { e.session(&off_ok).await["busy"] == false }).await,
        "the turn went on to its end"
    );
    assert!(
        until(20, || async { !kill_acts().is_empty() }).await,
        "YOLO looked at the other one"
    );
    let r = kill_acts();
    assert_eq!(
        (r[0][0].clone(), r[0][1].clone(), r[0][2].clone()),
        (json!("open"), json!("declined"), json!("tool.full")),
        "{r:?}"
    );
    assert!(!texts(&e, &off_kill).await.contains("perm:selected"));

    // ST-26: a session without a bot runs under full access; turning full access off asks its
    // harness back to asking (before, no mode was sent and the page read `applied` with an empty
    // `wanted` while the harness still ran in full access). Nothing reads as applied until the
    // Node confirmed `ask`.
    let nobot = item(&e, "笔记 @say nobot @sleep 1500 @say done").await;
    let c = rusqlite::Connection::open(&e.db).expect("db");
    c.busy_timeout(Duration::from_secs(5)).expect("timeout");
    c.execute("UPDATE sessions SET bot_id = NULL WHERE id = ?", [&nobot])
        .expect("no bot");
    let iid = e.session(&nobot).await["item_id"]
        .as_str()
        .expect("item")
        .to_string();
    let row = || async {
        let (s, v) = e.call("GET", &format!("/api/yolo/items/{iid}"), None).await;
        assert_eq!(s, 200, "{v}");
        v["sessions"][0].clone()
    };
    assert!(
        until(30, || async {
            let r = row().await;
            r["status"] == "applied" && r["applied"] == "full_access"
        })
        .await,
        "opened under full access: {}",
        row().await
    );
    assert_eq!(row().await["wanted_from"], "full_access");
    assert!(
        until(20, || async { e.session(&nobot).await["busy"] == false }).await,
        "its turn ended"
    );
    let (s, v) = e
        .call("PUT", "/api/yolo", Some(json!({"mode": "off"})))
        .await;
    assert_eq!(s, 200, "{v}");
    let r = row().await;
    assert_eq!(r["wanted"], "ask", "never an empty wanted: {r}");
    assert_eq!(r["wanted_from"], "node_default", "{r}");
    assert!(
        !(r["status"] == "applied" && r["applied"] == "full_access"),
        "not applied while the harness still runs full access: {r}"
    );
    assert!(
        until(30, || async {
            let r = row().await;
            r["status"] == "applied" && r["applied"] == "ask"
        })
        .await,
        "the Node confirmed ask: {}",
        row().await
    );
}
