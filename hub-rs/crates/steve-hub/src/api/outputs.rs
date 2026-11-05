//! `/api/items/{id}/artifacts/{art}/preview` and `/raw`: what an item's output holds, so the item page can
//! show it. A document, file or folder is read only inside the places the item worked in (its worktree or
//! scratch dir, its project, the temp dir); a branch or commit comes from the project's git. Nothing is written.

use std::collections::HashMap;
use std::path::{Component, Path as FsPath, PathBuf};

use axum::Extension;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::Response;
use serde_json::{Value, json};
use steve_core::git::{git, is_git};

use super::{ApiResult, AppState, Ctx, Noun, blocking, ok, own, str_of};
use crate::backend::BackendError;
use crate::config::home_dir;
use crate::error::{ApiError, json_response};
use crate::files::disposition;

/// Text shown inline; longer files are cut and say so.
const TEXT_MAX: usize = 512 * 1024;
/// A diff beyond this is cut.
const DIFF_MAX: usize = 400 * 1024;
/// Images, PDFs and media served as they are, up to this size.
const RAW_MAX: u64 = 64 * 1024 * 1024;
const LIST_MAX: usize = 500;

/// One output with what is needed to find it.
pub(crate) struct Out {
    pub kind: String,
    pub title: String,
    pub loc: Option<String>,
    pub cwd: Option<PathBuf>,
    pub project: Option<PathBuf>,
    pub branch: Option<String>,
    /// Where a file may be read from: the item's working places and the temp dir.
    pub roots: Vec<PathBuf>,
}

async fn load(st: &AppState, ws: &str, id: &str, art: &str) -> Result<Out, ApiError> {
    let (ws, id, art, cfg) = (
        ws.to_string(),
        id.to_string(),
        art.to_string(),
        st.cfg.clone(),
    );
    st.read(move |r| {
        let i = own(r.item_get(&id)?, &ws, Noun::Item)?;
        let a = r
            .artifacts_list(&id)?
            .into_iter()
            .find(|a| str_of(a, "id") == art)
            .ok_or_else(|| BackendError::not_found(steve_i18n::t!("hub.not_found.output")))?;
        let opt = |o: &crate::backend::Obj, k: &str| {
            o.get(k)
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        let project = match opt(&i, "project_id") {
            Some(p) => r
                .project_get(&p)?
                .filter(|p| p.get("workspace_id").and_then(Value::as_str) == Some(ws.as_str()))
                .and_then(|p| opt(&p, "path"))
                .map(|p| PathBuf::from(super::projects::expand(&p))),
            None => None,
        };
        let cwd = opt(&i, "cwd").map(PathBuf::from);
        let mut roots: Vec<PathBuf> = [cwd.clone(), project.clone()]
            .into_iter()
            .flatten()
            .collect();
        roots.push(cfg.worktrees.join(&id));
        roots.push(cfg.scratch.join(&id));
        roots.push(std::env::temp_dir());
        roots.push("/tmp".into());
        Ok(Out {
            kind: str_of(&a, "kind"),
            title: str_of(&a, "title"),
            loc: opt(&a, "location"),
            cwd,
            project,
            branch: opt(&i, "branch"),
            roots,
        })
    })
    .await
}

pub async fn preview(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path((id, art)): Path<(String, String)>,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult {
    let o = load(&st, &c.ws, &id, &art).await?;
    ok(blocking(move || Ok(describe(&o, q.get("path").map(String::as_str)))).await?)
}

pub async fn raw(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path((id, art)): Path<(String, String)>,
    Query(q): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> ApiResult {
    let o = load(&st, &c.ws, &id, &art).await?;
    let sub = q.get("path").cloned();
    let p = blocking(move || match locate(&o, sub.as_deref()) {
        Ok(Found::Disk(p, _)) if p.is_file() => Ok(p),
        Ok(_) => Err(ApiError::not_found(steve_i18n::t!("hub.output.not_a_file"))),
        Err(e) => Err(ApiError::not_found(e)),
    })
    .await?;
    let size = tokio::fs::metadata(&p).await.map(|m| m.len()).unwrap_or(0);
    if size > RAW_MAX {
        return Err(ApiError::Json(
            413,
            steve_i18n::t!("hub.output.too_large", size = human(size)),
        ));
    }
    let Ok(file) = tokio::fs::File::open(&p).await else {
        return Ok(json_response(
            404,
            &json!({ "error": steve_i18n::t!("hub.output.file_gone") }),
        ));
    };
    let name = p
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
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
    let Ok(body) = stream_file(file, start as u64, len as u64).await else {
        return Ok(json_response(
            404,
            &json!({ "error": steve_i18n::t!("hub.output.file_gone") }),
        ));
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
fn byte_range(v: &str, total: usize) -> Option<(usize, usize)> {
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

/// The preview of an output: a `type` and what that type needs.
pub(crate) fn describe(o: &Out, sub: Option<&str>) -> Value {
    if let Some(u) = o
        .loc
        .as_deref()
        .filter(|l| l.starts_with("http://") || l.starts_with("https://"))
    {
        return json!({ "type": "link", "url": u });
    }
    let k = o.kind.to_lowercase();
    if sub.is_none() {
        if k.contains("提交") || k == "commit" {
            return commit(o);
        }
        if k.contains("分支") || k == "branch" {
            return branch(o);
        }
    }
    match locate(o, sub) {
        Ok(Found::Disk(p, shown)) if p.is_dir() => dir(&p, &shown),
        Ok(Found::Disk(p, shown)) => file(&p, &shown),
        Ok(Found::Git(rev, rel, text)) => {
            let name = rel.rsplit('/').next().unwrap_or(&rel).to_string();
            let mut v = text_value(&name, text.as_bytes());
            v["path"] = json!(rel);
            v["from"] = json!(format!("git {rev}"));
            v
        }
        Err(e) => json!({ "type": "missing", "reason": e }),
    }
}

pub(crate) enum Found {
    /// The canonical path and how to show it.
    Disk(PathBuf, String),
    /// The file is gone from disk but git still has it: rev, path, content.
    Git(String, String, String),
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

/// Where the output is, and the entry `sub` below it when it is a folder.
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
    let Some(base) = candidates.iter().find(|p| p.exists()) else {
        // a document of a finished item: its worktree is gone, the branch or main still has it
        if !abs
            && sub.is_none()
            && plain(&loc).is_some()
            && let Some(g) = git_dir(o)
        {
            let revs = o.branch.iter().cloned().chain(["HEAD".to_string()]);
            for rev in revs {
                if let Ok(t) = git(&g, &["show", &format!("{rev}:{loc}")]) {
                    return Ok(Found::Git(rev, loc.clone(), t));
                }
            }
        }
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
            json!({ "type": form, "name": name, "mime": mime, "large": size > RAW_MAX })
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
fn text_value(name: &str, b: &[u8]) -> Value {
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

/// The git repository of the item: its project, or its own directory.
fn git_dir(o: &Out) -> Option<PathBuf> {
    [&o.project, &o.cwd]
        .into_iter()
        .flatten()
        .find(|d| d.exists() && is_git(d))
        .cloned()
}

/// A stat or diff with paths as they are (`docs/对话.md`, not `"docs/\345…"`); empty when git fails.
fn unquoted(g: &FsPath, args: &[&str]) -> String {
    let all: Vec<&str> = ["-c", "core.quotePath=false"]
        .iter()
        .chain(args)
        .copied()
        .collect();
    git(g, &all).unwrap_or_default()
}
fn cut_diff(d: String) -> (String, bool) {
    if d.len() <= DIFF_MAX {
        return (d, false);
    }
    let mut end = DIFF_MAX;
    while !d.is_char_boundary(end) {
        end -= 1;
    }
    (d[..end].to_string(), true)
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

fn commit(o: &Out) -> Value {
    let hash = [
        first_word(&o.title),
        o.loc.as_deref().map(first_word).unwrap_or_default(),
    ]
    .into_iter()
    .find(|h| is_hash(h));
    let Some(hash) = hash else {
        return json!({ "type": "missing", "reason": steve_i18n::t!("hub.output.no_commit_hash") });
    };
    let Some(g) = git_dir(o) else {
        return json!({ "type": "missing", "reason": steve_i18n::t!("hub.output.no_repo") });
    };
    let Ok(meta) = git(&g, &["show", "-s", "--format=%H%x1f%an%x1f%ct%x1f%B", hash]) else {
        return json!({ "type": "missing", "reason": steve_i18n::t!("hub.output.commit_gone", hash = hash) });
    };
    let f: Vec<&str> = meta.splitn(4, '\u{1f}').collect();
    let stat = unquoted(&g, &["show", "--format=", "--stat=120", hash]);
    let (diff, cut) = cut_diff(unquoted(
        &g,
        &["show", "--format=", "--patch", "--no-color", hash],
    ));
    json!({
        "type": "commit",
        "hash": f.first().copied().unwrap_or(hash),
        "author": f.get(1).copied().unwrap_or_default(),
        "at": f.get(2).and_then(|t| t.parse::<u64>().ok()).map(|t| t * 1000),
        "message": f.get(3).copied().unwrap_or_default().trim(),
        "stat": stat,
        "diff": diff,
        "truncated": cut,
    })
}

fn branch(o: &Out) -> Value {
    let Some(g) = git_dir(o) else {
        return json!({ "type": "missing", "reason": steve_i18n::t!("hub.output.no_repo") });
    };
    let has = |b: &str| {
        git(
            &g,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/heads/{b}"),
            ],
        )
        .is_ok()
    };
    let named = first_word(&o.title);
    let name = [Some(named), o.branch.as_deref()]
        .into_iter()
        .flatten()
        .find(|b| !b.is_empty() && !b.starts_with('-') && has(b));
    let Some(name) = name else {
        let shown = if named.is_empty() {
            o.branch.as_deref().unwrap_or_default()
        } else {
            named
        };
        return json!({ "type": "missing", "reason": steve_i18n::t!("hub.output.branch_gone", branch = shown) });
    };
    let base = ["main", "master"]
        .into_iter()
        .find(|b| *b != name && has(b));
    let range = base
        .map(|b| format!("{b}..{name}"))
        .unwrap_or_else(|| name.to_string());
    let log = git(
        &g,
        &[
            "log",
            "--no-color",
            "-n",
            "200",
            "--format=%h%x1f%s%x1f%an%x1f%ct",
            &range,
        ],
    )
    .unwrap_or_default();
    let commits: Vec<Value> = log
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| {
            let f: Vec<&str> = l.splitn(4, '\u{1f}').collect();
            json!({
                "hash": f.first().copied().unwrap_or_default(),
                "subject": f.get(1).copied().unwrap_or_default(),
                "author": f.get(2).copied().unwrap_or_default(),
                "at": f.get(3).and_then(|t| t.parse::<u64>().ok()).map(|t| t * 1000),
            })
        })
        .collect();
    let (stat, diff) = match base {
        Some(b) => {
            let span = format!("{b}...{name}");
            (
                unquoted(&g, &["diff", "--stat=120", &span]),
                unquoted(&g, &["diff", "--no-color", &span]),
            )
        }
        None => (String::new(), String::new()),
    };
    let (diff, cut) = cut_diff(diff);
    json!({
        "type": "branch",
        "branch": name,
        "base": base,
        "worktree": o.loc.as_deref().filter(|l| FsPath::new(l).exists()).map(|l| home_tilde(FsPath::new(l))),
        "commits": commits,
        "stat": stat,
        "diff": diff,
        "truncated": cut,
    })
}

fn human(n: u64) -> String {
    if n >= 1024 * 1024 {
        format!("{:.1} MB", n as f64 / 1024.0 / 1024.0)
    } else {
        format!("{} KB", n.div_ceil(1024))
    }
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

    fn g(dir: &FsPath, args: &[&str]) -> String {
        git(dir, args).unwrap_or_else(|e| panic!("git {args:?}: {e}"))
    }
    /// A repo on main with one commit.
    fn repo(root: &FsPath) -> PathBuf {
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

    fn out(
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
        // nothing above the output, even through its own folder
        assert_eq!(describe(&d, Some("../说明.md"))["type"], "missing");
        assert_eq!(describe(&d, Some("/etc/hosts"))["type"], "missing");
        assert!(matches!(locate(&d, Some("a.png")), Ok(Found::Disk(p, _)) if p.ends_with("a.png")));
        // Word, Excel and PowerPoint files go to the page as they are, to be rendered there
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
        // a link out of the worktree is followed to where it really is
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
    fn branches_commits_and_documents_from_git() {
        let t = tempfile::tempdir().unwrap();
        let r = repo(t.path());
        g(&r, &["checkout", "-q", "-b", "steve/st-1"]);
        std::fs::create_dir_all(r.join("docs")).unwrap();
        std::fs::write(r.join("docs/方案.md"), "# 方案\n").unwrap();
        g(&r, &["add", "."]);
        g(&r, &["commit", "-q", "-m", "加方案"]);
        let hash = g(&r, &["rev-parse", "--short", "HEAD"]);
        g(&r, &["checkout", "-q", "main"]);
        let gone = t.path().join("gone-worktree");

        let mut b = out(
            "分支",
            "steve/st-1（已 rebase 到 0.2.19）",
            Some(&gone.to_string_lossy()),
            &gone,
            Some(&r),
        );
        let v = describe(&b, None);
        assert_eq!(v["type"], "branch", "{v}");
        assert_eq!(v["base"], "main");
        assert_eq!(v["commits"][0]["subject"], "加方案");
        assert!(v["diff"].as_str().unwrap().contains("+# 方案"));
        assert_eq!(v["worktree"], Value::Null);

        let v = describe(
            &out("提交", &format!("{hash} 加方案"), None, &gone, Some(&r)),
            None,
        );
        assert_eq!(v["type"], "commit", "{v}");
        assert_eq!(v["message"], "加方案");
        assert!(v["stat"].as_str().unwrap().contains("docs/方案.md"));
        assert!(
            v["diff"]
                .as_str()
                .unwrap()
                .contains("diff --git a/docs/方案.md")
        );
        assert_eq!(
            describe(&out("提交", "1234567 不存在", None, &gone, Some(&r)), None)["type"],
            "missing"
        );

        // the worktree is gone and main does not have it: the branch still does
        b.kind = "文档".into();
        b.loc = Some("docs/方案.md".into());
        b.branch = Some("steve/st-1".into());
        let v = describe(&b, None);
        assert_eq!(v["type"], "text", "{v}");
        assert_eq!(v["text"], "# 方案");
        assert_eq!(v["from"], "git steve/st-1");

        b.kind = "分支".into();
        b.title = "steve/st-9".into();
        b.branch = None;
        assert_eq!(describe(&b, None)["type"], "missing");
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
