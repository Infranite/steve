//! Remote Nodes (N2): pairing, the `/node/ws` listener, the registry of live links, the guards
//! every Node request passes and the fleet of per-Node coordinators.
//!
//! On by default; `STEVE_NODE_LISTEN=0` disables it. NO_RESUME copies never mount it. The Hub decides, the
//! Node executes; ACP and MCP never cross the link. The listener rides the Hub's own port and
//! accepts loopback peers or explicitly configured TLS-terminating proxy peers.

pub mod fleet;
pub mod guard;
pub mod handover;
mod ingress;
pub mod install;
pub mod listener;
pub mod pairing;
pub mod registry;
pub mod tailcat;
pub mod vitals;
pub use ingress::Ingress;

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::routing::{get, post};
use rusqlite::OptionalExtension;
use steve_store::{Store, StoreError};

pub use fleet::NodeFleet;
pub use registry::NodeRegistry;

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Enabled unless explicitly disabled; database-copy isolation is enforced by the Hub wiring.
pub fn enabled(var: impl Fn(&str) -> Option<String>) -> bool {
    match var("STEVE_NODE_LISTEN").as_deref().map(str::trim) {
        None | Some("") | Some("1") => true,
        Some("0") | Some("off") => false,
        Some(_) => {
            tracing::warn!("STEVE_NODE_LISTEN must be 1 or 0; remote Node listener disabled");
            false
        }
    }
}

/// The TCP peer, when the server records it.
pub struct Peer(pub Option<SocketAddr>);

impl<S: Send + Sync> axum::extract::FromRequestParts<S> for Peer {
    type Rejection = std::convert::Infallible;
    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _: &S,
    ) -> Result<Self, Self::Rejection> {
        Ok(Peer(
            parts
                .extensions
                .get::<axum::extract::ConnectInfo<SocketAddr>>()
                .map(|c| c.0),
        ))
    }
}

pub struct NodeService {
    pub store: Store,
    pub hub_id: String,
    /// `sha256:…` of the Hub's certificate when it terminates TLS itself (`STEVE_NODE_TLS_PIN`).
    pub pin: Option<String>,
    pub ingress: Ingress,
    pub registry: Arc<NodeRegistry>,
    pub fleet: Arc<NodeFleet>,
    pub limiter: pairing::Limiter,
    /// Taken while a link is checked against the store and installed, and while the owner's
    /// revoke/pause/resume acts on the live link: one cannot slip between the other's steps.
    pub(crate) links: tokio::sync::Mutex<()>,
    /// Vault uses' values for the commands given to each Node (set once at boot; without it a
    /// Node's `secret.take` is `unsupported`).
    pub secrets: std::sync::OnceLock<Arc<crate::wire::node_secrets::NodeSecrets>>,
    /// The Hub's identity key (M1): it signs every welcome, and pairing hands its public half to
    /// the Node. Set once at boot ([`identity_at`]); unset (tests), welcomes go unsigned.
    pub identity: std::sync::OnceLock<Arc<steve_node_wire::Identity>>,
    /// Links still in the handshake ([`MAX_UNPROVEN`] at most).
    pub(crate) handshakes: Arc<tokio::sync::Semaphore>,
    /// Reaching the Node routes over tailcat (set once at boot).
    pub tailcat: std::sync::OnceLock<Arc<tailcat::Tailcat>>,
    /// Installing Nodes on other machines over tailcat's SSH (set once at boot).
    pub installs: std::sync::OnceLock<Arc<install::Installs>>,
}

const HUB_ID_KEY: &str = "node.hub_id";

/// The Hub's identity key in `home` (`secrets/hub-identity.key`, 0600), made the first time.
pub fn identity_at(home: &std::path::Path) -> std::io::Result<steve_node_wire::Identity> {
    let path = home.join("secrets/hub-identity.key");
    match steve_node_wire::Identity::load(&path) {
        Ok(id) => Ok(id),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let id = steve_node_wire::Identity::generate()?;
            id.save(&path)?;
            Ok(id)
        }
        Err(e) => Err(e),
    }
}

/// How many Node links may be in the handshake at once (M5); more are answered 503 before upgrading.
pub const MAX_UNPROVEN: usize = 64;

/// This Hub's random id, made once and kept in the settings.
pub fn hub_id(store: &Store) -> Result<String, StoreError> {
    store.transaction(|tx| {
        let old: Option<String> = tx
            .query_row(
                "SELECT value FROM settings WHERE workspace_id='default' AND key=?",
                [HUB_ID_KEY],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(v) = old {
            return Ok(v);
        }
        let raw = steve_node_wire::identity::random::<12>()
            .map_err(|e| StoreError::Conflict(e.to_string()))?;
        let id = format!("hub_{}", hex::encode(raw));
        tx.execute(
            "INSERT INTO settings(workspace_id,key,value,updated) VALUES ('default',?,?,?)",
            rusqlite::params![HUB_ID_KEY, id, now()],
        )?;
        Ok(id)
    })
}

impl NodeService {
    pub fn new(
        store: Store,
        fleet: Arc<NodeFleet>,
        pin: Option<String>,
    ) -> Result<Arc<Self>, StoreError> {
        Self::with_ingress(store, fleet, pin, Ingress::default())
    }

    fn with_ingress(
        store: Store,
        fleet: Arc<NodeFleet>,
        pin: Option<String>,
        ingress: Ingress,
    ) -> Result<Arc<Self>, StoreError> {
        Ok(Arc::new(NodeService {
            hub_id: hub_id(&store)?,
            store,
            pin,
            ingress,
            registry: Arc::new(NodeRegistry::default()),
            fleet,
            limiter: pairing::Limiter::default(),
            links: tokio::sync::Mutex::new(()),
            secrets: std::sync::OnceLock::new(),
            identity: std::sync::OnceLock::new(),
            handshakes: Arc::new(tokio::sync::Semaphore::new(MAX_UNPROVEN)),
            tailcat: std::sync::OnceLock::new(),
            installs: std::sync::OnceLock::new(),
        }))
    }
}

/// The service unless `STEVE_NODE_LISTEN=0`, over `fleet`; `None` otherwise (and on a store error,
/// which is logged: the Hub runs on without remote Nodes, their sessions wait).
pub fn from_vars(
    store: &Store,
    fleet: Arc<NodeFleet>,
    var: impl Fn(&str) -> Option<String>,
) -> Option<Arc<NodeService>> {
    if !enabled(&var) {
        return None;
    }
    let pin = var("STEVE_NODE_TLS_PIN").filter(|p| !p.is_empty());
    let ingress = match Ingress::from_vars(&var) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!("remote nodes disabled: {e}");
            return None;
        }
    };
    match NodeService::with_ingress(store.clone(), fleet, pin, ingress) {
        Ok(s) => Some(s),
        Err(e) => {
            tracing::error!("remote nodes disabled: {e}");
            None
        }
    }
}

/// `/node/pair` and `/node/ws`: no login, the code or the signature is the credential.
pub fn node_router(svc: Arc<NodeService>) -> Router {
    Router::new()
        .route("/node/pair", post(pairing::claim))
        .route("/node/rotate", post(pairing::rotate))
        .route("/node/ws", get(listener::upgrade))
        .with_state(svc)
}

/// The owner's routes, mounted under `/api` behind the login.
pub fn api_router(svc: Arc<NodeService>) -> Router {
    Router::new()
        .route("/nodes", get(pairing::list))
        .route("/nodes/pairings", post(pairing::create))
        .route("/nodes/tailcat", get(tailcat::get).put(tailcat::put))
        .route("/nodes/installs", post(install::create))
        .route("/nodes/installs/{id}", get(install::get))
        .route("/nodes/installs/{id}/run", post(install::run))
        .route("/nodes/{id}/{action}", post(pairing::act))
        .with_state(svc)
}
