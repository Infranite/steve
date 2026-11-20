use axum::{
    Router,
    body::Body,
    http::{HeaderMap, Request},
};
use serde_json::Value;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};
use steve_hub::{
    api::{self, AppState},
    bus::Bus,
    config::Config,
    wire::{
        self,
        node_runtime::{NodeDomain, NodeRuntime},
        workspaces::{LocalFace, Workspaces},
    },
};
use steve_store::{Store, StoreError};
use tower::ServiceExt;

pub struct Env {
    pub store: Store,
    pub st: AppState,
    pub app: Router,
    pub user: String,
    credential: String,
    pub item: steve_store::Item,
    pub root: PathBuf,
    pub ws: Arc<Workspaces>,
    _node: Option<Arc<steve_node::LocalNode>>,
    _home: tempfile::TempDir,
}
struct NoDomain;
impl NodeDomain for NoDomain {
    fn turn_ended(&self, _: &str, _: Option<&str>, _: &str, _: Option<String>) {}
}
impl Env {
    pub async fn new(local: bool) -> Self {
        steve_modules::net::forbid_in_this_process();
        let home = tempfile::tempdir().unwrap();
        let root = home.path().canonicalize().unwrap();
        let vars: HashMap<String, String> = [
            ("HOME", root.display().to_string()),
            ("STEVE_HOME", root.join("hub").display().to_string()),
            ("STEVE_PORT", "18990".into()),
            ("STEVE_LOCAL_NODE_PORT", "18993".into()),
            ("STEVE_NO_RESUME", "1".into()),
            ("STEVE_BACKUP", "off".into()),
            ("STEVE_VAULT_KEYSTORE", "file".into()),
            ("STEVE_FORBID_REAL_HOSTS", "1".into()),
            ("STEVE_NODE_MODE", "off".into()),
            ("STEVE_REPO", "/nonexistent".into()),
            ("STEVE_ADAPTER_CLAUDE", "/usr/bin/false".into()),
            ("STEVE_ADAPTER_CODEX", "/usr/bin/false".into()),
            ("CLAUDE_CODE_EXECUTABLE", "/usr/bin/false".into()),
            ("STEVE_WEB_DIR", root.join("no-web").display().to_string()),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
        let get = Arc::new(move |k: &str| vars.get(k).cloned());
        let cfg = Arc::new(Config::from_vars(&*get).unwrap());
        for d in [&cfg.data_dir, &cfg.scratch, &cfg.worktrees] {
            std::fs::create_dir_all(d).unwrap();
        }
        let bus = Bus::new(Arc::new(steve_runtime::cache::Caches::new(
            Default::default(),
        )));
        let w = wire::wire_with(&cfg, &bus, get).unwrap();
        let store = w.modules.as_ref().unwrap().store.clone();
        store.ensure_workspace().unwrap();
        let user = store
            .users()
            .create("sg42", "Contract owner", None, "owner")
            .unwrap();
        store.transaction::<_,StoreError>(|tx|{tx.execute("INSERT INTO bots(id,workspace_id,name,harness,updated) VALUES ('sg42-bot','default','Contract bot','claude-code',1)",[])?;Ok(())}).unwrap();
        let kind = store
            .tags()
            .kinds("default")
            .unwrap()
            .into_iter()
            .find(|t| t.name == "feat")
            .unwrap()
            .id;
        let item = store
            .items()
            .create(
                "default",
                &steve_store::NewItem {
                    title: "original".into(),
                    goal: "fixture".into(),
                    tags: vec![kind],
                    bot_id: "sg42-bot".into(),
                    requested_by: Some(user.clone()),
                    ..Default::default()
                },
            )
            .unwrap()
            .unwrap();
        let extra = wire::workspaces::configured(&cfg);
        let node = local.then(|| {
            Arc::new(
                steve_node::LocalNode::open(steve_node::NodeConfig {
                    ceiling: steve_node::PolicyCeiling::unlimited(),
                    node: steve_node_api::NodeId::new("sg42-node").unwrap(),
                    home: root.join("file-node"),
                    acp: steve_acp::AcpConfig {
                        declared: Default::default(),
                        adapters: Default::default(),
                        node: "node".into(),
                        claude_executable: "/usr/bin/false".into(),
                        steve_home: root.clone(),
                        codex_home: root.join("codex"),
                        user_home: root.clone(),
                        version: "test".into(),
                    },
                    roots: vec![],
                    capacity: steve_node_api::Capacity {
                        sessions: 0,
                        jobs: 0,
                        execs: 0,
                    },
                    storage: Default::default(),
                })
                .unwrap(),
            )
        });
        let face = node.as_ref().map(|n| LocalFace {
            node: n.clone(),
            runtime: NodeRuntime::new(store.clone(), n.clone(), Arc::new(NoDomain)),
        });
        let ws = Workspaces::new(store.clone(), face, None, extra);
        ws.refresh_roots().unwrap();
        let st = AppState::new(cfg, w.backend, w.domain, bus)
            .with_modules(w.modules)
            .with_workspaces(Some(ws.clone()));
        let (_, client, secret) = store
            .access()
            .client_create(
                &user,
                None,
                &steve_store::access::NewClient {
                    kind: "cli",
                    name: "Contract client",
                    ..Default::default()
                },
            )
            .unwrap()
            .unwrap();
        let credential = steve_store::access::credential(&client.id, &secret);
        let app = api::router(st.clone(), Router::new());
        Self {
            store,
            st,
            app,
            user,
            credential,
            item,
            root,
            ws,
            _node: node,
            _home: home,
        }
    }
    pub fn project(&self, name: &str, kind: &str) -> steve_store::Project {
        let path = self.root.join(name);
        std::fs::create_dir_all(&path).unwrap();
        self.store
            .projects()
            .create(
                "default",
                &steve_store::NewProject {
                    name: name.into(),
                    path: path.display().to_string(),
                    kind: kind.into(),
                    device_id: "hub".into(),
                    ..Default::default()
                },
            )
            .unwrap()
    }
    pub fn patch_item(&self, v: Value) {
        self.store
            .items()
            .patch(
                &self.item.id,
                &serde_json::from_value::<steve_store::ItemPatch>(v).unwrap(),
            )
            .unwrap();
    }
    pub fn session(&self) -> steve_store::Session {
        let cwd = self.st.cfg.scratch.join(&self.item.id);
        std::fs::create_dir_all(&cwd).unwrap();
        self.store
            .sessions()
            .create(&steve_store::NewSession {
                workspace_id: "default".into(),
                item_id: Some(self.item.id.clone()),
                cwd: cwd.display().to_string(),
                harness: "fixture".into(),
                ..Default::default()
            })
            .unwrap()
    }
    pub fn artifact(&self, name: &str, path: &Path) -> String {
        self.store
            .artifacts()
            .add(&self.item.id, "文件", name, Some(path.to_str().unwrap()))
            .unwrap();
        self.store
            .artifacts()
            .list(&self.item.id)
            .unwrap()
            .into_iter()
            .find(|a| a.title == name)
            .unwrap()
            .id
    }
    pub async fn raw(
        &self,
        method: &str,
        path: &str,
        v: Option<Value>,
        range: Option<&str>,
    ) -> (u16, HeaderMap, Vec<u8>) {
        let mut req = Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json")
            .header("x-steve-ws", "default")
            .header("authorization", format!("Bearer {}", self.credential));
        if let Some(r) = range {
            req = req.header("range", r);
        }
        let r = self
            .app
            .clone()
            .oneshot(
                req.body(
                    v.map(|v| Body::from(v.to_string()))
                        .unwrap_or_else(Body::empty),
                )
                .unwrap(),
            )
            .await
            .unwrap();
        let status = r.status().as_u16();
        let headers = r.headers().clone();
        let body = axum::body::to_bytes(r.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec();
        (status, headers, body)
    }
    pub async fn call(&self, method: &str, path: &str, v: Option<Value>) -> (u16, Value) {
        let (s, _, b) = self.raw(method, path, v, None).await;
        (s, serde_json::from_slice(&b).unwrap())
    }
    pub fn q<T: rusqlite::types::FromSql>(&self, sql: &str) -> T {
        self.store
            .reader(|c| Ok::<_, StoreError>(c.query_row(sql, [], |r| r.get(0))?))
            .unwrap()
    }
    pub async fn stop(self) {
        self.st.shutdown().await;
    }
}

pub fn zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    use std::io::{Cursor, Write};
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, bytes) in entries {
        writer
            .start_file(*name, zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(bytes).unwrap();
    }
    writer.finish().unwrap().into_inner()
}
