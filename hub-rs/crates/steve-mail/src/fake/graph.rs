//! Fake Microsoft Graph mail: `GET /me`, `mailFolders`
//! (list with paging, get by id or well-known name, `childFolders`),
//! folder `messages`, `/me/messages`, a message (`$select`), its MIME via
//! `$value`, `PATCH` (isRead, flag), `move` (new id, as Exchange does),
//! `DELETE`, `messages/delta` (paging via `@odata.nextLink`, final
//! `@odata.deltaLink`, `@removed` entries, `Prefer: odata.maxpagesize`,
//! `$filter=receivedDateTime ge …` on the initial round, 410 for expired
//! tokens), `sendMail` (MIME as base64 `text/plain`, or JSON), and the
//! OAuth `authorize`/`token` endpoints under `/{tenant}/oauth2/v2.0/`.
//!
//! Base URL for the adapter: [`FakeGraph::api_base`] (ends in `/v1.0/`).

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::{Path, Query as AxQuery, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Map, Value, json};

use super::http::{OAuth, Query, TokenRequest, iso, q_get, serve};
use super::mime::MailMeta;
use super::{FakeError, Lifeline, lock, now_secs};

const WELL_KNOWN: &[(&str, &str)] = &[
    ("inbox", "Inbox"),
    ("sentitems", "Sent Items"),
    ("drafts", "Drafts"),
    ("archive", "Archive"),
    ("deleteditems", "Deleted Items"),
    ("junkemail", "Junk Email"),
];

/// A folder as the fake stores it.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphFolder {
    pub id: String,
    pub display_name: String,
    pub parent_folder_id: Option<String>,
    /// `inbox`, `sentitems`… for the default folders.
    pub well_known_name: Option<String>,
}

/// A message as the fake stores it.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphMessage {
    pub id: String,
    pub folder_id: String,
    pub conversation_id: String,
    pub is_read: bool,
    pub raw: Vec<u8>,
}

/// A `sendMail` call.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphSent {
    /// The MIME, when sent as base64 `text/plain`.
    pub mime: Option<Vec<u8>>,
    /// The JSON body, when sent as `application/json`.
    pub json: Option<Value>,
    pub save_to_sent_items: bool,
}

#[derive(Debug, Clone)]
struct Msg {
    id: String,
    folder: String,
    conversation: String,
    raw: Arc<Vec<u8>>,
    meta: Arc<MailMeta>,
    is_read: bool,
    flagged: bool,
    received: i64,
    /// Delta sequence of the last change.
    seq: u64,
}

#[derive(Debug)]
struct St {
    base: String,
    email: String,
    display_name: String,
    folders: Vec<GraphFolder>,
    messages: Vec<Msg>,
    /// `(folder, message id, seq)` of messages that left a folder.
    removed: Vec<(String, String, u64)>,
    seq: u64,
    /// Delta tokens below this answer 410.
    delta_floor: u64,
    next: u64,
    page_size: usize,
    sent: Vec<GraphSent>,
    fail: Vec<(String, u16, Value)>,
}

impl St {
    fn new_id(&mut self, prefix: &str) -> String {
        self.next += 1;
        format!("{prefix}{:012x}", self.next)
    }

    fn bump(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }

    /// Folder id from an id or a well-known name.
    fn folder_id(&self, key: &str) -> Option<String> {
        self.folders
            .iter()
            .find(|f| {
                f.id == key || f.well_known_name.as_deref() == Some(&key.to_ascii_lowercase())
            })
            .map(|f| f.id.clone())
    }

    fn insert(&mut self, folder: &str, raw: Vec<u8>, is_read: bool, received: i64) -> Msg {
        let meta = Arc::new(MailMeta::parse(&raw));
        let conversation = meta
            .parent_ids()
            .find_map(|p| {
                self.messages
                    .iter()
                    .find(|m| m.meta.message_id.as_deref() == Some(p.as_str()))
                    .map(|m| m.conversation.clone())
            })
            .unwrap_or_else(|| self.new_id("AAQkConv"));
        let id = self.new_id("AAMkMsg");
        let seq = self.bump();
        let m = Msg {
            id,
            folder: folder.into(),
            conversation,
            raw: Arc::new(raw),
            meta,
            is_read,
            flagged: false,
            received,
            seq,
        };
        self.messages.push(m.clone());
        m
    }

    /// Moves a message: Exchange gives the moved copy a new id.
    fn move_to(&mut self, id: &str, dest: &str) -> Option<Msg> {
        let i = self.messages.iter().position(|m| m.id == id)?;
        let old = self.messages.remove(i);
        let seq = self.bump();
        self.removed.push((old.folder.clone(), old.id.clone(), seq));
        let new_id = self.new_id("AAMkMsg");
        let seq = self.bump();
        let m = Msg {
            id: new_id,
            folder: dest.into(),
            seq,
            ..old
        };
        self.messages.push(m.clone());
        Some(m)
    }
}

struct Shared {
    st: Mutex<St>,
    oauth: OAuth,
}

/// Handle to a running fake Graph API. Cloning shares the server.
#[derive(Clone)]
pub struct FakeGraph {
    shared: Arc<Shared>,
    _life: Arc<Lifeline>,
    addr: SocketAddr,
}

impl std::fmt::Debug for FakeGraph {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeGraph")
            .field("addr", &self.addr)
            .finish()
    }
}

fn err(status: u16, code: &str, message: &str) -> Response {
    (
        StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        Json(json!({"error": {"code": code, "message": message, "innerError": {"request-id": "fake"}}})),
    )
        .into_response()
}

fn not_found() -> Response {
    err(
        404,
        "ErrorItemNotFound",
        "The specified object was not found in the store.",
    )
}

impl FakeGraph {
    /// Starts the fake for mailbox `email` on a random port.
    pub async fn start(email: &str) -> Result<Self, FakeError> {
        let mut st = St {
            base: String::new(),
            email: email.into(),
            display_name: "Me".into(),
            folders: Vec::new(),
            messages: Vec::new(),
            removed: Vec::new(),
            seq: 0,
            delta_floor: 0,
            next: 0,
            page_size: 10,
            sent: Vec::new(),
            fail: Vec::new(),
        };
        for (wk, name) in WELL_KNOWN {
            let id = st.new_id("AAMkFolder");
            st.folders.push(GraphFolder {
                id,
                display_name: name.to_string(),
                parent_folder_id: None,
                well_known_name: Some(wk.to_string()),
            });
        }
        let shared = Arc::new(Shared {
            st: Mutex::new(st),
            oauth: OAuth::new(true),
        });
        let router = Router::new()
            .route("/v1.0/me", get(me))
            .route("/v1.0/me/mailFolders", get(list_folders))
            .route("/v1.0/me/mailFolders/{id}", get(get_folder))
            .route("/v1.0/me/mailFolders/{id}/childFolders", get(child_folders))
            .route("/v1.0/me/mailFolders/{id}/messages", get(folder_messages))
            .route("/v1.0/me/mailFolders/{id}/messages/delta", get(delta))
            .route("/v1.0/me/messages", get(all_messages))
            .route(
                "/v1.0/me/messages/{id}",
                get(get_message).patch(patch_message).delete(delete_message),
            )
            .route("/v1.0/me/messages/{id}/$value", get(mime_value))
            .route("/v1.0/me/messages/{id}/move", post(move_message))
            .route("/v1.0/me/sendMail", post(send_mail))
            .route("/{tenant}/oauth2/v2.0/token", post(token))
            .route("/{tenant}/oauth2/v2.0/authorize", get(authorize))
            .with_state(shared.clone());
        let (addr, life) = serve(router).await?;
        lock(&shared.st).base = format!("http://{addr}");
        Ok(Self {
            shared,
            _life: Arc::new(life),
            addr,
        })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }
    /// `http://127.0.0.1:<port>/v1.0/`
    pub fn api_base(&self) -> String {
        format!("http://{}/v1.0/", self.addr)
    }
    /// Token endpoint for tenant `common`.
    pub fn token_url(&self) -> String {
        format!("http://{}/common/oauth2/v2.0/token", self.addr)
    }
    pub fn authorize_url(&self) -> String {
        format!("http://{}/common/oauth2/v2.0/authorize", self.addr)
    }

    /// Default page size of lists and delta when the client sets none
    /// (Graph's is 10).
    pub fn set_page_size(&self, n: usize) {
        lock(&self.shared.st).page_size = n.max(1);
    }

    /// Folder id of a well-known name (`inbox`, `sentitems`…) or an id.
    pub fn folder_id(&self, key: &str) -> Option<String> {
        lock(&self.shared.st).folder_id(key)
    }

    /// Adds a user folder under `parent` (an id or well-known name, or the
    /// root when `None`); returns its id.
    pub fn add_folder(&self, name: &str, parent: Option<&str>) -> Result<String, FakeError> {
        let mut st = lock(&self.shared.st);
        let parent_id = match parent {
            Some(p) => Some(
                st.folder_id(p)
                    .ok_or_else(|| FakeError::NoSuchFolder(p.into()))?,
            ),
            None => None,
        };
        let id = st.new_id("AAMkFolder");
        st.folders.push(GraphFolder {
            id: id.clone(),
            display_name: name.into(),
            parent_folder_id: parent_id,
            well_known_name: None,
        });
        Ok(id)
    }

    /// Delivers an unread message into `folder` (id or well-known name),
    /// received now. Returns the message id.
    pub fn inject(&self, folder: &str, eml: &[u8]) -> Result<String, FakeError> {
        self.inject_at(folder, eml, now_secs())
    }

    /// Same with an explicit `receivedDateTime` (unix seconds).
    pub fn inject_at(&self, folder: &str, eml: &[u8], received: i64) -> Result<String, FakeError> {
        let mut st = lock(&self.shared.st);
        let f = st
            .folder_id(folder)
            .ok_or_else(|| FakeError::NoSuchFolder(folder.into()))?;
        Ok(st.insert(&f, eml.to_vec(), false, received).id)
    }

    /// Marks read/unread as another client would.
    pub fn set_read(&self, id: &str, read: bool) -> Result<(), FakeError> {
        let mut st = lock(&self.shared.st);
        let seq = st.bump();
        let m = st
            .messages
            .iter_mut()
            .find(|m| m.id == id)
            .ok_or_else(|| FakeError::NoSuchMessage(id.into()))?;
        m.is_read = read;
        m.seq = seq;
        Ok(())
    }

    /// Moves a message as another client would; returns the new id.
    pub fn move_message(&self, id: &str, dest: &str) -> Result<String, FakeError> {
        let mut st = lock(&self.shared.st);
        let d = st
            .folder_id(dest)
            .ok_or_else(|| FakeError::NoSuchFolder(dest.into()))?;
        st.move_to(id, &d)
            .map(|m| m.id)
            .ok_or_else(|| FakeError::NoSuchMessage(id.into()))
    }

    /// Hard-deletes a message (shows up as `@removed` in delta).
    pub fn delete(&self, id: &str) -> Result<(), FakeError> {
        let mut st = lock(&self.shared.st);
        let i = st
            .messages
            .iter()
            .position(|m| m.id == id)
            .ok_or_else(|| FakeError::NoSuchMessage(id.into()))?;
        let m = st.messages.remove(i);
        let seq = st.bump();
        st.removed.push((m.folder, m.id, seq));
        Ok(())
    }

    /// Makes every delta token issued so far invalid (410 Gone).
    pub fn expire_delta(&self) {
        let mut st = lock(&self.shared.st);
        st.delta_floor = st.seq + 1;
    }

    pub fn folders(&self) -> Vec<GraphFolder> {
        lock(&self.shared.st).folders.clone()
    }

    pub fn messages(&self) -> Vec<GraphMessage> {
        lock(&self.shared.st)
            .messages
            .iter()
            .map(|m| GraphMessage {
                id: m.id.clone(),
                folder_id: m.folder.clone(),
                conversation_id: m.conversation.clone(),
                is_read: m.is_read,
                raw: m.raw.as_ref().clone(),
            })
            .collect()
    }

    /// Every `sendMail`, oldest first.
    pub fn sent(&self) -> Vec<GraphSent> {
        lock(&self.shared.st).sent.clone()
    }

    /// The next request whose path ends with `path_suffix` answers
    /// `status` with `body` (e.g. 429 with a Graph error).
    pub fn fail_next(&self, path_suffix: &str, status: u16, body: Value) {
        lock(&self.shared.st)
            .fail
            .push((path_suffix.into(), status, body));
    }

    pub fn expire_tokens(&self) {
        self.shared.oauth.expire_access();
    }
    pub fn revoke_tokens(&self) {
        self.shared.oauth.revoke_all();
    }
    pub fn token_requests(&self) -> Vec<TokenRequest> {
        self.shared.oauth.requests()
    }

    /// Forgets messages, user folders, sent mail, tombstones and failures.
    pub fn reset(&self) {
        let mut st = lock(&self.shared.st);
        st.messages.clear();
        st.removed.clear();
        st.sent.clear();
        st.fail.clear();
        st.folders.retain(|f| f.well_known_name.is_some());
        st.delta_floor = 0;
    }
}

type S = State<Arc<Shared>>;

fn guard(sh: &Shared, headers: &HeaderMap, path: &str) -> Result<(), Box<Response>> {
    if !sh.oauth.authorized(headers) {
        return Err(Box::new(err(
            401,
            "InvalidAuthenticationToken",
            "Access token has expired or is not yet valid.",
        )));
    }
    let mut st = lock(&sh.st);
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
    ($sh:expr, $h:expr, $path:expr) => {
        if let Err(r) = guard(&$sh, &$h, $path) {
            return *r;
        }
    };
}

fn addr_json((name, addr): &(String, String)) -> Value {
    json!({"emailAddress": {"name": if name.is_empty() { addr } else { name }, "address": addr}})
}

fn message_json(m: &Msg) -> Map<String, Value> {
    let meta = &m.meta;
    let list = |l: &[(String, String)]| Value::Array(l.iter().map(addr_json).collect());
    let body = mail_parser::MessageParser::new()
        .parse(m.raw.as_slice())
        .map(|p| match p.body_html(0) {
            Some(h) => json!({"contentType": "html", "content": h}),
            None => json!({"contentType": "text", "content": p.body_text(0).unwrap_or_default()}),
        });
    let from = meta.from.as_ref().map(addr_json).unwrap_or(Value::Null);
    let v = json!({
        "@odata.etag": format!("W/\"{}\"", m.seq),
        "id": m.id,
        "createdDateTime": iso(m.received),
        "lastModifiedDateTime": iso(m.received),
        "receivedDateTime": iso(m.received),
        "sentDateTime": iso(meta.date.unwrap_or(m.received)),
        "hasAttachments": meta.has_attachments,
        "internetMessageId": meta.message_id.as_ref().map(|i| format!("<{i}>")),
        "subject": meta.subject,
        "bodyPreview": meta.snippet,
        "importance": "normal",
        "parentFolderId": m.folder,
        "conversationId": m.conversation,
        "isRead": m.is_read,
        "isDraft": false,
        "inferenceClassification": "focused",
        "body": body.unwrap_or(Value::Null),
        "sender": from,
        "from": from,
        "toRecipients": list(&meta.to),
        "ccRecipients": list(&meta.cc),
        "bccRecipients": list(&meta.bcc),
        "replyTo": list(&meta.reply_to),
        "flag": {"flagStatus": if m.flagged { "flagged" } else { "notFlagged" }},
    });
    match v {
        Value::Object(o) => o,
        _ => Map::new(),
    }
}

/// Applies `$select` (id and etag always stay, as in Graph).
fn select(mut o: Map<String, Value>, q: &Query) -> Value {
    if let Some(sel) = q_get(q, "$select") {
        let keep: Vec<&str> = sel.split(',').map(str::trim).collect();
        o.retain(|k, _| k == "id" || k == "@odata.etag" || keep.contains(&k.as_str()));
    }
    Value::Object(o)
}

fn page_size(st: &St, q: &Query, h: &HeaderMap) -> usize {
    if let Some(t) = q_get(q, "$top").and_then(|t| t.parse::<usize>().ok()) {
        return t.clamp(1, 1000);
    }
    h.get_all("prefer")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .find_map(|p| {
            p.trim()
                .strip_prefix("odata.maxpagesize=")
                .and_then(|n| n.parse().ok())
        })
        .unwrap_or(st.page_size)
        .max(1)
}

/// A page of a list; `nextLink` keeps every query parameter and sets `$skip`.
fn paged(st: &St, path: &str, q: &Query, h: &HeaderMap, items: Vec<Value>) -> Value {
    let top = page_size(st, q, h);
    let skip: usize = q_get(q, "$skip").and_then(|s| s.parse().ok()).unwrap_or(0);
    let total = items.len();
    let page: Vec<Value> = items.into_iter().skip(skip).take(top).collect();
    let mut body = json!({"@odata.context": format!("{}/v1.0/$metadata", st.base), "value": page});
    if skip + top < total {
        let mut params: Vec<String> = q
            .iter()
            .filter(|(k, _)| k != "$skip" && k != "$top")
            .map(|(k, v)| format!("{k}={}", enc(v)))
            .collect();
        params.push(format!("$top={top}"));
        params.push(format!("$skip={}", skip + top));
        body["@odata.nextLink"] = Value::String(format!("{}{path}?{}", st.base, params.join("&")));
    }
    body
}

fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b',' | b'$' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

async fn me(State(sh): S, h: HeaderMap) -> Response {
    guard!(sh, h, "/me");
    let st = lock(&sh.st);
    Json(json!({
        "id": "fake-user-id",
        "displayName": st.display_name,
        "mail": st.email,
        "userPrincipalName": st.email,
    }))
    .into_response()
}

fn folder_json(st: &St, f: &GraphFolder) -> Value {
    let msgs = st.messages.iter().filter(|m| m.folder == f.id);
    let (total, unread) = msgs.fold((0, 0), |(t, u), m| (t + 1, u + usize::from(!m.is_read)));
    json!({
        "id": f.id,
        "displayName": f.display_name,
        "parentFolderId": f.parent_folder_id.clone().unwrap_or_else(|| "AAMkRoot".into()),
        "childFolderCount": st.folders.iter().filter(|c| c.parent_folder_id.as_deref() == Some(&f.id)).count(),
        "unreadItemCount": unread,
        "totalItemCount": total,
        "isHidden": false,
    })
}

async fn list_folders(State(sh): S, h: HeaderMap, AxQuery(q): AxQuery<Query>) -> Response {
    guard!(sh, h, "/mailFolders");
    let st = lock(&sh.st);
    let items = st
        .folders
        .iter()
        .filter(|f| f.parent_folder_id.is_none())
        .map(|f| folder_json(&st, f))
        .collect();
    Json(paged(&st, "/v1.0/me/mailFolders", &q, &h, items)).into_response()
}

async fn get_folder(State(sh): S, h: HeaderMap, Path(id): Path<String>) -> Response {
    guard!(sh, h, "/mailFolders/{id}");
    let st = lock(&sh.st);
    let Some(fid) = st.folder_id(&id) else {
        return not_found();
    };
    match st.folders.iter().find(|f| f.id == fid) {
        Some(f) => Json(folder_json(&st, f)).into_response(),
        None => not_found(),
    }
}

async fn child_folders(
    State(sh): S,
    h: HeaderMap,
    Path(id): Path<String>,
    AxQuery(q): AxQuery<Query>,
) -> Response {
    guard!(sh, h, "/childFolders");
    let st = lock(&sh.st);
    let Some(fid) = st.folder_id(&id) else {
        return not_found();
    };
    let items = st
        .folders
        .iter()
        .filter(|f| f.parent_folder_id.as_deref() == Some(&fid))
        .map(|f| folder_json(&st, f))
        .collect();
    Json(paged(
        &st,
        &format!("/v1.0/me/mailFolders/{id}/childFolders"),
        &q,
        &h,
        items,
    ))
    .into_response()
}

fn sorted_messages<'a>(st: &'a St, folder: Option<&str>) -> Vec<&'a Msg> {
    let mut v: Vec<&Msg> = st
        .messages
        .iter()
        .filter(|m| folder.is_none_or(|f| m.folder == f))
        .collect();
    v.sort_by(|a, b| b.received.cmp(&a.received).then(b.id.cmp(&a.id)));
    v
}

async fn folder_messages(
    State(sh): S,
    h: HeaderMap,
    Path(id): Path<String>,
    AxQuery(q): AxQuery<Query>,
) -> Response {
    guard!(sh, h, "/messages");
    let st = lock(&sh.st);
    let Some(fid) = st.folder_id(&id) else {
        return not_found();
    };
    let items = sorted_messages(&st, Some(&fid))
        .into_iter()
        .map(|m| select(message_json(m), &q))
        .collect();
    Json(paged(
        &st,
        &format!("/v1.0/me/mailFolders/{id}/messages"),
        &q,
        &h,
        items,
    ))
    .into_response()
}

async fn all_messages(State(sh): S, h: HeaderMap, AxQuery(q): AxQuery<Query>) -> Response {
    guard!(sh, h, "/me/messages");
    let st = lock(&sh.st);
    let items = sorted_messages(&st, None)
        .into_iter()
        .map(|m| select(message_json(m), &q))
        .collect();
    Json(paged(&st, "/v1.0/me/messages", &q, &h, items)).into_response()
}

async fn get_message(
    State(sh): S,
    h: HeaderMap,
    Path(id): Path<String>,
    AxQuery(q): AxQuery<Query>,
) -> Response {
    guard!(sh, h, "/messages/{id}");
    let st = lock(&sh.st);
    match st.messages.iter().find(|m| m.id == id) {
        Some(m) => Json(select(message_json(m), &q)).into_response(),
        None => not_found(),
    }
}

async fn mime_value(State(sh): S, h: HeaderMap, Path(id): Path<String>) -> Response {
    guard!(sh, h, "/$value");
    let st = lock(&sh.st);
    match st.messages.iter().find(|m| m.id == id) {
        Some(m) => (
            [(header::CONTENT_TYPE, "text/plain")],
            m.raw.as_ref().clone(),
        )
            .into_response(),
        None => not_found(),
    }
}

async fn patch_message(
    State(sh): S,
    h: HeaderMap,
    Path(id): Path<String>,
    Json(b): Json<Value>,
) -> Response {
    guard!(sh, h, "/messages/{id}");
    let mut st = lock(&sh.st);
    let seq = st.bump();
    let Some(m) = st.messages.iter_mut().find(|m| m.id == id) else {
        return not_found();
    };
    if let Some(r) = b.get("isRead").and_then(Value::as_bool) {
        m.is_read = r;
    }
    if let Some(f) = b.pointer("/flag/flagStatus").and_then(Value::as_str) {
        m.flagged = f == "flagged";
    }
    m.seq = seq;
    let m = m.clone();
    Json(Value::Object(message_json(&m))).into_response()
}

async fn move_message(
    State(sh): S,
    h: HeaderMap,
    Path(id): Path<String>,
    Json(b): Json<Value>,
) -> Response {
    guard!(sh, h, "/move");
    let mut st = lock(&sh.st);
    let Some(dest) = b
        .get("destinationId")
        .and_then(Value::as_str)
        .and_then(|d| st.folder_id(d))
    else {
        return err(400, "ErrorInvalidIdMalformed", "Id is malformed.");
    };
    match st.move_to(&id, &dest) {
        Some(m) => (StatusCode::CREATED, Json(Value::Object(message_json(&m)))).into_response(),
        None => not_found(),
    }
}

async fn delete_message(State(sh): S, h: HeaderMap, Path(id): Path<String>) -> Response {
    guard!(sh, h, "/messages/{id}");
    let mut st = lock(&sh.st);
    let Some(i) = st.messages.iter().position(|m| m.id == id) else {
        return not_found();
    };
    let m = st.messages.remove(i);
    let seq = st.bump();
    st.removed.push((m.folder, m.id, seq));
    StatusCode::NO_CONTENT.into_response()
}

/// Tokens: `$deltatoken=<seq>`; `$skiptoken=<since>.<snapshot>.<offset>`.
async fn delta(
    State(sh): S,
    h: HeaderMap,
    Path(id): Path<String>,
    AxQuery(q): AxQuery<Query>,
) -> Response {
    guard!(sh, h, "/delta");
    let st = lock(&sh.st);
    let Some(fid) = st.folder_id(&id) else {
        return not_found();
    };
    let (since, snapshot, offset) = if let Some(t) = q_get(&q, "$skiptoken") {
        let p: Vec<u64> = t.split('.').filter_map(|x| x.parse().ok()).collect();
        if p.len() != 3 {
            return err(400, "BadRequest", "Invalid skip token.");
        }
        (p[0], p[1], p[2] as usize)
    } else if let Some(t) = q_get(&q, "$deltatoken") {
        let Ok(t) = t.parse::<u64>() else {
            return err(400, "BadRequest", "Invalid delta token.");
        };
        (t, st.seq, 0)
    } else {
        (0, st.seq, 0)
    };
    if since > 0 && since < st.delta_floor {
        return err(
            410,
            "SyncStateNotFound",
            "The sync state generation is not found or has expired.",
        );
    }
    // Initial round only: $filter=receivedDateTime ge|gt <ISO>.
    let floor_ts = (since == 0)
        .then(|| q_get(&q, "$filter"))
        .flatten()
        .and_then(|f| {
            let mut w = f.split_whitespace();
            let (field, op, val) = (w.next()?, w.next()?, w.next()?);
            (field == "receivedDateTime" && (op == "ge" || op == "gt"))
                .then(|| mail_parser::DateTime::parse_rfc3339(val).map(|d| d.to_timestamp()))
                .flatten()
        });
    let mut items: Vec<(u64, Value)> = st
        .messages
        .iter()
        .filter(|m| m.folder == fid && m.seq > since && m.seq <= snapshot)
        .filter(|m| floor_ts.is_none_or(|t| m.received >= t))
        .map(|m| (m.seq, select(message_json(m), &q)))
        .collect();
    if since > 0 {
        items.extend(
            st.removed
                .iter()
                .filter(|(f, _, s)| *f == fid && *s > since && *s <= snapshot)
                .map(|(_, mid, s)| {
                    (*s, json!({"@odata.type": "#microsoft.graph.message", "id": mid, "@removed": {"reason": "deleted"}}))
                }),
        );
    }
    items.sort_by_key(|(s, _)| *s);
    let top = page_size(&st, &q, &h);
    let total = items.len();
    let page: Vec<Value> = items
        .into_iter()
        .skip(offset)
        .take(top)
        .map(|(_, v)| v)
        .collect();
    let path = format!("{}/v1.0/me/mailFolders/{id}/messages/delta", st.base);
    let sel = q_get(&q, "$select")
        .map(|s| format!("&$select={}", enc(s)))
        .unwrap_or_default();
    let mut body = json!({"@odata.context": format!("{}/v1.0/$metadata#Collection(message)", st.base), "value": page});
    if offset + top < total {
        body["@odata.nextLink"] = Value::String(format!(
            "{path}?$skiptoken={since}.{snapshot}.{}{sel}",
            offset + top
        ));
    } else {
        body["@odata.deltaLink"] = Value::String(format!("{path}?$deltatoken={snapshot}{sel}"));
    }
    Json(body).into_response()
}

async fn send_mail(State(sh): S, h: HeaderMap, body: Bytes) -> Response {
    guard!(sh, h, "/sendMail");
    let ct = h
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    let mut st = lock(&sh.st);
    let sent = if ct.starts_with("text/plain") {
        let text: String = String::from_utf8_lossy(&body).split_whitespace().collect();
        let Ok(mime) = STANDARD.decode(text) else {
            return err(
                400,
                "ErrorMimeContentInvalidBase64String",
                "Invalid base64 string for MIME content.",
            );
        };
        let meta = MailMeta::parse(&mime);
        if meta.to.is_empty() && meta.cc.is_empty() && meta.bcc.is_empty() {
            return err(
                400,
                "ErrorInvalidRecipients",
                "At least one recipient isn't valid.",
            );
        }
        GraphSent {
            mime: Some(mime),
            json: None,
            save_to_sent_items: true,
        }
    } else {
        let Ok(v) = serde_json::from_slice::<Value>(&body) else {
            return err(400, "RequestBodyRead", "Cannot read the request body.");
        };
        if v.pointer("/message/toRecipients")
            .and_then(Value::as_array)
            .is_none_or(|a| a.is_empty())
        {
            return err(
                400,
                "ErrorInvalidRecipients",
                "At least one recipient isn't valid.",
            );
        }
        let save = v
            .get("saveToSentItems")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        GraphSent {
            mime: None,
            json: Some(v),
            save_to_sent_items: save,
        }
    };
    if sent.save_to_sent_items
        && let Some(mime) = &sent.mime
        && let Some(f) = st.folder_id("sentitems")
    {
        let mime = mime.clone();
        st.insert(&f, mime, true, now_secs());
    }
    st.sent.push(sent);
    StatusCode::ACCEPTED.into_response()
}

async fn token(
    State(sh): S,
    Path(_tenant): Path<String>,
    Form(form): Form<BTreeMap<String, String>>,
) -> Response {
    sh.oauth.token(form)
}

async fn authorize(
    State(sh): S,
    Path(_tenant): Path<String>,
    AxQuery(q): AxQuery<BTreeMap<String, String>>,
) -> Response {
    sh.oauth.authorize(&q)
}
