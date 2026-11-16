//! User-only management; public reads carry a separately authorized, expiring share token.
use std::collections::HashMap;

use axum::{
    Extension, Router,
    extract::{Path, Query, Request, State},
    http::{HeaderMap, HeaderValue, header},
    response::Response,
    routing::get,
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{ApiResult, AppState, Ctx, blocking, ok};
use crate::{
    deliveries::{Delivery, MAX_FILE, Publisher, Service},
    error::ApiError,
};

/// How a served resource may behave in the browser.
enum Policy {
    /// The default: no scripts, forms, frames or connections.
    Static,
    /// An interactive frame: scripts in an opaque-origin sandbox, resources only below this prefix.
    Frame(String),
}

fn service(st: &AppState) -> Result<Service, ApiError> {
    st.deliveries
        .clone()
        .ok_or_else(|| ApiError::Json(503, "delivery storage unavailable".into()))
}

fn missing() -> ApiError {
    ApiError::not_found("delivery not found")
}
fn db(e: impl std::fmt::Display) -> ApiError {
    ApiError::internal(e.to_string())
}

async fn owned(st: &AppState, c: &Ctx, id: String) -> Result<(Service, Delivery), ApiError> {
    let svc = service(st)?;
    let (s, ws) = (svc.clone(), c.ws.clone());
    let d = blocking(move || s.get(&id, Some(&ws)).map_err(db)?.ok_or_else(missing)).await?;
    Ok((svc, d))
}

async fn shared(st: &AppState, token: String) -> Result<(Service, Delivery), ApiError> {
    let svc = service(st)?;
    let s = svc.clone();
    let d = blocking(move || s.shared(&token).map_err(db)?.ok_or_else(missing)).await?;
    Ok((svc, d))
}

fn no_store(mut r: Response) -> Response {
    let h = r.headers_mut();
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    r
}

/// Signed-in metadata: whether this user manages it, and a frame of their own when interactive.
fn signed_metadata(svc: &Service, d: &Delivery, me: &str) -> Result<Value, ApiError> {
    let mut v = svc.metadata(d, None);
    v["can_manage"] = json!(svc.can_manage(d, me).map_err(db)?);
    v["frame_url"] = json!(svc.frame_url(d, me));
    Ok(v)
}

pub(super) async fn metadata(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
) -> ApiResult {
    let (svc, d) = owned(&st, &c, id).await?;
    let v = blocking(move || signed_metadata(&svc, &d, &c.me)).await?;
    Ok(no_store(ok(v)?))
}

/// The deliveries this user manages in the current workspace ("我的发布"), newest first.
pub(super) async fn list(State(st): State<AppState>, Extension(c): Extension<Ctx>) -> ApiResult {
    let svc = service(&st)?;
    let v = blocking(move || {
        let rows = svc.owned(&c.ws, &c.me).map_err(db)?;
        rows.iter()
            .map(|d| signed_metadata(&svc, d, &c.me))
            .collect::<Result<Vec<_>, _>>()
    })
    .await?;
    Ok(no_store(ok(json!(v))?))
}

fn truthy(v: Option<&String>) -> bool {
    v.is_some_and(|v| matches!(v.as_str(), "1" | "true" | "yes"))
}

/// A signed-in user's own upload (`steve publish`): the same limits and immutable versions as a
/// Bot's delivery, with no item. A browser session or a user access client; Bot session, MCP and
/// Node tokens never establish a user context here.
pub(super) async fn upload(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Query(q): Query<HashMap<String, String>>,
    req: Request,
) -> ApiResult {
    user_action(&c, req.headers())?;
    let svc = service(&st)?;
    let (bytes, name) = super::files::read_upload(&q, req, MAX_FILE, || {
        ApiError::Json(413, "delivery exceeds 64 MiB".into())
    })
    .await?;
    let name = name
        .filter(|n| !n.trim().is_empty())
        .ok_or_else(|| ApiError::Json(400, "a file name (?name=) is required".into()))?;
    let title = q.get("title").cloned().unwrap_or_default();
    let entrypoint = q.get("entrypoint").filter(|e| !e.is_empty()).cloned();
    let interactive = truthy(q.get("interactive"));
    let v = blocking(move || {
        let d = svc
            .publish(
                Publisher::User {
                    id: &c.me,
                    workspace: &c.ws,
                },
                &title,
                &name,
                &bytes,
                entrypoint.as_deref(),
                interactive,
            )
            .map_err(|e| ApiError::Json(400, e))?;
        signed_metadata(&svc, &d, &c.me)
    })
    .await?;
    Ok(no_store(ok(v)?))
}

#[derive(Deserialize)]
pub(super) struct Patch {
    interactive: bool,
}
/// Turn a delivery's interactive preview on or off: only a user who manages it.
pub(super) async fn patch(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<Patch>,
) -> ApiResult {
    user_action(&c, &headers)?;
    let (svc, d) = owned(&st, &c, id).await?;
    let v = blocking(move || {
        if !svc.can_manage(&d, &c.me).map_err(db)? {
            return Err(ApiError::Json(
                403,
                "only the user who manages this delivery can change its preview".into(),
            ));
        }
        svc.set_interactive(&d, body.interactive).map_err(db)?;
        let d = svc
            .get(&d.id, Some(&d.workspace_id))
            .map_err(db)?
            .ok_or_else(missing)?;
        signed_metadata(&svc, &d, &c.me)
    })
    .await?;
    Ok(no_store(ok(v)?))
}
async fn public_metadata(State(st): State<AppState>, Path(token): Path<String>) -> ApiResult {
    let (svc, d) = shared(&st, token.clone()).await?;
    Ok(no_store(ok(svc.metadata(&d, Some(&token)))?))
}

pub(super) async fn raw(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> ApiResult {
    let (svc, d) = owned(&st, &c, id).await?;
    bytes(svc, d, None, headers, Policy::Static).await
}
pub(super) async fn content(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path((id, path)): Path<(String, String)>,
    headers: HeaderMap,
) -> ApiResult {
    let (svc, d) = owned(&st, &c, id).await?;
    bytes(svc, d, Some(path), headers, Policy::Static).await
}
async fn public_raw(
    State(st): State<AppState>,
    Path(token): Path<String>,
    headers: HeaderMap,
) -> ApiResult {
    let (svc, d) = shared(&st, token).await?;
    bytes(svc, d, None, headers, Policy::Static).await
}
async fn public_content(
    State(st): State<AppState>,
    Path((token, path)): Path<(String, String)>,
    headers: HeaderMap,
) -> ApiResult {
    let (svc, d) = shared(&st, token).await?;
    bytes(svc, d, Some(path), headers, Policy::Static).await
}

/// A share's interactive frame: the live share AND the delivery's switch, on every request.
async fn public_frame(
    State(st): State<AppState>,
    Path((token, path)): Path<(String, String)>,
    headers: HeaderMap,
) -> ApiResult {
    framed_only(&headers)?;
    let svc = service(&st)?;
    let (s, t) = (svc.clone(), token.clone());
    let d = blocking(move || s.shared_frame(&t).map_err(db)?.ok_or_else(missing)).await?;
    let prefix = format!("{}/api/delivery-shares/{token}/frame/", svc.base);
    bytes(svc, d, Some(path), headers, Policy::Frame(prefix)).await
}

/// A signed-in page's interactive frame. Authorized by its grant alone: the opaque-origin frame
/// sends no usable Steve cookie, and none is read here.
async fn grant_frame(
    State(st): State<AppState>,
    Path((grant, path)): Path<(String, String)>,
    headers: HeaderMap,
) -> ApiResult {
    framed_only(&headers)?;
    let svc = service(&st)?;
    let (s, g) = (svc.clone(), grant.clone());
    let d = blocking(move || s.framed(&g).map_err(db)?.ok_or_else(missing)).await?;
    let prefix = format!("{}/api/delivery-frames/{grant}/", svc.base);
    bytes(svc, d, Some(path), headers, Policy::Frame(prefix)).await
}

/// An interactive frame renders only inside a delivery page, whose own policy keeps the frame on
/// this installation. Opened as a page of its own, its script could navigate that page anywhere
/// (no CSP directive governs a document's own navigation), so a top-level load is refused.
fn framed_only(headers: &HeaderMap) -> Result<(), ApiError> {
    if headers.get("sec-fetch-dest").and_then(|v| v.to_str().ok()) == Some("document") {
        return Err(ApiError::Json(
            403,
            "an interactive preview opens only inside its delivery page".into(),
        ));
    }
    Ok(())
}

async fn bytes(
    svc: Service,
    d: Delivery,
    path: Option<String>,
    headers: HeaderMap,
    policy: Policy,
) -> ApiResult {
    let csp = match &policy {
        Policy::Frame(prefix) => Some(svc.frame_policy(prefix)),
        Policy::Static => None,
    };
    let inline = path.is_some();
    let (file, size, mime, name) =
        blocking(move || svc.open(&d, path.as_deref()).map_err(|_| missing())).await?;
    let range = headers
        .get(header::RANGE)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| super::outputs::byte_range(h, size as usize));
    if headers.contains_key(header::RANGE) && range.is_none() {
        let mut r = Response::new(axum::body::Body::empty());
        *r.status_mut() = axum::http::StatusCode::RANGE_NOT_SATISFIABLE;
        r.headers_mut().insert(
            header::CONTENT_RANGE,
            HeaderValue::from_str(&format!("bytes */{size}")).map_err(db)?,
        );
        return Ok(no_store(r));
    }
    let (start, len) = range
        .map(|(a, b)| (a as u64, (b - a + 1) as u64))
        .unwrap_or((0, size));
    let body = super::outputs::stream_file(tokio::fs::File::from_std(file), start, len)
        .await
        .map_err(db)?;
    let mut r = Response::new(body);
    if let Some((a, b)) = range {
        *r.status_mut() = axum::http::StatusCode::PARTIAL_CONTENT;
        r.headers_mut().insert(
            header::CONTENT_RANGE,
            HeaderValue::from_str(&format!("bytes {a}-{b}/{size}")).map_err(db)?,
        );
    }
    let h = r.headers_mut();
    // Text is UTF-8 (Markdown, code, logs): say so, or a browser may guess another encoding.
    let textual =
        mime.starts_with("text/") || mime == "application/json" || mime == "image/svg+xml";
    let typed = if textual && !mime.contains("charset") {
        format!("{mime}; charset=utf-8")
    } else {
        mime.clone()
    };
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&typed).map_err(db)?,
    );
    h.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&crate::files::disposition(&name, inline)).map_err(db)?,
    );
    h.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&len.to_string()).map_err(db)?,
    );
    h.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    // Response-level sandbox survives direct navigation and all package page jumps. No active
    // code, forms, workers, frames, plugins or network connections. allow-same-origin retains
    // login cookies on relative assets; scripts remain unconditionally disabled by BOTH CSP
    // and sandbox. The client must not add allow-scripts to its iframe either.
    if let Some(csp) = csp {
        h.insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_str(&csp).map_err(db)?,
        );
    } else {
        h.insert(header::CONTENT_SECURITY_POLICY,HeaderValue::from_static(if mime=="application/pdf" {
        // The browser's native PDF viewer needs its own document context, as in outputs::raw.
        "default-src 'none'; script-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'"
    } else {
        "sandbox allow-same-origin; default-src 'none'; script-src 'none'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self' data:; media-src 'self'; connect-src 'none'; object-src 'none'; frame-src 'none'; worker-src 'none'; base-uri 'none'; form-action 'none'"
    }));
    }
    h.insert(
        header::HeaderName::from_static("cross-origin-resource-policy"),
        // Steve's supported web client can live on a different origin/port from the Hub.
        // Read authorization still happens before serving any bytes, and CSP disables active
        // content. CORP must not prevent that client's image and PDF previews.
        HeaderValue::from_static("cross-origin"),
    );
    Ok(no_store(r))
}

/// Independent user action with an explicit workspace header. `require_user` accepts browser
/// sessions and user access clients; session-scoped Bot/MCP/Node tokens never establish a Ctx.
fn user_action(c: &Ctx, headers: &HeaderMap) -> Result<(), ApiError> {
    if headers.get("x-steve-ws").and_then(|v| v.to_str().ok()) != Some(c.ws.as_str())
        || headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) == Some("null")
    {
        return Err(ApiError::Json(
            403,
            "sharing requires an independent user login and explicit workspace".into(),
        ));
    }
    Ok(())
}

#[derive(Deserialize)]
pub(super) struct CreateShare {
    expires_at: i64,
}
pub(super) async fn create_share(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<CreateShare>,
) -> ApiResult {
    user_action(&c, &headers)?;
    let (svc, d) = owned(&st, &c, id).await?;
    let share = blocking(move || {
        svc.create_share(&d, &c.me, body.expires_at)
            .map_err(|e| ApiError::Json(400, e))
    })
    .await?;
    Ok(no_store(ok(
        json!({"token":share.token,"url":share.url,"expires_at":share.expires_at}),
    )?))
}
pub(super) async fn shares(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> ApiResult {
    user_action(&c, &headers)?;
    let (svc, d) = owned(&st, &c, id).await?;
    Ok(no_store(ok(json!(
        blocking(move || svc.shares(&d).map_err(db)).await?
    ))?))
}
pub(super) async fn revoke(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path((id, token)): Path<(String, String)>,
    headers: HeaderMap,
) -> ApiResult {
    user_action(&c, &headers)?;
    let (svc, d) = owned(&st, &c, id).await?;
    if !blocking(move || svc.revoke(&d, &token).map_err(db)).await? {
        return Err(missing());
    }
    Ok(no_store(ok(json!({"ok":true}))?))
}

pub(super) fn public_router(st: AppState) -> Router {
    Router::new()
        .route("/delivery-shares/{token}", get(public_metadata))
        .route("/delivery-shares/{token}/raw", get(public_raw))
        .route(
            "/delivery-shares/{token}/content/{*path}",
            get(public_content),
        )
        .route("/delivery-shares/{token}/frame/{*path}", get(public_frame))
        .route("/delivery-frames/{grant}/{*path}", get(grant_frame))
        .with_state(st)
}
