//! `/api/sessions*`, `/api/queued/:id`, `/api/local-sessions*`.

use std::collections::HashMap;

use axum::Extension;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use serde_json::{Value, json};

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

pub async fn list(State(st): State<AppState>, Extension(c): Extension<Ctx>) -> ApiResult {
    let d = st.domain.clone();
    st.read(move |r| ok(objs(super::sessions_out(r, d.as_ref(), &c.ws)?)))
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

/// `Number(after ?? 0)` compared with integer seqs: NaN/+∞ match nothing, otherwise `seq > floor(x)`.
fn after_bound(q: Option<&str>) -> Option<i64> {
    let x = q.map(crate::config::js_number).unwrap_or(0.0);
    if x.is_nan() || x == f64::INFINITY {
        None
    } else if x == f64::NEG_INFINITY {
        Some(i64::MIN)
    } else {
        Some(x.floor() as i64)
    }
}

pub async fn events(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult {
    let after = after_bound(q.get("after").map(String::as_str));
    let bounded = q.get("page").is_some_and(|v| v == "bounded");
    st.read(move |r| {
        own(r.session_get(&id)?, &c.ws, Noun::Session)?;
        match after {
            Some(a) => {
                let mut body = EventsBody::new();
                if bounded && r.events_page_raw(&id, a, &mut |row| body.push(row))? {
                    return Ok(body.finish());
                }
                if r.events_raw(&id, a, &mut |row| body.push(row))? {
                    return Ok(body.finish());
                }
                let evs = r.events_after(&id, a)?.into_iter().map(|mut e| {
                    let mut d = e.get("data").cloned().unwrap_or(Value::Null);
                    let kind = e.get("kind").and_then(Value::as_str).unwrap_or_default();
                    system_shown(&kind.to_owned(), &mut d);
                    e.insert("data".into(), super::slim::ev_out(d));
                    e
                });
                ok(objs(evs.collect()))
            }
            None => ok(json!([])),
        }
    })
    .await
}

/// A system event the Hub wrote itself in zh-CN (a message parked for a Node that is not enabled
/// here), its `text` in the request's language.
fn system_shown(kind: &str, data: &mut Value) -> bool {
    if kind != "system" || data.get("parked") != Some(&Value::Bool(true)) {
        return false;
    }
    let Some(t) = data
        .get("text")
        .and_then(Value::as_str)
        .and_then(|t| steve_i18n::reword!(t, &["hub.session.node_parked"]))
    else {
        return false;
    };
    data["text"] = Value::String(t);
    true
}

/// The events response from stored JSON text: small rows that `evOut` would leave alone pass through as
/// stored; everything else is parsed, slimmed and re-serialized like the TS does for every row.
/// The JSON array of an events page, built row by row as the rows are read: a 5000-event page of a
/// large session is ~30 MB, and holding the rows next to the body doubled what several concurrent
/// pages kept in memory.
struct EventsBody(String);

impl EventsBody {
    fn new() -> Self {
        Self(String::from("["))
    }

    fn push(&mut self, (seq, ts, kind, data): crate::backend::RawEvent) {
        use std::fmt::Write;
        let out = &mut self.0;
        if out.len() > 1 {
            out.push(',');
        }
        // ≤ 4000 bytes means no string can exceed 4000 UTF-16 units; no `_meta`, `rawInput` or `image` to trim
        let clean = data.len() <= 4000
            && !data.contains("\"_meta\"")
            && !data.contains("\"rawInput\"")
            && !data.contains("\"image\"")
            && !data.contains("\\u")
            && serde_json::from_str::<serde::de::IgnoredAny>(&data).is_ok()
            && !has_integer_keys(&data);
        let _ = write!(
            out,
            "{{\"seq\":{seq},\"ts\":{ts},\"kind\":{},\"data\":",
            serde_json::to_string(&kind).unwrap_or_default()
        );
        let parked = kind == "system" && data.contains("\"parked\"");
        if clean && !parked {
            out.push_str(&data);
        } else if parked {
            let mut v = serde_json::from_str::<Value>(&data).unwrap_or(Value::Null);
            system_shown(&kind, &mut v);
            out.push_str(
                &serde_json::to_string(&super::slim::ev_out(v)).unwrap_or_else(|_| "null".into()),
            );
        } else {
            let v = serde_json::from_str::<Value>(&data).unwrap_or(Value::Null);
            out.push_str(
                &serde_json::to_string(&super::slim::ev_out(v)).unwrap_or_else(|_| "null".into()),
            );
        }
        out.push('}');
    }

    fn finish(mut self) -> axum::response::Response {
        self.0.push(']');
        self.0.shrink_to_fit();
        axum::response::IntoResponse::into_response((
            axum::http::StatusCode::OK,
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            self.0,
        ))
    }
}

/// `"123":` somewhere: JS would reorder integer-like keys on parse, so such rows take the slow path.
fn has_integer_keys(data: &str) -> bool {
    let b = data.as_bytes();
    let mut i = 0;
    while let Some(p) = data[i..].find("\":") {
        let end = i + p;
        let mut j = end;
        while j > 0 && b[j - 1].is_ascii_digit() {
            j -= 1;
        }
        if j < end && j > 0 && b[j - 1] == b'"' {
            return true;
        }
        i = end + 2;
    }
    false
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

/// `DELETE /api/queued/:id` (the older route): a missing row is a 404 as in the TS Hub; a row a turn
/// takes in between is a 409 like the session-scoped route.
pub async fn drop_queued(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
) -> ApiResult {
    let (qid, ws) = (id.clone(), c.ws.clone());
    let sid = st
        .read(move |r| {
            let sid = r
                .queued_get(&qid)?
                .map(|q| str_of(&q, "session_id"))
                .unwrap_or_default();
            own(r.session_get(&sid)?, &ws, Noun::Queued)?;
            Ok(sid)
        })
        .await?;
    st.domain.queued_remove(&sid, &id).await?;
    ok_true()
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
            use rusqlite::OptionalExtension;
            let row = store
                .reader::<_, steve_store::StoreError>(|conn| {
                    Ok(conn
                        .query_row(
                            "SELECT node_id, state_dir FROM runtime_sessions WHERE session_id = ?1",
                            [&sid],
                            |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)),
                        )
                        .optional()?)
                })
                .ok()
                .flatten();
            Ok(match row {
                Some((node_id, _)) if node_id != "local" && node_id != "local-jobs" => {
                    crate::resume::SessionState::RemoteNode
                }
                Some((_, Some(dir))) if !dir.trim().is_empty() => {
                    crate::resume::SessionState::IsolatedLocal {
                        harness_dir: data_dir
                            .join("node")
                            .join("state")
                            .join(dir)
                            .join("harness"),
                        expose_path: is_owner,
                    }
                }
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
        use rusqlite::OptionalExtension;
        store
            .reader::<_, steve_store::StoreError>(|conn| {
                Ok(conn
                    .query_row(
                        "SELECT node_id, principal, state_dir FROM runtime_sessions WHERE session_id = ?1",
                        [&sid],
                        |r| {
                            Ok((
                                r.get::<_, String>(0)?,
                                r.get::<_, Option<String>>(1)?,
                                r.get::<_, Option<String>>(2)?,
                            ))
                        },
                    )
                    .optional()?)
            })
            .map_err(|e| ApiError::Json(500, e.to_string()))
    })
    .await?;
    let (bound, node_id, principal, raw_state_dir) = match row {
        Some((node_id, principal, state_dir)) => (
            true,
            Some(node_id).filter(|v| !v.trim().is_empty()),
            principal.filter(|v| !v.trim().is_empty()),
            state_dir,
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

pub async fn local(State(st): State<AppState>) -> ApiResult {
    let files = match st.node.as_ref().map(|n| n.node()) {
        // the local Node on: its own list, the Hub reads no harness files
        Some(node) => crate::system::node_candidates(node.as_ref(), 60)
            .await
            .map_err(|e| {
                tracing::warn!("history of the local Node: {e}");
                ApiError::internal(steve_i18n::t!("hub.import.list_failed"))
            })?,
        None => {
            let home = st.cfg.home.clone();
            blocking(move || Ok(crate::system::local_candidates(&home, 60))).await?
        }
    };
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
    let (home, bus) = (st.cfg.home.clone(), st.bus.clone());
    // file reads first, outside the core lock; with the local Node on, it reads them, and only
    // what it lists
    let prepared = match st.node.as_ref().map(|n| n.node()) {
        Some(node) => crate::system::node_prepare(node.as_ref(), &keys).await,
        None => {
            blocking(move || {
                Ok(keys
                    .iter()
                    .map(|k| crate::system::prepare_import(&home, k))
                    .collect::<Vec<_>>())
            })
            .await?
        }
    };
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
    use super::{EventsBody, after_bound, has_integer_keys};
    use http_body_util::BodyExt;

    /// The parked-session notice reads in the request's language; zh-CN as stored.
    #[tokio::test]
    async fn the_parked_notice_by_language() {
        let stored = steve_i18n::stored(|| steve_i18n::t!("hub.session.node_parked"));
        let row = serde_json::json!({ "text": stored, "parked": true }).to_string();
        let page = |l| {
            steve_i18n::with(l, || {
                let mut b = EventsBody::new();
                b.push((1, 10, "system".to_string(), row.clone()));
                b.push((
                    2,
                    11,
                    "user".to_string(),
                    r#"{"text":"该会话"}"#.to_string(),
                ));
                b.finish()
            })
        };
        let text = |r: axum::response::Response| async move {
            let body = r.into_body().collect().await.unwrap().to_bytes();
            let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
            (v[0]["data"]["text"].clone(), v[1]["data"]["text"].clone())
        };
        let (en, user) = text(page(steve_i18n::EN)).await;
        assert!(
            en.as_str()
                .unwrap()
                .starts_with("This session is bound to a Node"),
            "{en}"
        );
        assert_eq!(user, "该会话");
        let (zh, _) = text(page(steve_i18n::ZH_CN)).await;
        assert_eq!(zh, stored.as_str());
        assert!(stored.starts_with("该会话绑定在 Node 上"));
    }

    #[tokio::test]
    async fn raw_events_match_the_slow_path() {
        let rows = vec![
            (
                1,
                10,
                "user".to_string(),
                r#"{"text":"hi","via":"me"}"#.to_string(),
            ),
            (
                2,
                11,
                "tool_call".to_string(),
                r#"{"title":"x","_meta":{"a":1},"rawInput":{}}"#.to_string(),
            ),
            (3, 12, "x".to_string(), "not json".to_string()),
            (4, 13, "y".to_string(), r#"{"b":1,"2":0}"#.to_string()),
        ];
        let mut b = EventsBody::new();
        for r in rows {
            b.push(r);
        }
        let body = b
            .finish()
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes();
        assert_eq!(
            String::from_utf8_lossy(&body),
            r#"[{"seq":1,"ts":10,"kind":"user","data":{"text":"hi","via":"me"}},{"seq":2,"ts":11,"kind":"tool_call","data":{"title":"x"}},{"seq":3,"ts":12,"kind":"x","data":null},{"seq":4,"ts":13,"kind":"y","data":{"b":1,"2":0}}]"#
        );
        assert!(has_integer_keys(r#"{"b":1,"2":0}"#));
        assert!(!has_integer_keys(r#"{"a2":1,"x":"3"}"#));
    }

    #[test]
    fn after_like_sqlite_number_compare() {
        assert_eq!(after_bound(None), Some(0));
        assert_eq!(after_bound(Some("")), Some(0));
        assert_eq!(after_bound(Some("12")), Some(12));
        assert_eq!(after_bound(Some("2.5")), Some(2));
        assert_eq!(after_bound(Some("-2.5")), Some(-3));
        assert_eq!(after_bound(Some("abc")), None);
        assert_eq!(after_bound(Some("Infinity")), None);
    }
}
