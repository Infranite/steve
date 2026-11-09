//! Remote Nodes over tailcat (A6): the owner turns it on in Devices, and Nodes without a route to
//! the Hub's HTTPS reach it at `tailcat:<address>:<port>` (see [`steve_node_wire::tailcat`]).
//!
//! What goes into the tunnel is a listener on `127.0.0.1:<port>` that answers `/node/pair` and
//! `/node/ws` only, never the API: a tunnelled peer arrives from loopback, which the Hub's own port
//! trusts for more than the Node routes do. The key lives in `$STEVE_HOME/tailcat/` and keeps the
//! address across restarts; `tailcat serve` runs as a child, started again when it exits. Off, or
//! without the program, nothing listens and nothing runs.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use axum::Json;
use axum::extract::State;
use axum::response::Response;
use rusqlite::OptionalExtension;
use serde::Deserialize;
use serde_json::{Value, json};
use steve_modules::Caller;
use steve_node_wire::tailcat;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::task::JoinHandle;

use super::NodeService;
use crate::error::json_response;

/// The setting that keeps it on across restarts.
pub const KEY: &str = "nodes.tailcat";

pub struct Tailcat {
    dir: PathBuf,
    port: u16,
    svc: Weak<NodeService>,
    state: Mutex<Running>,
}

#[derive(Default)]
struct Running {
    on: bool,
    address: Option<String>,
    up: bool,
    error: Option<String>,
    tasks: Vec<JoinHandle<()>>,
    /// The running `tailcat serve`, killed here when the Hub exits (the task would not get to it).
    pid: Option<u32>,
}

/// The one this Hub runs, for [`shutdown`].
static CURRENT: Mutex<Option<Weak<Tailcat>>> = Mutex::new(None);

/// Ends `tailcat serve` before the Hub exits, so no orphan keeps the address.
pub fn shutdown() {
    let current = CURRENT.lock().unwrap().as_ref().and_then(Weak::upgrade);
    if let Some(t) = current {
        t.stop();
    }
}

impl Tailcat {
    /// `home`: the Hub's home; `port`: what the Node routes listen on in the tunnel.
    pub fn new(home: &std::path::Path, port: u16, svc: &Arc<NodeService>) -> Arc<Tailcat> {
        let t = Arc::new(Tailcat {
            dir: home.join("tailcat"),
            port,
            svc: Arc::downgrade(svc),
            state: Mutex::new(Running::default()),
        });
        *CURRENT.lock().unwrap() = Some(Arc::downgrade(&t));
        t
    }

    pub fn status(&self) -> Value {
        let s = self.state.lock().unwrap();
        json!({
            "available": tailcat::program().is_some(),
            "enabled": s.on,
            "running": s.up,
            "address": s.address,
            "hub_url": s.address.as_deref().map(|a| tailcat::hub_url(a, self.port)),
            "port": self.port,
            "error": s.error,
        })
    }

    pub fn stop(&self) {
        let mut s = self.state.lock().unwrap();
        for t in s.tasks.drain(..) {
            t.abort();
        }
        #[cfg(unix)]
        if let Some(pid) = s.pid.take() {
            // SAFETY: a plain signal to our own child's pid
            unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
        }
        *s = Running::default();
    }

    /// Opens the listener and starts the tunnel; an error says why it cannot (no program, port
    /// taken, no key), and nothing is left running.
    pub async fn start(self: &Arc<Self>) -> Result<(), String> {
        self.stop();
        let program = tailcat::program()
            .ok_or("tailcat is not installed on the Hub's machine (or set STEVE_TAILCAT)")?;
        let svc = self.svc.upgrade().ok_or("remote Nodes are off")?;
        let key = self.key(&program).await?;
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", self.port))
            .await
            .map_err(|e| format!("127.0.0.1:{}: {e}", self.port))?;
        use axum::serve::ListenerExt as _;
        let listener =
            listener.tap_io(|tcp: &mut tokio::net::TcpStream| steve_node_wire::sock::low_water(tcp));
        let app = super::node_router(svc);
        let serve = tokio::spawn(async move {
            let _ = axum::serve(
                listener,
                app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await;
        });
        let me = Arc::downgrade(self);
        let port = self.port;
        let run = tokio::spawn(async move { tunnel(me, program, key, port).await });
        let mut s = self.state.lock().unwrap();
        s.on = true;
        s.tasks = vec![serve, run];
        Ok(())
    }

    /// The saved key, made once with a fixed relay region so the address stays the same.
    async fn key(&self, program: &std::path::Path) -> Result<PathBuf, String> {
        let key = self.dir.join("hub.private.json");
        if tokio::fs::metadata(&key).await.is_ok() {
            return Ok(key);
        }
        tokio::fs::create_dir_all(&self.dir)
            .await
            .map_err(|e| format!("{}: {e}", self.dir.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = tokio::fs::set_permissions(&self.dir, std::fs::Permissions::from_mode(0o700)).await;
        }
        let out = tokio::process::Command::new(program)
            .arg("genkey")
            .arg(format!("--key={}", key.display()))
            .arg("--fixed-region")
            .stdin(std::process::Stdio::null())
            .output()
            .await
            .map_err(|e| format!("tailcat genkey: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "tailcat genkey: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        if let Some(a) = String::from_utf8_lossy(&out.stdout)
            .lines()
            .find_map(tailcat::address_in)
        {
            self.state.lock().unwrap().address = Some(a.to_string());
        }
        Ok(key)
    }

    fn note(&self, f: impl FnOnce(&mut Running)) {
        f(&mut self.state.lock().unwrap());
    }
}

/// `tailcat serve <port>` for as long as it is on, started again after it exits.
async fn tunnel(me: Weak<Tailcat>, program: PathBuf, key: PathBuf, port: u16) {
    let mut backoff = Duration::from_secs(1);
    loop {
        let child = tokio::process::Command::new(&program)
            .arg(format!("--key={}", key.display()))
            .args(["serve", &port.to_string()])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn();
        let mut last = String::new();
        match child {
            Err(e) => last = format!("tailcat serve: {e}"),
            Ok(mut child) => {
                let pid = child.id();
                if let Some(t) = me.upgrade() {
                    t.note(|s| s.pid = pid);
                }
                if let Some(err) = child.stderr.take() {
                    let mut lines = BufReader::new(err).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        if let Some(a) = tailcat::address_in(&line) {
                            let Some(t) = me.upgrade() else { return };
                            t.note(|s| {
                                s.address = Some(a.to_string());
                                s.up = true;
                                s.error = None;
                            });
                            tracing::info!("remote Nodes reach this Hub at {}", tailcat::hub_url(a, port));
                            backoff = Duration::from_secs(1);
                        } else {
                            tracing::debug!("tailcat: {line}");
                        }
                        last = line;
                    }
                }
                let status = child.wait().await;
                last = format!("tailcat serve exited ({}): {last}", status.map(|s| s.to_string()).unwrap_or_default());
            }
        }
        tracing::warn!("{last}");
        let Some(t) = me.upgrade() else { return };
        t.note(|s| {
            s.up = false;
            s.error = Some(last);
        });
        drop(t);
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(Duration::from_secs(60));
    }
}

/// Turns it back on at boot when the owner left it on; a failure is logged and shown in Devices.
pub async fn restore(t: Arc<Tailcat>, store: steve_store::Store) {
    let on = tokio::task::spawn_blocking(move || {
        store.settings().get::<bool>(steve_store::WS, KEY, false)
    })
    .await
    .ok()
    .and_then(Result::ok)
    .unwrap_or(false);
    if on && let Err(e) = t.start().await {
        tracing::warn!("remote Nodes over tailcat: {e}");
        t.note(|s| {
            s.on = true;
            s.error = Some(e);
        });
    }
}

pub async fn get(State(svc): State<Arc<NodeService>>, _caller: Caller) -> Response {
    match svc.tailcat.get() {
        Some(t) => json_response(200, &t.status()),
        None => json_response(200, &json!({ "available": false, "enabled": false })),
    }
}

#[derive(Deserialize)]
pub struct Put {
    enabled: bool,
}

/// On or off, the owner's only. On answers what started, or why it could not.
pub async fn put(
    State(svc): State<Arc<NodeService>>,
    caller: Caller,
    Json(b): Json<Put>,
) -> Response {
    let Some(t) = svc.tailcat.get().cloned() else {
        return json_response(404, &json!({ "error": "not available" }));
    };
    let store = svc.store.clone();
    let me = caller.me.clone();
    let owner = tokio::task::spawn_blocking(move || {
        store.reader(|c| {
            c.query_row(
                "SELECT role FROM users WHERE id=?",
                [&me],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(steve_store::StoreError::from)
        })
    })
    .await;
    if !matches!(owner, Ok(Ok(Some(ref r))) if r == "owner") {
        return json_response(403, &json!({ "error": steve_i18n::t!("hub.nodes.tailcat_owner_only") }));
    }
    let result = if b.enabled { t.start().await } else {
        t.stop();
        Ok(())
    };
    if result.is_ok() {
        let (store, me) = (svc.store.clone(), caller.me.clone());
        let mut p = serde_json::Map::new();
        p.insert(KEY.into(), json!(b.enabled));
        let saved = tokio::task::spawn_blocking(move || store.settings().set(steve_store::WS, &p, &me)).await;
        if !matches!(saved, Ok(Ok(()))) {
            tracing::error!("saving {KEY}: {saved:?}");
        }
    }
    match result {
        Ok(()) => {
            // the address comes with the first line of `tailcat serve`: a moment, not a request
            for _ in 0..50 {
                if !b.enabled || t.status()["running"] == true {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            json_response(200, &t.status())
        }
        Err(e) => json_response(409, &json!({ "error": e, "status": t.status() })),
    }
}
