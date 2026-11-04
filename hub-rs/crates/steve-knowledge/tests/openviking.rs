//! The OpenViking store against a fake server (in-memory files and tags; "meaning" is shared
//! characters): the index is mirrored by state (never chunks left out, removals followed), meaning
//! fills in after keywords, what the server returns is read back under the scope, and a server that
//! is down makes knowledge unavailable rather than falling back.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use axum::extract::{Query as AxQuery, State};
use axum::http::HeaderMap;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde_json::{Value, json};
use steve_knowledge::Knowledge;
use steve_knowledge::openviking::{Hybrid, Mirror, OpenViking, OpenVikingConfig, TOPIC};
use steve_knowledge::port::SqliteStore;
use steve_knowledge::retrieve::{Purpose, Query, retrieve};
use steve_modules::testkit::{ME, OTHER, TestEnv, WS, call, caller};
use steve_store::base::Viewer;
use steve_store::outbox::Dispatcher;

#[derive(Default)]
struct Files(Mutex<BTreeMap<String, (String, Vec<String>)>>);

type S = Arc<Files>;

fn authed(h: &HeaderMap) -> bool {
    h.get("x-api-key").and_then(|v| v.to_str().ok()) == Some("k")
}

async fn ready() -> Json<Value> {
    Json(json!({"status": "ready"}))
}

async fn mkdir() -> Json<Value> {
    Json(json!({"status": "ok"}))
}

async fn rm(State(s): State<S>, AxQuery(q): AxQuery<BTreeMap<String, String>>) -> Json<Value> {
    let uri = q.get("uri").cloned().unwrap_or_default();
    s.0.lock().unwrap().retain(|k, _| !k.starts_with(&uri));
    Json(json!({"status": "ok"}))
}

async fn batch(State(s): State<S>, h: HeaderMap, Json(b): Json<Value>) -> Json<Value> {
    assert!(authed(&h));
    assert_eq!(b["processing_mode"], "vectors_only");
    let mut f = s.0.lock().unwrap();
    for op in b["operations"].as_array().unwrap() {
        let tags = op["tags"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t.as_str().unwrap().to_lowercase())
            .collect();
        f.insert(
            op["uri"].as_str().unwrap().into(),
            (op["content"].as_str().unwrap().into(), tags),
        );
    }
    Json(json!({"status": "ok"}))
}

/// Shared characters of query and file, as a share of the query's.
async fn find(State(s): State<S>, h: HeaderMap, Json(b): Json<Value>) -> Json<Value> {
    assert!(authed(&h));
    let q: Vec<char> = b["query"]
        .as_str()
        .unwrap()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    let root = b["target_uri"].as_str().unwrap();
    let want: Vec<String> = b["tags"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t.as_str().unwrap().into())
        .collect();
    let min = b["score_threshold"].as_f64().unwrap();
    let f = s.0.lock().unwrap();
    let mut out: Vec<Value> = f
        .iter()
        .filter(|(u, (_, tags))| u.starts_with(root) && want.iter().all(|w| tags.contains(w)))
        .filter_map(|(u, (text, _))| {
            let hit = q.iter().filter(|c| text.contains(**c)).count() as f64;
            let score = hit / q.len().max(1) as f64;
            (score >= min).then(|| json!({"uri": u, "score": score}))
        })
        .collect();
    out.sort_by(|a, b| {
        b["score"]
            .as_f64()
            .unwrap()
            .total_cmp(&a["score"].as_f64().unwrap())
    });
    Json(json!({"status": "ok", "result": {"resources": out}}))
}

async fn server(files: S) -> String {
    let app = Router::new()
        .route("/ready", get(ready))
        .route("/api/v1/fs", delete(rm))
        .route("/api/v1/fs/mkdir", post(mkdir))
        .route("/api/v1/content/batch-write", post(batch))
        .route("/api/v1/search/find", post(find))
        .with_state(files);
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    url
}

struct H {
    env: TestEnv,
    kb: Knowledge,
    app: Router,
    files: S,
    ov: Arc<OpenViking>,
    dispatcher: Arc<Dispatcher>,
}

async fn setup() -> H {
    let env = TestEnv::new(|_| {});
    let files: S = Arc::default();
    let url = server(files.clone()).await;
    let ov = OpenViking::new(OpenVikingConfig {
        url,
        key: "k".into(),
        account: "steve".into(),
    });
    let kb = Knowledge::new(Arc::new(steve_knowledge::NoGuard), |_| {});
    kb.set_store(Arc::new(Hybrid {
        keywords: SqliteStore,
        meaning: ov.clone(),
    }));
    let app = steve_knowledge::routes(kb.clone()).with_state(env.modules.clone());
    let dispatcher = Arc::new(Dispatcher::new(vec![Arc::new(Mirror {
        ov: ov.clone(),
        store: env.store().clone(),
    })]));
    dispatcher.register(env.store()).unwrap();
    H {
        env,
        kb,
        app,
        files,
        ov,
        dispatcher,
    }
}

impl H {
    async fn note(&self, who: &str, title: &str, body: &str, extra: Value) -> String {
        let mut b = json!({"title": title, "body": body});
        if let Value::Object(o) = extra {
            for (k, v) in o {
                b[k] = v;
            }
        }
        let (s, v) = call(
            &self.app,
            Some(&caller(WS, who)),
            "POST",
            "/kb/notes",
            Some(&b),
        )
        .await;
        assert_eq!(s, 200, "{v}");
        v["id"].as_str().unwrap().to_string()
    }

    async fn mirror(&self) {
        let (d, store) = (self.dispatcher.clone(), self.env.store().clone());
        tokio::task::spawn_blocking(move || d.pump(&store, steve_store::now()).unwrap())
            .await
            .unwrap();
    }

    fn files(&self) -> BTreeMap<String, (String, Vec<String>)> {
        self.files.0.lock().unwrap().clone()
    }

    fn search(&self, who: &str, q: &str) -> Vec<String> {
        let viewer = Viewer::bare(WS, who);
        let store = self.kb.store();
        self.env
            .store()
            .reader(|c| retrieve(c, &Query::new(store, &viewer, q, Purpose::Search)))
            .unwrap()
            .into_iter()
            .map(|h| h.obj_id)
            .collect()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_index_is_mirrored_and_meaning_fills_in_under_the_scope() {
    let h = setup().await;
    let shared = h
        .note(
            ME,
            "部署手册",
            "上线前先备份数据库，再按回滚手册处理。",
            json!({}),
        )
        .await;
    let mine = h
        .note(
            ME,
            "我的笔记",
            "公网网关在杭州的轻量服务器上。",
            json!({"visibility": "private"}),
        )
        .await;
    let never = h
        .note(
            ME,
            "密码本",
            "这里的东西不能用。",
            json!({"level": "never"}),
        )
        .await;
    h.mirror().await;
    let files = h.files();
    let of = |id: &str| {
        files
            .keys()
            .filter(|k| k.contains(&format!("/{id}/")))
            .count()
    };
    assert!(of(&shared) >= 1 && of(&mine) >= 1, "{files:?}");
    assert_eq!(of(&never), 0, "never is not written");
    let (_, tags) = files
        .iter()
        .find(|(k, _)| k.contains(&format!("/{mine}/")))
        .unwrap()
        .1
        .clone();
    assert!(
        tags.contains(&"vis=private".to_string())
            && tags.contains(&format!("by={}", ME.to_lowercase())),
        "{tags:?}"
    );

    // no shared word with the text, but shared characters ("meaning" in the fake): found by the
    // semantic pass only, and only where the scope allows
    assert!(h.search(ME, "杭州网关在哪").contains(&mine));
    // the same characters in an order no keyword query matches: only the semantic pass finds it
    let sqlite_only = {
        let viewer = Viewer::bare(WS, ME);
        h.env
            .store()
            .reader(|c| {
                retrieve(
                    c,
                    &Query::new(&SqliteStore, &viewer, "关网州杭", Purpose::Search),
                )
            })
            .unwrap()
    };
    assert!(sqlite_only.is_empty(), "{sqlite_only:?}");
    assert!(h.search(ME, "关网州杭").contains(&mine));
    assert!(
        !h.search(OTHER, "杭州网关在哪").contains(&mine),
        "another's private note stays out"
    );

    // removed in the Hub's index → removed in the store
    let (s, v) = call(
        &h.app,
        Some(&caller(WS, ME)),
        "DELETE",
        &format!("/kb/docs/{mine}"),
        None,
    )
    .await;
    assert!(s == 200 || s == 204, "{v}");
    h.mirror().await;
    assert_eq!(
        h.files()
            .keys()
            .filter(|k| k.contains(&format!("/{mine}/")))
            .count(),
        0
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_that_is_down_makes_knowledge_unavailable() {
    let h = setup().await;
    h.note(ME, "部署手册", "上线前先备份数据库。", json!({}))
        .await;
    let down = OpenViking::new(OpenVikingConfig {
        url: "http://127.0.0.1:9".into(),
        key: "k".into(),
        account: "steve".into(),
    });
    let store = Hybrid {
        keywords: SqliteStore,
        meaning: down,
    };
    let viewer = Viewer::bare(WS, ME);
    let r = h
        .env
        .store()
        .reader(|c| retrieve(c, &Query::new(&store, &viewer, "部署", Purpose::Search)));
    assert!(r.is_err(), "no silent fall-back: {r:?}");
    assert!(h.ov.ready().is_ok());
    let _ = TOPIC;
}
