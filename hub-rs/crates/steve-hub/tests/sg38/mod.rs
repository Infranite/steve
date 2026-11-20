use axum::{Router, body::Body, extract::ConnectInfo, http::Request};
use serde_json::{Value, json};
use std::{collections::HashMap, sync::Arc};
use steve_hub::{
    bus::Bus,
    config::Config,
    wire::{self, tool_service::ToolService},
};
use steve_modules::{
    caps::Capabilities,
    vault::{Vault, keystore::FileKeyStore},
};
use steve_store::StoreError;
use tower::ServiceExt;

pub const BOT: &str = "sg38-bot";
pub const TOKEN: &str = "sg38-fixture-session";
pub struct Mcp {
    pub service: Arc<ToolService>,
    pub router: Router,
    pub session: steve_store::Session,
    pub user: String,
    pub access: String,
    domain: Arc<dyn steve_hub::backend::Domain>,
    _home: tempfile::TempDir,
}
impl Mcp {
    pub async fn new() -> Self {
        steve_modules::net::forbid_in_this_process();
        let home = tempfile::tempdir().unwrap();
        let vars: HashMap<String, String> = [
            ("HOME", home.path().display().to_string()),
            ("STEVE_HOME", home.path().join("hub").display().to_string()),
            ("STEVE_PORT", "18990".into()),
            ("STEVE_NO_RESUME", "1".into()),
            ("STEVE_VAULT_KEYSTORE", "file".into()),
            ("STEVE_BACKUP", "off".into()),
            ("STEVE_FORBID_REAL_HOSTS", "1".into()),
            ("STEVE_NODE_MODE", "off".into()),
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
        let caches = Arc::new(steve_runtime::cache::Caches::new(Default::default()));
        let w = wire::wire_with(&cfg, &Bus::new(caches.clone()), get).unwrap();
        let modules = w.modules.as_ref().unwrap().clone();
        let store = modules.store.clone();
        store.ensure_workspace().unwrap();
        let user = store
            .users()
            .create("sg38", "Contract owner", None, "owner")
            .unwrap();
        store.transaction::<_,StoreError>(|tx| {
            tx.execute("INSERT INTO bots(id,workspace_id,name,harness,updated) VALUES (?,'default','Contract bot','claude-code',1)",[BOT])?;
            tx.execute("INSERT INTO bot_workspaces(bot_id,workspace_id,created) VALUES (?,'default',1)",[BOT])?;
            Ok(())
        }).unwrap();
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
                    title: "Contract item".into(),
                    bot_id: BOT.into(),
                    tags: vec![kind],
                    requested_by: Some(user.clone()),
                    ..Default::default()
                },
            )
            .unwrap()
            .unwrap();
        let session = store
            .sessions()
            .create(&steve_store::NewSession {
                workspace_id: "default".into(),
                item_id: Some(item.id),
                bot_id: Some(BOT.into()),
                harness: "claude-code".into(),
                token: TOKEN.into(),
                ..Default::default()
            })
            .unwrap();
        let (_, access) = store
            .access()
            .token_create(&user, "Contract client", None)
            .unwrap();
        let vault = Vault::new(
            store.clone(),
            Arc::new(FileKeyStore::open(home.path().join("keys")).unwrap()),
        );
        vault.init("default", &user).unwrap();
        let caps = Capabilities::new(vault);
        let library = wire::library_mcp::LibraryMcp::new(
            caps.clone(),
            None,
            Arc::new(steve_runtime::cache::Caches::new(Default::default())),
        );
        let service = Arc::new(ToolService {
            mcp: Arc::new(wire::core::CoreMcp {
                public_url: cfg.public_url.clone(),
                store: store.clone(),
                core: w.tools.core.clone(),
                home: cfg.home.clone(),
                workspaces: w.workspaces.clone(),
            }),
            extra: wire::mcp_extra::Extra {
                cfg,
                caps,
                store: store.clone(),
                modules,
                ports: steve_modules::contacts::Ports::default(),
                kb: steve_knowledge::Knowledge::new(|_| {}),
                tools: w.tools.clone(),
                library,
            },
        });
        let router = Router::new().fallback(|| async { "passed through" }).layer(
            axum::middleware::from_fn_with_state(service.clone(), wire::mcp_extra::layer),
        );
        Self {
            service,
            router,
            session,
            user,
            access,
            domain: w.domain,
            _home: home,
        }
    }
    pub async fn raw(
        &self,
        method: &str,
        auth: Option<&str>,
        peer: Option<&str>,
        body: String,
    ) -> axum::response::Response {
        let mut req = Request::builder().method(method).uri("/mcp");
        if let Some(t) = auth {
            req = req.header("authorization", format!("Bearer {t}"));
        }
        let mut req = req.body(Body::from(body)).unwrap();
        if let Some(p) = peer {
            req.extensions_mut()
                .insert(ConnectInfo(p.parse::<std::net::SocketAddr>().unwrap()));
        }
        self.router.clone().oneshot(req).await.unwrap()
    }
    pub async fn rpc(&self, method: &str, params: Value) -> Value {
        let r = self
            .raw(
                "POST",
                Some(TOKEN),
                Some("127.0.0.1:1234"),
                json!({"jsonrpc":"2.0","id":7,"method":method,"params":params}).to_string(),
            )
            .await;
        assert_eq!(r.status(), 200);
        serde_json::from_slice(
            &axum::body::to_bytes(r.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap()
    }
    pub async fn stop(self) {
        self.domain.shutdown().await;
    }
}
