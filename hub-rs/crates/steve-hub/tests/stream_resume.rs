//! The `/api/state` watermark (`x-steve-cursor`) and the resumable `/api/stream?after=` on the wired Hub:
//! the watermark only moves with changes, a resume replays what changed after it (with ids, then
//! `stream.gap`), a resume at the last id replays nothing, a foreign epoch resets once, and another
//! workspace's frames never appear. Temp STEVE_HOME, adapters `/usr/bin/false`, no real host.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use serde_json::{Value, json};
use steve_hub::api::AppState;
use steve_hub::bus::{Bus, Cursor};
use steve_hub::config::Config;

struct Env {
    _home: tempfile::TempDir,
    base: String,
    http: reqwest::Client,
    cookie: String,
}

/// One SSE frame: its `id:` (if any) and its `data:` JSON.
#[derive(Debug)]
struct Sse {
    id: Option<String>,
    data: Value,
}

impl Env {
    async fn call(&self, method: &str, path: &str, ws: &str, body: Option<Value>) -> (u16, Value) {
        let m = reqwest::Method::from_bytes(method.as_bytes()).expect("method");
        let mut req = self
            .http
            .request(m, format!("{}{path}", self.base))
            .header("cookie", &self.cookie)
            .header("x-steve-ws", ws);
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

    /// `/api/state`'s watermark.
    async fn cursor(&self, ws: &str) -> String {
        let r = self
            .http
            .get(format!("{}/api/state", self.base))
            .header("cookie", &self.cookie)
            .header("x-steve-ws", ws)
            .send()
            .await
            .expect("state");
        assert_eq!(r.status().as_u16(), 200);
        let c = r
            .headers()
            .get("x-steve-cursor")
            .and_then(|v| v.to_str().ok())
            .expect("x-steve-cursor")
            .to_string();
        let body: Value = r.json().await.expect("state json");
        assert!(body["items"].is_array());
        c
    }

    /// The frames `/api/stream` sends within `for_ms` (pings dropped).
    async fn frames(&self, ws: &str, query: &str, for_ms: u64) -> Vec<Sse> {
        let r = self
            .http
            .get(format!("{}/api/stream{query}", self.base))
            .header("cookie", &self.cookie)
            .header("x-steve-ws", ws)
            .send()
            .await
            .expect("stream");
        let mut body = r.bytes_stream();
        let mut buf = String::new();
        let deadline = tokio::time::Instant::now() + Duration::from_millis(for_ms);
        while let Ok(Some(Ok(chunk))) = tokio::time::timeout_at(deadline, body.next()).await {
            buf.push_str(&String::from_utf8_lossy(&chunk));
        }
        buf.split("\n\n")
            .filter_map(|f| {
                let mut id = None;
                let mut data = None;
                for line in f.lines() {
                    if let Some(i) = line.strip_prefix("id: ") {
                        id = Some(i.to_string());
                    } else if let Some(d) = line.strip_prefix("data: ") {
                        data = serde_json::from_str(d).ok();
                    }
                }
                Some(Sse { id, data: data? })
            })
            .collect()
    }
}

async fn start() -> Env {
    steve_modules::net::forbid_in_this_process();
    let home = tempfile::tempdir().expect("tmp");
    let h = home.path().to_path_buf();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let port = listener.local_addr().expect("addr").port();
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
    }
}

fn n_of(c: &str) -> u64 {
    Cursor::parse(c).expect("cursor").n
}

/// The watermark once it is past `c` (the dispatcher sends a frame shortly after its write commits, and a
/// body computed in between keeps the older watermark until the cache's TTL).
async fn past(e: &Env, ws: &str, c: &str) -> String {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let next = e.cursor(ws).await;
        if n_of(&next) > n_of(c) || tokio::time::Instant::now() > deadline {
            return next;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn state_watermark_and_resumable_stream() {
    let e = start().await;
    let (s, item) = e
        .call(
            "POST",
            "/api/items",
            "default",
            Some(json!({ "goal": "x", "title": "x", "bot": "steve", "project": null, "mode": "propose" })),
        )
        .await;
    assert_eq!(s, 200, "{item}");
    let id = item["id"].as_str().expect("item id").to_string();
    let (s, w) = e
        .call(
            "POST",
            "/api/workspaces",
            "default",
            Some(json!({ "name": "另一个" })),
        )
        .await;
    assert_eq!(s, 200, "{w}");
    let ws2 = w["id"].as_str().expect("ws id").to_string();

    // the watermark parses and stays put while nothing changes
    tokio::time::sleep(Duration::from_millis(300)).await;
    let c = e.cursor("default").await;
    assert!(Cursor::parse(&c).is_some(), "{c}");
    assert_eq!(e.cursor("default").await, c);

    // a change moves it
    let (s, _) = e
        .call(
            "PATCH",
            &format!("/api/items/{id}"),
            "default",
            Some(json!({ "title": "改过" })),
        )
        .await;
    assert_eq!(s, 200);
    let (s, _) = e
        .call(
            "POST",
            "/api/contacts",
            &ws2,
            Some(json!({ "name": "李四" })),
        )
        .await;
    assert_eq!(s, 200);
    let after = past(&e, "default", &c).await;
    assert!(n_of(&after) > n_of(&c), "{c} → {after}");

    // resuming from the first watermark replays the item frame with a larger id, then one gap
    let got = e.frames("default", &format!("?after={c}"), 800).await;
    let hit = got
        .iter()
        .find(|f| f.data["type"] == "item" && f.data["id"] == id.as_str())
        .unwrap_or_else(|| panic!("item frame replayed: {got:?}"));
    let hit_id = hit.id.as_deref().expect("replayed frames carry an id");
    assert!(n_of(hit_id) > n_of(&c), "{hit_id} after {c}");
    assert_eq!(
        got.iter()
            .filter(|f| f.data["type"] == "stream.gap")
            .count(),
        1,
        "{got:?}"
    );
    assert!(
        got.iter()
            .filter(|f| f.data["type"] == "stream.gap")
            .all(|f| f.id.is_none())
    );
    assert!(!got.iter().any(|f| f.data["type"] == "stream.reset"));
    // another workspace's frames stay there
    assert!(!got.iter().any(|f| f.data["ws"] == ws2.as_str()), "{got:?}");
    let theirs = e.frames(&ws2, &format!("?after={c}"), 800).await;
    assert!(
        theirs
            .iter()
            .any(|f| f.data["type"] == "party" && f.data["ws"] == ws2.as_str()),
        "{theirs:?}"
    );

    // reconnecting with the last id seen replays nothing
    let last = got
        .iter()
        .filter_map(|f| f.id.clone())
        .next_back()
        .expect("an id");
    let again = e.frames("default", &format!("?after={last}"), 500).await;
    assert!(
        again.iter().all(|f| f.data["type"] == "stream.gap"),
        "{again:?}"
    );

    // a cursor from another epoch (a Hub restart) resets exactly once, at the head
    let stale = format!("ep_0000000000000000:{}", n_of(&after));
    let reset = e.frames("default", &format!("?after={stale}"), 500).await;
    let resets: Vec<&Sse> = reset
        .iter()
        .filter(|f| f.data["type"] == "stream.reset")
        .collect();
    assert_eq!(resets.len(), 1, "{reset:?}");
    let head = resets[0].id.as_deref().expect("reset id");
    assert_eq!(
        Cursor::parse(head).map(|c| c.epoch),
        Cursor::parse(&after).map(|c| c.epoch)
    );
    assert!(!reset.iter().any(|f| f.data["type"] == "stream.gap"));

    // Last-Event-ID works like ?after=
    let r = e
        .http
        .get(format!("{}/api/stream", e.base))
        .header("cookie", &e.cookie)
        .header("x-steve-ws", "default")
        .header("last-event-id", &stale)
        .send()
        .await
        .expect("stream");
    let mut body = r.bytes_stream();
    let first = tokio::time::timeout(Duration::from_secs(5), body.next())
        .await
        .expect("in time")
        .expect("chunk")
        .expect("bytes");
    assert!(
        String::from_utf8_lossy(&first).contains("\"type\":\"stream.reset\""),
        "{first:?}"
    );

    // without a cursor: no ids and no new frame types, as before
    let legacy = {
        let probe = e.frames("default", "", 1200);
        let change = async {
            tokio::time::sleep(Duration::from_millis(300)).await;
            e.call(
                "PATCH",
                &format!("/api/items/{id}"),
                "default",
                Some(json!({ "title": "又改" })),
            )
            .await
        };
        let (frames, (s, _)) = tokio::join!(probe, change);
        assert_eq!(s, 200);
        frames
    };
    assert!(
        legacy
            .iter()
            .any(|f| f.data["type"] == "item" && f.data["id"] == id.as_str()),
        "{legacy:?}"
    );
    assert!(legacy.iter().all(|f| f.id.is_none()), "{legacy:?}");
    assert!(
        !legacy.iter().any(|f| f.data["type"]
            .as_str()
            .is_some_and(|t| t.starts_with("stream."))),
        "{legacy:?}"
    );
}
