#![allow(dead_code)]
//! A Hub with remote Nodes enabled, served on a real 127.0.0.1 listener, and Nodes dialling it
//! over a real WebSocket (plain, loopback only).

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::Router;
use axum::extract::Request;
use axum::middleware::{self, Next};
use axum::response::Response;
use serde_json::{Value, json};
use steve_hub::nodes::{self, NodeFleet, NodeService};
use steve_hub::wire::node_runtime::{AttemptFacts, Binding, NodeDomain, NodeRuntime};
use steve_hub::wire::node_tools::Service;
use steve_node_api::testkit::build;
use steve_node_api::*;
use steve_node_wire::handshake::{self, NodeIntro};
use steve_node_wire::tls::{self, Trust};
use steve_node_wire::{Conn, ConnOptions, HubLink, Identity, NodeServer, identity};
use steve_store::{FnPublisher, Session, Store, StoreError};

#[derive(Default)]
pub struct Ends {
    pub turns: Mutex<Vec<(String, String)>>,
    pub attempts: Mutex<Vec<String>>,
}

impl NodeDomain for Ends {
    fn turn_ended(&self, session: &str, _: Option<&str>, stop: &str, _: Option<String>) {
        self.turns
            .lock()
            .unwrap()
            .push((session.into(), stop.into()));
    }
    fn attempt_finished(&self, _: &str, attempt: &str, _: &Completion, _: &AttemptFacts, _: i64) {
        self.attempts.lock().unwrap().push(attempt.into());
    }
}

/// Steve's tools as the Hub would serve them: one tool, counted.
#[derive(Default)]
pub struct FakeService {
    pub calls: Mutex<Vec<(String, String, Value)>>,
}

#[async_trait]
impl Service for FakeService {
    fn catalog(&self) -> Vec<ToolDescriptor> {
        vec![ToolDescriptor {
            name: "steve_update".into(),
            description: "Update the item.".into(),
            input_schema: json!({"type": "object", "properties": {"now": {"type": "string"}}}),
            read_only: false,
        }]
    }
    async fn call(&self, session: Session, name: &str, args: Value) -> Result<String, String> {
        self.calls
            .lock()
            .unwrap()
            .push((session.id.clone(), name.into(), args));
        Ok("Steve updated the item.".into())
    }
}

pub struct Hub {
    pub dir: tempfile::TempDir,
    pub store: Store,
    pub svc: Arc<NodeService>,
    pub fleet: Arc<NodeFleet>,
    pub addr: SocketAddr,
    pub ends: Arc<Ends>,
    pub service: Arc<FakeService>,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for Hub {
    fn drop(&mut self) {
        self.server.abort();
    }
}

/// The signed-in user is whoever `x-test-user` says (the real login sits in front in the Hub).
async fn caller(mut req: Request, next: Next) -> Response {
    let me = req
        .headers()
        .get("x-test-user")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    req.extensions_mut().insert(steve_modules::Caller {
        ws: "default".into(),
        me,
    });
    next.run(req).await
}

pub fn store_at(dir: &std::path::Path) -> Store {
    let store = Store::open(dir.join("steve.sqlite3"), FnPublisher(|_| {})).unwrap();
    store.ensure_workspace().unwrap();
    store
}

impl Hub {
    pub async fn start() -> Hub {
        let dir = tempfile::tempdir().unwrap();
        let store = store_at(dir.path());
        Hub::on(dir, store, None).await
    }

    /// A Hub on `store` (a restart: same database), on `port` when given.
    pub async fn on(dir: tempfile::TempDir, store: Store, port: Option<u16>) -> Hub {
        let ends = Arc::new(Ends::default());
        let service = Arc::new(FakeService::default());
        let fleet = NodeFleet::new(
            store.clone(),
            dir.path().join("data"),
            Some(ends.clone()),
            Some(service.clone()),
        );
        let svc = NodeService::new(store.clone(), fleet.clone(), None).unwrap();
        let app = Router::new().merge(nodes::node_router(svc.clone())).nest(
            "/api",
            nodes::api_router(svc.clone()).layer(middleware::from_fn(caller)),
        );
        let mut listener = None;
        for _ in 0..100 {
            match tokio::net::TcpListener::bind(("127.0.0.1", port.unwrap_or(0))).await {
                Ok(l) => {
                    listener = Some(l);
                    break;
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
            }
        }
        let listener = listener.expect("bind");
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });
        Hub {
            dir,
            store,
            svc,
            fleet,
            addr,
            ends,
            service,
            server,
        }
    }

    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }
    pub fn ws_url(&self) -> String {
        format!("ws://{}/node/ws", self.addr)
    }

    pub async fn post(&self, user: &str, path: &str, body: Value) -> (u16, Value) {
        let r = reqwest::Client::new()
            .post(format!("{}{path}", self.url()))
            .header("x-test-user", user)
            .json(&body)
            .send()
            .await
            .unwrap();
        let s = r.status().as_u16();
        (s, r.json().await.unwrap_or(Value::Null))
    }

    pub async fn code(&self, user: &str) -> String {
        let (s, v) = self.post(user, "/api/nodes/pairings", json!({})).await;
        assert_eq!(s, 200, "{v}");
        v["code"].as_str().unwrap().to_string()
    }

    /// Claims `code` with `id` the way the CLI does.
    pub async fn claim(&self, code: &str, id: &Identity) -> (u16, Value) {
        let pk = id.public();
        let sig = id.sign(&identity::pair_message(code, &pk));
        self.post(
            "",
            "/node/pair",
            json!({"code": code, "pubkey": hex::encode(pk), "name": "test", "sig": hex::encode(sig)}),
        )
        .await
    }

    /// A new identity paired and confirmed by `user`: its node id. `user` owns the default
    /// workspace alone (a remote Node takes only its owner's own work, H1).
    pub async fn paired(&self, user: &str) -> (Identity, NodeId) {
        self.owns_default(user);
        let id = Identity::generate().unwrap();
        let code = self.code(user).await;
        let (s, v) = self.claim(&code, &id).await;
        assert_eq!(s, 200, "{v}");
        let node = v["node_id"].as_str().unwrap().to_string();
        let (s, _) = self
            .post(user, &format!("/api/nodes/{node}/confirm"), json!({}))
            .await;
        assert_eq!(s, 200);
        (id, NodeId::new(node).unwrap())
    }

    /// `user` is an owner of the default workspace.
    pub fn owns_default(&self, user: &str) {
        self.store
            .transaction::<_, StoreError>(|tx| {
                tx.execute(
                    "INSERT OR IGNORE INTO users(id,workspace_id,subject,name,role,created) VALUES (?1,'default',?1,?1,'owner',1)",
                    [user],
                )?;
                tx.execute(
                    "INSERT OR IGNORE INTO workspace_members(workspace_id,user_id,role,created) VALUES ('default',?1,'owner',1)",
                    [user],
                )?;
                Ok(())
            })
            .unwrap();
    }

    pub fn session(&self, id: &str) {
        self.store
            .transaction::<_, StoreError>(|tx| {
                tx.execute(
                    "INSERT INTO sessions(id,workspace_id,harness,cwd,status,created,updated) VALUES (?,'default','claude-code','.','active',1,1)",
                    [id],
                )?;
                Ok(())
            })
            .unwrap();
    }

    pub fn q<T: rusqlite::types::FromSql>(&self, sql: &str) -> T {
        self.store
            .reader(|c| Ok::<_, StoreError>(c.query_row(sql, [], |r| r.get(0))?))
            .unwrap()
    }

    /// Binds `session` to `node`'s coordinator (started here) with Steve's tools allowed.
    pub fn bind(&self, node: &NodeId, session: &str) -> Arc<NodeRuntime> {
        let rt = self.fleet.start(node).unwrap();
        let mut policy = build::policy();
        policy.tools = GrantId::new("hub").unwrap();
        rt.bind(&Binding {
            session: session.into(),
            resource: format!("item:{session}"),
            prepare: PrepareKind::Scratch,
            choice: build::choice(),
            policy,
        })
        .unwrap();
        rt
    }
}

/// A Node's end of a link: its connection and what its Hub-side ports go through.
pub struct Linked {
    pub conn: Arc<Conn>,
    pub link: Arc<HubLink>,
    pub generation: u64,
}

pub async fn dial(
    hub: &Hub,
    id: &Identity,
    node: &NodeId,
    serve: Arc<dyn Node>,
    incarnation: u64,
) -> Result<Linked, handshake::Refused> {
    dial_url(&hub.ws_url(), &hub.svc.hub_id, id, node, serve, incarnation).await
}

pub async fn dial_url(
    url: &str,
    hub_id: &str,
    id: &Identity,
    node: &NodeId,
    serve: Arc<dyn Node>,
    incarnation: u64,
) -> Result<Linked, handshake::Refused> {
    dial_url_as(
        url,
        hub_id,
        id,
        node,
        serve,
        incarnation,
        steve_node_api::PROTOCOL,
    )
    .await
}

/// [`dial_url`] as a Node whose hello says `proto` (an earlier release's).
pub async fn dial_url_as(
    url: &str,
    hub_id: &str,
    id: &Identity,
    node: &NodeId,
    serve: Arc<dyn Node>,
    incarnation: u64,
    proto: (u16, u16),
) -> Result<Linked, handshake::Refused> {
    let (mut sink, mut stream) = tls::connect(url, &Trust::LoopbackPlaintext).await.unwrap();
    let (_, w) = handshake::dial_as(
        &mut sink,
        &mut stream,
        NodeIntro {
            node,
            identity: id,
            incarnation,
            heads: vec![],
            caps: None,
            hub_id: Some(hub_id),
        },
        proto,
    )
    .await?;
    let conn = Conn::spawn(
        sink,
        stream,
        w.generation,
        NodeServer::new(serve),
        ConnOptions {
            side: "n",
            ..Default::default()
        },
    );
    let link = HubLink::new();
    link.set(Some(conn.clone()));
    Ok(Linked {
        conn,
        link,
        generation: w.generation,
    })
}

pub async fn until(what: &str, secs: u64, f: impl Fn() -> bool) {
    let t0 = std::time::Instant::now();
    while !f() {
        assert!(
            t0.elapsed() < Duration::from_secs(secs),
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

impl Hub {
    /// The Hub process dies and comes back on the same database and port: every link drops, every
    /// coordinator is gone, and a new one starts from what the store says.
    pub async fn restart(mut self) -> Hub {
        for n in self.fleet.nodes() {
            self.svc.registry.kill(&n, "hub restart");
        }
        self.server.abort();
        let port = self.addr.port();
        let dir = std::mem::replace(&mut self.dir, tempfile::tempdir().unwrap());
        drop(self);
        tokio::time::sleep(Duration::from_millis(300)).await;
        let store = store_at(dir.path());
        Hub::on(dir, store, Some(port)).await
    }

    /// What the coordinator and the audit trail say (for failing tests).
    pub fn dump(&self) -> String {
        self.store
            .reader(|c| {
                let mut out = String::new();
                for sql in [
                    "SELECT operation_id||' '||method||' '||state||' '||COALESCE(receipt,'') FROM node_commands",
                    "SELECT what||' '||COALESCE(safe_detail,'') FROM node_audit",
                    "SELECT session_id||' '||phase FROM runtime_sessions",
                ] {
                    let mut st = c.prepare(sql)?;
                    for r in st.query_map([], |r| r.get::<_, String>(0))? {
                        out.push_str(&r?);
                        out.push('\n');
                    }
                }
                Ok::<_, StoreError>(out)
            })
            .unwrap()
    }
}
