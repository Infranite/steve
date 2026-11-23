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
    let mut listener = None;
    for port in 18720..18740 {
        if let Ok(bound) = tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
            listener = Some(bound);
            break;
        }
    }
    let listener = listener.expect("an available test port");
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
        ("STEVE_ADAPTER_PI", "/usr/bin/false".into()),
        ("STEVE_NODE_LISTEN", "0".into()),
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
            subject_type: "question".into(),
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
    assert_eq!(snap["views"]["decision"][0]["tier"], "decide");
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
    let (arrived, op) = frames
        .iter()
        .find_map(|(at, frame)| {
            if frame["type"] != "patch" || frame["rev"].as_u64()? < rev {
                return None;
            }
            frame["ops"]
                .as_array()?
                .iter()
                .find(|op| op["id"] == id.as_str())
                .map(|op| (*at, op))
        })
        .expect("the patch for the dismissal");
    assert_eq!(op["op"], "upsert");
    assert_eq!(op["view"]["status"], "dismissed");
    assert!(
        arrived.saturating_duration_since(answered) <= Duration::from_millis(200),
        "patch came {:?} after the answer",
        arrived.saturating_duration_since(answered)
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

#[tokio::test(flavor = "multi_thread")]
async fn channel_tiers_follow_commits_and_bulk_ignore_rechecks_stars() {
    use steve_store::{WS, testkit::hub::views as fixture};

    let e = start().await;
    fixture::seed_channel_tiers(&e.store).unwrap();
    let (star, quiet, failed) = fixture::channel_decisions(&e.store).unwrap();
    let row = |snap: Value, id: &str| {
        snap["views"]["decision"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["id"] == id)
            .unwrap()
            .clone()
    };
    let (_, snap) = e.snapshot().await;
    assert_eq!(row(snap, &star)["tier"], "draft");

    // One commit changes the star and a different conversation. A snapshot can see both right away.
    fixture::star_and_reply(&e.store).unwrap();
    let (_, snap) = e.snapshot().await;
    assert_eq!(row(snap.clone(), &star)["tier"], "decide");
    assert_eq!(row(snap.clone(), &failed)["tier"], "decide");
    assert_eq!(row(snap, &quiet)["folded"], "replied");

    // The same call would keep the star even if the client's old draft list still contains it.
    fixture::remove_reply(&e.store).unwrap();
    let (_, before) = e.snapshot().await;
    let original = row(before, &quiet)["draft"].as_str().unwrap().to_owned();
    let edit = |previous: &str, text: &str| {
        e.http
            .patch(format!("{}/api/decisions/{quiet}/draft", e.base))
            .header("cookie", &e.cookie)
            .header("x-steve-ws", WS)
            .json(&serde_json::json!({ "previous": previous, "text": text }))
    };
    assert_eq!(
        edit(&original, "edited reply")
            .send()
            .await
            .unwrap()
            .status()
            .as_u16(),
        200
    );
    assert_eq!(
        edit(&original, "stale edit")
            .send()
            .await
            .unwrap()
            .status()
            .as_u16(),
        409
    );
    let (_, snap) = e.snapshot().await;
    assert_eq!(row(snap, &quiet)["draft"], "edited reply");
    let revisions = fixture::reply_revisions(&e.store).unwrap();
    assert_eq!(revisions.len(), 1);
    assert_eq!(revisions[0].1["before"]["text"], original);

    let r = e
        .http
        .post(format!("{}/api/decisions/dismiss-drafts", e.base))
        .header("cookie", &e.cookie)
        .header("x-steve-ws", WS)
        .json(&serde_json::json!({"ids": [&star, &quiet, &failed]}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    assert_eq!(
        r.json::<Value>().await.unwrap()["ids"],
        serde_json::json!([quiet])
    );
    let (_, snap) = e.snapshot().await;
    assert_eq!(row(snap.clone(), &star)["tier"], "decide");
    assert_eq!(row(snap, &failed)["tier"], "decide");
}

#[tokio::test(flavor = "multi_thread")]
async fn item_and_session_patches_preserve_recorded_identity_and_join_history() {
    use steve_store::{
        ItemPatch, NewItem, NewSession, NewTag, SessionPatch, node_sessions, runtime,
    };
    let e = start().await;
    let ws = "default";
    let tags = e.store.tags();
    let category = tags.kinds(ws).unwrap()[0].id.clone();
    let source = tags
        .create(
            ws,
            &NewTag {
                name: "source".into(),
                cat: "topic".into(),
                ..Default::default()
            },
            "owner",
        )
        .unwrap()
        .id;
    let target = tags
        .create(
            ws,
            &NewTag {
                name: "target".into(),
                cat: "topic".into(),
                ..Default::default()
            },
            "owner",
        )
        .unwrap()
        .id;
    let item = e
        .store
        .items()
        .create(
            ws,
            &NewItem {
                title: "Review".into(),
                goal: "Review the design".into(),
                bot_id: "steve".into(),
                tags: vec![category, source.clone()],
                ..Default::default()
            },
        )
        .unwrap()
        .unwrap();
    let first = e
        .store
        .sessions()
        .create(&NewSession {
            workspace_id: ws.into(),
            item_id: Some(item.id.clone()),
            bot_id: Some("reviewer".into()),
            harness: "review-tool".into(),
            model: Some("recorded-model".into()),
            token: "test-secret".into(),
            ..Default::default()
        })
        .unwrap();
    let other = e
        .store
        .sessions()
        .create(&NewSession {
            workspace_id: ws.into(),
            item_id: Some(item.id.clone()),
            bot_id: Some("writer".into()),
            harness: "writing-tool".into(),
            ..Default::default()
        })
        .unwrap();
    let get = |path: String| {
        let request = e.get(&path);
        async move {
            request
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap()
                .json::<Value>()
                .await
                .unwrap()
        }
    };
    let snap = get("/api/snapshot?kinds=item,session".into()).await;
    assert_eq!(snap["views"]["session"].as_array().unwrap().len(), 2);
    let cursor = snap["cursor"].as_str().unwrap();

    e.store
        .timeline()
        .add(&item.id, "owner", "comment", "Detail update", None, &[])
        .unwrap();
    tags.merge(&source, &target).unwrap();
    e.store
        .write::<_, steve_store::StoreError>(|tx| {
            runtime::bind_session_in(
                tx,
                &runtime::SessionBinding {
                    session: &first.id,
                    node: "review-node",
                    workspace: &serde_json::json!({"id":"workspace"}),
                    node_session: "recorded-native",
                    config_revision: "v1",
                },
                1,
            )?;
            node_sessions::queue(
                tx,
                "test-queue",
                &first.id,
                "text only in the queue",
                "user",
                &[],
                2,
            )?;
            node_sessions::changed_event(tx, &first.id)
        })
        .unwrap();
    let updated = get("/api/snapshot?kinds=item,session".into()).await;
    let viewed = updated["views"]["session"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == first.id)
        .unwrap();
    assert_eq!(viewed["harness"], "review-tool");
    assert_eq!(viewed["model"], "recorded-model");
    assert_eq!(viewed["node_id"], "review-node");
    assert_eq!(viewed["queued"], 1);
    assert!(viewed.get("token_hash").is_none());
    assert!(viewed.get("import_key").is_none());
    assert!(viewed.get("last_seq").is_none());
    assert_ne!(
        updated["views"]["item"][0]["detail_stamp"],
        snap["views"]["item"][0]["detail_stamp"]
    );
    let tags_after = updated["views"]["item"][0]["tags"].as_array().unwrap();
    assert!(tags_after.iter().any(|tag| tag == &target));
    assert!(!tags_after.iter().any(|tag| tag == &source));
    let frames = e.frames(cursor, 250, None).await;
    assert!(
        !frames
            .iter()
            .any(|(_, f)| f["type"] == "item" || f["type"] == "session")
    );
    let ops: Vec<_> = frames
        .iter()
        .filter_map(|(_, f)| f["ops"].as_array())
        .flatten()
        .collect();
    assert!(
        ops.iter()
            .any(|op| op["type"] == "item" && op["id"] == item.id)
    );
    assert!(
        ops.iter()
            .any(|op| op["type"] == "session" && op["id"] == first.id && op["view"]["queued"] == 1)
    );

    tags.remove(&target).unwrap();
    e.store
        .sessions()
        .patch(
            &other.id,
            &SessionPatch {
                status: Some("ended".into()),
                ..Default::default()
            },
        )
        .unwrap();
    e.store
        .items()
        .patch(
            &item.id,
            &ItemPatch {
                archived: Some(true),
                ..Default::default()
            },
        )
        .unwrap();
    let archived = get("/api/views/page?kind=item&status=archived&limit=1".into()).await;
    assert_eq!(archived["views"][0]["id"], item.id);
    assert!(
        !archived["views"][0]["tags"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tag| tag == &target)
    );
    let ended = get("/api/views/page?kind=session&status=ended&limit=1".into()).await;
    assert_eq!(ended["views"][0]["id"], other.id);
    assert!(ended["views"][0].get("model").is_none());
    assert!(ended["views"][0].get("node_id").is_none());
    let all = get(format!("/api/views?kind=session&items={}", item.id)).await;
    assert_eq!(all["views"].as_array().unwrap().len(), 2);
    let snapshot = get("/api/snapshot?kinds=item,session".into()).await;
    assert!(snapshot["views"]["item"].as_array().unwrap().is_empty());
    assert_eq!(snapshot["views"]["session"][0]["id"], first.id);
}
