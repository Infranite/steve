//! Test helpers for the module crates (contacts.md §0.10): a store in a temp dir with the dev user and
//! two workspaces, a scripted model gateway, and a way to call a router as some user.
//! Never touches `~/.steve`.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, header};
use serde_json::Value;
use steve_store::{ChangeEvent, FnPublisher, Store};
use tempfile::TempDir;
use tower::ServiceExt;

use crate::{Caller, FakeClock, Gateway, GatewayOut, Modules, Registry, Tier};

/// The dev user (`dev` subject), member of both workspaces.
pub const ME: &str = "usr_dev";
/// A second user in `default` only, for visibility tests.
pub const OTHER: &str = "usr_other";
/// The default workspace and a second one, for the 404 tests.
pub const WS: &str = "default";
pub const WS2: &str = "w2";

/// A scripted model gateway. Answers, in this order: the first [`FakeGateway::script`] whose prefix the
/// prompt starts with (reusable), else the next queued [`FakeGateway::reply`]/[`FakeGateway::fail`],
/// else `{}`. Records every call so tests can assert on the prompts.
#[derive(Default)]
pub struct FakeGateway {
    scripts: Mutex<Vec<(String, Result<GatewayOut, String>)>>,
    replies: Mutex<VecDeque<Result<GatewayOut, String>>>,
    /// Every prompt received, in order.
    pub prompts: Mutex<Vec<String>>,
    /// Every call's tier and prompt, in order.
    pub calls: Mutex<Vec<(Tier, String)>>,
}

fn out(text: impl Into<String>) -> GatewayOut {
    GatewayOut {
        text: text.into(),
        cost_usd: None,
        model: "fake".into(),
        ..GatewayOut::default()
    }
}

impl FakeGateway {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
    /// Answers `text` to every prompt starting with `prefix`.
    pub fn script(&self, prefix: impl Into<String>, text: impl Into<String>) {
        self.script_out(prefix, out(text));
    }
    /// Like [`FakeGateway::script`] with a full answer (cost, model).
    pub fn script_out(&self, prefix: impl Into<String>, o: GatewayOut) {
        lock(&self.scripts).push((prefix.into(), Ok(o)));
    }
    /// Fails every prompt starting with `prefix`.
    pub fn script_fail(&self, prefix: impl Into<String>, err: impl Into<String>) {
        lock(&self.scripts).push((prefix.into(), Err(err.into())));
    }
    /// Queues one answer.
    pub fn reply(&self, s: impl Into<String>) {
        lock(&self.replies).push_back(Ok(out(s)));
    }
    /// Queues one failure.
    pub fn fail(&self, s: impl Into<String>) {
        lock(&self.replies).push_back(Err(s.into()));
    }
    pub fn last_prompt(&self) -> Option<String> {
        lock(&self.prompts).last().cloned()
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[async_trait]
impl Gateway for FakeGateway {
    async fn run(&self, tier: Tier, prompt: &str, _: Duration) -> Result<GatewayOut, String> {
        lock(&self.prompts).push(prompt.into());
        lock(&self.calls).push((tier, prompt.into()));
        if let Some((_, r)) = lock(&self.scripts)
            .iter()
            .find(|(p, _)| prompt.starts_with(p.as_str()))
        {
            return r.clone();
        }
        lock(&self.replies)
            .pop_front()
            .unwrap_or_else(|| Ok(out("{}")))
    }
}

/// A fresh store with users and workspaces, the modules state, and the published events.
pub struct TestEnv {
    pub dir: TempDir,
    pub modules: Modules,
    pub gateway: Arc<FakeGateway>,
    /// The modules' clock, starting at the real time.
    pub clock: Arc<FakeClock>,
    pub events: Arc<Mutex<Vec<ChangeEvent>>>,
}

impl TestEnv {
    /// Migrated store in a temp dir; `register` fills the registry like the integrator would.
    pub fn new(register: impl FnOnce(&mut Registry)) -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let events: Arc<Mutex<Vec<ChangeEvent>>> = Arc::default();
        let sink = events.clone();
        let store = Store::open(
            dir.path().join("steve.sqlite3"),
            FnPublisher(move |e| sink.lock().unwrap_or_else(|p| p.into_inner()).push(e)),
        )
        .expect("open store");
        store.ensure_workspace().expect("workspace");
        store
            .with_conn(|c| {
                c.execute_batch(
                    "INSERT INTO workspaces (id, name, created) VALUES ('w2', '另一个', 1);
                     INSERT INTO users (id, workspace_id, subject, name, role, created) VALUES ('usr_dev', 'default', 'dev', 'Dev', 'owner', 1), ('usr_other', 'default', 'other', 'Other', 'member', 1);
                     INSERT INTO workspace_members (workspace_id, user_id, role, created) VALUES ('default', 'usr_dev', 'owner', 1), ('w2', 'usr_dev', 'owner', 1), ('default', 'usr_other', 'member', 1);",
                )?;
                Ok(())
            })
            .expect("seed");
        let gateway = FakeGateway::new();
        let mut reg = Registry::new();
        register(&mut reg);
        let clock = Arc::new(FakeClock::new(steve_store::now()));
        let modules = Modules::with_clock(store, gateway.clone(), reg, clock.clone());
        events.lock().unwrap_or_else(|p| p.into_inner()).clear();
        Self {
            dir,
            modules,
            gateway,
            clock,
            events,
        }
    }

    pub fn store(&self) -> &Store {
        &self.modules.store
    }

    /// Returns and clears the events published so far.
    pub fn take_events(&self) -> Vec<ChangeEvent> {
        std::mem::take(&mut *self.events.lock().unwrap_or_else(|p| p.into_inner()))
    }
}

pub fn caller(ws: &str, me: &str) -> Caller {
    Caller {
        ws: ws.into(),
        me: me.into(),
    }
}

/// Sends one request to `router` as `who` (the extension the integrator's auth layer would insert);
/// returns status and JSON body (`Value::Null` for an empty body).
pub async fn call(
    router: &Router,
    who: Option<&Caller>,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let mut req = Request::builder().method(method).uri(path);
    if body.is_some() {
        req = req.header(header::CONTENT_TYPE, "application/json");
    }
    let mut req = req
        .body(match body {
            Some(b) => Body::from(b.to_string()),
            None => Body::empty(),
        })
        .expect("request");
    if let Some(c) = who {
        req.extensions_mut().insert(c.clone());
    }
    let res = router.clone().oneshot(req).await.expect("infallible");
    let status = res.status().as_u16();
    let bytes = axum::body::to_bytes(res.into_body(), 16 * 1024 * 1024)
        .await
        .expect("body");
    let v = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into()))
    };
    (status, v)
}

/// Sends a raw body (for malformed-JSON tests).
pub async fn call_raw(
    router: &Router,
    who: &Caller,
    method: &str,
    path: &str,
    body: &str,
) -> (u16, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .expect("request");
    req.extensions_mut().insert(who.clone());
    let res = router.clone().oneshot(req).await.expect("infallible");
    let status = res.status().as_u16();
    let bytes = axum::body::to_bytes(res.into_body(), 1024 * 1024)
        .await
        .expect("body");
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}
