//! The webhook entries (github.md §5.1, §12): `POST /hooks/github` for the App, and the PAT
//! repository hooks that arrive on the signals route `POST /hooks/<connection_id>` and are handed
//! here by adapter.
//!
//! Unauthenticated by design, so strict and in this order:
//! 1. body ≤ 1 MiB (`Content-Length` first, then while reading), else `413`;
//! 2. `X-Hub-Signature-256` checked over the raw bytes with the right secret, in constant time;
//!    anything wrong → `401` with an empty body, and nothing written;
//! 3. `X-GitHub-Event` / `X-GitHub-Delivery` / JSON body present, else `400`;
//! 4. ≤ 120 deliveries per connection per minute, else `429`;
//! 5. a known `X-GitHub-Delivery` → `200` (redelivery, not processed again);
//! 6. `gh_deliveries(status=ok)` and onto the queue → `202`. The work happens off the request
//!    (GitHub wants an answer within 10 seconds); its result lands on the delivery row.
//!
//! Before the signature is checked only reads happen: the App comes from the
//! `X-GitHub-Hook-Installation-Target-ID` header (or the payload's installation), the secret from
//! an in-memory cache filled from the vault without logging
//! ([`Vault::with_credential_unlogged`](steve_modules::vault::Vault::with_credential_unlogged)).
//! The vault's daily “used by Steve” row is written only after a signature verified with it, so a
//! forged request leaves every table as it was, cold cache or not.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};

use axum::body::{Body, Bytes};
use axum::extract::Path;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Extension, Router};
use serde_json::Value;
use steve_modules::ApiError;
use tokio::sync::{Notify, mpsc};
use zeroize::Zeroizing;

use crate::adapters::github_app::WEBHOOK_SECRET;
use crate::repo::{self, AppRow, DeliveryRow};
use crate::sync::Source;
use crate::{GitHub, Inner, signature};

/// Largest body accepted.
pub const BODY_LIMIT: usize = 1024 * 1024;
/// Deliveries per connection per minute (same as signals).
pub const RATE_PER_MINUTE: usize = 120;
/// The vault field of a repository hook's secret item.
pub const HOOK_SECRET_FIELD: &str = "webhook_secret";

/// `/hooks/github`, at the root (not under `/api`, no session).
pub fn routes<S: Clone + Send + Sync + 'static>(gh: GitHub) -> Router<S> {
    Router::new()
        .route(crate::manifest::HOOK_PATH, post(app_hook))
        .layer(Extension(gh))
}

/// For the signals route: is `connection_id` a connection whose hooks this module handles?
pub fn handles(adapter: &str) -> bool {
    adapter == crate::adapters::github_pat::ID
}

fn empty(code: StatusCode) -> Response {
    (code, [(header::CACHE_CONTROL, "no-store")]).into_response()
}

/// Reads at most [`BODY_LIMIT`] bytes; `Err(413)` beyond.
pub async fn read_body(headers: &HeaderMap, body: Body) -> Result<Bytes, Box<Response>> {
    let declared = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    if declared.is_some_and(|n| n > BODY_LIMIT as u64) {
        return Err(Box::new(empty(StatusCode::PAYLOAD_TOO_LARGE)));
    }
    axum::body::to_bytes(body, BODY_LIMIT)
        .await
        .map_err(|_| Box::new(empty(StatusCode::PAYLOAD_TOO_LARGE)))
}

async fn app_hook(Extension(gh): Extension<GitHub>, headers: HeaderMap, body: Body) -> Response {
    let bytes = match read_body(&headers, body).await {
        Ok(b) => b,
        Err(r) => return *r,
    };
    gh.receive_app(&headers, &bytes).await
}

/// `POST /hooks/<connection_id>` when the connection is a `github_pat` (the signals route calls
/// this; see [`handles`]). Same rules as `/hooks/github`.
pub async fn connection_hook(
    Extension(gh): Extension<GitHub>,
    Path(connection_id): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let bytes = match read_body(&headers, body).await {
        Ok(b) => b,
        Err(r) => return *r,
    };
    gh.receive_connection(&connection_id, &headers, &bytes)
        .await
}

fn header_str<'a>(h: &'a HeaderMap, k: &str) -> Option<&'a str> {
    h.get(k).and_then(|v| v.to_str().ok()).map(str::trim)
}

/// Delivery ids and event names: short, printable, no separators that could confuse a key.
fn token_ok(s: &str, max: usize) -> bool {
    !s.is_empty()
        && s.len() <= max
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

/// One job for the worker.
struct Job {
    source: Source,
    delivery_id: String,
    event: String,
    payload: Value,
}

/// The delivery queue: one worker per service, started on first use, holding only a weak
/// reference so dropping the service stops it.
#[derive(Default)]
pub(crate) struct Queue {
    tx: Mutex<Option<mpsc::UnboundedSender<Job>>>,
    pub(crate) pending: AtomicUsize,
    idle: Notify,
}

/// Sliding one-minute windows per key.
#[derive(Default)]
pub(crate) struct Limiter(Mutex<HashMap<String, VecDeque<i64>>>);

impl Limiter {
    fn allow(&self, key: &str, now: i64) -> bool {
        let mut m = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if m.len() > 10_000 {
            m.retain(|_, q| q.back().is_some_and(|t| now - t < 60_000));
        }
        let q = m.entry(key.to_string()).or_default();
        while q.front().is_some_and(|t| now - t >= 60_000) {
            q.pop_front();
        }
        if q.len() >= RATE_PER_MINUTE {
            return false;
        }
        q.push_back(now);
        true
    }
}

/// A cached webhook secret and whether its use has been logged in the vault yet (only after a
/// signature verified with it).
#[derive(Clone)]
struct Cached {
    secret: Zeroizing<String>,
    logged: bool,
}

/// Webhook secrets by `app:<app id>` / `item:<vault item>`. Never expire on their own; dropped
/// when the repo or App they belong to changes.
#[derive(Default)]
pub(crate) struct Secrets(Mutex<HashMap<String, Cached>>);

impl Secrets {
    fn get(&self, k: &str) -> Option<Cached> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(k)
            .cloned()
    }
    fn put(&self, k: &str, secret: Zeroizing<String>, logged: bool) {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(k.into(), Cached { secret, logged });
    }
    /// Marks the use as logged; `true` if it was not yet (the caller writes the log row).
    fn mark_logged(&self, k: &str) -> bool {
        let mut m = self.0.lock().unwrap_or_else(|e| e.into_inner());
        match m.get_mut(k) {
            Some(c) if !c.logged => {
                c.logged = true;
                true
            }
            _ => false,
        }
    }
    pub(crate) fn forget(&self, k: &str) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).remove(k);
    }
}

/// Where a webhook secret lives in the vault, and who uses it (for the use log).
struct SecretRef {
    key: String,
    ws: String,
    item: String,
    field: &'static str,
    actor: String,
    purpose: &'static str,
}

impl GitHub {
    /// `/hooks/github` after the body is read.
    pub async fn receive_app(&self, headers: &HeaderMap, body: &[u8]) -> Response {
        let Some(sig) = header_str(headers, signature::SIGNATURE_HEADER) else {
            return empty(StatusCode::UNAUTHORIZED);
        };
        let Some(app) = self.hook_app(headers, body) else {
            return empty(StatusCode::UNAUTHORIZED);
        };
        let sref = SecretRef {
            key: format!("app:{}", app.app_id),
            ws: app.workspace_id.clone(),
            item: app.credential_item.clone(),
            field: WEBHOOK_SECRET,
            actor: app.id.clone(),
            purpose: "GitHub App",
        };
        let Some(secret) = self.webhook_secret(&sref).await else {
            return empty(StatusCode::SERVICE_UNAVAILABLE);
        };
        if !signature::verify(secret.as_bytes(), body, sig) {
            return empty(StatusCode::UNAUTHORIZED);
        }
        self.log_secret_use(&sref).await;
        let payload: Value = match serde_json::from_slice(body) {
            Ok(v) => v,
            Err(_) => return empty(StatusCode::BAD_REQUEST),
        };
        // The connection of the installation (for the rate limit), else the App.
        let rate_key = payload["installation"]["id"]
            .as_i64()
            .and_then(|i| {
                self.store()
                    .reader(|c| {
                        repo::installation_connections(c, &app.workspace_id, i)
                            .map_err(ApiError::from)
                    })
                    .ok()
                    .and_then(|v| v.into_iter().next())
            })
            .unwrap_or_else(|| app.id.clone());
        self.accept(
            Source::App {
                ws: app.workspace_id.clone(),
            },
            &rate_key,
            headers,
            payload,
        )
    }

    /// A PAT repository hook (`/hooks/<connection_id>`) after the body is read.
    pub async fn receive_connection(
        &self,
        connection_id: &str,
        headers: &HeaderMap,
        body: &[u8],
    ) -> Response {
        let Some(sig) = header_str(headers, signature::SIGNATURE_HEADER) else {
            return empty(StatusCode::UNAUTHORIZED);
        };
        let ws = match self
            .store()
            .reader(|c| repo::connection_kind(c, connection_id).map_err(ApiError::from))
        {
            Ok(Some((ws, adapter))) if handles(&adapter) => ws,
            _ => return empty(StatusCode::UNAUTHORIZED),
        };
        // Which repo's secret: the payload says, but nothing is trusted until it verifies.
        let Ok(payload) = serde_json::from_slice::<Value>(body) else {
            return empty(StatusCode::UNAUTHORIZED);
        };
        let Some(gh_id) = payload["repository"]["id"].as_i64() else {
            return empty(StatusCode::UNAUTHORIZED);
        };
        let r = match self
            .store()
            .reader(|c| repo::repo_by_gh(c, &ws, gh_id).map_err(ApiError::from))
        {
            Ok(Some(r)) if r.connection_id == connection_id => r,
            _ => return empty(StatusCode::UNAUTHORIZED),
        };
        let Some(item) = r.poll_state["hook_item"].as_str().map(String::from) else {
            return empty(StatusCode::UNAUTHORIZED);
        };
        let sref = SecretRef {
            key: format!("item:{item}"),
            ws: ws.clone(),
            item,
            field: HOOK_SECRET_FIELD,
            actor: r.id.clone(),
            purpose: "GitHub Webhook",
        };
        let Some(secret) = self.webhook_secret(&sref).await else {
            return empty(StatusCode::SERVICE_UNAVAILABLE);
        };
        if !signature::verify(secret.as_bytes(), body, sig) {
            return empty(StatusCode::UNAUTHORIZED);
        }
        self.log_secret_use(&sref).await;
        self.accept(
            Source::Conn {
                ws,
                connection_id: connection_id.into(),
            },
            connection_id,
            headers,
            payload,
        )
    }

    /// Steps 3–6, after a valid signature.
    fn accept(
        &self,
        source: Source,
        rate_key: &str,
        headers: &HeaderMap,
        payload: Value,
    ) -> Response {
        let (Some(event), Some(delivery)) = (
            header_str(headers, "X-GitHub-Event").filter(|e| token_ok(e, 64)),
            header_str(headers, "X-GitHub-Delivery").filter(|d| token_ok(d, 100)),
        ) else {
            return empty(StatusCode::BAD_REQUEST);
        };
        let now = self.now();
        if !self.0.limiter.allow(rate_key, now) {
            return empty(StatusCode::TOO_MANY_REQUESTS);
        }
        let ws = match &source {
            Source::App { ws } | Source::Conn { ws, .. } => Some(ws.clone()),
            Source::Poll { .. } => None,
        };
        let row = DeliveryRow {
            delivery_id: delivery.into(),
            workspace_id: ws,
            repo_id: None,
            event: event.into(),
            action: payload["action"]
                .as_str()
                .map(|a| a.chars().take(64).collect()),
            status: "ok".into(),
            error: None,
            received: now,
        };
        let fresh = match self
            .store()
            .transaction(|tx| repo::insert_delivery(tx, &row).map_err(ApiError::from))
        {
            Ok(f) => f,
            Err(e) => {
                tracing::error!(error = %e, "github delivery not recorded");
                return empty(StatusCode::INTERNAL_SERVER_ERROR);
            }
        };
        if !fresh {
            return empty(StatusCode::OK);
        }
        self.enqueue(Job {
            source,
            delivery_id: row.delivery_id,
            event: row.event,
            payload,
        });
        empty(StatusCode::ACCEPTED)
    }

    fn enqueue(&self, job: Job) {
        let q = &self.0.queue;
        q.pending.fetch_add(1, Ordering::SeqCst);
        let mut tx = q.tx.lock().unwrap_or_else(|e| e.into_inner());
        if tx.as_ref().is_none_or(|t| t.is_closed()) {
            let (t, rx) = mpsc::unbounded_channel();
            tokio::spawn(worker(Arc::downgrade(&self.0), rx));
            *tx = Some(t);
        }
        if let Some(t) = tx.as_ref()
            && let Err(e) = t.send(job)
        {
            tracing::error!(delivery = %e.0.delivery_id, "github queue closed");
            self.done_one();
        }
    }

    pub(crate) fn done_one(&self) {
        let q = &self.0.queue;
        if q.pending.fetch_sub(1, Ordering::SeqCst) == 1 {
            q.idle.notify_waiters();
        }
    }

    /// Waits until every accepted delivery and every queued effect (opening, merging a PR) has
    /// been processed (tests, shutdown).
    pub async fn settle(&self) {
        loop {
            let n = self.0.queue.idle.notified();
            let e = self.0.effects.idle.notified();
            let deliveries = self.0.queue.pending.load(Ordering::SeqCst);
            let effects = self.0.effects.pending.load(Ordering::SeqCst);
            match (deliveries, effects) {
                (0, 0) => return,
                (0, _) => e.await,
                _ => n.await,
            }
        }
    }

    /// Processes one delivery and records how it went.
    async fn run_job(&self, job: Job) {
        let out = self.apply(&job.source, &job.event, &job.payload).await;
        if out.status == "failed" {
            tracing::warn!(delivery = %job.delivery_id, event = %job.event, error = ?out.error, "github delivery failed");
        }
        let r = self.store().transaction(|tx| {
            repo::set_delivery_result(
                tx,
                &job.delivery_id,
                out.ws.as_deref(),
                out.repo_id.as_deref(),
                out.status,
                out.error.as_deref(),
            )
            .map_err(ApiError::from)
        });
        if let Err(e) = r {
            tracing::error!(error = %e, "github delivery result not recorded");
        }
    }

    /// The App a delivery claims to be for: the hook target header, else the payload's
    /// installation (or `hook.app_id` of a ping). Reads only.
    fn hook_app(&self, headers: &HeaderMap, body: &[u8]) -> Option<AppRow> {
        let from_header = (header_str(headers, "X-GitHub-Hook-Installation-Target-Type")
            == Some("integration"))
        .then(|| header_str(headers, "X-GitHub-Hook-Installation-Target-ID"))
        .flatten()
        .and_then(|v| v.parse::<i64>().ok());
        let app_id = match from_header {
            Some(a) => a,
            None => {
                let v: Value = serde_json::from_slice(body).ok()?;
                match v["hook"]["app_id"].as_i64() {
                    Some(a) => a,
                    None => {
                        let inst = v["installation"]["id"].as_i64()?;
                        self.store()
                            .reader(|c| repo::app_of_installation(c, inst).map_err(ApiError::from))
                            .ok()
                            .flatten()?
                    }
                }
            }
        };
        self.store()
            .reader(|c| repo::app_by_app_id(c, app_id).map_err(ApiError::from))
            .ok()
            .flatten()
    }

    /// A webhook secret from the cache, else opened from the vault without logging (nothing is
    /// written before the signature is checked).
    async fn webhook_secret(&self, r: &SecretRef) -> Option<Zeroizing<String>> {
        if let Some(c) = self.0.secrets.get(&r.key) {
            return Some(c.secret);
        }
        let (gh, ws, item, field) = (self.clone(), r.ws.clone(), r.item.clone(), r.field);
        let s = tokio::task::spawn_blocking(move || {
            gh.connections()
                .vault()
                .with_credential_unlogged(&ws, &item, |c| {
                    c.text(field).map(|v| Zeroizing::new(v.to_string()))
                })
                .map_err(|e| tracing::warn!(error = %e, "github webhook secret unavailable"))
                .ok()
                .flatten()
        })
        .await
        .ok()
        .flatten()
        .filter(|v| !v.is_empty())?;
        self.0.secrets.put(&r.key, s.clone(), false);
        Some(s)
    }

    /// After a signature verified: the vault's use log, once per cached secret (the vault itself
    /// keeps it to one row per item, actor and day).
    async fn log_secret_use(&self, r: &SecretRef) {
        if !self.0.secrets.mark_logged(&r.key) {
            return;
        }
        let (gh, ws, item, actor, purpose) = (
            self.clone(),
            r.ws.clone(),
            r.item.clone(),
            r.actor.clone(),
            r.purpose,
        );
        let res = tokio::task::spawn_blocking(move || {
            gh.connections()
                .vault()
                .log_internal_use(&ws, &item, &actor, purpose)
        })
        .await;
        if !matches!(res, Ok(Ok(()))) {
            tracing::warn!("github webhook secret use not logged");
        }
    }

    /// A secret Steve just created (it is logged as created, not as used).
    pub(crate) fn remember_secret(&self, item: &str, s: Zeroizing<String>) {
        self.0.secrets.put(&format!("item:{item}"), s, false);
    }

    pub(crate) fn forget_secret(&self, item: &str) {
        self.0.secrets.forget(&format!("item:{item}"));
    }
}

async fn worker(inner: Weak<Inner>, mut rx: mpsc::UnboundedReceiver<Job>) {
    while let Some(job) = rx.recv().await {
        let Some(inner) = inner.upgrade() else {
            break;
        };
        let gh = GitHub(inner);
        gh.run_job(job).await;
        gh.done_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limiter_allows_120_a_minute_per_key() {
        let l = Limiter::default();
        for i in 0..RATE_PER_MINUTE {
            assert!(l.allow("a", 1_000 + i as i64));
        }
        assert!(!l.allow("a", 2_000));
        assert!(l.allow("b", 2_000), "keys are separate");
        assert!(l.allow("a", 61_000), "the window slides");
    }

    #[test]
    fn delivery_tokens() {
        assert!(token_ok("72d3162e-cc78-11e3-81ab-4c9367dc0958", 100));
        assert!(token_ok("pull_request_review", 64));
        assert!(!token_ok("", 64));
        assert!(!token_ok("a b", 64));
        assert!(!token_ok("a:b", 64));
        assert!(!token_ok(&"a".repeat(65), 64));
    }
}
