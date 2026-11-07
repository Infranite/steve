//! The collaboration API uses the same workspace boundary as the parent item. The probe domain
//! records authorization context, so a missing ownership check cannot accidentally pass this test.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{Value, json};
use steve_hub::api::AppState;
use steve_hub::backend::{BResult, Domain};
use steve_hub::bus::Bus;
use steve_hub::config::Config;

#[derive(Default)]
struct Probe {
    calls: Mutex<Vec<Value>>,
}

#[async_trait]
impl Domain for Probe {
    async fn workflow_get(&self, id: &str) -> BResult<Value> {
        self.calls.lock().unwrap().push(json!({"get": id}));
        Ok(json!({"item_id": id, "revision": 1, "units": []}))
    }

    async fn dependency_get(&self, id: &str) -> BResult<Value> {
        self.calls
            .lock()
            .unwrap()
            .push(json!({"dependency_get": id}));
        Ok(json!({"item_id": id, "waiting": false}))
    }

    async fn dependency_wait(
        &self,
        id: &str,
        cycle: i64,
        resume: &str,
        reason: &str,
    ) -> BResult<Value> {
        self.calls
            .lock()
            .unwrap()
            .push(json!({"dependency_wait": id, "cycle":cycle, "resume":resume, "reason":reason}));
        Ok(json!({"item_id":id, "waiting":true}))
    }

    async fn workflow_change(&self, id: &str, args: Value, actor: &str) -> BResult<Value> {
        self.calls
            .lock()
            .unwrap()
            .push(json!({"item": id, "args": args, "actor": actor}));
        Ok(json!({"item_id": id, "revision": 2}))
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn workflow_routes_require_login_and_parent_workspace_ownership() {
    steve_modules::net::forbid_in_this_process();
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    assert_ne!(port, 8440);
    let vars: HashMap<String, String> = [
        ("HOME", h.display().to_string()),
        ("STEVE_HOME", h.join("steve").display().to_string()),
        ("STEVE_HOST", "127.0.0.1".into()),
        ("STEVE_PORT", port.to_string()),
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
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    let vars = Arc::new(vars);
    let get = {
        let vars = vars.clone();
        move |k: &str| vars.get(k).cloned()
    };
    let cfg = Arc::new(Config::from_vars(&get).unwrap());
    for dir in [&cfg.data_dir, &cfg.worktrees, &cfg.scratch] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let bus = Bus::new();
    let w = steve_hub::wire::wire_with(&cfg, &bus, Arc::new(get)).unwrap();
    w.domain.seed().await.unwrap();
    let probe = Arc::new(Probe::default());
    let st = AppState::new(cfg.clone(), w.backend, probe.clone(), bus).with_modules(w.modules);
    let app = steve_hub::app::app(st, w.auth, w.mcp, w.module_routes, w.root_routes);
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    let base = format!("http://127.0.0.1:{port}");
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let login = http.get(format!("{base}/auth/dev")).send().await.unwrap();
    let cookie = login
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|v| v.starts_with("steve_session="))
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let db = rusqlite::Connection::open(cfg.db_path()).unwrap();
    db.execute_batch(
        "INSERT INTO workspaces(id,name,created) VALUES('other','Other',1);
         INSERT INTO items(id,workspace_id,no,title,status,stage,created,updated)
         VALUES('local-item','default',990,'Local','run','执行',1,1),
               ('other-item','other',991,'Other','run','执行',1,1);",
    )
    .unwrap();

    for route in ["workflow", "dependency-wait"] {
        let r = http
            .get(format!("{base}/api/items/local-item/{route}"))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status().as_u16(), 401);
        for method in [reqwest::Method::GET, reqwest::Method::POST] {
            let r = http
                .request(method, format!("{base}/api/items/other-item/{route}"))
                .header("cookie", &cookie)
                .json(&json!({"action": "pause"}))
                .send()
                .await
                .unwrap();
            assert_eq!(r.status().as_u16(), 404);
        }
        let r = http
            .get(format!("{base}/api/items/other-item/{route}"))
            .header("cookie", &cookie)
            .header("x-steve-ws", "other")
            .send()
            .await
            .unwrap();
        assert_eq!(
            r.status().as_u16(),
            409,
            "being logged in does not grant another workspace"
        );
        let r = http
            .post(format!("{base}/api/items/local-item/{route}"))
            .header("cookie", &cookie)
            .header("sec-fetch-site", "same-origin")
            .json(&json!({"action": "pause"}))
            .send()
            .await
            .unwrap();
        assert_eq!(
            r.status().as_u16(),
            409,
            "browser writes must name their workspace"
        );
    }
    assert!(
        probe.calls.lock().unwrap().is_empty(),
        "denied requests never reach the domain"
    );

    let r = http
        .get(format!("{base}/api/items/local-item/workflow"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    assert_eq!(r.json::<Value>().await.unwrap()["item_id"], "local-item");
    let args = json!({"action": "pause", "expected_revision": 1, "reason": "Review"});
    let r = http
        .post(format!("{base}/api/items/local-item/workflow"))
        .header("cookie", &cookie)
        .header("x-steve-ws", "default")
        .header("sec-fetch-site", "same-origin")
        .json(&args)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let user: String = db
        .query_row(
            "SELECT user_id FROM workspace_members WHERE workspace_id='default' LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        probe.calls.lock().unwrap()[1],
        json!({"item":"local-item", "args":args, "actor":user})
    );
    let r = http
        .get(format!("{base}/api/items/local-item/dependency-wait"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    assert_eq!(r.json::<Value>().await.unwrap()["waiting"], false);
    let body =
        json!({"expected_cycle":1,"resume":"automatic","reason":"wait for the prerequisite"});
    let r = http
        .post(format!("{base}/api/items/local-item/dependency-wait"))
        .header("cookie", &cookie)
        .header("x-steve-ws", "default")
        .header("sec-fetch-site", "same-origin")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    assert_eq!(
        probe.calls.lock().unwrap().last().unwrap(),
        &json!({"dependency_wait":"local-item","cycle":1,"resume":"automatic","reason":"wait for the prerequisite"})
    );
    let before = probe.calls.lock().unwrap().len();
    let r = http
        .post(format!("{base}/api/items/local-item/dependency-wait"))
        .header("cookie", &cookie)
        .header("x-steve-ws", "default")
        .json(&json!({"expected_cycle":1.5,"resume":"automatic","reason":"invalid cycle"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 400);
    assert_eq!(probe.calls.lock().unwrap().len(), before);
    server.abort();
}
