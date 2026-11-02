//! `/api/sessions*`, `/api/queued/:id`, `/api/local-sessions*`.

use std::collections::HashMap;

use axum::Extension;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use serde_json::{Value, json};

use super::{ApiResult, AppState, Ctx, blocking, body, objs, ok, ok_true, own, sess_out, str_of};
use crate::backend::Obj;
use crate::error::ApiError;
use crate::validate::{Check, Num, Str};

async fn own_session(st: &AppState, ws: &str, id: &str) -> Result<Obj, ApiError> {
    let (id, ws) = (id.to_string(), ws.to_string());
    st.read(move |r| Ok(own(r.session_get(&id)?, &ws, "会话")?))
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
        let s = own(r.session_get(&id)?, &c.ws, "会话")?;
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
        own(r.session_get(&id)?, &c.ws, "会话")?;
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
                    let d = e.get("data").cloned().unwrap_or(Value::Null);
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
        if clean {
            out.push_str(&data);
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
        return Err(ApiError::bad_request("导入的会话是只读的"));
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
        own(r.session_get(&id)?, &c.ws, "会话")?;
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
        let i = own(r.item_get(&item)?, &c.ws, "事项")?;
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
            own(r.session_get(&sid)?, &ws, "排队消息")?;
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
        return Err(ApiError::bad_request("参数不对：text"));
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
        return Err(ApiError::bad_request("导入的会话是只读的"));
    }
    ok(json!(st.domain.queued_send_now(&id, &qid).await?))
}

pub async fn local(State(st): State<AppState>) -> ApiResult {
    let home = st.cfg.home.clone();
    let files = blocking(move || Ok(crate::system::local_candidates(&home, 60))).await?;
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
    // file reads first, outside the core lock
    let prepared = blocking(move || {
        Ok(keys
            .iter()
            .map(|k| crate::system::prepare_import(&home, k))
            .collect::<Vec<_>>())
    })
    .await?;
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
