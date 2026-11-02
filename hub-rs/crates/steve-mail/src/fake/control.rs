//! HTTP control interface over a set of running fakes, for the
//! `steve-fake-mail` binary (web development against fake mailboxes).
//! Rust tests use the handles directly instead.
//!
//! Listens on 127.0.0.1 only and has no authentication: it controls fake
//! mailboxes with fixture data, nothing else.
//!
//! - `GET  /control` — ports, accounts, CA certificate (PEM)
//! - `GET  /control/fixtures` — fixture names
//! - `POST /control/inject` `{target: imap|gmail|graph, user?, folder?, labels?, fixture? | eml?}`
//! - `POST /control/set_flags` `{user?, folder, uid, flags}` (IMAP)
//! - `POST /control/bump_uidvalidity` `{user?, folder}` (IMAP)
//! - `POST /control/drop_connection` — IMAP and SMTP
//! - `POST /control/fail_next` `{target, command, response}` (imap/smtp) or
//!   `{target, path, status, body}` (gmail/graph)
//! - `POST /control/expire` `{target: gmail|graph, what: history|delta|tokens}`
//! - `GET  /control/sent?target=smtp|gmail|graph`
//! - `POST /control/reset`

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde::Deserialize;
use serde_json::{Value, json};

use super::imap::Inject;
use super::{FakeError, FakeGmail, FakeGraph, FakeImap, FakeSmtp, Lifeline, fixtures};

/// The fakes a [`ControlServer`] drives; any may be absent.
#[derive(Debug, Clone, Default)]
pub struct Fakes {
    pub imap: Option<FakeImap>,
    pub smtp: Option<FakeSmtp>,
    pub gmail: Option<FakeGmail>,
    pub graph: Option<FakeGraph>,
    /// Login used when a request names no `user`.
    pub default_user: String,
}

/// A running control server.
#[derive(Clone)]
pub struct ControlServer {
    addr: SocketAddr,
    _life: Arc<Lifeline>,
}

impl std::fmt::Debug for ControlServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ControlServer")
            .field("addr", &self.addr)
            .finish()
    }
}

impl ControlServer {
    /// Serves the control interface for `fakes` on a random port.
    pub async fn start(fakes: Fakes) -> Result<Self, FakeError> {
        let router = Router::new()
            .route("/control", get(status))
            .route("/control/fixtures", get(list_fixtures))
            .route("/control/inject", post(inject))
            .route("/control/set_flags", post(set_flags))
            .route("/control/bump_uidvalidity", post(bump))
            .route("/control/drop_connection", post(drop_conn))
            .route("/control/fail_next", post(fail_next))
            .route("/control/expire", post(expire))
            .route("/control/sent", get(sent))
            .route("/control/reset", post(reset))
            .with_state(Arc::new(fakes));
        let (addr, life) = super::http::serve(router).await?;
        Ok(Self {
            addr,
            _life: Arc::new(life),
        })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// `http://127.0.0.1:<port>/control`
    pub fn url(&self) -> String {
        format!("http://{}/control", self.addr)
    }
}

type S = State<Arc<Fakes>>;

fn bad(msg: impl std::fmt::Display) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error": msg.to_string()})),
    )
        .into_response()
}

fn missing(what: &str) -> Response {
    bad(format!("{what} fake is not running"))
}

async fn status(State(f): S) -> Response {
    let ca = f
        .imap
        .as_ref()
        .map(|i| i.ca().ca_pem().to_string())
        .or_else(|| f.smtp.as_ref().map(|s| s.ca().ca_pem().to_string()));
    Json(json!({
        "user": f.default_user,
        "imap": f.imap.as_ref().map(|i| json!({"host": i.host(), "port": i.port(), "tls": format!("{:?}", i.tls_mode())})),
        "smtp": f.smtp.as_ref().map(|s| json!({"host": s.host(), "port": s.port(), "tls": format!("{:?}", s.tls_mode())})),
        "gmail": f.gmail.as_ref().map(|g| json!({"api_base": g.api_base(), "token_url": g.token_url(), "authorize_url": g.authorize_url()})),
        "graph": f.graph.as_ref().map(|g| json!({"api_base": g.api_base(), "token_url": g.token_url(), "authorize_url": g.authorize_url()})),
        "ca_pem": ca,
    }))
    .into_response()
}

async fn list_fixtures() -> Response {
    Json(json!(
        fixtures::ALL.iter().map(|(n, _)| *n).collect::<Vec<_>>()
    ))
    .into_response()
}

#[derive(Deserialize)]
struct InjectReq {
    target: String,
    user: Option<String>,
    folder: Option<String>,
    labels: Option<Vec<String>>,
    fixture: Option<String>,
    /// Raw message text, or base64 when `eml_base64` is true.
    eml: Option<String>,
    #[serde(default)]
    eml_base64: bool,
    flags: Option<Vec<String>>,
    internal_date: Option<i64>,
}

async fn inject(State(f): S, Json(r): Json<InjectReq>) -> Response {
    let raw: Vec<u8> = match (&r.fixture, &r.eml) {
        (Some(name), _) => match fixtures::get(name) {
            Some(b) => b.to_vec(),
            None => return bad(format!("no fixture {name}")),
        },
        (None, Some(e)) if r.eml_base64 => match STANDARD.decode(e) {
            Ok(b) => b,
            Err(e) => return bad(e),
        },
        (None, Some(e)) => e.replace("\r\n", "\n").replace('\n', "\r\n").into_bytes(),
        (None, None) => return bad("fixture or eml required"),
    };
    let user = r.user.clone().unwrap_or_else(|| f.default_user.clone());
    match r.target.as_str() {
        "imap" => {
            let Some(imap) = &f.imap else {
                return missing("imap");
            };
            let opts = Inject {
                flags: r.flags.clone().unwrap_or_default(),
                internal_date: r.internal_date,
            };
            match imap.inject_with(&user, r.folder.as_deref().unwrap_or("INBOX"), &raw, opts) {
                Ok(uid) => Json(json!({"uid": uid})).into_response(),
                Err(e) => bad(e),
            }
        }
        "gmail" => {
            let Some(g) = &f.gmail else {
                return missing("gmail");
            };
            let labels = r
                .labels
                .clone()
                .unwrap_or_else(|| vec!["INBOX".into(), "UNREAD".into()]);
            let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
            let id = match r.internal_date {
                Some(ts) => g.inject_at(&raw, &labels, ts * 1000),
                None => g.inject(&raw, &labels),
            };
            Json(json!({"id": id})).into_response()
        }
        "graph" => {
            let Some(g) = &f.graph else {
                return missing("graph");
            };
            let folder = r.folder.as_deref().unwrap_or("inbox");
            let res = match r.internal_date {
                Some(ts) => g.inject_at(folder, &raw, ts),
                None => g.inject(folder, &raw),
            };
            match res {
                Ok(id) => Json(json!({"id": id})).into_response(),
                Err(e) => bad(e),
            }
        }
        other => bad(format!("unknown target {other}")),
    }
}

#[derive(Deserialize)]
struct FlagsReq {
    user: Option<String>,
    folder: String,
    uid: u32,
    flags: Vec<String>,
}

async fn set_flags(State(f): S, Json(r): Json<FlagsReq>) -> Response {
    let Some(imap) = &f.imap else {
        return missing("imap");
    };
    let user = r.user.unwrap_or_else(|| f.default_user.clone());
    let flags: Vec<&str> = r.flags.iter().map(String::as_str).collect();
    match imap.set_flags(&user, &r.folder, r.uid, &flags) {
        Ok(()) => Json(json!({"ok": true})).into_response(),
        Err(e) => bad(e),
    }
}

#[derive(Deserialize)]
struct FolderReq {
    user: Option<String>,
    folder: String,
}

async fn bump(State(f): S, Json(r): Json<FolderReq>) -> Response {
    let Some(imap) = &f.imap else {
        return missing("imap");
    };
    let user = r.user.unwrap_or_else(|| f.default_user.clone());
    match imap.bump_uidvalidity(&user, &r.folder) {
        Ok(v) => Json(json!({"uidvalidity": v})).into_response(),
        Err(e) => bad(e),
    }
}

async fn drop_conn(State(f): S) -> Response {
    if let Some(i) = &f.imap {
        i.drop_connections();
    }
    if let Some(s) = &f.smtp {
        s.drop_connections();
    }
    Json(json!({"ok": true})).into_response()
}

#[derive(Deserialize)]
struct FailReq {
    target: String,
    command: Option<String>,
    response: Option<String>,
    path: Option<String>,
    status: Option<u16>,
    body: Option<Value>,
}

async fn fail_next(State(f): S, Json(r): Json<FailReq>) -> Response {
    let cmd = || r.command.clone().unwrap_or_default();
    let resp = || r.response.clone().unwrap_or_default();
    let path = || r.path.clone().unwrap_or_default();
    let status = r.status.unwrap_or(500);
    let body = || r.body.clone().unwrap_or(Value::Null);
    match r.target.as_str() {
        "imap" => match &f.imap {
            Some(i) => i.fail_next(&cmd(), &resp()),
            None => return missing("imap"),
        },
        "smtp" => match &f.smtp {
            Some(s) => s.fail_next(&cmd(), &resp()),
            None => return missing("smtp"),
        },
        "gmail" => match &f.gmail {
            Some(g) => g.fail_next(&path(), status, body()),
            None => return missing("gmail"),
        },
        "graph" => match &f.graph {
            Some(g) => g.fail_next(&path(), status, body()),
            None => return missing("graph"),
        },
        other => return bad(format!("unknown target {other}")),
    }
    Json(json!({"ok": true})).into_response()
}

#[derive(Deserialize)]
struct ExpireReq {
    target: String,
    what: String,
}

async fn expire(State(f): S, Json(r): Json<ExpireReq>) -> Response {
    match (r.target.as_str(), r.what.as_str(), &f.gmail, &f.graph) {
        ("gmail", "history", Some(g), _) => g.expire_history(),
        ("gmail", "tokens", Some(g), _) => g.expire_tokens(),
        ("graph", "delta", _, Some(g)) => g.expire_delta(),
        ("graph", "tokens", _, Some(g)) => g.expire_tokens(),
        _ => return bad("unknown target/what, or fake not running"),
    }
    Json(json!({"ok": true})).into_response()
}

#[derive(Deserialize)]
struct SentReq {
    target: String,
}

async fn sent(State(f): S, Query(r): Query<SentReq>) -> Response {
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    let list: Vec<Value> = match r.target.as_str() {
        "smtp" => match &f.smtp {
            Some(s) => s
                .sent()
                .iter()
                .map(|m| json!({"from": m.from, "rcpt": m.rcpt, "auth_user": m.auth_user, "tls": m.tls, "at": m.at, "data": text(&m.data)}))
                .collect(),
            None => return missing("smtp"),
        },
        "gmail" => match &f.gmail {
            Some(g) => g
                .sent()
                .iter()
                .map(|m| json!({"id": m.id, "thread_id": m.thread_id, "raw": text(&m.raw)}))
                .collect(),
            None => return missing("gmail"),
        },
        "graph" => match &f.graph {
            Some(g) => g
                .sent()
                .iter()
                .map(|m| json!({"mime": m.mime.as_deref().map(text), "json": m.json, "save_to_sent_items": m.save_to_sent_items}))
                .collect(),
            None => return missing("graph"),
        },
        other => return bad(format!("unknown target {other}")),
    };
    Json(Value::Array(list)).into_response()
}

async fn reset(State(f): S) -> Response {
    if let Some(i) = &f.imap {
        i.reset();
    }
    if let Some(s) = &f.smtp {
        s.reset();
    }
    if let Some(g) = &f.gmail {
        g.reset();
    }
    if let Some(g) = &f.graph {
        g.reset();
    }
    Json(json!({"ok": true})).into_response()
}
