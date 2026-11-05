//! Fake Gmail API: `users.getProfile`, `users.messages`
//! (`list` with `q`/`labelIds`/paging, `get` in `raw|full|metadata|minimal`,
//! `send`, `modify`, `batchModify`, `trash`, `untrash`, `delete`,
//! `attachments.get`), `users.threads.get`, `users.history.list` (with the
//! 404 of an expired `startHistoryId`), `users.labels` (`list`, `get`,
//! `create`), and the OAuth `authorize`/`token` endpoints. `watch` is not
//! implemented (push is phase two).
//!
//! Point the adapter's `api_base` at [`FakeGmail::api_base`] (it ends in
//! `/gmail/v1/`); OAuth lives at [`FakeGmail::token_url`] /
//! [`FakeGmail::authorize_url`]. Requests need `Authorization: Bearer` with
//! [`INITIAL_ACCESS_TOKEN`] or a token from the token endpoint.

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::extract::{Path, Query as AxQuery, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE, URL_SAFE_NO_PAD};
use mail_parser::{MessageParser, MessagePart};
use serde::Deserialize;
use serde_json::{Value, json};

use super::http::TokenRequest;
pub use super::http::{INITIAL_ACCESS_TOKEN, INITIAL_REFRESH_TOKEN};
use super::http::{OAuth, Query, q_all, q_get, serve};
use super::mime::MailMeta;
use super::{FakeError, Lifeline, lock, now_secs};

const SYSTEM_LABELS: &[&str] = &[
    "INBOX",
    "SENT",
    "DRAFT",
    "SPAM",
    "TRASH",
    "UNREAD",
    "STARRED",
    "IMPORTANT",
    "CHAT",
    "CATEGORY_PERSONAL",
    "CATEGORY_SOCIAL",
    "CATEGORY_PROMOTIONS",
    "CATEGORY_UPDATES",
    "CATEGORY_FORUMS",
];

/// A message as stored by the fake Gmail.
#[derive(Debug, Clone, PartialEq)]
pub struct GmailMessage {
    pub id: String,
    pub thread_id: String,
    pub label_ids: BTreeSet<String>,
    /// Milliseconds, like the API's `internalDate`.
    pub internal_date_ms: i64,
    pub raw: Vec<u8>,
}

/// A `messages.send` call.
#[derive(Debug, Clone, PartialEq)]
pub struct GmailSent {
    /// Id of the message created in SENT.
    pub id: String,
    pub thread_id: String,
    /// `threadId` the client passed, if any.
    pub requested_thread_id: Option<String>,
    /// Decoded MIME as sent.
    pub raw: Vec<u8>,
}

#[derive(Debug, Clone)]
struct Msg {
    id: String,
    thread_id: String,
    labels: BTreeSet<String>,
    internal_ms: i64,
    raw: Arc<Vec<u8>>,
    meta: Arc<MailMeta>,
    history_id: u64,
}

#[derive(Debug, Clone)]
enum Change {
    Added,
    Deleted,
    LabelsAdded(Vec<String>),
    LabelsRemoved(Vec<String>),
}

#[derive(Debug, Clone)]
struct History {
    id: u64,
    msg_id: String,
    thread_id: String,
    /// Labels of the message right after the change.
    labels: Vec<String>,
    change: Change,
}

#[derive(Debug)]
struct St {
    email: String,
    messages: Vec<Msg>,
    user_labels: Vec<(String, String)>,
    history: Vec<History>,
    history_id: u64,
    /// `startHistoryId` below this answers 404.
    history_floor: u64,
    next_id: u64,
    sent: Vec<GmailSent>,
    fail: Vec<(String, u16, Value)>,
}

impl St {
    fn new_id(&mut self) -> String {
        self.next_id += 1;
        format!("{:016x}", self.next_id)
    }

    fn record(&mut self, m: &Msg, change: Change) -> u64 {
        self.history_id += 1;
        let id = self.history_id;
        self.history.push(History {
            id,
            msg_id: m.id.clone(),
            thread_id: m.thread_id.clone(),
            labels: m.labels.iter().cloned().collect(),
            change,
        });
        id
    }

    fn insert(
        &mut self,
        raw: Vec<u8>,
        labels: BTreeSet<String>,
        internal_ms: i64,
        thread: Option<String>,
    ) -> Msg {
        let meta = Arc::new(MailMeta::parse(&raw));
        let id = self.new_id();
        let thread_id = thread
            .filter(|t| self.messages.iter().any(|m| &m.thread_id == t))
            .or_else(|| {
                meta.parent_ids().find_map(|p| {
                    self.messages
                        .iter()
                        .find(|m| m.meta.message_id.as_deref() == Some(p.as_str()))
                        .map(|m| m.thread_id.clone())
                })
            })
            .unwrap_or_else(|| id.clone());
        let mut m = Msg {
            id,
            thread_id,
            labels,
            internal_ms,
            raw: Arc::new(raw),
            meta,
            history_id: 0,
        };
        m.history_id = self.record(&m, Change::Added);
        self.messages.push(m.clone());
        m
    }

    fn modify(&mut self, id: &str, add: &[String], remove: &[String]) -> Option<Msg> {
        let i = self.messages.iter().position(|m| m.id == id)?;
        let mut m = self.messages[i].clone();
        let added: Vec<String> = add
            .iter()
            .filter(|l| m.labels.insert((*l).clone()))
            .cloned()
            .collect();
        let removed: Vec<String> = remove
            .iter()
            .filter(|l| m.labels.remove(*l))
            .cloned()
            .collect();
        if !added.is_empty() {
            m.history_id = self.record(&m, Change::LabelsAdded(added));
        }
        if !removed.is_empty() {
            m.history_id = self.record(&m, Change::LabelsRemoved(removed));
        }
        self.messages[i] = m.clone();
        Some(m)
    }

    fn label_exists(&self, id: &str) -> bool {
        SYSTEM_LABELS.contains(&id) || self.user_labels.iter().any(|(i, _)| i == id)
    }
}

struct Shared {
    st: Mutex<St>,
    oauth: OAuth,
}

/// Handle to a running fake Gmail API. Cloning shares the server.
#[derive(Clone)]
pub struct FakeGmail {
    shared: Arc<Shared>,
    _life: Arc<Lifeline>,
    addr: SocketAddr,
}

impl std::fmt::Debug for FakeGmail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeGmail")
            .field("addr", &self.addr)
            .finish()
    }
}

fn err(code: u16, status: &str, message: &str) -> Response {
    let reason = match code {
        400 => "badRequest",
        401 => "authError",
        404 => "notFound",
        429 => "rateLimitExceeded",
        _ => "backendError",
    };
    (
        StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        Json(json!({"error": {
            "code": code,
            "message": message,
            "errors": [{"message": message, "domain": "global", "reason": reason}],
            "status": status,
        }})),
    )
        .into_response()
}

fn not_found() -> Response {
    err(404, "NOT_FOUND", "Requested entity was not found.")
}

impl FakeGmail {
    /// Starts the fake for mailbox `email` on a random port.
    pub async fn start(email: &str) -> Result<Self, FakeError> {
        let shared = Arc::new(Shared {
            st: Mutex::new(St {
                email: email.into(),
                messages: Vec::new(),
                user_labels: Vec::new(),
                history: Vec::new(),
                history_id: 1000,
                history_floor: 0,
                next_id: 0x18f0_0000_0000_0000,
                sent: Vec::new(),
                fail: Vec::new(),
            }),
            oauth: OAuth::new(false),
        });
        let u = "/gmail/v1/users/{user}";
        let router = Router::new()
            .route(&format!("{u}/profile"), get(profile))
            .route(&format!("{u}/messages"), get(list_messages))
            .route(&format!("{u}/messages/send"), post(send))
            .route(&format!("{u}/messages/batchModify"), post(batch_modify))
            .route(
                &format!("{u}/messages/{{id}}"),
                get(get_message).delete(delete_message),
            )
            .route(&format!("{u}/messages/{{id}}/modify"), post(modify))
            .route(&format!("{u}/messages/{{id}}/trash"), post(trash))
            .route(&format!("{u}/messages/{{id}}/untrash"), post(untrash))
            .route(
                &format!("{u}/messages/{{id}}/attachments/{{aid}}"),
                get(attachment),
            )
            .route(&format!("{u}/threads/{{id}}"), get(get_thread))
            .route(&format!("{u}/history"), get(history))
            .route(&format!("{u}/labels"), get(labels).post(create_label))
            .route(&format!("{u}/labels/{{id}}"), get(get_label))
            .route("/token", post(token))
            .route("/authorize", get(authorize))
            .with_state(shared.clone());
        let (addr, life) = serve(router).await?;
        Ok(Self {
            shared,
            _life: Arc::new(life),
            addr,
        })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }
    /// `http://127.0.0.1:<port>/gmail/v1/`
    pub fn api_base(&self) -> String {
        format!("http://{}/gmail/v1/", self.addr)
    }
    pub fn token_url(&self) -> String {
        format!("http://{}/token", self.addr)
    }
    pub fn authorize_url(&self) -> String {
        format!("http://{}/authorize", self.addr)
    }

    /// Adds a received message (labels e.g. `["INBOX", "UNREAD"]`), now.
    /// Threads by `References`/`In-Reply-To`. Returns the message id.
    pub fn inject(&self, eml: &[u8], labels: &[&str]) -> String {
        self.inject_at(eml, labels, now_secs() * 1000)
    }

    /// Same with an explicit `internalDate` in milliseconds.
    pub fn inject_at(&self, eml: &[u8], labels: &[&str], internal_date_ms: i64) -> String {
        let mut st = lock(&self.shared.st);
        let labels = labels.iter().map(|l| l.to_string()).collect();
        st.insert(eml.to_vec(), labels, internal_date_ms, None).id
    }

    /// Adds and removes labels as another client would (writes history).
    pub fn modify_labels(&self, id: &str, add: &[&str], remove: &[&str]) -> Result<(), FakeError> {
        let add: Vec<String> = add.iter().map(|s| s.to_string()).collect();
        let remove: Vec<String> = remove.iter().map(|s| s.to_string()).collect();
        lock(&self.shared.st)
            .modify(id, &add, &remove)
            .map(|_| ())
            .ok_or_else(|| FakeError::NoSuchMessage(id.into()))
    }

    /// Deletes a message for good (writes a `messagesDeleted` record).
    pub fn delete(&self, id: &str) -> Result<(), FakeError> {
        let mut st = lock(&self.shared.st);
        let i = st
            .messages
            .iter()
            .position(|m| m.id == id)
            .ok_or_else(|| FakeError::NoSuchMessage(id.into()))?;
        let m = st.messages.remove(i);
        st.record(&m, Change::Deleted);
        Ok(())
    }

    /// Adds a user label; returns its id (`Label_<n>`).
    pub fn add_label(&self, name: &str) -> String {
        let mut st = lock(&self.shared.st);
        let id = format!("Label_{}", st.user_labels.len() + 1);
        st.user_labels.push((id.clone(), name.into()));
        id
    }

    /// Makes every history id issued so far too old: `history.list` with
    /// any of them answers 404, as Gmail does after about a week.
    pub fn expire_history(&self) {
        let mut st = lock(&self.shared.st);
        st.history_floor = st.history_id + 1;
        st.history_id += 1; // the profile's historyId stays valid
    }

    /// The current `historyId`.
    pub fn history_id(&self) -> u64 {
        lock(&self.shared.st).history_id
    }

    /// Every stored message, oldest first.
    pub fn messages(&self) -> Vec<GmailMessage> {
        lock(&self.shared.st)
            .messages
            .iter()
            .map(|m| GmailMessage {
                id: m.id.clone(),
                thread_id: m.thread_id.clone(),
                label_ids: m.labels.clone(),
                internal_date_ms: m.internal_ms,
                raw: m.raw.as_ref().clone(),
            })
            .collect()
    }

    /// Every `messages.send`, oldest first.
    pub fn sent(&self) -> Vec<GmailSent> {
        lock(&self.shared.st).sent.clone()
    }

    /// The next request whose path ends with `path_suffix` (e.g.
    /// `"/history"`, `"/messages/send"`) answers `status` with `body`.
    pub fn fail_next(&self, path_suffix: &str, status: u16, body: Value) {
        lock(&self.shared.st)
            .fail
            .push((path_suffix.into(), status, body));
    }

    /// Invalidates every access token; refresh tokens keep working.
    pub fn expire_tokens(&self) {
        self.shared.oauth.expire_access();
    }

    /// Invalidates access and refresh tokens (the user revoked the app).
    pub fn revoke_tokens(&self) {
        self.shared.oauth.revoke_all();
    }

    /// Requests received by the token endpoint.
    pub fn token_requests(&self) -> Vec<TokenRequest> {
        self.shared.oauth.requests()
    }

    /// Forgets messages, labels, history, sent mail and scripted failures.
    pub fn reset(&self) {
        let mut st = lock(&self.shared.st);
        st.messages.clear();
        st.user_labels.clear();
        st.history.clear();
        st.sent.clear();
        st.fail.clear();
        st.history_floor = 0;
    }
}

type S = State<Arc<Shared>>;

/// Auth, scripted failures and the `{user}` path segment.
fn guard(sh: &Shared, headers: &HeaderMap, user: &str, path: &str) -> Result<(), Box<Response>> {
    if !sh.oauth.authorized(headers) {
        return Err(Box::new(err(
            401,
            "UNAUTHENTICATED",
            "Request had invalid authentication credentials. Expected OAuth 2 access token.",
        )));
    }
    let mut st = lock(&sh.st);
    if user != "me" && user != st.email {
        return Err(Box::new(err(
            400,
            "FAILED_PRECONDITION",
            "Precondition check failed.",
        )));
    }
    if let Some(i) = st
        .fail
        .iter()
        .position(|(p, _, _)| path.ends_with(p.as_str()))
    {
        let (_, code, body) = st.fail.remove(i);
        return Err(Box::new(
            (
                StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                Json(body),
            )
                .into_response(),
        ));
    }
    Ok(())
}

macro_rules! guard {
    ($sh:expr, $h:expr, $user:expr, $path:expr) => {
        if let Err(r) = guard(&$sh, &$h, &$user, $path) {
            return *r;
        }
    };
}

async fn profile(State(sh): S, h: HeaderMap, Path(user): Path<String>) -> Response {
    guard!(sh, h, user, "/profile");
    let st = lock(&sh.st);
    let threads: BTreeSet<&str> = st.messages.iter().map(|m| m.thread_id.as_str()).collect();
    Json(json!({
        "emailAddress": st.email,
        "messagesTotal": st.messages.len(),
        "threadsTotal": threads.len(),
        "historyId": st.history_id.to_string(),
    }))
    .into_response()
}

/// The `q` subset adapters use: `newer_than:/older_than:` (d/m/y),
/// `after:/before:` (`YYYY/MM/DD` or epoch seconds), `is:unread|read|starred`,
/// `in:inbox|sent|spam|trash|anywhere`, `label:`, `from:`, `to:`,
/// `subject:`, `has:attachment`; other words match the raw message.
fn q_matches(q: &str, m: &Msg, now_ms: i64) -> bool {
    q.split_whitespace().all(|term| {
        let (neg, term) = match term.strip_prefix('-') {
            Some(t) => (true, t),
            None => (false, term),
        };
        let hit = match term.split_once(':') {
            Some(("newer_than", v)) | Some(("older_than", v)) => {
                let (n, unit) = v.split_at(v.len().saturating_sub(1));
                let n: i64 = n.parse().unwrap_or(0);
                let days = match unit {
                    "d" => n,
                    "m" => n * 30,
                    "y" => n * 365,
                    _ => 0,
                };
                let cutoff = now_ms - days * 86_400_000;
                if term.starts_with("newer") {
                    m.internal_ms >= cutoff
                } else {
                    m.internal_ms < cutoff
                }
            }
            Some((k @ ("after" | "before"), v)) => {
                let secs = v.parse::<i64>().ok().or_else(|| {
                    let p: Vec<i64> = v.split('/').filter_map(|x| x.parse().ok()).collect();
                    (p.len() == 3).then(|| {
                        crate::fake::imap::days_from_civil(p[0], p[1] as u32, p[2] as u32) * 86400
                    })
                });
                match secs {
                    Some(s) if k == "after" => m.internal_ms >= s * 1000,
                    Some(s) => m.internal_ms < s * 1000,
                    None => false,
                }
            }
            Some(("is", "unread")) => m.labels.contains("UNREAD"),
            Some(("is", "read")) => !m.labels.contains("UNREAD"),
            Some(("is", "starred")) => m.labels.contains("STARRED"),
            Some(("in", "anywhere")) => true,
            Some(("in", v)) | Some(("label", v)) => {
                m.labels.iter().any(|l| l.eq_ignore_ascii_case(v))
            }
            Some(("from", v)) => m.meta.from.as_ref().is_some_and(|(n, a)| {
                format!("{n} {a}")
                    .to_lowercase()
                    .contains(&v.to_lowercase())
            }),
            Some(("to", v)) => m.meta.to.iter().any(|(n, a)| {
                format!("{n} {a}")
                    .to_lowercase()
                    .contains(&v.to_lowercase())
            }),
            Some(("subject", v)) => m.meta.subject.to_lowercase().contains(&v.to_lowercase()),
            Some(("has", "attachment")) => m.meta.has_attachments,
            _ => String::from_utf8_lossy(&m.raw)
                .to_lowercase()
                .contains(&term.to_lowercase()),
        };
        hit != neg
    })
}

async fn list_messages(
    State(sh): S,
    h: HeaderMap,
    Path(user): Path<String>,
    AxQuery(q): AxQuery<Query>,
) -> Response {
    guard!(sh, h, user, "/messages");
    let st = lock(&sh.st);
    let query = q_get(&q, "q").unwrap_or("");
    let labels = q_all(&q, "labelIds");
    let spam_trash = q_get(&q, "includeSpamTrash") == Some("true")
        || query.contains("in:spam")
        || query.contains("in:trash")
        || query.contains("in:anywhere");
    let max: usize = q_get(&q, "maxResults")
        .and_then(|m| m.parse().ok())
        .unwrap_or(100)
        .clamp(1, 500);
    let offset: usize = q_get(&q, "pageToken")
        .and_then(|p| p.parse().ok())
        .unwrap_or(0);
    let now_ms = now_secs() * 1000;
    let mut hits: Vec<&Msg> = st
        .messages
        .iter()
        .filter(|m| labels.iter().all(|l| m.labels.contains(*l)))
        .filter(|m| spam_trash || !(m.labels.contains("SPAM") || m.labels.contains("TRASH")))
        .filter(|m| q_matches(query, m, now_ms))
        .collect();
    hits.sort_by(|a, b| b.internal_ms.cmp(&a.internal_ms).then(b.id.cmp(&a.id)));
    let total = hits.len();
    let page: Vec<Value> = hits
        .iter()
        .skip(offset)
        .take(max)
        .map(|m| json!({"id": m.id, "threadId": m.thread_id}))
        .collect();
    let mut body = json!({"resultSizeEstimate": total});
    if !page.is_empty() {
        body["messages"] = Value::Array(page);
    }
    if offset + max < total {
        body["nextPageToken"] = Value::String((offset + max).to_string());
    }
    Json(body).into_response()
}

fn b64url(data: &[u8]) -> String {
    URL_SAFE.encode(data)
}

/// Header value as Gmail shows it: decoded text for unstructured fields,
/// otherwise the unfolded raw value.
fn headers_json(part: &MessagePart<'_>, only: &[&str]) -> Vec<Value> {
    part.headers()
        .iter()
        .filter(|hd| only.is_empty() || only.iter().any(|o| o.eq_ignore_ascii_case(hd.raw_name())))
        .map(|hd| {
            let v = hd.value();
            let value = match (v.as_text(), v.as_text_list()) {
                (Some(t), None) => t.to_string(),
                _ => String::from_utf8_lossy(hd.raw_value())
                    .replace("\r\n", "")
                    .replace('\n', "")
                    .trim()
                    .to_string(),
            };
            json!({"name": hd.raw_name(), "value": value})
        })
        .collect()
}

fn mime_type(p: &MessagePart<'_>) -> String {
    match p.content_type() {
        Some(ct) => format!(
            "{}/{}",
            ct.ctype().to_ascii_lowercase(),
            ct.subtype().unwrap_or("plain").to_ascii_lowercase()
        ),
        None => "text/plain".into(),
    }
}

fn payload(p: &MessagePart<'_>, part_id: String, msg_id: &str, top: bool, only: &[&str]) -> Value {
    let mut v = json!({
        "partId": part_id,
        "mimeType": mime_type(p),
        "filename": p.attachment_name().unwrap_or(""),
        "headers": headers_json(p, if top { only } else { &[] }),
    });
    if p.is_multipart() {
        let parts: Vec<Value> = p
            .children()
            .enumerate()
            .map(|(i, c)| {
                let id = if part_id.is_empty() {
                    i.to_string()
                } else {
                    format!("{part_id}.{i}")
                };
                payload(&c, id, msg_id, false, only)
            })
            .collect();
        v["body"] = json!({"size": 0});
        v["parts"] = Value::Array(parts);
    } else {
        let data = p.decoded();
        if p.attachment_name().is_some() && !p.is_text() {
            v["body"] = json!({"attachmentId": format!("{msg_id}.{}", if part_id.is_empty() { "0" } else { &part_id }), "size": data.len()});
        } else {
            v["body"] = json!({"size": data.len(), "data": b64url(&data)});
        }
    }
    v
}

fn message_json(m: &Msg, format: &str, only: &[&str]) -> Value {
    let mut v = json!({
        "id": m.id,
        "threadId": m.thread_id,
        "labelIds": m.labels.iter().collect::<Vec<_>>(),
        "snippet": m.meta.snippet,
        "historyId": m.history_id.to_string(),
        "internalDate": m.internal_ms.to_string(),
        "sizeEstimate": m.raw.len(),
    });
    match format {
        "raw" => v["raw"] = Value::String(b64url(&m.raw)),
        "minimal" => {}
        _ => {
            if let Some(msg) = MessageParser::new().parse(m.raw.as_slice()) {
                let root = msg.root().root_part();
                let mut p = payload(&root, String::new(), &m.id, true, only);
                if format == "metadata"
                    && let Some(o) = p.as_object_mut()
                {
                    o.remove("parts");
                    o.remove("body");
                }
                v["payload"] = p;
            }
        }
    }
    v
}

async fn get_message(
    State(sh): S,
    h: HeaderMap,
    Path((user, id)): Path<(String, String)>,
    AxQuery(q): AxQuery<Query>,
) -> Response {
    guard!(sh, h, user, "/messages/{id}");
    let st = lock(&sh.st);
    let Some(m) = st.messages.iter().find(|m| m.id == id) else {
        return not_found();
    };
    let format = q_get(&q, "format").unwrap_or("full");
    if !matches!(format, "raw" | "full" | "metadata" | "minimal") {
        return err(400, "INVALID_ARGUMENT", "Invalid value for format");
    }
    Json(message_json(m, format, &q_all(&q, "metadataHeaders"))).into_response()
}

async fn attachment(
    State(sh): S,
    h: HeaderMap,
    Path((user, id, aid)): Path<(String, String, String)>,
) -> Response {
    guard!(sh, h, user, "/attachments");
    let st = lock(&sh.st);
    let Some(m) = st.messages.iter().find(|m| m.id == id) else {
        return not_found();
    };
    let Some(path) = aid.strip_prefix(&format!("{id}.")) else {
        return not_found();
    };
    let Some(msg) = MessageParser::new().parse(m.raw.as_slice()) else {
        return not_found();
    };
    let mut part = msg.root().root_part();
    for idx in path.split('.') {
        let Ok(i) = idx.parse::<usize>() else {
            return not_found();
        };
        if !part.is_multipart() {
            break;
        }
        let Some(c) = part.children().nth(i) else {
            return not_found();
        };
        part = c;
    }
    let data = part.decoded();
    Json(json!({"attachmentId": aid, "size": data.len(), "data": b64url(&data)})).into_response()
}

async fn get_thread(
    State(sh): S,
    h: HeaderMap,
    Path((user, id)): Path<(String, String)>,
    AxQuery(q): AxQuery<Query>,
) -> Response {
    guard!(sh, h, user, "/threads/{id}");
    let st = lock(&sh.st);
    let format = q_get(&q, "format").unwrap_or("full");
    let msgs: Vec<Value> = st
        .messages
        .iter()
        .filter(|m| m.thread_id == id)
        .map(|m| message_json(m, format, &q_all(&q, "metadataHeaders")))
        .collect();
    if msgs.is_empty() {
        return not_found();
    }
    let hid = st
        .messages
        .iter()
        .filter(|m| m.thread_id == id)
        .map(|m| m.history_id)
        .max()
        .unwrap_or(0);
    Json(json!({"id": id, "historyId": hid.to_string(), "messages": msgs})).into_response()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SendBody {
    raw: String,
    thread_id: Option<String>,
}

fn decode_raw(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    URL_SAFE_NO_PAD
        .decode(s.trim_end_matches('='))
        .ok()
        .or_else(|| STANDARD.decode(s).ok())
}

async fn send(
    State(sh): S,
    h: HeaderMap,
    Path(user): Path<String>,
    Json(b): Json<SendBody>,
) -> Response {
    guard!(sh, h, user, "/messages/send");
    let Some(raw) = decode_raw(&b.raw) else {
        return err(400, "INVALID_ARGUMENT", "Invalid value for ByteString");
    };
    let meta = MailMeta::parse(&raw);
    if meta.to.is_empty() && meta.cc.is_empty() && meta.bcc.is_empty() {
        return err(400, "INVALID_ARGUMENT", "Recipient address required");
    }
    let mut st = lock(&sh.st);
    let email = st.email.clone();
    let mut labels: BTreeSet<String> = ["SENT".to_string()].into();
    if meta
        .to
        .iter()
        .chain(&meta.cc)
        .chain(&meta.bcc)
        .any(|(_, a)| a.eq_ignore_ascii_case(&email))
    {
        labels.extend(["INBOX".to_string(), "UNREAD".to_string()]);
    }
    let m = st.insert(raw.clone(), labels, now_secs() * 1000, b.thread_id.clone());
    st.sent.push(GmailSent {
        id: m.id.clone(),
        thread_id: m.thread_id.clone(),
        requested_thread_id: b.thread_id,
        raw,
    });
    Json(json!({"id": m.id, "threadId": m.thread_id, "labelIds": m.labels.iter().collect::<Vec<_>>()})).into_response()
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct ModifyBody {
    ids: Vec<String>,
    add_label_ids: Vec<String>,
    remove_label_ids: Vec<String>,
}

async fn modify(
    State(sh): S,
    h: HeaderMap,
    Path((user, id)): Path<(String, String)>,
    Json(b): Json<ModifyBody>,
) -> Response {
    guard!(sh, h, user, "/modify");
    let mut st = lock(&sh.st);
    if let Some(bad) = b.add_label_ids.iter().find(|l| !st.label_exists(l)) {
        return err(400, "INVALID_ARGUMENT", &format!("Invalid label: {bad}"));
    }
    match st.modify(&id, &b.add_label_ids, &b.remove_label_ids) {
        Some(m) => Json(message_json(&m, "minimal", &[])).into_response(),
        None => not_found(),
    }
}

async fn batch_modify(
    State(sh): S,
    h: HeaderMap,
    Path(user): Path<String>,
    Json(b): Json<ModifyBody>,
) -> Response {
    guard!(sh, h, user, "/messages/batchModify");
    let mut st = lock(&sh.st);
    for id in &b.ids {
        st.modify(id, &b.add_label_ids, &b.remove_label_ids);
    }
    StatusCode::NO_CONTENT.into_response()
}

async fn trash(State(sh): S, h: HeaderMap, Path((user, id)): Path<(String, String)>) -> Response {
    guard!(sh, h, user, "/trash");
    let mut st = lock(&sh.st);
    match st.modify(&id, &["TRASH".into()], &["INBOX".into()]) {
        Some(m) => Json(message_json(&m, "minimal", &[])).into_response(),
        None => not_found(),
    }
}

async fn untrash(State(sh): S, h: HeaderMap, Path((user, id)): Path<(String, String)>) -> Response {
    guard!(sh, h, user, "/untrash");
    let mut st = lock(&sh.st);
    match st.modify(&id, &[], &["TRASH".into()]) {
        Some(m) => Json(message_json(&m, "minimal", &[])).into_response(),
        None => not_found(),
    }
}

async fn delete_message(
    State(sh): S,
    h: HeaderMap,
    Path((user, id)): Path<(String, String)>,
) -> Response {
    guard!(sh, h, user, "/messages/{id}");
    let mut st = lock(&sh.st);
    let Some(i) = st.messages.iter().position(|m| m.id == id) else {
        return not_found();
    };
    let m = st.messages.remove(i);
    st.record(&m, Change::Deleted);
    StatusCode::NO_CONTENT.into_response()
}

async fn history(
    State(sh): S,
    h: HeaderMap,
    Path(user): Path<String>,
    AxQuery(q): AxQuery<Query>,
) -> Response {
    guard!(sh, h, user, "/history");
    let st = lock(&sh.st);
    let Some(start) = q_get(&q, "startHistoryId").and_then(|s| s.parse::<u64>().ok()) else {
        return err(400, "INVALID_ARGUMENT", "Missing startHistoryId");
    };
    if start < st.history_floor || start > st.history_id {
        return not_found();
    }
    let types = q_all(&q, "historyTypes");
    let label = q_get(&q, "labelId");
    let max: usize = q_get(&q, "maxResults")
        .and_then(|m| m.parse().ok())
        .unwrap_or(100)
        .clamp(1, 500);
    let offset: usize = q_get(&q, "pageToken")
        .and_then(|p| p.parse().ok())
        .unwrap_or(0);
    let wanted = |c: &Change| {
        let name = match c {
            Change::Added => "messageAdded",
            Change::Deleted => "messageDeleted",
            Change::LabelsAdded(_) => "labelAdded",
            Change::LabelsRemoved(_) => "labelRemoved",
        };
        types.is_empty() || types.contains(&name)
    };
    let recs: Vec<&History> = st
        .history
        .iter()
        .filter(|r| r.id > start && wanted(&r.change))
        .filter(|r| label.is_none_or(|l| r.labels.iter().any(|x| x == l)))
        .collect();
    let page: Vec<Value> = recs
        .iter()
        .skip(offset)
        .take(max)
        .map(|r| {
            let msg = json!({"id": r.msg_id, "threadId": r.thread_id, "labelIds": r.labels});
            let mut v = json!({"id": r.id.to_string(), "messages": [{"id": r.msg_id, "threadId": r.thread_id}]});
            match &r.change {
                Change::Added => v["messagesAdded"] = json!([{"message": msg}]),
                Change::Deleted => v["messagesDeleted"] = json!([{"message": msg}]),
                Change::LabelsAdded(l) => v["labelsAdded"] = json!([{"message": msg, "labelIds": l}]),
                Change::LabelsRemoved(l) => v["labelsRemoved"] = json!([{"message": msg, "labelIds": l}]),
            }
            v
        })
        .collect();
    let mut body = json!({"historyId": st.history_id.to_string()});
    if !page.is_empty() {
        body["history"] = Value::Array(page);
    }
    if offset + max < recs.len() {
        body["nextPageToken"] = Value::String((offset + max).to_string());
    }
    Json(body).into_response()
}

fn label_json(st: &St, id: &str, name: &str, system: bool, counts: bool) -> Value {
    let mut v = json!({"id": id, "name": name, "type": if system { "system" } else { "user" }});
    if !system {
        v["labelListVisibility"] = json!("labelShow");
        v["messageListVisibility"] = json!("show");
    }
    if counts {
        let msgs: Vec<&Msg> = st
            .messages
            .iter()
            .filter(|m| m.labels.contains(id))
            .collect();
        let threads: BTreeSet<&str> = msgs.iter().map(|m| m.thread_id.as_str()).collect();
        let unread = msgs.iter().filter(|m| m.labels.contains("UNREAD")).count();
        v["messagesTotal"] = json!(msgs.len());
        v["messagesUnread"] = json!(unread);
        v["threadsTotal"] = json!(threads.len());
    }
    v
}

async fn labels(State(sh): S, h: HeaderMap, Path(user): Path<String>) -> Response {
    guard!(sh, h, user, "/labels");
    let st = lock(&sh.st);
    let mut out: Vec<Value> = SYSTEM_LABELS
        .iter()
        .map(|l| label_json(&st, l, l, true, false))
        .collect();
    out.extend(
        st.user_labels
            .iter()
            .map(|(i, n)| label_json(&st, i, n, false, false)),
    );
    Json(json!({"labels": out})).into_response()
}

async fn get_label(
    State(sh): S,
    h: HeaderMap,
    Path((user, id)): Path<(String, String)>,
) -> Response {
    guard!(sh, h, user, "/labels/{id}");
    let st = lock(&sh.st);
    if SYSTEM_LABELS.contains(&id.as_str()) {
        return Json(label_json(&st, &id, &id, true, true)).into_response();
    }
    match st.user_labels.iter().find(|(i, _)| *i == id) {
        Some((i, n)) => Json(label_json(&st, i, n, false, true)).into_response(),
        None => not_found(),
    }
}

#[derive(Deserialize)]
struct LabelBody {
    name: String,
}

async fn create_label(
    State(sh): S,
    h: HeaderMap,
    Path(user): Path<String>,
    Json(b): Json<LabelBody>,
) -> Response {
    guard!(sh, h, user, "/labels");
    let mut st = lock(&sh.st);
    if st.user_labels.iter().any(|(_, n)| *n == b.name) {
        return err(409, "ALREADY_EXISTS", "Label name exists or conflicts");
    }
    let id = format!("Label_{}", st.user_labels.len() + 1);
    st.user_labels.push((id.clone(), b.name.clone()));
    Json(label_json(&st, &id, &b.name, false, false)).into_response()
}

async fn token(State(sh): S, Form(form): Form<BTreeMap<String, String>>) -> Response {
    sh.oauth.token(form)
}

async fn authorize(State(sh): S, AxQuery(q): AxQuery<BTreeMap<String, String>>) -> Response {
    sh.oauth.authorize(&q)
}
