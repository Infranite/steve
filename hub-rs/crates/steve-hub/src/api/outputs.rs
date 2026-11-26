//! `/api/items/{id}/artifacts/{art}/preview` and `/raw`: what an item's output holds, so the item
//! page can show it. Hub-owned durable deliverables (`<home>/workspaces/<id>/outputs/`) are served
//! directly from the Hub; every project/worktree file, folder, branch, and commit is read strictly
//! through the Node that holds the workspace via `steve-node-api`.

use std::collections::HashMap;
use std::path::{Component, Path as FsPath, PathBuf};

use axum::Extension;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::Response;
use serde_json::{Value, json};

use super::projects::offline;
use super::{ApiResult, AppState, Ctx, Noun, blocking, ok, own, str_of};
use crate::config::home_dir;
use crate::error::BackendError;
use crate::error::{ApiError, json_response};
use crate::files::disposition;

mod node;

/// Text shown inline; longer files are cut and say so.
const TEXT_MAX: usize = 512 * 1024;
/// A diff beyond this is cut.
const DIFF_MAX: usize = 400 * 1024;
/// Images and Office files the page renders itself (it reads them whole): above this size the preview
/// says the file is large and offers the download. `/raw` serves every size, video and audio by range.
const RAW_MAX: u64 = 64 * 1024 * 1024;

/// Is a file of `form` and `size` too large for the page to render itself (images, Office)?
pub(crate) fn large(form: &str, size: u64) -> bool {
    matches!(form, "image" | "office") && size > RAW_MAX
}
const LIST_MAX: usize = 500;

/// One output with what is needed to find it.
#[derive(Clone)]
pub(crate) struct Out {
    pub kind: String,
    pub title: String,
    pub loc: Option<String>,
    pub cwd: Option<PathBuf>,
    pub project: Option<PathBuf>,
    pub branch: Option<String>,
    /// Allowed roots for this output (Hub-owned durable outputs directory when served from Hub, or
    /// the item's places when resolved on its Node).
    pub roots: Vec<PathBuf>,
}

/// The output, and where its Node reads it ([`node::view`]). Returns `None` ONLY for Hub-owned
/// durable deliverables (`<home>/workspaces/<id>/outputs/`).
async fn load(
    st: &AppState,
    ws: &str,
    id: &str,
    art: &str,
) -> Result<(Out, Option<node::View>), ApiError> {
    let (ws, item, art, cfg) = (
        ws.to_string(),
        id.to_string(),
        art.to_string(),
        st.cfg.clone(),
    );
    let id = item.clone();
    let (mut o, project) = st
        .read(move |r| {
            let i = own(r.item_get(&id)?, &ws, Noun::Item)?;
            let a = r
                .artifacts_list(&id)?
                .into_iter()
                .find(|a| str_of(a, "id") == art)
                .ok_or_else(|| BackendError::not_found(steve_i18n::t!("hub.not_found.output")))?;
            let opt = |o: &crate::wire::store::Obj, k: &str| {
                o.get(k)
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            };
            let row = match opt(&i, "project_id") {
                Some(p) => r
                    .project_get(&p)?
                    .filter(|p| p.get("workspace_id").and_then(Value::as_str) == Some(ws.as_str())),
                None => None,
            };
            let project_id = row.as_ref().and_then(|p| opt(p, "id"));
            let project = row
                .and_then(|p| opt(&p, "path"))
                .map(|p| PathBuf::from(super::projects::expand(&p)));
            let cwd = opt(&i, "cwd").map(PathBuf::from);
            let mut roots: Vec<PathBuf> = [cwd.clone(), project.clone()]
                .into_iter()
                .flatten()
                .collect();
            roots.push(cfg.worktrees.join(&id));
            roots.push(cfg.scratch.join(&id));
            roots.push(cfg.home.join("workspaces").join(&id).join("outputs"));
            let o = Out {
                kind: str_of(&a, "kind"),
                title: str_of(&a, "title"),
                loc: opt(&a, "location"),
                cwd,
                project,
                branch: opt(&i, "branch"),
                roots,
            };
            Ok((o, project_id))
        })
        .await?;
    // Durable deliverables belong to the Hub, even when the item ran on a remote Node that is
    // now offline. `locate` still canonicalizes and checks the Hub-owned durable outputs dir only.
    let durable = st.cfg.home.join("workspaces").join(&item).join("outputs");
    let v = if o.loc.as_ref().is_some_and(|loc| {
        let p = FsPath::new(loc);
        p.is_absolute() && crate::config::resolve(p).starts_with(&durable)
    }) {
        o.roots = vec![durable];
        None
    } else if o
        .loc
        .as_deref()
        .is_some_and(|l| l.starts_with("http://") || l.starts_with("https://"))
    {
        // External URL link previews need neither disk nor Node access.
        None
    } else {
        let ws = st.workspaces.clone().ok_or_else(offline)?;
        Some(node::view(ws, item, project, &o).await?)
    };
    Ok((o, v))
}

pub async fn preview(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path((id, art)): Path<(String, String)>,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult {
    let (o, v) = load(&st, &c.ws, &id, &art).await?;
    let sub = q.get("path").cloned();
    match v {
        None => ok(blocking(move || Ok(describe(&o, sub.as_deref()))).await?),
        Some(v) => ok(node::describe(o, v, sub).await?),
    }
}

/// Where the bytes of a raw output come from.
enum Src {
    Disk(PathBuf),
    Node(node::Addr, node::Entry),
}

pub async fn raw(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path((id, art)): Path<(String, String)>,
    Query(q): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> ApiResult {
    let (o, v) = load(&st, &c.ws, &id, &art).await?;
    let sub = q.get("path").cloned();
    let src = raw_source(o, v, sub).await?;
    let (size, name) = match &src {
        Src::Disk(p) => (
            tokio::fs::metadata(p).await.map(|m| m.len()).unwrap_or(0),
            p.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
        ),
        Src::Node(a, e) => (e.size(), a.name()),
    };
    let file = match &src {
        Src::Disk(p) => match tokio::fs::File::open(p).await {
            Ok(f) => Some(f),
            Err(_) => {
                return Ok(json_response(
                    404,
                    &json!({ "error": steve_i18n::t!("hub.output.file_gone") }),
                ));
            }
        },
        Src::Node(..) => None,
    };
    let (form, mime) = media(&name);
    let inline = form.is_some() && q.get("download").map(String::as_str) != Some("1");
    let total = size as usize;
    // one byte range, so Safari can play a video
    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| byte_range(v, total));
    let (status, (start, len), span) = match range {
        Some((a, b)) => (206, (a, b - a + 1), Some((a, b))),
        None => (200, (0, total), None),
    };
    // only the requested span is read, as it is sent
    let body = match (src, file) {
        (Src::Node(a, e), _) => node::stream(a, &e, start as u64, len as u64),
        (Src::Disk(_), Some(file)) => match stream_file(file, start as u64, len as u64).await {
            Ok(body) => body,
            Err(_) => {
                return Ok(json_response(
                    404,
                    &json!({ "error": steve_i18n::t!("hub.output.file_gone") }),
                ));
            }
        },
        (Src::Disk(_), None) => unreachable!("opened above"),
    };
    let mut res = Response::new(body);
    *res.status_mut() = axum::http::StatusCode::from_u16(status).unwrap_or_default();
    let h = res.headers_mut();
    let mut put = |k: header::HeaderName, v: String| {
        if let Ok(v) = HeaderValue::from_str(&v) {
            h.insert(k, v);
        }
    };
    put(
        header::CONTENT_TYPE,
        if inline {
            mime
        } else {
            "application/octet-stream"
        }
        .into(),
    );
    put(header::X_CONTENT_TYPE_OPTIONS, "nosniff".into());
    put(header::CONTENT_DISPOSITION, disposition(&name, inline));
    put(header::ACCEPT_RANGES, "bytes".into());
    put(header::CONTENT_LENGTH, len.to_string());
    if let Some((a, b)) = span {
        put(header::CONTENT_RANGE, format!("bytes {a}-{b}/{total}"));
    }
    // the browser's PDF viewer does not run in a sandboxed document; everything else gets no script at all
    if form != Some("pdf") {
        put(header::CONTENT_SECURITY_POLICY, "default-src 'none'; img-src 'self' data:; media-src 'self'; style-src 'unsafe-inline'; sandbox".into());
    }
    put(
        header::HeaderName::from_static("cross-origin-resource-policy"),
        "same-origin".into(),
    );
    put(header::CACHE_CONTROL, "private, no-cache".into());
    Ok(res)
}

/// Resolve one downloadable file, preserving Node errors and rejecting directory previews.
async fn raw_source(o: Out, v: Option<node::View>, sub: Option<String>) -> Result<Src, ApiError> {
    fn not_a_file() -> ApiError {
        ApiError::not_found(steve_i18n::t!("hub.output.not_a_file"))
    }
    fn on_disk(f: Result<Found, String>) -> Result<PathBuf, ApiError> {
        match f {
            Ok(Found::Disk(p, _)) if p.is_file() => Ok(p),
            Ok(_) => Err(not_a_file()),
            Err(e) => Err(ApiError::not_found(e)),
        }
    }
    Ok(match &v {
        None => Src::Disk(blocking(move || on_disk(locate(&o, sub.as_deref()))).await?),
        Some(v) => match node::locate(&o, v, sub.as_deref()).await {
            Ok(node::Located::Node(a, e)) if !e.is_dir() => Src::Node(a, e),
            Ok(_) => return Err(not_a_file()),
            Err(node::Fail::Missing(r)) => return Err(ApiError::not_found(r)),
            Err(node::Fail::Api(e)) => return Err(e),
        },
    })
}

/// `len` bytes of `file` from `start`, streamed from disk instead of read into memory first.
pub(super) async fn stream_file(
    mut file: tokio::fs::File,
    start: u64,
    len: u64,
) -> std::io::Result<Body> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};
    if start > 0 {
        file.seek(std::io::SeekFrom::Start(start)).await?;
    }
    Ok(Body::from_stream(tokio_util::io::ReaderStream::new(
        file.take(len),
    )))
}

/// `bytes=a-b`, `bytes=a-` or `bytes=-n` within `total`.
pub(super) fn byte_range(v: &str, total: usize) -> Option<(usize, usize)> {
    let (a, b) = v.strip_prefix("bytes=")?.split_once('-')?;
    if total == 0 || a.contains(',') || b.contains(',') {
        return None;
    }
    let (a, b) = match (a.trim(), b.trim()) {
        ("", n) => (total.saturating_sub(n.parse().ok()?), total - 1),
        (a, "") => (a.parse().ok()?, total - 1),
        (a, b) => (a.parse().ok()?, b.parse::<usize>().ok()?.min(total - 1)),
    };
    (a <= b && b < total).then_some((a, b))
}

/// The preview of a Hub-owned durable deliverable (or link): a `type` and what that type needs.
pub(crate) fn describe(o: &Out, sub: Option<&str>) -> Value {
    if let Some(u) = o
        .loc
        .as_deref()
        .filter(|l| l.starts_with("http://") || l.starts_with("https://"))
    {
        return json!({ "type": "link", "url": u });
    }
    match locate(o, sub) {
        Ok(Found::Disk(p, shown)) if p.is_dir() => dir(&p, &shown),
        Ok(Found::Disk(p, shown)) => file(&p, &shown),
        Err(e) => json!({ "type": "missing", "reason": e }),
    }
}

/// A document git still has at `rev` (returned by the Node's git face).
pub(crate) fn from_git(rev: &str, rel: &str, text: &str) -> Value {
    let name = rel.rsplit('/').next().unwrap_or(rel).to_string();
    let mut v = text_value(&name, text.as_bytes());
    v["path"] = json!(rel);
    v["from"] = json!(format!("git {rev}"));
    v
}

pub(crate) enum Found {
    /// The canonical path and how to show it (Hub-owned durable deliverable only).
    Disk(PathBuf, String),
}

/// Only plain names below the output: no `..`, no absolute path.
fn plain(sub: &str) -> Option<PathBuf> {
    let p = PathBuf::from(sub);
    p.components()
        .all(|c| matches!(c, Component::Normal(_)))
        .then_some(p)
}

fn inside(p: &FsPath, roots: &[PathBuf]) -> bool {
    roots
        .iter()
        .filter_map(|r| r.canonicalize().ok())
        .any(|r| p.starts_with(r))
}

/// Where a Hub-owned durable deliverable is, and the entry `sub` below it when it is a folder.
pub(crate) fn locate(o: &Out, sub: Option<&str>) -> Result<Found, String> {
    let Some(loc) = o.loc.as_deref().map(str::trim).filter(|l| !l.is_empty()) else {
        return Err(steve_i18n::t!("hub.output.no_location"));
    };
    let loc = super::projects::expand(loc);
    let abs = FsPath::new(&loc).is_absolute();
    let candidates: Vec<PathBuf> = if abs {
        vec![PathBuf::from(&loc)]
    } else {
        [&o.cwd, &o.project]
            .into_iter()
            .flatten()
            .map(|d| d.join(&loc))
            .collect()
    };
    let Some(base) = candidates.iter().find(|p| p.canonicalize().is_ok()) else {
        return Err(steve_i18n::t!("hub.output.cleaned_up", loc = loc));
    };
    let base = base.canonicalize().map_err(|e| e.to_string())?;
    if !inside(&base, &o.roots) {
        return Err(steve_i18n::t!("hub.output.outside_roots"));
    }
    let shown = home_tilde(&base);
    match sub.filter(|s| !s.is_empty()) {
        None => Ok(Found::Disk(base, shown)),
        Some(s) => {
            let rel = plain(s).ok_or_else(|| steve_i18n::t!("hub.output.bad_path"))?;
            let p = base
                .join(&rel)
                .canonicalize()
                .map_err(|_| steve_i18n::t!("hub.output.path_missing", path = s))?;
            if !p.starts_with(&base) {
                return Err(steve_i18n::t!("hub.output.bad_path"));
            }
            Ok(Found::Disk(
                p,
                format!("{}/{}", shown.trim_end_matches('/'), s),
            ))
        }
    }
}

fn home_tilde(p: &FsPath) -> String {
    let s = p.to_string_lossy().into_owned();
    let home = home_dir().to_string_lossy().into_owned();
    match s.strip_prefix(&home) {
        Some(rest) if home != "/" && (rest.is_empty() || rest.starts_with('/')) => {
            format!("~{rest}")
        }
        _ => s,
    }
}

fn modified(m: &std::fs::Metadata) -> Value {
    m.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| json!(d.as_millis() as u64))
        .unwrap_or(Value::Null)
}

fn dir(p: &FsPath, shown: &str) -> Value {
    let mut ents: Vec<(bool, String, u64, Value)> = std::fs::read_dir(p)
        .map(|r| {
            r.filter_map(|e| e.ok())
                .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
                .filter_map(|e| {
                    let m = e.metadata().ok()?;
                    Some((
                        !m.is_dir(),
                        e.file_name().to_string_lossy().into_owned(),
                        m.len(),
                        modified(&m),
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    ents.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
    let more = ents.len() > LIST_MAX;
    let entries: Vec<Value> = ents
        .into_iter()
        .take(LIST_MAX)
        .map(|(file, name, size, m)| {
            let form = if file { media(&name).0 } else { None };
            json!({ "name": name, "dir": !file, "size": if file { json!(size) } else { Value::Null }, "modified": m, "form": form })
        })
        .collect();
    json!({ "type": "dir", "path": shown, "entries": entries, "truncated": more })
}

/// `(form, mime)` by extension: the forms shown as themselves; anything else is tried as text.
pub(crate) fn media(name: &str) -> (Option<&'static str>, &'static str) {
    let ext = name
        .rsplit_once('.')
        .map(|(_, e)| e.to_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "png" => (Some("image"), "image/png"),
        "jpg" | "jpeg" => (Some("image"), "image/jpeg"),
        "gif" => (Some("image"), "image/gif"),
        "webp" => (Some("image"), "image/webp"),
        "avif" => (Some("image"), "image/avif"),
        "bmp" => (Some("image"), "image/bmp"),
        "ico" => (Some("image"), "image/x-icon"),
        "svg" => (Some("image"), "image/svg+xml"),
        "pdf" => (Some("pdf"), "application/pdf"),
        "mp4" | "m4v" => (Some("video"), "video/mp4"),
        "webm" => (Some("video"), "video/webm"),
        "mov" => (Some("video"), "video/quicktime"),
        "mp3" => (Some("audio"), "audio/mpeg"),
        "wav" => (Some("audio"), "audio/wav"),
        "m4a" => (Some("audio"), "audio/mp4"),
        "ogg" | "oga" => (Some("audio"), "audio/ogg"),
        "flac" => (Some("audio"), "audio/flac"),
        // Office Open XML: the page unzips and renders them itself
        "docx" => (
            Some("office"),
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        ),
        "xlsx" => (
            Some("office"),
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        ),
        "pptx" => (
            Some("office"),
            "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        ),
        _ => (None, "application/octet-stream"),
    }
}

/// The code language for a file name, as the web highlighter knows it.
fn lang(name: &str) -> &'static str {
    let lower = name.to_lowercase();
    if matches!(lower.as_str(), "dockerfile" | "makefile") {
        return if lower == "dockerfile" {
            "dockerfile"
        } else {
            "makefile"
        };
    }
    let ext = lower.rsplit_once('.').map(|(_, e)| e).unwrap_or_default();
    match ext {
        "md" | "markdown" | "mdx" => "markdown",
        "html" | "htm" => "html",
        "csv" => "csv",
        "tsv" => "tsv",
        "json" | "jsonl" | "ndjson" => "json",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "tsx",
        "js" | "mjs" | "cjs" => "javascript",
        "jsx" => "jsx",
        "py" => "python",
        "rs" => "rust",
        "go" => "go",
        "java" => "java",
        "kt" => "kotlin",
        "swift" => "swift",
        "c" | "h" => "c",
        "cc" | "cpp" | "hpp" => "cpp",
        "rb" => "ruby",
        "sh" | "bash" | "zsh" => "bash",
        "css" => "css",
        "scss" => "scss",
        "sql" => "sql",
        "yaml" | "yml" => "yaml",
        "toml" => "toml",
        "xml" | "plist" => "xml",
        "diff" | "patch" => "diff",
        "mermaid" | "mmd" => "mermaid",
        "php" => "php",
        "lua" => "lua",
        "r" => "r",
        "scala" => "scala",
        "dart" => "dart",
        "vue" => "vue",
        "svelte" => "svelte",
        "graphql" | "gql" => "graphql",
        "proto" => "proto",
        "ini" | "cfg" | "conf" => "ini",
        "tex" => "latex",
        _ => "text",
    }
}

fn file(p: &FsPath, shown: &str) -> Value {
    let name = p
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let m = std::fs::metadata(p).ok();
    let size = m.as_ref().map(|m| m.len()).unwrap_or(0);
    let mut v = match media(&name) {
        (Some(form), mime) => {
            json!({ "type": form, "name": name, "mime": mime, "large": large(form, size) })
        }
        (None, _) => {
            let mut buf = Vec::with_capacity(TEXT_MAX.min(size as usize) + 1);
            if let Ok(f) = std::fs::File::open(p) {
                use std::io::Read;
                let _ = f.take(TEXT_MAX as u64 + 1).read_to_end(&mut buf);
            }
            text_value(&name, &buf)
        }
    };
    v["path"] = json!(shown);
    v["size"] = json!(size);
    v["modified"] = m.as_ref().map(modified).unwrap_or(Value::Null);
    v
}

/// Text when it reads as text (cut to `TEXT_MAX` at a character), else `binary`.
pub(crate) fn text_value(name: &str, b: &[u8]) -> Value {
    let cut = b.len() > TEXT_MAX;
    let b = &b[..b.len().min(TEXT_MAX)];
    if b[..b.len().min(8000)].contains(&0) {
        return json!({ "type": "binary", "name": name });
    }
    let s = match std::str::from_utf8(b) {
        Ok(s) => s,
        // the cut may split a character; anything else wrong is not text
        Err(e) if cut && b.len() - e.valid_up_to() < 4 => {
            std::str::from_utf8(&b[..e.valid_up_to()]).unwrap_or_default()
        }
        Err(_) => return json!({ "type": "binary", "name": name }),
    };
    json!({ "type": "text", "name": name, "lang": lang(name), "text": s, "truncated": cut })
}

fn is_hash(s: &str) -> bool {
    (7..=40).contains(&s.len()) && s.bytes().all(|c| c.is_ascii_hexdigit())
}

/// The first word of a title: `3aed210 对话页…` → `3aed210`, `steve/st-25（已 rebase）` → `steve/st-25`.
fn first_word(s: &str) -> &str {
    s.trim()
        .split(|c: char| c.is_whitespace() || c == '（' || c == '(')
        .next()
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stream_file_sends_only_the_span() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("v.bin");
        std::fs::write(&p, b"0123456789").unwrap();
        let read = |a, n| {
            let p = p.clone();
            async move {
                let f = tokio::fs::File::open(&p).await.unwrap();
                let b = stream_file(f, a, n).await.unwrap();
                axum::body::to_bytes(b, usize::MAX).await.unwrap()
            }
        };
        assert_eq!(&read(0, 10).await[..], b"0123456789");
        assert_eq!(&read(3, 4).await[..], b"3456");
        assert_eq!(&read(9, 1).await[..], b"9");
    }

    pub(super) fn g(dir: &FsPath, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("git");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// A repo on main with one commit.
    pub(super) fn repo(root: &FsPath) -> PathBuf {
        let r = root.join("repo");
        std::fs::create_dir_all(&r).unwrap();
        g(&r, &["init", "-q", "-b", "main"]);
        g(&r, &["config", "user.email", "t@t"]);
        g(&r, &["config", "user.name", "t"]);
        g(&r, &["config", "commit.gpgsign", "false"]);
        std::fs::write(r.join("a.txt"), "a\n").unwrap();
        g(&r, &["add", "."]);
        g(&r, &["commit", "-q", "-m", "a"]);
        r
    }

    pub(super) fn out(
        kind: &str,
        title: &str,
        loc: Option<&str>,
        cwd: &FsPath,
        project: Option<&FsPath>,
    ) -> Out {
        Out {
            kind: kind.into(),
            title: title.into(),
            loc: loc.map(str::to_string),
            cwd: Some(cwd.to_path_buf()),
            project: project.map(FsPath::to_path_buf),
            branch: None,
            roots: [Some(cwd), project]
                .into_iter()
                .flatten()
                .map(FsPath::to_path_buf)
                .collect(),
        }
    }

    #[test]
    fn documents_folders_and_media() {
        let t = tempfile::tempdir().unwrap();
        let w = t.path().join("wt");
        std::fs::create_dir_all(w.join("docs/shots")).unwrap();
        std::fs::write(w.join("docs/说明.md"), "# 标题\n\n正文").unwrap();
        std::fs::write(w.join("docs/shots/a.png"), [0x89, b'P', b'N', b'G', 0, 0]).unwrap();
        std::fs::write(w.join("docs/shots/b.bin"), [1, 0, 2]).unwrap();
        std::fs::write(w.join("docs/shots/.hidden"), "x").unwrap();

        let v = describe(&out("文档", "说明", Some("docs/说明.md"), &w, None), None);
        assert_eq!(v["type"], "text");
        assert_eq!(v["lang"], "markdown");
        assert_eq!(v["text"], "# 标题\n\n正文");

        let d = out(
            "文件",
            "截图",
            Some(&w.join("docs/shots/").to_string_lossy()),
            &w,
            None,
        );
        let v = describe(&d, None);
        assert_eq!(v["type"], "dir", "{v}");
        let names: Vec<&str> = v["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["a.png", "b.bin"]);
        assert_eq!(v["entries"][0]["form"], "image");
        assert_eq!(describe(&d, Some("a.png"))["type"], "image");
        assert_eq!(describe(&d, Some("b.bin"))["type"], "binary");
        assert_eq!(describe(&d, Some("../说明.md"))["type"], "missing");
        assert_eq!(describe(&d, Some("/etc/hosts"))["type"], "missing");
        assert!(matches!(locate(&d, Some("a.png")), Ok(Found::Disk(p, _)) if p.ends_with("a.png")));
        std::fs::write(w.join("docs/报告.docx"), [b'P', b'K', 3, 4, 0]).unwrap();
        let v = describe(&out("文档", "报告", Some("docs/报告.docx"), &w, None), None);
        assert_eq!(
            (v["type"].as_str(), v["name"].as_str()),
            (Some("office"), Some("报告.docx")),
            "{v}"
        );
        assert_eq!(media("a.XLSX").0, Some("office"));
        assert_eq!(media("a.pptx").0, Some("office"));
        assert_eq!(media("a.doc").0, None);
    }

    #[test]
    fn only_inside_the_items_places() {
        let t = tempfile::tempdir().unwrap();
        let (w, elsewhere) = (t.path().join("wt"), t.path().join("secret"));
        std::fs::create_dir_all(&w).unwrap();
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::write(elsewhere.join("key"), "s").unwrap();
        let o = out(
            "文件",
            "x",
            Some(&elsewhere.join("key").to_string_lossy()),
            &w,
            None,
        );
        let v = describe(&o, None);
        assert_eq!(v["type"], "missing");
        assert!(v["reason"].as_str().unwrap().contains("不在"), "{v}");
        std::os::unix::fs::symlink(&elsewhere, w.join("link")).unwrap();
        assert_eq!(
            describe(&out("文件", "x", Some("link/key"), &w, None), None)["type"],
            "missing"
        );
        assert_eq!(
            describe(&out("文件", "x", Some("nope.md"), &w, None), None)["type"],
            "missing"
        );
        assert_eq!(
            describe(
                &out("链接", "x", Some("https://a.example/x"), &w, None),
                None
            )["url"],
            "https://a.example/x"
        );
    }

    #[test]
    fn long_text_is_cut_at_a_character() {
        let s = "中".repeat(TEXT_MAX / 3 + 10);
        let v = text_value("a.txt", s.as_bytes());
        assert_eq!(v["type"], "text");
        assert_eq!(v["truncated"], true);
        assert!(v["text"].as_str().unwrap().len() <= TEXT_MAX);
        assert_eq!(text_value("a.txt", &[0xff, 0xfe, 0x41])["type"], "binary");
    }

    #[test]
    fn ranges() {
        assert_eq!(byte_range("bytes=0-1", 10), Some((0, 1)));
        assert_eq!(byte_range("bytes=5-", 10), Some((5, 9)));
        assert_eq!(byte_range("bytes=-3", 10), Some((7, 9)));
        assert_eq!(byte_range("bytes=8-100", 10), Some((8, 9)));
        assert_eq!(byte_range("bytes=11-12", 10), None);
        assert_eq!(byte_range("bytes=0-1,3-4", 10), None);
    }
}

/// Routes mounted behind the shared API login and body-limit layers.
pub(crate) fn routes() -> axum::Router<super::AppState> {
    use super::*;
    Router::new()
        .route("/items/{id}/artifacts/{art}/preview", get(outputs::preview))
        .route("/items/{id}/artifacts/{art}/raw", get(outputs::raw))
}
