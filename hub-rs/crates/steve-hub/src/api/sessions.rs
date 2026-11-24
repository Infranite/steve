//! `/api/sessions*`, `/api/local-sessions*`.

use std::collections::HashMap;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::response::IntoResponse;
use axum::{Extension, Json};
use serde::Deserialize;
use serde_json::{Value, json};
use steve_store::{StoreError, Views};

use super::{
    ApiResult, AppState, Ctx, Noun, blocking, body, objs, ok, ok_true, own, sess_out, str_of,
};
use crate::backend::Obj;
use crate::error::ApiError;
use crate::validate::{Check, Num, Str};

async fn own_session(st: &AppState, ws: &str, id: &str) -> Result<Obj, ApiError> {
    let (id, ws) = (id.to_string(), ws.to_string());
    st.read(move |r| Ok(own(r.session_get(&id)?, &ws, Noun::Session)?))
        .await
}

pub async fn search(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult {
    let q = q.get("q").cloned().unwrap_or_default();
    st.read(move |r| ok(json!(r.sessions_search(&c.ws, &q)?)))
        .await
}

pub async fn get_one(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
) -> ApiResult {
    let d = st.domain.clone();
    st.read(move |r| {
        let s = own(r.session_get(&id)?, &c.ws, Noun::Session)?;
        ok(Value::Object(sess_out(r, d.as_ref(), s)?))
    })
    .await
}

#[derive(Deserialize)]
pub struct TranscriptQuery {
    before: Option<i64>,
    after: Option<i64>,
    through: Option<i64>,
    align: Option<String>,
    limit: Option<u32>,
}

#[derive(Deserialize)]
pub struct MarksQuery {
    every: Option<usize>,
}

async fn transcript_read<T: Send + 'static>(
    st: AppState,
    c: Ctx,
    id: String,
    read: impl FnOnce(
        &mut steve_store::views::Transcript<'_, steve_store::R<'_>>,
        &str,
    ) -> steve_store::Result<T>
    + Send
    + 'static,
) -> Result<T, ApiError> {
    let store = super::views::store(&st)?;
    blocking(move || {
        store
            .read(|r| {
                let mut transcript = r.transcript();
                if !transcript.exists(&c.ws, &id)? {
                    return Ok(None);
                }
                read(&mut transcript, &id).map(Some)
            })
            .map_err(|e: StoreError| ApiError::internal(e.to_string()))?
            .ok_or_else(|| ApiError::Json(404, Noun::Session.missing()))
    })
    .await
}

pub async fn events(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    Query(q): Query<TranscriptQuery>,
) -> ApiResult {
    if q.after.is_some() && (q.before.is_some() || q.align.is_some())
        || q.after.is_none() && q.through.is_some()
        || q.align.as_deref().is_some_and(|align| align != "turn")
    {
        return Err(ApiError::bad_request("invalid transcript cursor"));
    }
    let limit = q.limit.unwrap_or(400).clamp(1, 1000);
    // Translate inside the blocking task, where the request's locale is carried.
    let page = transcript_read(st, c, id, move |transcript, id| {
        let mut page = match q.after {
            Some(after) => transcript.after(id, after, q.through, limit)?,
            None => transcript.before(id, q.before, limit)?,
        };
        for event in &mut page.events {
            system_shown(&event.kind, &mut event.data);
            event.data = super::slim::ev_out(std::mem::take(&mut event.data));
        }
        Ok(page)
    })
    .await?;
    crate::metrics::once!(histogram "steve_transcript_page_events")
        .record(page.events.len() as f64);
    Ok(Json(page).into_response())
}

pub async fn marks(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    Query(q): Query<MarksQuery>,
) -> ApiResult {
    let every = q.every.unwrap_or(1000).clamp(1, 5000);
    let marks =
        transcript_read(st, c, id, move |transcript, id| transcript.marks(id, every)).await?;
    Ok(Json(marks).into_response())
}

/// A system event the Hub wrote itself in zh-CN (a message parked for a Node that is not enabled
/// here), its `text` in the request's language.
fn system_shown(kind: &str, data: &mut Value) -> bool {
    if kind != "system" {
        return false;
    }
    let keys: &[&str] = if data.get("parked") == Some(&Value::Bool(true)) {
        &["hub.session.node_parked"]
    } else if data.get("steve_protocol_hash").is_some() {
        &["hub.session.protocol_updated"]
    } else {
        return false;
    };
    let Some(t) = data
        .get("text")
        .and_then(Value::as_str)
        .and_then(|t| steve_i18n::reword!(t, keys))
    else {
        return false;
    };
    data["text"] = Value::String(t);
    true
}

pub async fn message(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    b: Bytes,
) -> ApiResult {
    let s = own_session(&st, &c.ws, &id).await?;
    if crate::js::truthy(s.get("readonly")) {
        return Err(ApiError::bad_request(steve_i18n::t!(
            "hub.session.imported_readonly"
        )));
    }
    let v = body(&b)?;
    let mut k = Check::object(&v);
    let text = k.str("text", Str::min(1));
    let mode = k
        .opt_enum("mode", &["插话", "排队"])
        .unwrap_or_else(|| "排队".into());
    let files = super::files::file_ids(&mut k);
    k.finish()?;
    let refs = super::files::refs(&st, &c.ws, files).await?;
    ok(json!({ "result": st.domain.session_message(&id, &text, &mode, refs).await? }))
}

pub async fn cancel(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
) -> ApiResult {
    own_session(&st, &c.ws, &id).await?;
    ok(json!({ "ok": st.domain.cancel(&id).await? }))
}

pub async fn end(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
) -> ApiResult {
    own_session(&st, &c.ws, &id).await?;
    st.domain.end_session(&id).await?;
    ok_true()
}

/// `POST /sessions/{id}/continue`: the item goes on in a new session (U10).
pub async fn continue_new(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
) -> ApiResult {
    own_session(&st, &c.ws, &id).await?;
    let ns = st.domain.continue_in_new_session(&id, &c.me).await?;
    ok(json!({ "session": ns }))
}

/// `POST /sessions/{id}/handoff`: hand off an item session to a fresh session (same or different
/// Bot, harness, and/or model) carrying deterministic working memory through W3.1 placement.
pub async fn handoff(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    b: Bytes,
) -> ApiResult {
    own_session(&st, &c.ws, &id).await?;
    let target = super::items::parse_handoff_body(&b)?;
    ok(st.domain.handoff_session(&id, target, &c.me).await?)
}

pub async fn patch_one(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    b: Bytes,
) -> ApiResult {
    own_session(&st, &c.ws, &id).await?;
    let v = body(&b)?;
    let mut k = Check::object(&v);
    let title = k.str("title", Str::len(1, 80));
    k.finish()?;
    st.write(move |r| {
        let mut p = Obj::new();
        p.insert("title".into(), json!(title));
        r.session_patch(&id, &p)?;
        ok_true()
    })
    .await
}

pub async fn delete_one(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
) -> ApiResult {
    st.write(move |r| {
        own(r.session_get(&id)?, &c.ws, Noun::Session)?;
        crate::system::remove_imported(r, &id)?;
        ok_true()
    })
    .await
}

pub async fn fork(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    b: Bytes,
) -> ApiResult {
    own_session(&st, &c.ws, &id).await?;
    let v = body(&b)?;
    let mut k = Check::object(&v);
    let seq = k.num("seq", Num::ANY);
    let goal = k.opt_str("goal", Str::ANY).unwrap_or_default();
    let as_session = k.opt_bool("asSession").unwrap_or(false);
    k.finish()?;
    ok(Value::Object(
        st.domain
            .fork(&c.ws, &id, seq, &goal, as_session, &c.me)
            .await?,
    ))
}

pub async fn link(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    b: Bytes,
) -> ApiResult {
    own_session(&st, &c.ws, &id).await?;
    let v = body(&b)?;
    let mut k = Check::object(&v);
    let item = k.str("item", Str::ANY);
    k.finish()?;
    st.write(move |r| {
        let i = own(r.item_get(&item)?, &c.ws, Noun::Item)?;
        let iid = str_of(&i, "id");
        let mut p = Obj::new();
        p.insert("item_id".into(), json!(iid));
        p.insert("no".into(), json!(r.sessions_of_item(&iid)?.len() + 1));
        r.session_patch(&id, &p)?;
        r.timeline_add(&iid, &c.me, "session", "关联了一个已有会话", Some(&id))?;
        ok_true()
    })
    .await
}

/// `GET /api/sessions/:id/queued`: the rows waiting for the session's next turn, in order.
pub async fn queued_list(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
) -> ApiResult {
    own_session(&st, &c.ws, &id).await?;
    st.read(move |r| ok(objs(r.queued_list(&id)?))).await
}

/// `PATCH /api/sessions/:id/queued/:qid {text}` → the row after.
pub async fn queued_edit(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path((id, qid)): Path<(String, String)>,
    b: Bytes,
) -> ApiResult {
    own_session(&st, &c.ws, &id).await?;
    let v = body(&b)?;
    let mut k = Check::object(&v);
    let text = k.str("text", Str::min(1));
    k.finish()?;
    let text = text.trim().to_string();
    if text.is_empty() {
        return Err(ApiError::Invalid(vec!["text".into()]));
    }
    ok(json!(st.domain.queued_edit(&id, &qid, &text).await?))
}

/// `DELETE /api/sessions/:id/queued/:qid` → the row as it was (the composer takes it back).
pub async fn queued_remove(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path((id, qid)): Path<(String, String)>,
) -> ApiResult {
    own_session(&st, &c.ws, &id).await?;
    ok(json!(st.domain.queued_remove(&id, &qid).await?))
}

/// `POST /api/sessions/:id/queued/:qid/send-now` → `{result: interrupted | sent | waiting, reason?}`.
pub async fn queued_send_now(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path((id, qid)): Path<(String, String)>,
) -> ApiResult {
    let s = own_session(&st, &c.ws, &id).await?;
    if crate::js::truthy(s.get("readonly")) {
        return Err(ApiError::bad_request(steve_i18n::t!(
            "hub.session.imported_readonly"
        )));
    }
    ok(json!(st.domain.queued_send_now(&id, &qid).await?))
}

/// `GET /api/sessions/:id/resume`: the harness's own session id, the command that resumes it there, the
/// working directory, and whether the harness's transcript of that id is on this machine.
pub async fn resume(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
) -> ApiResult {
    let s = own_session(&st, &c.ws, &id).await?;
    let homes = crate::resume::Homes::of(&st.cfg);
    let is_owner = c.user.get("role").and_then(Value::as_str) == Some("owner");
    let session_state = if let Some(store) = st.modules.as_ref().map(|m| m.store.clone()) {
        let sid = id.clone();
        let data_dir = st.cfg.data_dir.clone();
        blocking(move || {
            let row = store
                .read(|r| steve_store::node_sessions::principal_state(r, &sid))
                .ok()
                .flatten();
            Ok(match row {
                Some(ref row) if row.node != "local" && row.node != "local-jobs" => {
                    crate::resume::SessionState::RemoteNode
                }
                Some(steve_store::node_sessions::PrincipalState {
                    state_dir: Some(dir),
                    ..
                }) if !dir.trim().is_empty() => crate::resume::SessionState::IsolatedLocal {
                    harness_dir: data_dir
                        .join("node")
                        .join("state")
                        .join(dir)
                        .join("harness"),
                    expose_path: is_owner,
                },
                _ => crate::resume::SessionState::AmbientLocal,
            })
        })
        .await?
    } else {
        crate::resume::SessionState::AmbientLocal
    };
    // the local Node on: it finds ambient transcripts, the Hub reads no ambient harness files
    if let Some(node) = st.node.as_ref().map(|n| n.node()) {
        return ok(crate::resume::info_on_state(&homes, &s, node.as_ref(), &session_state).await);
    }
    let v = blocking(move || Ok(crate::resume::info_for_state(&homes, &s, &session_state))).await?;
    ok(v)
}

/// `GET /api/sessions/:id/principal-state`: the recorded Node binding and independent harness
/// state directory of `id` (`STEVE_PRINCIPAL_STATE=on`), for the instance owner only.
pub async fn principal_state(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
) -> ApiResult {
    let s = own_session(&st, &c.ws, &id).await?;
    if c.user.get("role").and_then(Value::as_str) != Some("owner") {
        return Err(ApiError::Json(
            403,
            "Only the instance owner can inspect principal state directories.".into(),
        ));
    }
    let store = st
        .modules
        .as_ref()
        .map(|m| m.store.clone())
        .ok_or_else(|| ApiError::Json(503, "Principal state inspection unavailable.".into()))?;
    let harness = s
        .get("harness")
        .and_then(Value::as_str)
        .filter(|v| !v.trim().is_empty())
        .map(str::to_string);
    let sid = id.clone();
    let row = blocking(move || {
        store
            .read(|r| steve_store::node_sessions::principal_state(r, &sid))
            .map_err(|e| ApiError::Json(500, e.to_string()))
    })
    .await?;
    let (bound, node_id, principal, raw_state_dir) = match row {
        Some(row) => (
            true,
            Some(row.node).filter(|v| !v.trim().is_empty()),
            row.principal.filter(|v| !v.trim().is_empty()),
            row.state_dir,
        ),
        None => (false, None, None, None),
    };
    let state_dir = raw_state_dir.filter(|v| !v.trim().is_empty());
    let independent = state_dir.is_some();
    let harness_rel_dir = state_dir.as_ref().map(|dir| format!("state/{dir}/harness"));
    let local_harness_dir = match (&state_dir, node_id.as_deref()) {
        (Some(dir), Some("local")) => Some(
            st.cfg
                .data_dir
                .join("node")
                .join("state")
                .join(dir)
                .join("harness")
                .to_string_lossy()
                .into_owned(),
        ),
        _ => None,
    };
    ok(json!({
        "bound": bound,
        "session_id": id,
        "harness": harness,
        "node_id": node_id,
        "principal": principal,
        "independent": independent,
        "state_dir": state_dir,
        "harness_rel_dir": harness_rel_dir,
        "local_harness_dir": local_harness_dir,
    }))
}

/// History belongs to the jobs Node, independently of local session execution.
fn history_node(st: &AppState) -> Result<&dyn steve_node_api::Discovery, ApiError> {
    st.history
        .as_deref()
        .ok_or_else(|| ApiError::Json(503, steve_i18n::t!("hub.import.list_failed")))
}

pub async fn local(State(st): State<AppState>) -> ApiResult {
    let files = crate::system::node_candidates(history_node(&st)?, 60)
        .await
        .map_err(|e| {
            tracing::warn!("history of the local Node: {e}");
            ApiError::internal(steve_i18n::t!("hub.import.list_failed"))
        })?;
    st.read(move |r| {
        let done = r.import_keys()?;
        let out: Vec<Value> = files
            .into_iter()
            .map(|s| {
                let imported = done.contains(&s.key);
                let mut v = serde_json::to_value(&s).unwrap_or(Value::Null);
                if let Some(o) = v.as_object_mut() {
                    o.insert("imported".into(), json!(imported));
                }
                v
            })
            .collect();
        ok(Value::Array(out))
    })
    .await
}

pub async fn import(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    b: Bytes,
) -> ApiResult {
    let v = body(&b)?;
    let mut k = Check::object(&v);
    let keys = k.str_array("keys", Some(1), Some(50));
    k.finish()?;
    let bus = st.bus.clone();
    // Node file reads happen outside the core lock, and only for its discovery keys.
    let prepared = crate::system::node_prepare(history_node(&st)?, &keys).await;
    st.write(move |r| {
        let out: Vec<Value> = prepared
            .into_iter()
            .map(
                |p| match crate::system::import_prepared(r, &bus, &c.ws, p) {
                    Ok(id) => json!(id),
                    Err(e) => json!({ "error": e.message }),
                },
            )
            .collect();
        ok(Value::Array(out))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::system_shown;
    use serde_json::json;

    #[test]
    fn protocol_sync_notice_reads_in_the_request_language() {
        let mut data = json!({"text": "已同步当前 Steve 工作协议", "steve_protocol_hash": "h"});
        assert!(steve_i18n::with(steve_i18n::EN, || system_shown(
            "system", &mut data
        )));
        assert_eq!(data["text"], "Synced the current Steve working protocol");
        assert_eq!(data["steve_protocol_hash"], "h");
    }

    /// The parked-session notice reads in the request's language; zh-CN as stored.
    #[test]
    fn the_parked_notice_by_language() {
        let stored = steve_i18n::stored(|| steve_i18n::t!("hub.session.node_parked"));
        let mut data = json!({ "text": stored, "parked": true });
        steve_i18n::with(steve_i18n::EN, || system_shown("system", &mut data));
        assert!(
            data["text"]
                .as_str()
                .unwrap()
                .starts_with("This session is bound to a Node")
        );
    }
}

/// Routes mounted behind the shared API login and body-limit layers.
pub(crate) fn routes() -> axum::Router<super::AppState> {
    use super::*;
    Router::new()
        .route("/sessions/search", get(sessions::search))
        .route(
            "/sessions/{id}",
            get(sessions::get_one)
                .patch(sessions::patch_one)
                .delete(sessions::delete_one),
        )
        .route("/sessions/{id}/events", get(sessions::events))
        .route("/sessions/{id}/events/marks", get(sessions::marks))
        .route("/sessions/{id}/messages", post(sessions::message))
        .route("/sessions/{id}/cancel", post(sessions::cancel))
        .route("/sessions/{id}/end", post(sessions::end))
        .route("/sessions/{id}/continue", post(sessions::continue_new))
        .route("/sessions/{id}/handoff", post(sessions::handoff))
        .route("/sessions/{id}/fork", post(sessions::fork))
        .route("/sessions/{id}/link", post(sessions::link))
        .route("/sessions/{id}/queued", get(sessions::queued_list))
        .route("/sessions/{id}/resume", get(sessions::resume))
        .route(
            "/sessions/{id}/principal-state",
            get(sessions::principal_state),
        )
        .route(
            "/sessions/{id}/queued/{qid}",
            patch(sessions::queued_edit).delete(sessions::queued_remove),
        )
        .route(
            "/sessions/{id}/queued/{qid}/send-now",
            post(sessions::queued_send_now),
        )
        .route("/local-sessions", get(sessions::local))
        .route("/local-sessions/import", post(sessions::import))
}
