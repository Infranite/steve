//! W3.4 synthetic replay fixtures — Hub HTTP, Dedupe, ACL, Attention & S2 Projection driver (18 scenarios).
//!
//! Exercises the real wired `steve-hub` Axum application, `AppState`, `Dedupe`, `Projection`,
//! `/api/snapshot`, `/api/views/page`, `/api/stream`, `/api/me/attention`, and `/api/thread*`
//! against an isolated temp SQLite database with `/usr/bin/false` adapters and no real hosts.
//!
//! Run in baseline mode:
//!   `cargo test --manifest-path hub-rs/Cargo.toml -p steve-hub --test thread_replay -- --nocapture`
//! Run in strict W3.4 acceptance mode:
//!   `STEVE_THREAD_REPLAY_STRICT=1 cargo test --manifest-path hub-rs/Cargo.toml -p steve-hub --test thread_replay -- --nocapture`

#[path = "../../../tests/thread_replay/report.rs"]
mod report;

use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::Request;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use steve_hub::api::AppState;
use steve_hub::bus::Bus;
use steve_hub::config::Config;
use steve_hub::projection::Projection;
use steve_store::{
    CommitOutcome, CommitReplyInput, NewDecision, NewItem, NewWorkspace, Store, Views, WS, thread,
};
use tower::ServiceExt;

use report::Report;

struct Env {
    _home: tempfile::TempDir,
    home_path: PathBuf,
    app: Router,
    jobs: Arc<steve_hub::jobs::Tasks>,
    owner_cookie: String,
    owner_id: String,
    store: Store,
    projection: Arc<Projection>,
}

async fn send_oneshot(app: &Router, mut req: Request<Body>) -> axum::response::Response {
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 18701))));
    app.clone().oneshot(req).await.unwrap()
}

async fn boot_on(
    home_path: PathBuf,
) -> (
    Router,
    Arc<steve_hub::jobs::Tasks>,
    String,
    String,
    Store,
    Arc<Projection>,
) {
    steve_modules::net::forbid_in_this_process();
    let port: u16 = 18701;
    let mut vars: HashMap<String, String> = HashMap::new();
    for (k, v) in [
        ("HOME", home_path.display().to_string()),
        ("STEVE_HOME", home_path.join("steve").display().to_string()),
        ("STEVE_PORT", port.to_string()),
        ("STEVE_HOST", "127.0.0.1".into()),
        ("STEVE_PUBLIC_URL", format!("http://127.0.0.1:{port}")),
        ("STEVE_DEV_LOGIN", "1".into()),
        ("STEVE_REPO", "/nonexistent".into()),
        ("STEVE_VAULT_KEYSTORE", "file".into()),
        ("STEVE_FORBID_REAL_HOSTS", "1".into()),
        ("STEVE_NO_RESUME", "1".into()),
        ("STEVE_BACKUP", "off".into()),
        ("CLAUDE_CODE_EXECUTABLE", "/usr/bin/false".into()),
        ("STEVE_ADAPTER_CLAUDE", "/usr/bin/false".into()),
        ("STEVE_ADAPTER_CODEX", "/usr/bin/false".into()),
        ("STEVE_ADAPTER_PI", "/usr/bin/false".into()),
        ("STEVE_NODE_LISTEN", "0".into()),
        (
            "STEVE_WEB_DIR",
            home_path.join("no-web").display().to_string(),
        ),
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
    let st = AppState::new(cfg.clone(), w.backend, w.domain, bus)
        .with_modules(w.modules)
        .with_oidc(w.oidc.clone());
    steve_hub::app::boot(&st).await.expect("boot");
    for job in w.jobs {
        job.start(&st.jobs);
    }
    let jobs = st.jobs.clone();
    let projection = st.projection.clone().expect("projection");
    let app = steve_hub::app::app(st, w.auth, w.mcp, w.module_routes, w.root_routes);
    let dev_req = Request::builder()
        .method("GET")
        .uri("/auth/dev")
        .header("host", format!("127.0.0.1:{port}"))
        .body(Body::empty())
        .unwrap();
    let r = send_oneshot(&app, dev_req).await;
    let status = r.status();
    let owner_cookie = r
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|c| c.starts_with("steve_session="))
        .map(|c| c.split(';').next().unwrap_or_default().to_string())
        .unwrap_or_else(|| {
            panic!(
                "missing steve_session cookie from /auth/dev (status={status}, headers={:?})",
                r.headers()
            )
        });
    let owner_id = store.access().owner().unwrap().expect("owner").id;
    (app, jobs, owner_cookie, owner_id, store, projection)
}

impl Env {
    async fn start() -> Self {
        let home = tempfile::tempdir().expect("tmp");
        let home_path = home.path().to_path_buf();
        let (app, jobs, owner_cookie, owner_id, store, projection) =
            boot_on(home_path.clone()).await;
        Self {
            _home: home,
            home_path,
            app,
            jobs,
            owner_cookie,
            owner_id,
            store,
            projection,
        }
    }

    async fn restart(&mut self) {
        self.jobs.stop().await;
        let run_dir = self.home_path.join("steve/run");
        let _ = std::fs::remove_dir_all(&run_dir);
        tokio::time::sleep(Duration::from_millis(20)).await;
        let (app, jobs, owner_cookie, owner_id, store, projection) =
            boot_on(self.home_path.clone()).await;
        self.app = app;
        self.jobs = jobs;
        self.owner_cookie = owner_cookie;
        self.owner_id = owner_id;
        self.store = store;
        self.projection = projection;
    }

    async fn req(
        &self,
        method: &str,
        path: &str,
        cookie: Option<&str>,
        ws: &str,
        body: Option<Value>,
    ) -> (u16, Option<u64>, Value) {
        let mut builder = Request::builder()
            .method(method)
            .uri(path)
            .header("host", "127.0.0.1:18701")
            .header("x-steve-ws", ws);
        if let Some(c) = cookie {
            builder = builder.header("cookie", c);
        }
        let body = if let Some(b) = body {
            builder = builder.header("content-type", "application/json");
            Body::from(serde_json::to_vec(&b).unwrap())
        } else {
            Body::empty()
        };
        let req = builder.body(body).unwrap();
        let r = send_oneshot(&self.app, req).await;
        let status = r.status().as_u16();
        let rev = r
            .headers()
            .get("x-steve-rev")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok());
        let bytes = r.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let val = serde_json::from_str(&text).unwrap_or(Value::String(text));
        (status, rev, val)
    }

    async fn post_raw(
        &self,
        path: &str,
        cookie: &str,
        ws: &str,
        bytes: &'static [u8],
    ) -> (u16, Value) {
        let req = Request::builder()
            .method("POST")
            .uri(path)
            .header("host", "127.0.0.1:18701")
            .header("cookie", cookie)
            .header("x-steve-ws", ws)
            .body(Body::from(bytes))
            .unwrap();
        let r = send_oneshot(&self.app, req).await;
        let status = r.status().as_u16();
        let bytes = r.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let val = serde_json::from_str(&text).unwrap_or(Value::String(text));
        (status, val)
    }

    async fn wait_messages(&self, ws: &str, min: usize) -> Vec<steve_store::Message> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let ms = self.store.messages().list(ws, "steve", 200).unwrap();
            if ms.len() >= min {
                return ms;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "timeout waiting for {min} messages, got {}",
                ms.len()
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    fn make_user_cookie(&self, sub: &str, role: &str) -> (String, String) {
        let uid = self.store.users().create(sub, sub, None, role).unwrap();
        let tok = format!("tok_{sub}");
        self.store
            .auth_sessions()
            .create(&uid, &tok, 3_600_000)
            .unwrap();
        (uid, format!("steve_session={tok}"))
    }

    fn seed_pending_message(&self, ws: &str, text: &str) -> String {
        let now = steve_store::now();
        let outcome = self
            .store
            .write(|w| {
                thread::submit_message(
                    w,
                    &steve_store::SubmitInput {
                        ws,
                        space: "steve",
                        actor_key: &self.owner_id,
                        submit_key: &steve_store::uid("seed"),
                        payload_digest: &steve_store::sha(text),
                        text,
                        item_id: None,
                        meta: &Value::Null,
                        user_id: Some(&self.owner_id),
                        project_id: None,
                        conv_json: Some(r#"{"space":"steve","research":true,"items":true,"owner_only":false,"start":true,"hands_on":false}"#),
                        to_item: None,
                        to_reply_to: None,
                        file_ids: &[],
                        files_json: None,
                        mentions_json: None,
                        enqueue_task: true,
                        validate_refs: false,
                    },
                )
            })
            .unwrap();
        let _ = now;
        match outcome {
            steve_store::SubmitOutcome::Accepted { message_id, .. } => message_id,
            other => panic!("unexpected seed submit outcome: {other:?}"),
        }
    }

    fn seed_committed_reply(
        &self,
        ws: &str,
        space: &str,
        user_id: Option<&str>,
        q: &str,
        a: &str,
    ) -> (String, String, String, String, i64) {
        let now = steve_store::now();
        let origin = self
            .store
            .messages()
            .add(ws, "me", q, None, &json!({}), space)
            .unwrap();
        let lease = steve_store::uid("lease");
        let (task_id, generation, reply_id) = self
            .store
            .write(|w| {
                let _ = thread::enqueue_revision(
                    w,
                    &steve_store::NewRevisionTask {
                        ws,
                        space,
                        origin_message_id: &origin,
                        user_id,
                        project_id: None,
                        conv_json: None,
                        to_item: None,
                        to_reply_to: None,
                        files_json: None,
                        mentions_json: None,
                        reply_to_message_id: None,
                        forced_kind: Some("answer"),
                        route_json: None,
                        offer_json: None,
                        prev_receipt_id: None,
                        read_task_intent_key: None,
                    },
                )?;
                let claimed =
                    thread::claim_head(w, ws, space, &lease, now)?.expect("claimed reply task");
                let commit = thread::commit_reply(
                    w,
                    &CommitReplyInput {
                        task_id: &claimed.id,
                        lease_token: &lease,
                        expected_generation: claimed.generation,
                        allow_cancelled_stop: false,
                        outcome: "answered",
                        reply_text: Some(a),
                        existing_reply_id: None,
                        reply_item_id: None,
                        reply_meta: &json!({"origin": origin, "run_receipt": true}),
                        error_code: None,
                        route_kind: "answer",
                        model_kind: "answer",
                        route_guard: None,
                        route_model: None,
                        route_confidence: None,
                        route_signals: &[],
                        route_fallback: false,
                        route_ms: 1,
                        route_outcome: None,
                        item_id: None,
                        dispatch_ref_id: None,
                        delivery_ref_id: None,
                        model_call_id: None,
                        user_id,
                        project_id: None,
                        answer_model: None,
                        harness: None,
                        harness_session: None,
                        tools: 0,
                        turns: 1,
                        first_text_ms: Some(1),
                        error_detail: None,
                        record_attention_commit: space == "steve" && user_id.is_some(),
                    },
                )?;
                let reply_id = match commit {
                    CommitOutcome::Committed {
                        reply_message_id: Some(r),
                        ..
                    } => r,
                    other => panic!("expected committed reply: {other:?}"),
                };
                Ok::<_, steve_store::StoreError>((claimed.id, claimed.generation, reply_id))
            })
            .unwrap();
        (origin, reply_id, task_id, lease, generation)
    }

    async fn snapshot_messages_match_store(&self, ws: &str) -> bool {
        let (st, _, snap) = self
            .req(
                "GET",
                "/api/snapshot?kinds=message",
                Some(&self.owner_cookie),
                ws,
                None,
            )
            .await;
        if st != 200 {
            return false;
        }
        let proj_msgs: BTreeMap<String, Value> = snap["views"]["message"]["views"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|v| (v["id"].as_str().unwrap().to_string(), v.clone()))
            .collect();
        let store_msgs: BTreeMap<String, Value> = self
            .store
            .read(|r| r.message_views().hot())
            .unwrap()
            .into_iter()
            .filter(|v| v.ws.as_str() == ws)
            .map(|v| (v.id.to_string(), serde_json::to_value(v).unwrap()))
            .collect();
        proj_msgs == store_msgs
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn w34_thread_replay_hub() {
    let mut report = Report::default();

    // 01: Same client_id + same payload deduped in-process
    {
        let e = Env::start().await;
        let body = json!({"text": "/汇报", "client_id": "cid_same_1"});
        let (s1, _, _) = e
            .req(
                "POST",
                "/api/thread",
                Some(&e.owner_cookie),
                WS,
                Some(body.clone()),
            )
            .await;
        let (s2, _, _) = e
            .req("POST", "/api/thread", Some(&e.owner_cookie), WS, Some(body))
            .await;
        let ms = e.wait_messages(WS, 2).await;
        tokio::time::sleep(Duration::from_millis(60)).await;
        let ms2 = e.store.messages().list(WS, "steve", 200).unwrap();
        let user_msgs = ms2.iter().filter(|m| m.author == "me").count();
        let steve_msgs = ms2.iter().filter(|m| m.author == "steve").count();
        report.case(
            "hub.submit.01_same_client_id_same_payload_deduped",
            &[
                ("both_ok", s1 == 200 && s2 == 200),
                ("single_user_message", user_msgs == 1 && ms.len() >= 2),
                ("le1_final_reply_per_intent", steve_msgs == 1),
            ],
            &[],
        );
    }

    // 02: Same client_id + DIFFERENT payload must return 409 Conflict!
    {
        let e = Env::start().await;
        let (s1, _, _) = e
            .req(
                "POST",
                "/api/thread",
                Some(&e.owner_cookie),
                WS,
                Some(json!({"text": "/汇报", "client_id": "cid_diff_1"})),
            )
            .await;
        let (s2, _, _) = e
            .req(
                "POST",
                "/api/thread",
                Some(&e.owner_cookie),
                WS,
                Some(json!({"text": "完全不同的内容", "client_id": "cid_diff_1"})),
            )
            .await;
        e.wait_messages(WS, 2).await;
        report.case(
            "hub.submit.02_same_client_id_different_payload_conflict",
            &[
                ("first_ok", s1 == 200),
                ("submit_payload_conflict_rejected", s2 == 409),
            ],
            &[],
        );
    }

    // 03: Same client_id across Hub restart must NOT duplicate message or reply!
    {
        let mut e = Env::start().await;
        let body = json!({"text": "/汇报", "client_id": "cid_restart_1"});
        let (s1, _, _) = e
            .req(
                "POST",
                "/api/thread",
                Some(&e.owner_cookie),
                WS,
                Some(body.clone()),
            )
            .await;
        e.wait_messages(WS, 2).await;
        e.restart().await;
        let (s2, _, _) = e
            .req("POST", "/api/thread", Some(&e.owner_cookie), WS, Some(body))
            .await;
        tokio::time::sleep(Duration::from_millis(80)).await;
        let ms = e.store.messages().list(WS, "steve", 200).unwrap();
        let user_msgs = ms.iter().filter(|m| m.author == "me").count();
        let steve_msgs = ms.iter().filter(|m| m.author == "steve").count();
        report.case(
            "hub.submit.03_same_client_id_across_hub_restart_deduped",
            &[
                ("both_200", s1 == 200 && s2 == 200),
                ("submit_idempotent_across_restart", user_msgs == 1),
                ("le1_final_reply_per_intent", steve_msgs <= 1),
            ],
            &[],
        );
    }

    // 04: Withdraw queued message, restart Hub, retry old client_id -> tombstone must prevent resurrection!
    {
        let mut e = Env::start().await;
        // Hold execution in maintenance so the submitted message stays queued until withdrawn.
        e.store.maintenance_set(true).unwrap();
        let body = json!({"text": "/汇报", "client_id": "cid_withdrawn_1"});
        let (s1, _, _) = e
            .req(
                "POST",
                "/api/thread",
                Some(&e.owner_cookie),
                WS,
                Some(body.clone()),
            )
            .await;
        let ms = e.store.messages().list(WS, "steve", 200).unwrap();
        let mid = ms.last().expect("queued msg").id.clone();
        let (s_wd, _, _) = e
            .req(
                "DELETE",
                &format!("/api/thread/queued/{mid}"),
                Some(&e.owner_cookie),
                WS,
                None,
            )
            .await;
        e.store.maintenance_set(false).unwrap();
        e.restart().await;
        let before = e.store.messages().list(WS, "steve", 200).unwrap().len();
        let (s2, _, _) = e
            .req("POST", "/api/thread", Some(&e.owner_cookie), WS, Some(body))
            .await;
        tokio::time::sleep(Duration::from_millis(80)).await;
        let after = e.store.messages().list(WS, "steve", 200).unwrap().len();
        let tomb = e.store.messages().get(&mid).unwrap().unwrap();
        report.case(
            "hub.submit.04_withdrawn_client_id_retry_after_restart_no_resurrect",
            &[
                ("withdraw_ok", s1 == 200 && s_wd == 200 && s2 == 409),
                (
                    "withdrawn_tombstone_no_resurrect",
                    after == before && tomb.meta["withdrawn"] == json!(true),
                ),
            ],
            &[],
        );
    }

    // 05: Same client_id across different users does NOT collide
    {
        let e = Env::start().await;
        let (_, u2_cookie) = e.make_user_cookie("local:u2_sub5", "member");
        let body = json!({"text": "/汇报", "client_id": "cid_shared"});
        let (s1, _, _) = e
            .req(
                "POST",
                "/api/thread",
                Some(&e.owner_cookie),
                WS,
                Some(body.clone()),
            )
            .await;
        let (s2, _, _) = e
            .req("POST", "/api/thread", Some(&u2_cookie), WS, Some(body))
            .await;
        let ms = e.wait_messages(WS, 4).await;
        let user_msgs = ms.iter().filter(|m| m.author == "me").count();
        report.case(
            "hub.submit.05_client_id_isolated_across_users",
            &[
                ("both_200", s1 == 200 && s2 == 200),
                ("both_stored", user_msgs == 2),
            ],
            &[],
        );
    }

    // 06: Empty and overlong text rejected with 400 and 0 messages stored
    {
        let e = Env::start().await;
        let (s_empty, _, _) = e
            .req(
                "POST",
                "/api/thread",
                Some(&e.owner_cookie),
                WS,
                Some(json!({"text": ""})),
            )
            .await;
        let big = "a".repeat(20_005);
        let (s_big, _, _) = e
            .req(
                "POST",
                "/api/thread",
                Some(&e.owner_cookie),
                WS,
                Some(json!({"text": big})),
            )
            .await;
        let count = e.store.messages().list(WS, "steve", 200).unwrap().len();
        report.case(
            "hub.submit.06_empty_and_overlong_text_rejected_zero_write",
            &[
                ("rejected_400", s_empty == 400 && s_big == 400),
                ("zero_messages_stored", count == 0),
            ],
            &[],
        );
    }

    // 07: Unauthenticated requests to thread endpoints rejected with 401 and 0 writes
    {
        let e = Env::start().await;
        let (s1, _, _) = e
            .req(
                "POST",
                "/api/thread",
                None,
                WS,
                Some(json!({"text": "/汇报"})),
            )
            .await;
        let (s2, _, _) = e.req("POST", "/api/thread/stop", None, WS, None).await;
        let (s3, _, _) = e
            .req(
                "PATCH",
                "/api/thread/queued/m1",
                None,
                WS,
                Some(json!({"text": "x"})),
            )
            .await;
        let (s4, _, _) = e
            .req("DELETE", "/api/thread/queued/m1", None, WS, None)
            .await;
        let (s5, _, _) = e
            .req(
                "POST",
                "/api/thread/m1/offer",
                None,
                WS,
                Some(json!({"take": true})),
            )
            .await;
        let count = e.store.messages().list(WS, "steve", 200).unwrap().len();
        report.case(
            "hub.acl.01_unauthenticated_thread_routes_rejected",
            &[
                ("all_401", [s1, s2, s3, s4, s5].iter().all(|&s| s == 401)),
                ("zero_writes", count == 0),
            ],
            &[],
        );
    }

    // 08: Non-member accessing foreign workspace thread & snapshot fails closed (0 writes, 0 leak)
    {
        let e = Env::start().await;
        let (_, u2_cookie) = e.make_user_cookie("local:u2_acl", "member");
        let ws2 = e
            .store
            .workspaces()
            .create(
                &NewWorkspace {
                    name: "PrivateWS".into(),
                    description: None,
                    item_prefix: Some("PR".into()),
                },
                &e.owner_id,
            )
            .unwrap()
            .id;
        e.store
            .messages()
            .add(&ws2, "me", "PRIVATE_WS2_SECRET", None, &json!({}), "steve")
            .unwrap();
        let (s_post, _, _) = e
            .req(
                "POST",
                "/api/thread",
                Some(&u2_cookie),
                &ws2,
                Some(json!({"text": "/汇报"})),
            )
            .await;
        let (s_snap, _, snap_val) = e
            .req(
                "GET",
                "/api/snapshot?kinds=message",
                Some(&u2_cookie),
                &ws2,
                None,
            )
            .await;
        let leaked = snap_val.to_string().contains("PRIVATE_WS2_SECRET");
        let ws2_msgs = e.store.messages().list(&ws2, "steve", 200).unwrap().len();
        report.case(
            "hub.acl.02_non_member_workspace_rejected_zero_leak",
            &[
                ("post_rejected", matches!(s_post, 403 | 404 | 409)),
                (
                    "snapshot_rejected_no_leak",
                    matches!(s_snap, 403 | 404 | 409) && !leaked,
                ),
                ("zero_foreign_writes", ws2_msgs == 1),
            ],
            &[],
        );
    }

    // 09: Cross-workspace attachment reference rejected with 404 and 0 messages stored
    {
        let e = Env::start().await;
        let ws2 = e
            .store
            .workspaces()
            .create(
                &NewWorkspace {
                    name: "FilesWS".into(),
                    description: None,
                    item_prefix: Some("FL".into()),
                },
                &e.owner_id,
            )
            .unwrap()
            .id;
        let (up_status, uploaded) = e
            .post_raw(
                "/api/files?name=secret.txt",
                &e.owner_cookie,
                &ws2,
                b"top secret bytes",
            )
            .await;
        assert_eq!(up_status, 200);
        let fid = uploaded["id"].as_str().unwrap();
        let (s, _, _) = e
            .req(
                "POST",
                "/api/thread",
                Some(&e.owner_cookie),
                WS,
                Some(json!({"text": "读这个文件", "files": [fid]})),
            )
            .await;
        let count = e.store.messages().list(WS, "steve", 200).unwrap().len();
        report.case(
            "hub.acl.03_cross_workspace_attachment_rejected_zero_message",
            &[
                ("rejected_404", s == 404),
                ("zero_messages_stored", count == 0),
            ],
            &[],
        );
    }

    // 10: Attention replies isolates users, guests, notices, and uncommitted drafts
    {
        let e = Env::start().await;
        let (u2_id, _) = e.make_user_cookie("local:u2_att", "member");
        // Seed committed main reply for owner
        let (_, r_own, _, _, _) =
            e.seed_committed_reply(WS, "steve", Some(&e.owner_id), "owner q", "owner ans");
        // Seed committed main reply for u2 (must NOT appear for owner)
        let _ = e.seed_committed_reply(WS, "steve", Some(&u2_id), "u2 q", "u2 ans");
        // Seed guest reply (must NOT appear in MAIN replies attention)
        let _ = e.seed_committed_reply(WS, "chan:sp1", Some(&e.owner_id), "guest q", "guest ans");
        // Seed uncommitted active task + notice (neither may appear)
        let m_run = e
            .store
            .messages()
            .add(WS, "me", "draft q", None, &json!({}), "steve")
            .unwrap();
        let _ = e
            .store
            .write(|w| {
                thread::enqueue_revision(
                    w,
                    &steve_store::NewRevisionTask {
                        ws: WS,
                        space: "steve",
                        origin_message_id: &m_run,
                        user_id: Some(&e.owner_id),
                        project_id: None,
                        conv_json: None,
                        to_item: None,
                        to_reply_to: None,
                        files_json: None,
                        mentions_json: None,
                        reply_to_message_id: None,
                        forced_kind: Some("research"),
                        route_json: None,
                        offer_json: None,
                        prev_receipt_id: None,
                        read_task_intent_key: None,
                    },
                )
            })
            .unwrap();
        let _ = e
            .store
            .write(|w| {
                thread::post_notice(
                    w,
                    WS,
                    "system",
                    "fail",
                    "notice",
                    None,
                    true,
                    steve_store::now(),
                )
            })
            .unwrap();

        let (st, _, att) = e
            .req(
                "GET",
                "/api/me/attention?kind=replies",
                Some(&e.owner_cookie),
                WS,
                None,
            )
            .await;
        let entries = att["categories"][0]["entries"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let only_own = entries.len() == 1 && entries[0]["id"].as_str() == Some(r_own.as_str());
        report.case(
            "hub.acl.04_attention_replies_isolates_users_guests_notices_drafts",
            &[
                ("http_200", st == 200),
                ("only_own_committed_reply", only_own),
            ],
            &[],
        );
    }

    // 11: Attention read_replies is monotonic and stale/duplicate commit on same task adds 0 unread
    {
        let e = Env::start().await;
        let (m1, r1, task_id, lease, generation) =
            e.seed_committed_reply(WS, "steve", Some(&e.owner_id), "q1", "ans1");
        // Re-attempt commit with the same lease/generation -> fenced out (0 duplicate unread)
        let second = e
            .store
            .write(|w| {
                thread::commit_reply(
                    w,
                    &CommitReplyInput {
                        task_id: &task_id,
                        lease_token: &lease,
                        expected_generation: generation,
                        allow_cancelled_stop: false,
                        outcome: "answered",
                        reply_text: Some("ans1 dup"),
                        existing_reply_id: None,
                        reply_item_id: None,
                        reply_meta: &json!({"origin": m1, "run_receipt": true}),
                        error_code: None,
                        route_kind: "answer",
                        model_kind: "answer",
                        route_guard: None,
                        route_model: None,
                        route_confidence: None,
                        route_signals: &[],
                        route_fallback: false,
                        route_ms: 1,
                        route_outcome: None,
                        item_id: None,
                        dispatch_ref_id: None,
                        delivery_ref_id: None,
                        model_call_id: None,
                        user_id: Some(&e.owner_id),
                        project_id: None,
                        answer_model: None,
                        harness: None,
                        harness_session: None,
                        tools: 0,
                        turns: 1,
                        first_text_ms: Some(1),
                        error_detail: None,
                        record_attention_commit: true,
                    },
                )
            })
            .unwrap();
        assert!(matches!(second, CommitOutcome::Fenced));
        let (_, _, att1) = e
            .req(
                "GET",
                "/api/me/attention?kind=replies",
                Some(&e.owner_cookie),
                WS,
                None,
            )
            .await;
        let count1 = att1["categories"][0]["count"].as_u64().unwrap_or(99);
        let snap = att1["categories"][0]["snapshot"]
            .as_str()
            .unwrap()
            .to_string();
        let (sr1, _, rd1) = e
            .req(
                "POST",
                "/api/me/attention/read",
                Some(&e.owner_cookie),
                WS,
                Some(json!({"snapshot": snap, "through_id": r1})),
            )
            .await;
        let (sr2, _, rd2) = e
            .req(
                "POST",
                "/api/me/attention/read",
                Some(&e.owner_cookie),
                WS,
                Some(json!({"snapshot": snap, "through_id": r1})),
            )
            .await;
        let (_, _, att2) = e
            .req(
                "GET",
                "/api/me/attention?kind=replies",
                Some(&e.owner_cookie),
                WS,
                None,
            )
            .await;
        let count2 = att2["categories"][0]["count"].as_u64().unwrap_or(99);
        report.case(
            "hub.acl.05_attention_read_replies_monotonic_and_idempotent",
            &[
                (
                    "duplicate_reply_zero_new_unread",
                    count1 == 1 && count2 == 0,
                ),
                (
                    "read_monotonic_idempotent",
                    sr1 == 200
                        && sr2 == 200
                        && rd1["updated"] == json!(true)
                        && rd2["updated"] == json!(false),
                ),
            ],
            &[],
        );
    }

    // 12: POST /api/thread (/汇报) -> x-steve-rev present, snapshot == page == store, reconcile == 0
    {
        let e = Env::start().await;
        let (s, rev, _) = e
            .req(
                "POST",
                "/api/thread",
                Some(&e.owner_cookie),
                WS,
                Some(json!({"text": "/汇报"})),
            )
            .await;
        e.wait_messages(WS, 2).await;
        let snap_eq = e.snapshot_messages_match_store(WS).await;
        let diffs = e.projection.reconcile().await.unwrap();
        let (sp, _, page) = e
            .req(
                "GET",
                "/api/views/page?kind=message",
                Some(&e.owner_cookie),
                WS,
                None,
            )
            .await;
        let page_len = page["views"].as_array().map_or(0, Vec::len);
        report.case(
            "hub.proj.01_post_thread_digest_matches_snapshot_and_page",
            &[
                ("http_200_with_rev", s == 200 && rev.is_some()),
                (
                    "s2_projection_zero_diff",
                    diffs == 0 && snap_eq && sp == 200 && page_len == 2,
                ),
            ],
            &[],
        );
    }

    // 13: Queued edit & withdraw over HTTP keep S2 message projection at 0 diff
    {
        let e = Env::start().await;
        let mid = e.seed_pending_message(WS, "待修改");
        let (se, rev_e, _) = e
            .req(
                "PATCH",
                &format!("/api/thread/queued/{mid}"),
                Some(&e.owner_cookie),
                WS,
                Some(json!({"text": "已修改"})),
            )
            .await;
        let eq_edit = e.snapshot_messages_match_store(WS).await;
        let d_edit = e.projection.reconcile().await.unwrap();
        let (sw, rev_w, wd_body) = e
            .req(
                "DELETE",
                &format!("/api/thread/queued/{mid}"),
                Some(&e.owner_cookie),
                WS,
                None,
            )
            .await;
        let eq_wd = e.snapshot_messages_match_store(WS).await;
        let d_wd = e.projection.reconcile().await.unwrap();
        report.case(
            "hub.proj.02_queued_edit_and_withdraw_zero_projection_diff",
            &[
                (
                    "edit_and_withdraw_ok_with_rev",
                    se == 200
                        && sw == 200
                        && rev_e.is_some()
                        && rev_w.is_some()
                        && wd_body["text"] == json!("已修改"),
                ),
                (
                    "s2_projection_zero_diff",
                    d_edit == 0 && d_wd == 0 && eq_edit && eq_wd,
                ),
            ],
            &[],
        );
    }

    // 14: POST /api/thread/:id/offer take updates message + item + session projections (0 diffs) & 2nd is 409
    {
        let e = Env::start().await;
        let origin = e
            .store
            .messages()
            .add(WS, "me", "改排版", None, &json!({}), "steve")
            .unwrap();
        let reply = e
            .store
            .messages()
            .add(
                WS,
                "steve",
                "要我改吗？",
                None,
                &json!({"origin": origin, "offer": {"title": "改排版", "goal": "改markdown", "state": "open"}}),
                "steve",
            )
            .unwrap();
        let (s1, rev1, _) = e
            .req(
                "POST",
                &format!("/api/thread/{reply}/offer"),
                Some(&e.owner_cookie),
                WS,
                Some(json!({"take": true})),
            )
            .await;
        let (s2, _, _) = e
            .req(
                "POST",
                &format!("/api/thread/{reply}/offer"),
                Some(&e.owner_cookie),
                WS,
                Some(json!({"take": true})),
            )
            .await;
        let snap_eq = e.snapshot_messages_match_store(WS).await;
        let diffs = e.projection.reconcile().await.unwrap();
        report.case(
            "hub.proj.03_offer_take_updates_linked_projections_zero_diff",
            &[
                (
                    "first_200_second_409",
                    s1 == 200 && rev1.is_some() && s2 == 409,
                ),
                (
                    "le1_dispatch_per_intent",
                    e.store.items().list(WS, false).unwrap().len() == 1,
                ),
                ("s2_projection_zero_diff", diffs == 0 && snap_eq),
            ],
            &[],
        );
    }

    // 15: POST /api/thread/:id/suggest item updates projections (0 diffs) & 2nd is 409
    {
        let e = Env::start().await;
        let origin = e
            .store
            .messages()
            .add(WS, "me", "优化列表", None, &json!({}), "steve")
            .unwrap();
        let card = e
            .store
            .messages()
            .add(
                WS,
                "steve",
                "建项吗？",
                None,
                &json!({
                    "origin": origin,
                    "suggest": {
                        "state": "open",
                        "choices": [{"id": "item", "label": "建项", "title": "优化列表", "goal": "优化渲染"}]
                    }
                }),
                "steve",
            )
            .unwrap();
        let (s1, rev1, _) = e
            .req(
                "POST",
                &format!("/api/thread/{card}/suggest"),
                Some(&e.owner_cookie),
                WS,
                Some(json!({"choice": "item"})),
            )
            .await;
        let (s2, _, _) = e
            .req(
                "POST",
                &format!("/api/thread/{card}/suggest"),
                Some(&e.owner_cookie),
                WS,
                Some(json!({"choice": "item"})),
            )
            .await;
        let snap_eq = e.snapshot_messages_match_store(WS).await;
        let diffs = e.projection.reconcile().await.unwrap();
        report.case(
            "hub.proj.04_suggest_item_updates_projections_zero_diff",
            &[
                (
                    "first_200_second_409",
                    s1 == 200 && rev1.is_some() && s2 == 409,
                ),
                (
                    "le1_dispatch_per_intent",
                    e.store.items().list(WS, false).unwrap().len() == 1,
                ),
                ("s2_projection_zero_diff", diffs == 0 && snap_eq),
            ],
            &[],
        );
    }

    // 16: Notice post + dismiss invalidates MessageView host projection (0 diffs!)
    {
        let e = Env::start().await;
        let notice = e
            .store
            .write(|w| {
                thread::post_notice(
                    w,
                    WS,
                    "release",
                    "fail",
                    "发布失败：冲突",
                    None,
                    true,
                    steve_store::now(),
                )
            })
            .unwrap();
        let line_id = notice.host_message_id.expect("host notice message");
        // Prime snapshot so projection holds the unclosed notice line
        let (_, _, snap_before) = e
            .req(
                "GET",
                "/api/snapshot?kinds=message",
                Some(&e.owner_cookie),
                WS,
                None,
            )
            .await;
        let _ = snap_before;
        let (sd, _, _) = e
            .req(
                "POST",
                &format!("/api/notifications/{line_id}/dismiss"),
                Some(&e.owner_cookie),
                WS,
                None,
            )
            .await;
        let snap_eq_before_reconcile = e.snapshot_messages_match_store(WS).await;
        let diffs = e.projection.reconcile().await.unwrap();
        report.case(
            "hub.proj.05_notice_dismiss_invalidates_message_projection",
            &[
                ("dismiss_200", sd == 200),
                (
                    "s2_projection_zero_diff",
                    diffs == 0 && snap_eq_before_reconcile,
                ),
            ],
            &[],
        );
    }

    // 17: Flowback update + hiding old card_only host keeps S2 projection at 0 diffs
    {
        let e = Env::start().await;
        let item = e
            .store
            .items()
            .create(
                WS,
                &NewItem {
                    title: "回流测试".into(),
                    goal: "回流".into(),
                    bot_id: "steve".into(),
                    tags: vec![e.store.tags().kinds(WS).unwrap().remove(0).id],
                    ..Default::default()
                },
            )
            .unwrap()
            .unwrap();
        let origin = e
            .store
            .messages()
            .add(WS, "me", "做回流", None, &json!({}), "steve")
            .unwrap();
        let announce = e
            .store
            .messages()
            .add(
                WS,
                "steve",
                "交给它了",
                Some(&item.id),
                &json!({"origin": origin}),
                "steve",
            )
            .unwrap();
        e.store
            .write(|w| {
                thread::record_dispatch(
                    w,
                    WS,
                    "steve",
                    &origin,
                    &item.id,
                    Some(&announce),
                    false,
                    true,
                    steve_store::now(),
                )
            })
            .unwrap();
        // Add 4 messages below so flowback moves card to a new card_only message
        for i in 0..4 {
            e.store
                .messages()
                .add(
                    WS,
                    "me",
                    &format!("后续消息 {i}"),
                    None,
                    &json!({}),
                    "steve",
                )
                .unwrap();
        }
        let db = steve_core::repo::Db::new(e.store.clone());
        steve_core::thread::flowback::flow(
            &db,
            &item.id,
            steve_core::thread::flowback::Update {
                kind: "fail",
                text: "构建失败".into(),
                ..Default::default()
            },
        )
        .unwrap();
        let snap_eq = e.snapshot_messages_match_store(WS).await;
        let diffs = e.projection.reconcile().await.unwrap();
        report.case(
            "hub.proj.06_flowback_card_move_zero_projection_diff",
            &[("s2_projection_zero_diff", diffs == 0 && snap_eq)],
            &[],
        );
    }

    // 18: Linked decision dismiss + thread item/session/message snapshot matches store with 0 diffs
    {
        let e = Env::start().await;
        let origin = e
            .store
            .messages()
            .add(WS, "me", "做新功能", None, &json!({}), "steve")
            .unwrap();
        let reply = e
            .store
            .messages()
            .add(
                WS,
                "steve",
                "要做成事项吗？",
                None,
                &json!({"origin": origin, "offer": {"title": "新功能", "goal": "做新功能", "state": "open"}}),
                "steve",
            )
            .unwrap();
        let (so, _, offered) = e
            .req(
                "POST",
                &format!("/api/thread/{reply}/offer"),
                Some(&e.owner_cookie),
                WS,
                Some(json!({"take": true})),
            )
            .await;
        let item_id = offered["item"]["id"].as_str().unwrap().to_string();
        let dec = e
            .store
            .decisions()
            .create(&NewDecision {
                workspace_id: WS.into(),
                kind: "ask".into(),
                subject_type: "item".into(),
                subject_id: item_id.clone(),
                item_id: Some(item_id),
                question: "需要确认吗？".into(),
                requested_by: "steve".into(),
                ..Default::default()
            })
            .unwrap()
            .unwrap();
        let (sd, rev_d, _) = e
            .req(
                "POST",
                &format!("/api/decisions/{}/dismiss", dec.id),
                Some(&e.owner_cookie),
                WS,
                None,
            )
            .await;
        let (ss, _, snap) = e
            .req(
                "GET",
                "/api/snapshot?kinds=message,item,decision,session",
                Some(&e.owner_cookie),
                WS,
                None,
            )
            .await;
        let diffs = e.projection.reconcile().await.unwrap();
        let has_all_kinds = snap["views"]["message"]["views"].is_array()
            && snap["views"]["item"].is_array()
            && snap["views"]["decision"].is_array()
            && snap["views"]["session"].is_array();
        report.case(
            "hub.proj.07_all_affected_s2_views_zero_projection_diff",
            &[
                (
                    "actions_ok_with_rev",
                    so == 200 && sd == 200 && rev_d.is_some() && ss == 200,
                ),
                ("s2_projection_zero_diff", diffs == 0 && has_all_kinds),
            ],
            &[],
        );
    }

    report.finish(18);
}
