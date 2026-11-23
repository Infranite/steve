//! Read views (`steve_types::view`): `GET /api/snapshot` (the hot views, from the projection's
//! memory), `GET /api/views` (by id) and `GET /api/views/page` (what is outside the hot set). What
//! changes afterwards arrives on `/api/stream` as `patch` frames; every answer carries the `rev` a
//! client merges by.

use std::hash::{DefaultHasher, Hash, Hasher};

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post};
use axum::{Extension, Json};
use serde::{Deserialize, Serialize};
use steve_store::Views;
use steve_store::views::{DecisionPage, Page, SettledCursor};
use steve_types::{ById, PageOf, View, ViewKind};

use super::{AppState, Ctx};
use crate::error::ApiError;

type ApiResult = Result<Response, ApiError>;

/// Ids one `GET /api/views` may ask for.
const IDS_MAX: usize = 500;
const PAGE_MAX: u32 = 500;
const PAGE_DEFAULT: u32 = 100;

pub(crate) fn routes() -> axum::Router<AppState> {
    axum::Router::new()
        .route("/snapshot", get(snapshot))
        .route("/views", get(by_id))
        .route("/views/page", get(page))
        .route("/decisions/dismiss-drafts", post(dismiss_drafts))
        .route("/decisions/{id}/draft", patch(edit_draft))
}

fn unavailable() -> ApiError {
    ApiError::Json(503, "unavailable".into())
}

fn invalid(field: &str) -> ApiError {
    ApiError::Invalid(vec![field.into()])
}

fn kind(s: &str) -> Result<ViewKind, ApiError> {
    ViewKind::parse(s).ok_or_else(|| invalid("kind"))
}

fn json(v: &impl Serialize) -> Result<Vec<u8>, ApiError> {
    serde_json::to_vec(v).map_err(|e| ApiError::internal(e.to_string()))
}

fn json_body(body: Vec<u8>) -> Response {
    ([(header::CONTENT_TYPE, "application/json")], body).into_response()
}

#[derive(Deserialize)]
struct SnapshotQuery {
    /// Comma-separated kinds; all of them when absent.
    kinds: Option<String>,
}

/// `GET /api/snapshot?kinds=decision`: no store call; 304 while nothing in the workspace changed.
async fn snapshot(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Query(q): Query<SnapshotQuery>,
    headers: HeaderMap,
) -> ApiResult {
    let mut kinds = match q.kinds.as_deref().filter(|k| !k.is_empty()) {
        Some(ks) => ks.split(',').map(kind).collect::<Result<Vec<_>, _>>()?,
        None => ViewKind::ALL.to_vec(),
    };
    kinds.sort();
    kinds.dedup();
    let p = st.projection.clone().ok_or_else(unavailable)?;
    let calls = steve_store::instrumentation::RequestCalls::default();
    let snap = calls
        .scope(p.snapshot(&c.ws))
        .await
        .ok_or_else(unavailable)?;
    crate::metrics::once!(histogram "steve_projection_snapshot_store_calls")
        .record(calls.count() as f64);
    let names: Vec<&str> = kinds.iter().map(|k| k.as_str()).collect();
    // A client can ask for another workspace with the same local change counter.
    let mut scope = DefaultHasher::new();
    c.ws.hash(&mut scope);
    let etag = format!(
        r#""{}:{:x}:{}:{}""#,
        st.bus.epoch(),
        scope.finish(),
        snap.changed,
        names.join(",")
    );
    if headers
        .get(header::IF_NONE_MATCH)
        .is_some_and(|v| v.as_bytes() == etag.as_bytes())
    {
        return Ok((StatusCode::NOT_MODIFIED, [(header::ETAG, etag)]).into_response());
    }
    let body = json(&snap.into_snapshot(st.bus.epoch(), &kinds))?;
    let mut r = json_body(body);
    r.headers_mut().insert(
        header::ETAG,
        etag.parse().map_err(|_| ApiError::internal("etag"))?,
    );
    Ok(r)
}

pub(super) fn store(st: &AppState) -> Result<steve_store::Store, ApiError> {
    st.modules
        .as_ref()
        .map(|m| m.store.clone())
        .ok_or_else(unavailable)
}

#[derive(Deserialize)]
struct ByIdQuery {
    kind: String,
    ids: Option<String>,
    items: Option<String>,
}

/// `GET /api/views?kind=decision&ids=a,b`: the views (hot or not) of this workspace's entities.
async fn by_id(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Query(q): Query<ByIdQuery>,
) -> ApiResult {
    let k = kind(&q.kind)?;
    let routes = st.runtime_routes();
    if k == ViewKind::Session
        && q.ids.is_none()
        && let Some(items) = q.items
    {
        let items = parse_ids(&items)?;
        let (store, rev) = (store(&st)?, st.bus.rev());
        let views = super::blocking(move || {
            store
                .read(|r| r.session_views().of_items(&c.ws, &items))
                .map_err(|e| ApiError::internal(e.to_string()))
        })
        .await?;
        let views = views
            .into_iter()
            .map(|mut s| {
                routes.apply(&mut s);
                View::Session(s)
            })
            .collect();
        return Ok(json_body(json(&ById {
            rev,
            views,
            missing: Vec::new(),
        })?));
    }
    let ids = parse_ids(q.ids.as_deref().unwrap_or_default())?;
    let (store, rev) = (store(&st)?, st.bus.rev());
    let ws = c.ws.clone();
    let asked = ids.clone();
    let views = super::blocking(move || {
        crate::projection::read_by_id(&store, &routes, k, &ws, &asked)
            .map_err(|e| ApiError::internal(e.to_string()))
    })
    .await?;
    let found: std::collections::HashSet<String> =
        views.iter().map(|v| v.id().to_owned()).collect();
    let missing = ids.into_iter().filter(|id| !found.contains(id)).collect();
    Ok(json_body(json(&ById {
        rev,
        views,
        missing,
    })?))
}

fn parse_ids(input: &str) -> Result<Vec<String>, ApiError> {
    let ids: Vec<String> = input
        .split(',')
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();
    if ids.is_empty() || ids.len() > IDS_MAX {
        return Err(invalid("ids"));
    }
    Ok(ids)
}

#[derive(Deserialize)]
struct PageQuery {
    kind: String,
    /// `settled` (resolved or expired) for decisions.
    status: Option<String>,
    since: Option<i64>,
    before: Option<String>,
    limit: Option<u32>,
}

/// `GET /api/views/page?kind=decision&status=settled&since=<ms>&before=<cursor>&limit=`: settled
/// decisions, newest first.
async fn page(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Query(q): Query<PageQuery>,
) -> ApiResult {
    let kind = kind(&q.kind)?;
    let expected = match kind {
        ViewKind::Decision => Some("settled"),
        ViewKind::Item => Some("archived"),
        ViewKind::Session => Some("ended"),
        ViewKind::Message => None,
    };
    if q.status.as_deref() != expected {
        return Err(invalid("status"));
    }
    let before = match q.before.as_deref().filter(|b| !b.is_empty()) {
        Some(b) => Some(SettledCursor::parse(b).ok_or_else(|| invalid("before"))?),
        None => None,
    };
    let limit = q.limit.unwrap_or(PAGE_DEFAULT).clamp(1, PAGE_MAX);
    let (store, rev) = (store(&st)?, st.bus.rev());
    let ws = c.ws.clone();
    let routes = st.runtime_routes();
    let (mut views, next) = super::blocking(move || {
        store
            .read(|r| {
                Ok::<_, steve_store::StoreError>(match kind {
                    ViewKind::Decision => {
                        let (vs, next) = r.decision_views().settled(
                            &ws,
                            &DecisionPage {
                                since: q.since.unwrap_or(0),
                                before,
                                limit,
                            },
                        )?;
                        (
                            vs.into_iter().map(View::Decision).collect::<Vec<_>>(),
                            next.map(|c| c.to_string()),
                        )
                    }
                    ViewKind::Item => {
                        let (vs, next) = r.item_views().archived(&ws, &Page { before, limit })?;
                        (vs.into_iter().map(View::Item).collect(), next)
                    }
                    ViewKind::Message => {
                        let (vs, next) = r.message_views().history(&ws, &Page { before, limit })?;
                        (vs.into_iter().map(View::Message).collect(), next)
                    }
                    ViewKind::Session => {
                        let (vs, next) = r.session_views().ended(&ws, &Page { before, limit })?;
                        (vs.into_iter().map(View::Session).collect(), next)
                    }
                })
            })
            .map_err(|e| ApiError::internal(e.to_string()))
    })
    .await?;
    for v in &mut views {
        routes.apply_to(v);
    }
    Ok(json_body(json(&PageOf { rev, views, next })?))
}

#[derive(Deserialize)]
struct DismissDrafts {
    ids: Vec<String>,
}

async fn dismiss_drafts(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Json(mut input): Json<DismissDrafts>,
) -> ApiResult {
    if input.ids.len() > IDS_MAX {
        return Err(invalid("ids"));
    }
    input.ids.sort_unstable();
    input.ids.dedup();
    let m = st.modules.clone().ok_or_else(unavailable)?;
    let caller = steve_modules::Caller { ws: c.ws, me: c.me };
    let result = tokio::task::spawn_blocking(crate::metrics::carry(steve_i18n::carry(move || {
        steve_modules::decisions::dismiss_drafts(&m, &caller, &input.ids)
    })))
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(match result {
        Ok(ids) => Json(serde_json::json!({ "ids": ids })).into_response(),
        Err(e) => e.into_response(),
    })
}

#[derive(Deserialize)]
struct DraftEdit {
    previous: String,
    text: String,
}

async fn edit_draft(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    Json(input): Json<DraftEdit>,
) -> ApiResult {
    let store = store(&st)?;
    let result = super::blocking(move || {
        Ok(steve_channels::respond::edit_reply_draft(
            &store,
            &c.ws,
            &c.me,
            &id,
            &input.previous,
            &input.text,
        ))
    })
    .await?;
    Ok(match result {
        Ok(()) => Json(serde_json::json!({ "ok": true })).into_response(),
        Err(e) => e.into_response(),
    })
}
