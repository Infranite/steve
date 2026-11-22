//! Read views (`steve_types::view`): `GET /api/snapshot` (the hot views, from the projection's
//! memory), `GET /api/views` (by id) and `GET /api/views/page` (what is outside the hot set). What
//! changes afterwards arrives on `/api/stream` as `patch` frames; every answer carries the `rev` a
//! client merges by.

use axum::Extension;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde::{Deserialize, Serialize};
use steve_store::Views;
use steve_store::views::{DecisionPage, SettledCursor};
use steve_types::{ById, PageOf, Rev, ViewKind};

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
    let snap = p.snapshot(&c.ws).await.ok_or_else(unavailable)?;
    let names: Vec<&str> = kinds.iter().map(|k| k.as_str()).collect();
    let etag = format!(
        "\"{}:{}:{}\"",
        st.bus.epoch(),
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

fn store(st: &AppState) -> Result<steve_store::Store, ApiError> {
    st.modules
        .as_ref()
        .map(|m| m.store.clone())
        .ok_or_else(unavailable)
}

/// The revision a read made now is at least as of.
fn seen(st: &AppState) -> Result<u64, ApiError> {
    st.projection
        .as_ref()
        .map(|p| p.seen())
        .ok_or_else(unavailable)
}

#[derive(Deserialize)]
struct ByIdQuery {
    kind: String,
    ids: String,
}

/// `GET /api/views?kind=decision&ids=a,b`: the views (hot or not) of this workspace's entities.
async fn by_id(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Query(q): Query<ByIdQuery>,
) -> ApiResult {
    let k = kind(&q.kind)?;
    let ids: Vec<String> = q
        .ids
        .split(',')
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();
    if ids.is_empty() || ids.len() > IDS_MAX {
        return Err(invalid("ids"));
    }
    let (store, rev) = (store(&st)?, seen(&st)?);
    let ws = c.ws.clone();
    let asked = ids.clone();
    let views = super::blocking(move || {
        crate::projection::read_by_id(&store, k, &ws, &asked)
            .map_err(|e| ApiError::internal(e.to_string()))
    })
    .await?;
    let found: std::collections::HashSet<String> = views
        .iter()
        .map(|v| match v {
            steve_types::View::Decision(d) => d.id.to_string(),
        })
        .collect();
    let missing = ids.into_iter().filter(|id| !found.contains(id)).collect();
    Ok(json_body(json(&ById {
        rev: Rev(rev),
        views,
        missing,
    })?))
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
    match kind(&q.kind)? {
        ViewKind::Decision => {}
    }
    if q.status.as_deref() != Some("settled") {
        return Err(invalid("status"));
    }
    let before = match q.before.as_deref().filter(|b| !b.is_empty()) {
        Some(b) => Some(SettledCursor::parse(b).ok_or_else(|| invalid("before"))?),
        None => None,
    };
    let p = DecisionPage {
        since: q.since.unwrap_or(0),
        before,
        limit: q.limit.unwrap_or(PAGE_DEFAULT).clamp(1, PAGE_MAX),
    };
    let (store, rev) = (store(&st)?, seen(&st)?);
    let ws = c.ws.clone();
    let (views, next) = super::blocking(move || {
        store
            .read(|r| r.decision_views().settled(&ws, &p))
            .map_err(|e| ApiError::internal(e.to_string()))
    })
    .await?;
    Ok(json_body(json(&PageOf {
        rev: Rev(rev),
        views,
        next: next.map(|n| n.to_string()),
    })?))
}
