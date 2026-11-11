//! Static web app with SPA fallback (main.ts since the separate web release): `STEVE_WEB_DIR` is
//! `web/current`, a symlink a web release switches while the Hub runs, so every request resolves it
//! again (nothing is cached). Files are served like `@hono/node-server`'s `serveStatic` (Hono's MIME
//! table, `Last-Modified`, directories → `index.html`); `/` and `*.html` get `cache-control: no-store`,
//! `/assets/*` are immutable. Every other GET is `index.html` (no-store), except a missing `/assets/*`
//! file (404: a tab on a pruned build must not get HTML as a script) and no web installed (503).
//! `/release.json` is never served.

use std::path::{Component, Path, PathBuf};

use axum::body::Body;
use axum::extract::Request;
use axum::http::{Method, StatusCode, header};
use axum::response::{IntoResponse, Response};

use crate::config::Config;
use crate::error::ApiError;

/// Hono's `baseMimes` (hono/utils/mime).
fn mime_of(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    match ext.as_str() {
        "aac" => "audio/aac",
        "avi" => "video/x-msvideo",
        "avif" => "image/avif",
        "av1" => "video/av1",
        "bmp" => "image/bmp",
        "css" => "text/css; charset=utf-8",
        "csv" => "text/csv; charset=utf-8",
        "eot" => "application/vnd.ms-fontobject",
        "epub" => "application/epub+zip",
        "gif" => "image/gif",
        "gz" => "application/gzip",
        "htm" | "html" => "text/html; charset=utf-8",
        "ico" => "image/x-icon",
        "ics" => "text/calendar; charset=utf-8",
        "jpeg" | "jpg" => "image/jpeg",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" | "map" => "application/json",
        "jsonld" => "application/ld+json",
        "mid" | "midi" => "audio/x-midi",
        "mp3" => "audio/mpeg",
        "mp4" => "video/mp4",
        "mpeg" => "video/mpeg",
        "oga" => "audio/ogg",
        "ogv" => "video/ogg",
        "ogx" => "application/ogg",
        "opus" => "audio/opus",
        "otf" => "font/otf",
        "pdf" => "application/pdf",
        "png" => "image/png",
        "rtf" => "application/rtf",
        "svg" => "image/svg+xml; charset=utf-8",
        "tif" | "tiff" => "image/tiff",
        "ts" => "video/mp2t",
        "ttf" => "font/ttf",
        "txt" => "text/plain; charset=utf-8",
        "wasm" => "application/wasm",
        "webm" => "video/webm",
        "weba" => "audio/webm",
        "webmanifest" => "application/manifest+json",
        "webp" => "image/webp",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "xhtml" => "application/xhtml+xml; charset=utf-8",
        "xml" => "application/xml; charset=utf-8",
        "zip" => "application/zip",
        "3gp" => "video/3gpp",
        "3g2" => "video/3gpp2",
        "gltf" => "model/gltf+json",
        "glb" => "model/gltf-binary",
        _ => "application/octet-stream",
    }
}

/// The file under `root` a URL path names, or `None` for anything that would leave it.
fn resolve(root: &Path, url_path: &str) -> Option<PathBuf> {
    let decoded =
        crate::files::decode_uri_component(url_path).unwrap_or_else(|| url_path.to_string());
    let mut p = root.to_path_buf();
    for c in Path::new(decoded.trim_start_matches('/')).components() {
        match c {
            Component::Normal(s) => p.push(s),
            Component::CurDir => {}
            _ => return None,
        }
    }
    if p.is_dir() {
        p.push("index.html");
    }
    p.is_file().then_some(p)
}

/// RFC 7231 date, like `Date.toUTCString()`.
fn http_date(t: std::time::SystemTime) -> String {
    let secs = t
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let wd = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"][days.rem_euclid(7) as usize];
    // civil from days (Howard Hinnant)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    let mon = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ][(m - 1) as usize];
    format!(
        "{wd}, {d:02} {mon} {y} {:02}:{:02}:{:02} GMT",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

async fn file(path: &Path, head: bool) -> Option<Response> {
    let meta = tokio::fs::metadata(path).await.ok()?;
    let body = if head {
        Body::empty()
    } else {
        Body::from(tokio::fs::read(path).await.ok()?)
    };
    let mut res = (StatusCode::OK, body).into_response();
    let h = res.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static(mime_of(path)),
    );
    if let Ok(v) = meta.len().to_string().parse() {
        h.insert(header::CONTENT_LENGTH, v);
    }
    if let Some(v) = meta
        .modified()
        .ok()
        .map(http_date)
        .and_then(|d| d.parse().ok())
    {
        h.insert(header::LAST_MODIFIED, v);
    }
    Some(res)
}

/// `release.json` of the web build being served (`""` without one), read on every call: a web
/// release switches the link without restarting the Hub. `String(x.version ?? "")`.
pub fn web_version(cfg: &Config) -> String {
    let Ok(text) = std::fs::read_to_string(cfg.web_dir.join("release.json")) else {
        return String::new();
    };
    match serde_json::from_str::<serde_json::Value>(&text) {
        Ok(v) => match v.get("version") {
            None | Some(serde_json::Value::Null) => String::new(),
            Some(x) => crate::js::string(x),
        },
        Err(_) => String::new(),
    }
}

/// `c.text("not found", 404)`.
fn not_found() -> Response {
    crate::error::text_response(404, "not found")
}

/// The cache header main.ts adds to a `200` static file without one.
fn cache_for(url_path: &str) -> Option<&'static str> {
    if url_path == "/" || url_path.ends_with(".html") {
        Some("no-store")
    } else if url_path.starts_with("/assets/") {
        Some("public, max-age=31536000, immutable")
    } else {
        None
    }
}

/// Serves an unmatched request: a static file, else the SPA index for GET/HEAD, else 404.
/// What the Hub's own port answers for a page: the web client, unless it has a port of its own
/// ([`Config::web_port`]); then the Hub's port is the API only and says where the pages are.
pub async fn serve(cfg: &Config, req: Request) -> Response {
    if let Some(port) = cfg.web_port {
        if !matches!(*req.method(), Method::GET | Method::HEAD) {
            return ApiError::no_route().into_response();
        }
        if let Some(to) = page_redirect(port, &req) {
            return (StatusCode::FOUND, [(header::LOCATION, to)]).into_response();
        }
        return (
            StatusCode::NOT_FOUND,
            axum::Json(serde_json::json!({
                "error": format!("the web client is served on port {port}"),
                "web_port": port,
            })),
        )
            .into_response();
    }
    pages(cfg, req).await
}

/// Where a browser that asked the Hub's own port for a page is sent: the same host and path on the
/// web client's port. Someone who types the Hub's address, or follows a link to it from another
/// Hub's page, lands on its pages; the fragment of the address goes along by the browser's doing.
/// Only for a page navigation that reached the Hub directly: a script's call gets the JSON answer,
/// and behind a proxy the address the browser used is not this one.
fn page_redirect(web_port: u16, req: &Request) -> Option<String> {
    let h = req.headers();
    let wants_page = h
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|a| a.contains("text/html"));
    if !wants_page || h.contains_key("x-forwarded-proto") || h.contains_key("x-forwarded-host") {
        return None;
    }
    let host = host_of(h)?;
    let path = req
        .uri()
        .path_and_query()
        .map(|p| p.as_str())
        .unwrap_or("/");
    Some(format!("http://{host}:{web_port}{path}"))
}

/// The host a request was addressed to, without its port, as it goes into a URL (an IPv6 address
/// keeps its brackets).
fn host_of(headers: &axum::http::HeaderMap) -> Option<String> {
    headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .and_then(|h| reqwest::Url::parse(&format!("http://{h}")).ok())
        .and_then(|u| u.host_str().map(str::to_string))
}

/// The web client on its own port (A3): its pages, and `/hub.json`, the address of the Hub that
/// serves them as seen from the page (a suggestion the page shows when it asks which Hub to use).
pub fn web_only(cfg: std::sync::Arc<Config>) -> axum::Router {
    let c = cfg.clone();
    axum::Router::new()
        .route(
            "/hub.json",
            axum::routing::get(move |headers: axum::http::HeaderMap| {
                let port = c.listen.port;
                async move {
                    let host = host_of(&headers).unwrap_or_else(|| "127.0.0.1".into());
                    axum::Json(serde_json::json!({ "hub": format!("http://{host}:{port}") }))
                }
            }),
        )
        // sign-in belongs to the Hub: a provider's callback registered on this address goes on there
        .route(
            "/auth/{*rest}",
            axum::routing::get({
                let cfg = cfg.clone();
                move |headers: axum::http::HeaderMap, uri: axum::http::Uri| {
                    let port = cfg.listen.port;
                    async move {
                        let host = host_of(&headers).unwrap_or_else(|| "127.0.0.1".into());
                        let to = format!(
                            "http://{host}:{port}{}",
                            uri.path_and_query().map(|p| p.as_str()).unwrap_or("/")
                        );
                        axum::response::Redirect::temporary(&to)
                    }
                }
            }),
        )
        .fallback(move |req: Request| {
            let cfg = cfg.clone();
            async move { pages(&cfg, req).await }
        })
}

/// The web client's files, and its `index.html` for any other page path.
async fn pages(cfg: &Config, req: Request) -> Response {
    let get = matches!(*req.method(), Method::GET | Method::HEAD);
    if !get {
        return ApiError::no_route().into_response();
    }
    let path = req.uri().path();
    if path == "/release.json" {
        return not_found();
    }
    let head = *req.method() == Method::HEAD;
    if let Some(p) = resolve(&cfg.web_dir, path)
        && let Some(mut res) = file(&p, head).await
    {
        if let Some(c) = cache_for(path) {
            res.headers_mut()
                .insert(header::CACHE_CONTROL, header::HeaderValue::from_static(c));
        }
        return res;
    }
    if path.starts_with("/assets/") {
        return not_found();
    }
    index(cfg, head).await
}

async fn index(cfg: &Config, head: bool) -> Response {
    let Ok(html) = tokio::fs::read(cfg.web_dir.join("index.html")).await else {
        // before the first web release there is no index.html yet
        let mut r = crate::error::text_response(503, "Steve web is not installed");
        r.headers_mut().insert(
            header::CACHE_CONTROL,
            header::HeaderValue::from_static("no-store"),
        );
        return r;
    };
    let body = if head {
        Body::empty()
    } else {
        Body::from(html)
    };
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "text/html; charset=UTF-8"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        body,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mime_and_paths() {
        assert_eq!(
            mime_of(Path::new("a/app.JS")),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(mime_of(Path::new("x")), "application/octet-stream");
        let d = tempfile::tempdir().expect("tmp");
        std::fs::write(d.path().join("index.html"), "x").expect("w");
        assert_eq!(resolve(d.path(), "/"), Some(d.path().join("index.html")));
        assert_eq!(resolve(d.path(), "/../etc/passwd"), None);
        assert_eq!(resolve(d.path(), "/nope.js"), None);
    }

    #[test]
    fn a_page_asked_of_the_api_port_goes_to_the_web_port() {
        let req = |hs: &[(&str, &str)], uri: &str| {
            let mut b = axum::http::Request::builder().uri(uri);
            for (k, v) in hs {
                b = b.header(*k, *v);
            }
            b.body(axum::body::Body::empty()).unwrap()
        };
        let page = "text/html,application/xhtml+xml,*/*;q=0.8";
        assert_eq!(
            host_of(req(&[("host", "[fd00::2]:8441")], "/hub.json").headers()).as_deref(),
            Some("[fd00::2]"),
            "bracketed once: /hub.json and the sign-in redirect build their address from it"
        );
        assert_eq!(
            page_redirect(
                8441,
                &req(&[("host", "192.168.1.20:8440"), ("accept", page)], "/")
            )
            .as_deref(),
            Some("http://192.168.1.20:8441/")
        );
        assert_eq!(
            page_redirect(
                8441,
                &req(&[("host", "[fd00::2]:8440"), ("accept", page)], "/x?y=1")
            )
            .as_deref(),
            Some("http://[fd00::2]:8441/x?y=1"),
            "an IPv6 host keeps its brackets, the path and query go along"
        );
        assert_eq!(
            page_redirect(
                8441,
                &req(&[("host", "192.168.1.20:8440"), ("accept", "*/*")], "/")
            ),
            None,
            "a script's call is told, not moved"
        );
        assert_eq!(
            page_redirect(
                8441,
                &req(
                    &[
                        ("host", "steve.example"),
                        ("accept", page),
                        ("x-forwarded-proto", "https")
                    ],
                    "/"
                )
            ),
            None,
            "behind a proxy the browser's address is not this one"
        );
    }

    #[test]
    fn dates_like_js() {
        let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_793_458_800);
        assert_eq!(http_date(t), "Sat, 31 Oct 2026 15:00:00 GMT");
        assert_eq!(
            http_date(std::time::UNIX_EPOCH),
            "Thu, 01 Jan 1970 00:00:00 GMT"
        );
    }
}
