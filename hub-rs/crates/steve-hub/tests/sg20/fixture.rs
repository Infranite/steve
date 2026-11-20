use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::{Router, body::Body, http::Request};
use serde_json::{Value, json};
use steve_hub::{
    api::AppState,
    backend::{BResult, BackendError, CreateItem, Domain, Obj, Route},
    bus::Bus,
    config::Config,
};
use steve_store::{NewItem, Store};
use tower::ServiceExt;

#[derive(Default)]
pub struct Script {
    pub fail: &'static str,
    pub title: String,
    pub calls: Vec<Value>,
    pub made: Vec<CreateItem>,
}

pub struct DomainPort {
    pub store: Store,
    pub script: Mutex<Script>,
}

impl DomainPort {
    fn check(&self, step: &str, call: Value) -> BResult<()> {
        let mut s = self.script.lock().unwrap();
        s.calls.push(call);
        if s.fail == step {
            Err(BackendError::msg(format!("{step} refused")))
        } else {
            Ok(())
        }
    }
}

#[async_trait::async_trait]
impl Domain for DomainPort {
    async fn route(&self, ws: &str, text: &str) -> BResult<Route> {
        self.check("route", json!(["route", ws, text]))?;
        Ok(Route {
            kind: "task".into(),
            title: self.script.lock().unwrap().title.clone(),
            bot: "writer".into(),
            project: None,
            tag_ids: vec![kind(&self.store, ws)],
            new_tags: vec![],
            reply: String::new(),
        })
    }

    async fn create_item(&self, p: CreateItem) -> BResult<Obj> {
        self.check("create", json!(["create"]))?;
        self.script.lock().unwrap().made.push(p.clone());
        if self.script.lock().unwrap().fail == "missing-created-id" {
            return Ok(Obj::new());
        }
        let item = self
            .store
            .items()
            .create(
                &p.ws,
                &NewItem {
                    title: p.title,
                    goal: p.goal,
                    bot_id: p.bot.unwrap(),
                    tags: p.tag_ids,
                    source: Some((p.source[0].clone(), p.source[1].clone())),
                    ..Default::default()
                },
            )
            .map_err(|e| BackendError::msg(e.to_string()))?
            .unwrap();
        Ok(json!({"id": item.id}).as_object().unwrap().clone())
    }

    async fn supplement(&self, id: &str, text: &str, mode: &str, files: Vec<Value>) -> BResult<()> {
        self.check("supplement", json!(["supplement", id, text, mode, files]))
    }

    async fn item_action(&self, id: &str, action: &str, actor: &str) -> BResult<Obj> {
        self.check("cancel", json!(["action", id, action, actor]))?;
        self.store
            .transaction(|tx| -> steve_store::Result<_> {
                tx.execute("UPDATE items SET status='drop' WHERE id=?", [id])?;
                Ok(())
            })
            .map_err(|e| BackendError::msg(e.to_string()))?;
        Ok(Obj::new())
    }
}

pub fn kind(store: &Store, ws: &str) -> String {
    store
        .tags()
        .kinds(ws)
        .unwrap()
        .into_iter()
        .find(|t| t.name == "feat")
        .unwrap()
        .id
}

pub struct Env {
    pub app: Router,
    pub unavailable: Router,
    pub st: AppState,
    pub store: Store,
    pub domain: Arc<DomainPort>,
    pub user: String,
    pub token: String,
    wired_domain: Arc<dyn Domain>,
    _home: tempfile::TempDir,
}

impl Env {
    pub async fn new() -> Self {
        steve_modules::net::forbid_in_this_process();
        let home = tempfile::tempdir().unwrap();
        let vars: HashMap<String, String> = [
            ("HOME", home.path().display().to_string()),
            ("STEVE_HOME", home.path().join("hub").display().to_string()),
            ("STEVE_PORT", "18990".into()),
            ("STEVE_NO_RESUME", "1".into()),
            ("STEVE_VAULT_KEYSTORE", "file".into()),
            ("STEVE_FORBID_REAL_HOSTS", "1".into()),
            ("STEVE_BACKUP", "off".into()),
            ("STEVE_REPO", "/nonexistent".into()),
            ("STEVE_ADAPTER_CLAUDE", "/usr/bin/false".into()),
            ("STEVE_ADAPTER_CODEX", "/usr/bin/false".into()),
            ("CLAUDE_CODE_EXECUTABLE", "/usr/bin/false".into()),
            (
                "STEVE_WEB_DIR",
                home.path().join("no-web").display().to_string(),
            ),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
        let get = Arc::new(move |k: &str| vars.get(k).cloned());
        let cfg = Arc::new(Config::from_vars(&*get).unwrap());
        for p in [&cfg.data_dir, &cfg.worktrees, &cfg.scratch] {
            std::fs::create_dir_all(p).unwrap();
        }
        let bus = Bus::new(Arc::new(steve_runtime::cache::Caches::new(
            Default::default(),
        )));
        let w = steve_hub::wire::wire_with(&cfg, &bus, get).unwrap();
        let store = w.modules.as_ref().unwrap().store.clone();
        store.ensure_workspace().unwrap();
        let user = store
            .users()
            .create("sg20", "Protocol owner", None, "owner")
            .unwrap();
        let (_, token) = store
            .access()
            .token_create(&user, "SG20 caller", None)
            .unwrap();
        store
            .transaction(|tx| -> steve_store::Result<_> {
                tx.execute(
                    "INSERT INTO bots (id,workspace_id,name,harness,main,updated) VALUES ('writer','default','Writer','claude-code',0,1),('steve','default','Steve','claude-code',1,1)",
                    [],
                )?;
                tx.execute("INSERT INTO bot_workspaces (bot_id,workspace_id,created) VALUES ('writer','default',1),('steve','default',1)", [])?;
                Ok(())
            })
            .unwrap();
        let domain = Arc::new(DomainPort {
            store: store.clone(),
            script: Mutex::new(Script::default()),
        });
        let unavailable = steve_hub::a2a_server::routes(AppState::new(
            cfg.clone(),
            w.backend.clone(),
            domain.clone(),
            Bus::new(Arc::new(steve_runtime::cache::Caches::new(
                Default::default(),
            ))),
        ));
        let st = AppState::new(cfg, w.backend, domain.clone(), bus).with_modules(w.modules);
        let app = steve_hub::app::app(st.clone(), w.auth, w.mcp, w.module_routes, w.root_routes);
        Self {
            app,
            unavailable,
            st,
            store,
            domain,
            user,
            token,
            wired_domain: w.domain,
            _home: home,
        }
    }

    pub fn item(&self, ws: &str, title: &str) -> steve_store::Item {
        self.store
            .items()
            .create(
                ws,
                &NewItem {
                    title: title.into(),
                    goal: "fixture".into(),
                    bot_id: "writer".into(),
                    tags: vec![kind(&self.store, ws)],
                    ..Default::default()
                },
            )
            .unwrap()
            .unwrap()
    }

    pub async fn raw(
        &self,
        body: impl Into<Body>,
        auth: Option<&str>,
        ws: Option<&str>,
    ) -> axum::response::Response {
        let mut req = Request::post("/a2a");
        if let Some(auth) = auth {
            req = req.header("authorization", auth);
        }
        if let Some(ws) = ws {
            req = req.header("x-steve-ws", ws);
        }
        self.app
            .clone()
            .oneshot(req.body(body.into()).unwrap())
            .await
            .unwrap()
    }

    pub async fn rpc(&self, method: &str, params: Value) -> Value {
        let r = self
            .raw(
                json!({"jsonrpc":"2.0","id":"sg20","method":method,"params":params}).to_string(),
                Some(&format!("Bearer {}", self.token)),
                None,
            )
            .await;
        assert_eq!(r.status(), 200);
        body(r).await
    }

    pub async fn mcp(&self, args: Value) -> Value {
        let req = Request::post("/mcp")
            .header("authorization", format!("Bearer {}", self.token))
            .header("accept", "application/json, text/event-stream")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"jsonrpc":"2.0","id":17,"method":"tools/call","params":{
                    "name":"steve_item_create","arguments":args
                }})
                .to_string(),
            ))
            .unwrap();
        let r = self.app.clone().oneshot(req).await.unwrap();
        assert_eq!(r.status(), 200);
        body(r).await["result"].clone()
    }

    pub async fn stop(self) {
        self.st.shutdown().await;
        self.wired_domain.shutdown().await;
    }
}

pub async fn body(r: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(r.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}
