//! `/api/files` (attachments): upload as the raw body (name in `?name=` or `X-File-Name`, URL-encoded) or
//! multipart field `file`; the type comes from the bytes. Download: images inline, everything else as a
//! download, never sniffed or rendered as HTML.

use std::collections::HashMap;

use axum::Extension;
use axum::body::Body;
use axum::extract::{FromRequest, Multipart, Path, Query, Request, State};
use axum::http::{HeaderValue, header};
use axum::response::Response;
use serde_json::{Value, json};

use super::{ApiResult, AppState, Ctx, ok, own};
use crate::error::{ApiError, json_response};
use crate::files::{self as att, MAX_FILE, MAX_FILES};
use crate::validate::{Check, Str};

const BODY_LIMIT: usize = MAX_FILE + 64 * 1024;

/// `files: z.array(z.string().max(64)).max(10).optional()`.
pub(crate) fn file_ids(k: &mut Check) -> Option<Vec<String>> {
    k.opt_str_array_of("files", Str::max(64), None, Some(MAX_FILES))
}

/// Attachment ids → FileRefs of the current workspace; any other id is a 404 before anything happens.
pub(crate) async fn refs(
    st: &AppState,
    ws: &str,
    ids: Option<Vec<String>>,
) -> Result<Vec<Value>, ApiError> {
    let ws = ws.to_string();
    st.read(move |r| Ok(att::refs(r, &ws, ids.as_deref())?))
        .await
}

fn too_large() -> ApiError {
    ApiError::Json(413, format!("文件太大，最多 {} MB", MAX_FILE / 1024 / 1024))
}

pub async fn upload(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Query(q): Query<HashMap<String, String>>,
    req: Request,
) -> ApiResult {
    let declared = req
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok());
    if declared.is_some_and(|n| n > BODY_LIMIT) {
        // read (and drop) what the client sends so it gets the 413 instead of a reset connection
        let mut body = req.into_body().into_data_stream();
        use futures::StreamExt;
        let mut seen = 0usize;
        while let Some(Ok(chunk)) = body.next().await {
            seen += chunk.len();
            if seen > 4 * BODY_LIMIT {
                break;
            }
        }
        return Err(too_large());
    }
    let multipart = req
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|t| t.starts_with("multipart/form-data"));
    let raw_name = q.get("name").cloned().or_else(|| {
        req.headers()
            .get("x-file-name")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    });
    let (parts, body) = req.into_parts();
    let bytes = axum::body::to_bytes(body, BODY_LIMIT)
        .await
        .map_err(|_| too_large())?;
    let (buf, name) = if multipart {
        let req = Request::from_parts(parts, Body::from(bytes));
        // a malformed form is the client's: axum's own status for it (400, 413), not a 500
        let mut form = Multipart::from_request(req, &())
            .await
            .map_err(|e| ApiError::Json(e.status().as_u16(), e.body_text()))?;
        // parseBody keeps the last value of a repeated field
        let mut file = None;
        while let Some(field) = form
            .next_field()
            .await
            .map_err(|e| ApiError::Json(e.status().as_u16(), e.body_text()))?
        {
            if field.name() != Some("file") {
                continue;
            }
            let fname = field.file_name().map(str::to_string);
            let data = field
                .bytes()
                .await
                .map_err(|e| ApiError::Json(e.status().as_u16(), e.body_text()))?;
            file = fname.map(|n| (data, n));
        }
        let Some((data, n)) = file else {
            return Err(ApiError::bad_request("缺少文件（字段 file）"));
        };
        (data.to_vec(), Some(n))
    } else {
        let name = raw_name.map(|r| att::decode_uri_component(&r).unwrap_or(r));
        (bytes.to_vec(), name)
    };
    let data_dir = st.cfg.data_dir.clone();
    st.write(move |r| {
        let f = att::store(r, &data_dir, &c.ws, &buf, name.as_deref(), &c.me)?;
        ok(att::file_ref(&f))
    })
    .await
}

pub async fn download(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult {
    let f = st
        .read(move |r| Ok(own(r.file_get(&id)?, &c.ws, "附件")?))
        .await?;
    let path = att::blob_path(&st.cfg.data_dir, &f);
    let gone = || Ok(json_response(404, &json!({ "error": "附件的文件不见了" })));
    let Ok(file) = tokio::fs::File::open(&path).await else {
        return gone();
    };
    let Ok(len) = file.metadata().await.map(|m| m.len()) else {
        return gone();
    };
    let Ok(body) = super::outputs::stream_file(file, 0, len).await else {
        return gone();
    };
    let s = |k: &str| {
        f.get(k)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let (mime, name) = (s("mime"), s("name"));
    let inline = att::is_inline_image(&mime) && q.get("download").map(String::as_str) != Some("1");
    let mut res = Response::new(body);
    let h = res.headers_mut();
    let mut put = |k: header::HeaderName, v: String| {
        if let Ok(v) = HeaderValue::from_str(&v) {
            h.insert(k, v);
        }
    };
    put(header::CONTENT_TYPE, mime);
    put(header::CONTENT_LENGTH, len.to_string());
    put(header::X_CONTENT_TYPE_OPTIONS, "nosniff".into());
    put(header::CONTENT_DISPOSITION, att::disposition(&name, inline));
    put(
        header::CONTENT_SECURITY_POLICY,
        "default-src 'none'; sandbox".into(),
    );
    put(
        header::HeaderName::from_static("cross-origin-resource-policy"),
        "same-origin".into(),
    );
    put(
        header::CACHE_CONTROL,
        "private, max-age=31536000, immutable".into(),
    );
    Ok(res)
}
