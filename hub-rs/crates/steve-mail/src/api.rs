//! REST that exists so far: mailbox settings (E1's service) and drafts (E3), `to-item` and landing attachments (E5),
//! thread archive / read and `to-knowledge` (E6), and the reading side: the thread list and one thread,
//! `PATCH` of its state, a message's HTML / inline image / original, the IMAP presets. Mounted
//! under `/api` behind the `Caller` layer: `.merge(steve_mail::api::routes(mail))`.

use axum::extract::{Path, Query};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use serde::Serialize;
use serde_json::{Value, json};
use steve_modules::{ApiError, Body, Caller, Modules};

use crate::compose::{DraftRequest, QuickDraft};
use crate::drafts::{DraftPatch, NewDraft};
use crate::error::{AdapterError, MailError, MailResult};
use crate::settings::SettingsPatch;
use crate::threads::{ListQuery, ThreadPatch, View};
use crate::{Mail, repo};

impl IntoResponse for MailError {
    fn into_response(self) -> Response {
        let api = match self {
            // the noun stays in the error; the sentence is made here, in the request's language
            MailError::Api(ApiError::NotFound(noun)) => match crate::error::not_found_text(&noun) {
                Some(msg) => {
                    return (StatusCode::NOT_FOUND, Json(json!({ "error": msg }))).into_response();
                }
                None => ApiError::NotFound(noun),
            },
            MailError::Api(a) => a,
            MailError::Adapter(
                AdapterError::Auth(m)
                | AdapterError::Rejected(m)
                | AdapterError::Gone(m)
                | AdapterError::Config(m)
                | AdapterError::Transient(m),
            ) => ApiError::Rule(m),
            MailError::Adapter(AdapterError::Unsupported) => {
                ApiError::rule(steve_i18n::t!("mail.adapter.unsupported"))
            }
            e => ApiError::Internal(e.to_string()),
        };
        api.into_response()
    }
}

fn to<T: Serialize>(v: T) -> MailResult<Json<Value>> {
    Ok(Json(serde_json::to_value(v).map_err(ApiError::from)?))
}

/// The mail routes, relative to `/api`.
pub fn routes(mail: Mail) -> Router<Modules> {
    Router::new()
        .route("/mail/accounts", get(accounts))
        .route("/mail/accounts/{id}", axum::routing::patch(patch_account))
        .route("/mail/accounts/{id}/sync", post(sync_account))
        .route("/mail/presets", get(presets))
        .route("/mail/threads", get(list_threads))
        .route("/mail/threads/{id}", get(get_thread).patch(patch_thread))
        .route("/mail/messages/{id}/html", get(message_html))
        .route("/mail/messages/{id}/cid/{cid}", get(message_cid))
        .route("/mail/messages/{id}/raw", get(message_raw))
        .route("/mail/threads/{id}/draft", post(quick_draft))
        .route("/mail/drafts", post(create_draft))
        .route(
            "/mail/drafts/{id}",
            get(get_draft).put(put_draft).delete(delete_draft),
        )
        .route("/mail/drafts/{id}/send", post(send_draft))
        .route("/mail/drafts/{id}/cancel", post(cancel_draft))
        .route("/mail/drafts/{id}/check", post(check_draft))
        .route("/mail/threads/{id}/to-item", post(to_item))
        .route("/mail/threads/{id}/archive", post(archive_thread))
        .route("/mail/threads/{id}/read", post(read_thread))
        .route("/mail/threads/{id}/to-knowledge", post(to_knowledge))
        .route("/mail/attachments/{id}/file", post(land_attachment))
        .layer(Extension(mail))
}

/// One entry of `GET /mail/accounts`.
#[derive(Debug, Clone, Serialize)]
pub struct AccountOut {
    pub connection_id: String,
    pub address: String,
    pub adapter: String,
    pub status: String,
    pub last_sync: Option<i64>,
    pub unread: i64,
    pub settings: repo::AccountSettings,
}

impl Mail {
    /// The caller's mailboxes (connections with a mail adapter that they may use).
    pub fn accounts(&self, who: &Caller) -> MailResult<Vec<AccountOut>> {
        let rows = self
            .0
            .m
            .store
            .reader(|c| steve_modules::accounts::connections::repo::list(c, &who.ws))?;
        let mut out = Vec::new();
        for r in rows {
            if self.adapter(&r.adapter).is_none() || self.0.conns.get(who, &r.id).is_err() {
                continue;
            }
            out.push(self.account(who, &r.id)?);
        }
        Ok(out)
    }

    /// One mailbox (`404 没有这个连接`).
    pub fn account(&self, who: &Caller, id: &str) -> MailResult<AccountOut> {
        let c = self.0.conns.get(who, id).map_err(MailError::from)?;
        let settings = self.settings(who, id)?;
        let (address, unread) = self.0.m.store.reader(|c| -> MailResult<(String, i64)> {
            let address = steve_modules::accounts::connections::repo::get(c, id)?
                .and_then(|r| {
                    r.config
                        .get("address")
                        .and_then(Value::as_str)
                        .map(String::from)
                })
                .unwrap_or_default();
            Ok((address, repo::unread_of(c, id)?))
        })?;
        Ok(AccountOut {
            connection_id: c.id,
            address,
            adapter: c.adapter,
            status: c.status,
            last_sync: c.last_sync,
            unread,
            settings,
        })
    }
}

async fn accounts(Extension(m): Extension<Mail>, who: Caller) -> MailResult<Json<Value>> {
    let mm = m.clone();
    to(blocking(move || mm.accounts(&who)).await?)
}

async fn patch_account(
    Extension(m): Extension<Mail>,
    who: Caller,
    Path(id): Path<String>,
    Body(p): Body<SettingsPatch>,
) -> MailResult<Json<Value>> {
    let mm = m.clone();
    to(blocking(move || {
        mm.patch_settings(&who, &id, &p)?;
        mm.account(&who, &id)
    })
    .await?)
}

async fn sync_account(
    Extension(m): Extension<Mail>,
    who: Caller,
    Path(id): Path<String>,
) -> MailResult<Response> {
    let mm = m.clone();
    let (w, i) = (who.clone(), id.clone());
    blocking(move || mm.settings(&w, &i)).await?;
    m.sync_soon(&who.ws, &id);
    Ok((StatusCode::ACCEPTED, Json(json!({"ok": true}))).into_response())
}

async fn quick_draft(
    Extension(m): Extension<Mail>,
    who: Caller,
    Path(id): Path<String>,
    Body(p): Body<DraftRequest>,
) -> MailResult<Json<Value>> {
    to(m.quick_draft(
        &who,
        QuickDraft {
            thread_id: id,
            instruction: p.instruction,
            bot: p.bot,
            ..Default::default()
        },
    )
    .await?)
}

async fn create_draft(
    Extension(m): Extension<Mail>,
    who: Caller,
    Body(p): Body<NewDraft>,
) -> MailResult<Json<Value>> {
    to(blocking(move || m.create_draft(&who, p)).await?)
}

async fn get_draft(
    Extension(m): Extension<Mail>,
    who: Caller,
    Path(id): Path<String>,
) -> MailResult<Json<Value>> {
    to(blocking(move || m.get_draft(&who, &id)).await?)
}

async fn put_draft(
    Extension(m): Extension<Mail>,
    who: Caller,
    Path(id): Path<String>,
    Body(p): Body<DraftPatch>,
) -> MailResult<Json<Value>> {
    to(blocking(move || m.update_draft(&who, &id, p)).await?)
}

async fn delete_draft(
    Extension(m): Extension<Mail>,
    who: Caller,
    Path(id): Path<String>,
) -> MailResult<Json<Value>> {
    blocking(move || m.delete_draft(&who, &id)).await?;
    Ok(Json(json!({"ok": true})))
}

async fn send_draft(
    Extension(m): Extension<Mail>,
    who: Caller,
    Path(id): Path<String>,
) -> MailResult<Json<Value>> {
    to(blocking(move || m.send_draft(&who, &id)).await?)
}

/// The model's disclosure check on demand (a draft written without YOLO got only the local checks).
async fn check_draft(
    Extension(m): Extension<Mail>,
    who: Caller,
    Path(id): Path<String>,
) -> MailResult<Json<Value>> {
    to(m.check_draft(&who, &id).await?)
}

async fn cancel_draft(
    Extension(m): Extension<Mail>,
    who: Caller,
    Path(id): Path<String>,
) -> MailResult<Json<Value>> {
    to(blocking(move || m.cancel_draft(&who, &id)).await?)
}

async fn blocking<R: Send + 'static>(
    f: impl FnOnce() -> MailResult<R> + Send + 'static,
) -> MailResult<R> {
    tokio::task::spawn_blocking(steve_i18n::carry(f))
        .await
        .map_err(|e| MailError::Other(format!("task failed: {e}")))?
}

async fn to_item(
    Extension(m): Extension<Mail>,
    who: Caller,
    Path(id): Path<String>,
    Body(p): Body<crate::items::ToItem>,
) -> MailResult<Json<Value>> {
    let item = m.to_item(&who, &id, p).await?;
    Ok(Json(json!({ "item_id": item })))
}

/// `POST /mail/attachments/:id/file`: the FileRef, or the store's refusal with its status
/// (`413` too large, `415` type not allowed, `410` original gone).
async fn land_attachment(
    Extension(m): Extension<Mail>,
    who: Caller,
    Path(id): Path<String>,
) -> MailResult<Response> {
    let r = blocking(move || {
        let viewer = m.viewer_of(&who)?;
        m.land_attachment(&viewer, &id)
    })
    .await?;
    Ok(match r {
        Ok(f) => Json(serde_json::to_value(f).map_err(ApiError::from)?).into_response(),
        Err(r) => (
            StatusCode::from_u16(r.status).unwrap_or(StatusCode::BAD_REQUEST),
            Json(json!({ "error": r.message })),
        )
            .into_response(),
    })
}

impl Mail {
    /// `POST /mail/threads/:id/archive` for a thread the caller sees: [`Mail::archive_thread`].
    /// Returns how many messages moved out of the inbox.
    pub async fn archive_for(&self, who: &Caller, thread_id: &str) -> MailResult<usize> {
        let t = self.visible_thread(who, thread_id).await?;
        self.archive_thread(&who.ws, &t).await
    }

    /// `POST /mail/threads/:id/read`: every unread message of a thread the caller sees is marked
    /// read ([`Mail::mark_read`]). Returns how many changed.
    pub async fn read_for(&self, who: &Caller, thread_id: &str) -> MailResult<usize> {
        let t = self.visible_thread(who, thread_id).await?;
        let msgs = self.0.m.store.reader(|c| repo::thread_messages(c, &t))?;
        let mut n = 0;
        for m in msgs.iter().filter(|m| !m.flags.iter().any(|f| f == "seen")) {
            if self.mark_read(&who.ws, &m.id).await? {
                n += 1;
            }
        }
        Ok(n)
    }

    async fn visible_thread(&self, who: &Caller, thread_id: &str) -> MailResult<String> {
        let (m, w, id) = (self.clone(), who.clone(), thread_id.to_string());
        blocking(move || {
            let viewer = m.viewer_of(&w)?;
            m.0.m
                .store
                .reader(|c| m.own_thread(c, &viewer, &id))
                .map(|t| t.id)
        })
        .await
    }
}

async fn archive_thread(
    Extension(m): Extension<Mail>,
    who: Caller,
    Path(id): Path<String>,
) -> MailResult<Json<Value>> {
    let n = m.archive_for(&who, &id).await?;
    Ok(Json(json!({ "ok": true, "archived": n })))
}

async fn read_thread(
    Extension(m): Extension<Mail>,
    who: Caller,
    Path(id): Path<String>,
) -> MailResult<Json<Value>> {
    let n = m.read_for(&who, &id).await?;
    Ok(Json(json!({ "ok": true, "changed": n })))
}

async fn to_knowledge(
    Extension(m): Extension<Mail>,
    who: Caller,
    Path(id): Path<String>,
) -> MailResult<Json<Value>> {
    let doc = blocking(move || m.to_knowledge(&who, &id)).await?;
    Ok(Json(json!({ "doc_id": doc })))
}

// ---------------------------------------------------------------- reading

/// `GET /mail/presets`: the IMAP presets of the new-mailbox dialog.
async fn presets() -> Json<Value> {
    Json(json!(crate::adapters::imap::presets()))
}

/// The query of `GET /mail/threads`, parsed here so a bad value is `400 参数不对：<key>`.
fn list_query(q: &std::collections::HashMap<String, String>) -> MailResult<ListQuery> {
    let mut bad = Vec::new();
    let view = match q.get("view").map(String::as_str) {
        None | Some("") | Some("inbox") => View::Inbox,
        Some("needs_reply") => View::NeedsReply,
        Some("waiting") => View::Waiting,
        Some("sent") => View::Sent,
        Some("drafts") => View::Drafts,
        Some("all") => View::All,
        Some(_) => {
            bad.push("view".to_string());
            View::Inbox
        }
    };
    let mut num = |k: &str| -> Option<i64> {
        let v = q.get(k).filter(|v| !v.is_empty())?;
        match v.parse::<i64>() {
            Ok(n) => Some(n),
            Err(_) => {
                bad.push(k.to_string());
                None
            }
        }
    };
    let before = num("before");
    let limit = num("limit");
    if limit.is_some_and(|l| l < 1) {
        bad.push("limit".into());
    }
    if !bad.is_empty() {
        return Err(ApiError::Invalid(bad).into());
    }
    Ok(ListQuery {
        account: q.get("account").filter(|a| !a.is_empty()).cloned(),
        view,
        q: q.get("q").cloned(),
        before,
        limit: limit.map(|l| l as usize),
    })
}

async fn list_threads(
    Extension(m): Extension<Mail>,
    who: Caller,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> MailResult<Json<Value>> {
    let q = list_query(&q)?;
    to(blocking(move || m.list_threads(&who, &q)).await?)
}

async fn get_thread(
    Extension(m): Extension<Mail>,
    who: Caller,
    Path(id): Path<String>,
) -> MailResult<Json<Value>> {
    to(blocking(move || m.thread_detail(&who, &id)).await?)
}

async fn patch_thread(
    Extension(m): Extension<Mail>,
    who: Caller,
    Path(id): Path<String>,
    Body(p): Body<ThreadPatch>,
) -> MailResult<Json<Value>> {
    // in a blocking task, but the `done` follow-up is spawned on the runtime
    let handle = tokio::runtime::Handle::current();
    to(blocking(move || {
        let _g = handle.enter();
        m.patch_thread(&who, &id, &p)
    })
    .await?)
}

fn refused(r: crate::attach::Refused) -> Response {
    (
        StatusCode::from_u16(r.status).unwrap_or(StatusCode::BAD_REQUEST),
        Json(json!({ "error": r.message })),
    )
        .into_response()
}

/// Headers every message body or part is served with: never cached, never sniffed.
fn private_headers(content_type: &str) -> [(header::HeaderName, String); 4] {
    [
        (header::CONTENT_TYPE, content_type.to_string()),
        (header::CACHE_CONTROL, "no-store".into()),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff".into()),
        (header::REFERRER_POLICY, "no-referrer".into()),
    ]
}

/// `GET /mail/messages/:id/html[?images=1]`: sanitized HTML under a CSP without scripts or remote
/// loads (remote images only with `images=1`), for a sandboxed iframe.
async fn message_html(
    Extension(m): Extension<Mail>,
    who: Caller,
    Path(id): Path<String>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> MailResult<Response> {
    let images = q.get("images").is_some_and(|v| v == "1");
    Ok(
        match blocking(move || m.message_html(&who, &id, images)).await? {
            Ok(h) => {
                let mut res = (private_headers("text/html; charset=utf-8"), h.body).into_response();
                if let Ok(v) = header::HeaderValue::from_str(&h.csp) {
                    res.headers_mut().insert(header::CONTENT_SECURITY_POLICY, v);
                }
                res
            }
            Err(r) => refused(r),
        },
    )
}

/// `GET /mail/messages/:id/cid/:cid`.
async fn message_cid(
    Extension(m): Extension<Mail>,
    who: Caller,
    Path((id, cid)): Path<(String, String)>,
) -> MailResult<Response> {
    Ok(
        match blocking(move || m.message_cid(&who, &id, &cid)).await? {
            Ok(p) => {
                let mut res = (private_headers(&p.mime), p.bytes).into_response();
                res.headers_mut().insert(
                    header::CONTENT_SECURITY_POLICY,
                    header::HeaderValue::from_static("default-src 'none'; sandbox"),
                );
                if p.mime == "application/octet-stream" {
                    res.headers_mut().insert(
                        header::CONTENT_DISPOSITION,
                        header::HeaderValue::from_static("attachment"),
                    );
                }
                res
            }
            Err(r) => refused(r),
        },
    )
}

/// `GET /mail/messages/:id/raw`: the original as a download.
async fn message_raw(
    Extension(m): Extension<Mail>,
    who: Caller,
    Path(id): Path<String>,
) -> MailResult<Response> {
    let name = format!(
        "attachment; filename=\"{}.eml\"",
        id.replace(['"', '\\', '/'], "")
    );
    Ok(match blocking(move || m.message_raw(&who, &id)).await? {
        Ok(b) => {
            let mut res = (private_headers("message/rfc822"), b).into_response();
            if let Ok(v) = header::HeaderValue::from_str(&name) {
                res.headers_mut().insert(header::CONTENT_DISPOSITION, v);
            }
            res
        }
        Err(r) => refused(r),
    })
}
