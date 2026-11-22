//! Decisions as views on the wired Hub: `/api/snapshot` (from memory, ETag), `patch` frames on
//! `/api/stream` instead of thin `decision` events, `x-steve-rev` on a command, a snapshot right
//! after a command that already has it, and a resumed stream that replays the patch it missed.
//! Temp STEVE_HOME, adapters `/usr/bin/false`, no real host.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use serde_json::Value;
use steve_hub::api::AppState;
use steve_hub::bus::Bus;
use steve_hub::config::Config;

struct Env {
    _home: tempfile::TempDir,
    base: String,
    http: reqwest::Client,
    cookie: String,
    store: steve_store::Store,
}

impl Env {
    fn get(&self, path: &str) -> reqwest::RequestBuilder {
        self.http
            .get(format!("{}{path}", self.base))
            .header("cookie", &self.cookie)
            .header("x-steve-ws", "default")
    }

    async fn snapshot(&self) -> (String, Value) {
        let r = self
            .get("/api/snapshot?kinds=decision")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status().as_u16(), 200);
        let etag = r.headers()["etag"].to_str().unwrap().to_string();
        (etag, r.json().await.unwrap())
    }

    /// The frames `/api/stream?after=` sends within `for_ms`, each with when it arrived.
    async fn frames(
        &self,
        after: &str,
        for_ms: u64,
        ready: Option<tokio::sync::oneshot::Sender<()>>,
    ) -> Vec<(Instant, Value)> {
        let r = self
            .get(&format!("/api/stream?after={after}"))
            .send()
            .await
            .unwrap();
        if let Some(ready) = ready {
            ready.send(()).unwrap();
        }
        let mut body = r.bytes_stream();
        let (mut buf, mut out) = (String::new(), vec![]);
        let deadline = tokio::time::Instant::now() + Duration::from_millis(for_ms);
        while let Ok(Some(Ok(chunk))) = tokio::time::timeout_at(deadline, body.next()).await {
            buf.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(end) = buf.find("\n\n") {
                let frame: String = buf.drain(..end + 2).collect();
                for line in frame.lines() {
                    if let Some(d) = line.strip_prefix("data: ")
                        && let Ok(v) = serde_json::from_str(d)
                    {
                        out.push((Instant::now(), v));
                    }
                }
            }
        }
        out
    }
}

async fn start() -> Env {
    steve_modules::net::forbid_in_this_process();
    let home = tempfile::tempdir().expect("tmp");
    let h = home.path().to_path_buf();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
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
        std::fs::create_dir_all(d).unwrap();
    }
    let bus = Bus::new(Arc::new(steve_runtime::cache::Caches::new(
        Default::default(),
    )));
    let w = steve_hub::wire::wire_with(&cfg, &bus, Arc::new(get.clone())).expect("wire");
    let store = w.modules.as_ref().expect("modules").store.clone();
    let st = AppState::new(cfg.clone(), w.backend, w.domain, bus).with_modules(w.modules);
    steve_hub::app::boot(&st).await.expect("boot");
    for job in w.jobs {
        job.start(&st.jobs);
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
        .unwrap();
    let r = http.get(format!("{base}/auth/dev")).send().await.unwrap();
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
        store,
    }
}

fn ids(snapshot: &Value) -> Vec<&str> {
    snapshot["views"]["decision"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["id"].as_str().unwrap())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dismissed_decision_reaches_the_page_as_a_patch() {
    let e = start().await;
    let id = e
        .store
        .decisions()
        .create(&steve_store::NewDecision {
            workspace_id: "default".into(),
            kind: "ask".into(),
            subject_type: "test".into(),
            subject_id: "s".into(),
            question: "继续吗？".into(),
            requested_by: "steve".into(),
            ..Default::default()
        })
        .unwrap()
        .unwrap()
        .id;

    // the snapshot has it (as written, no translation), and answers 304 while nothing changes
    let (etag, snap) = e.snapshot().await;
    assert_eq!(ids(&snap), [id.as_str()]);
    assert_eq!(snap["views"]["decision"][0]["question"], "继续吗？");
    let cursor = snap["cursor"].as_str().unwrap().to_string();
    let r = e
        .get("/api/snapshot?kinds=decision")
        .header("if-none-match", &etag)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 304);

    // a command answers with the revision it committed; the patch follows within 200 ms, and the
    // thin decision event no longer reaches the page
    let (ready, opened) = tokio::sync::oneshot::channel();
    let stream = {
        let (cursor, e) = (cursor.clone(), &e);
        async move { e.frames(&cursor, 1500, Some(ready)).await }
    };
    let command = async {
        opened.await.unwrap();
        let r = e
            .http
            .post(format!("{}/api/decisions/{id}/dismiss", e.base))
            .header("cookie", &e.cookie)
            .header("x-steve-ws", "default")
            .send()
            .await
            .unwrap();
        let answered = Instant::now();
        assert_eq!(r.status().as_u16(), 200);
        let rev: u64 = r.headers()["x-steve-rev"]
            .to_str()
            .unwrap()
            .parse()
            .unwrap();
        // write then read: no waiting
        let (_, after) = e.snapshot().await;
        assert!(ids(&after).is_empty(), "{after}");
        assert!(after["rev"].as_u64().unwrap() >= rev);
        (answered, rev)
    };
    let (frames, (answered, rev)) = tokio::join!(stream, command);
    let patch = frames
        .iter()
        .find(|(_, f)| f["type"] == "patch" && f["ops"][0]["id"] == id.as_str())
        .expect("a patch frame");
    assert!(patch.1["rev"].as_u64().unwrap() >= rev);
    assert_eq!(patch.1["ops"][0]["op"], "upsert");
    assert_eq!(patch.1["ops"][0]["view"]["status"], "resolved");
    assert!(
        patch.0.saturating_duration_since(answered) <= Duration::from_millis(200),
        "patch came {:?} after the answer",
        patch.0.saturating_duration_since(answered)
    );
    assert!(!frames.iter().any(|(_, f)| f["type"] == "decision"));

    // a stream that was away from the snapshot's cursor gets the patch it missed
    let missed = e.frames(&cursor, 400, None).await;
    assert!(
        missed
            .iter()
            .any(|(_, f)| f["type"] == "patch" && f["ops"][0]["id"] == id.as_str()),
        "{missed:?}"
    );

    // outside the hot set: by id, and in the page of settled ones
    let r: Value = e
        .get(&format!("/api/views?kind=decision&ids={id},nope"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(r["views"][0]["status"], "resolved");
    assert_eq!(r["missing"], serde_json::json!(["nope"]));
    let r: Value = e
        .get("/api/views/page?kind=decision&status=settled&since=0")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(r["views"][0]["id"], id.as_str(), "{r}");
    assert!(r["views"][0]["undo_until"].is_null());
    assert!(r["next"].is_null());
}
