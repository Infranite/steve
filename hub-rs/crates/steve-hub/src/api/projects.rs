//! `/api/projects*` and `/api/fs/dirs`.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;

use axum::Extension;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use serde_json::{Value, json};

use super::misc::FAST;
use super::{ApiResult, AppState, Ctx, Noun, blocking, body, ok, own, own_bot};
use crate::backend::{NewProject, Obj};
use crate::config::home_dir;
use crate::error::ApiError;
use crate::js;
use crate::validate::{Check, Num, Str};
use crate::wire::workspaces::{At, PlacementError, Subject};
use steve_node_api as na;

/// `path.normalize`: folds `.`/`..` and repeated slashes, keeps a trailing slash.
pub fn normalize(p: &str) -> String {
    if p.is_empty() {
        return ".".into();
    }
    let abs = p.starts_with('/');
    let trailing = p.ends_with('/');
    let mut parts: Vec<&str> = vec![];
    for seg in p.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                if parts.last().is_some_and(|l| *l != "..") {
                    parts.pop();
                } else if !abs {
                    parts.push("..");
                }
            }
            s => parts.push(s),
        }
    }
    let mut out = parts.join("/");
    if abs {
        out.insert(0, '/');
    }
    if out.is_empty() {
        return if abs { "/".into() } else { ".".into() };
    }
    if trailing && !out.ends_with('/') {
        out.push('/');
    }
    out
}

/// `path.join(a, b)`.
pub fn join(a: &str, b: &str) -> String {
    let joined = [a, b]
        .iter()
        .filter(|s| !s.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join("/");
    normalize(&joined)
}

/// `~/x` → `$HOME/x`; anything else unchanged.
pub fn expand(p: &str) -> String {
    match p.strip_prefix("~/") {
        Some(rest) => join(&home_dir().to_string_lossy(), rest),
        None => p.to_string(),
    }
}

/// Node's fs error text, e.g. `ENOTDIR: not a directory, scandir '/x'`.
fn fs_error(e: &std::io::Error, syscall: &str, path: &str) -> ApiError {
    let (code, what) = match e.raw_os_error() {
        Some(20) => ("ENOTDIR", "not a directory"),
        Some(13) => ("EACCES", "permission denied"),
        Some(2) => ("ENOENT", "no such file or directory"),
        Some(21) => ("EISDIR", "illegal operation on a directory"),
        Some(1) => ("EPERM", "operation not permitted"),
        _ => return ApiError::internal(format!("{e}, {syscall} '{path}'")),
    };
    ApiError::internal(format!("{code}: {what}, {syscall} '{path}'"))
}

fn is_dir(p: &str) -> bool {
    FsPath::new(p).is_dir()
}

fn exists(p: &str) -> bool {
    FsPath::new(p).exists()
}

/* ── reading a directory of the Hub's own device ──
With the local Node on (`STEVE_NODE_MODE=local`) a directory under one of its roots is read through
its files face, and `dirs` browses only toward those roots (D11: no "anywhere under ~"). A directory
under none of them is one being made a project, so a new root: no Node allows it yet, and the Hub
checks it on its own disk (the machine is the same; the root follows once the project exists).
Without the local Node the Hub reads its own disk as always. */

/// Who reads a directory of the Hub's own device.
enum Via {
    /// The Hub itself: the local Node is off.
    Hub,
    /// The local Node, at this place under one of its roots.
    Node(Arc<dyn na::Node>, na::RootPath),
    /// Under none of the local Node's roots, which are these paths: a new root being chosen.
    Outside(Vec<PathBuf>),
}

async fn via(st: &AppState, path: &str) -> Result<Via, ApiError> {
    let Some(ws) = st.workspaces.clone() else {
        return Ok(Via::Hub);
    };
    let path = PathBuf::from(normalize(path));
    blocking(move || match ws.hub_path(&path) {
        Ok(p) => match p.at {
            At::Root(at) => Ok(Via::Node(p.node, at)),
            At::Workspace(_) => Err(ApiError::internal("a directory placed in a workspace")),
        },
        Err(PlacementError::LocalOff) => Ok(Via::Hub),
        Err(PlacementError::Outside) => Ok(Via::Outside(
            ws.local_roots()
                .map_err(placement_error)?
                .into_iter()
                .map(|r| r.path)
                .collect(),
        )),
        Err(e) => Err(placement_error(e)),
    })
    .await
}

/// `设备离线` is a 503: the device may come back, the request was fine.
pub(super) fn offline() -> ApiError {
    ApiError::Json(503, steve_i18n::t!("hub.workspaces.offline"))
}

pub(super) fn placement_error(e: PlacementError) -> ApiError {
    match e {
        PlacementError::Offline { .. } => offline(),
        e => ApiError::internal(e.to_string()),
    }
}

/// How a Node read went wrong, for the caller to answer as it always has.
#[derive(Debug, PartialEq)]
enum Miss {
    /// The Node has no files face yet: the Hub reads its own disk (the local Node only).
    Unsupported,
    /// Not there, not a directory, or refused (outside its roots, through a link).
    Gone,
    Fail(ApiError),
}

fn miss(e: na::ApiError) -> Miss {
    match e.code {
        na::ErrorCode::Unsupported => Miss::Unsupported,
        na::ErrorCode::Invalid | na::ErrorCode::Denied => Miss::Gone,
        na::ErrorCode::Offline => Miss::Fail(offline()),
        _ => Miss::Fail(ApiError::internal(e.safe_detail)),
    }
}

/// Entries listed of one directory at most: as many as a person could choose among.
const LIST_MAX: usize = 10_000;

/// The names right below `at`, sorted, dotfiles included, with whether each is a directory (a
/// linked directory is not one: the Node does not follow it).
async fn node_list(node: &dyn na::Node, at: &na::RootPath) -> Result<Vec<(String, bool)>, Miss> {
    let mut out = vec![];
    let mut cursor = None;
    loop {
        let page = node
            .list_root(
                at.clone(),
                na::PageRequest {
                    cursor,
                    limit: 1000,
                },
            )
            .await
            .map_err(miss)?;
        for f in page.items {
            let name = f.path.as_str().rsplit('/').next().unwrap_or_default();
            out.push((name.to_string(), f.is_dir));
        }
        cursor = page.next;
        if cursor.is_none() || out.len() >= LIST_MAX {
            break;
        }
    }
    out.sort();
    Ok(out)
}

fn has_git(names: &[(String, bool)]) -> bool {
    names.iter().any(|(n, _)| n == ".git")
}

fn below(at: &na::RootPath, name: &str) -> Option<na::RootPath> {
    let rel = match at.rel.as_str() {
        "" => name.to_string(),
        base => format!("{base}/{name}"),
    };
    Some(na::RootPath {
        root: at.root.clone(),
        rel: na::RelPath::new(rel).ok()?,
    })
}

/// The first `max` bytes of a file (or all of a shorter one); `None` when it is not there.
async fn node_head(
    node: &dyn na::Node,
    at: na::RootPath,
    max: usize,
) -> Result<Option<Vec<u8>>, Miss> {
    let mut out: Vec<u8> = vec![];
    loop {
        let r = node
            .read_file(na::FileRead {
                path: na::FileAddress::Root(at.clone()),
                expected: None,
                offset: out.len() as u64,
                max: (max - out.len()) as u32,
            })
            .await;
        let chunk = match r {
            Ok(c) => c,
            Err(e) => {
                return match miss(e) {
                    Miss::Gone => Ok(None),
                    m => Err(m),
                };
            }
        };
        let none = chunk.bytes.is_empty();
        out.extend(chunk.bytes);
        if chunk.eof || none || out.len() >= max {
            return Ok(Some(out));
        }
    }
}

/// The readme files `describe` shows, in order.
const README: [&str; 4] = ["README.md", "readme.md", "AGENTS.md", "package.json"];
/// The start of each readme the model sees, in UTF-16 units ([`js::slice`]).
const README_CHARS: usize = 2500;

/// [`describe_input`] through the Node, and whether the directory is a git checkout. A readme is
/// read only as far as its first [`README_CHARS`] need (four bytes each at most).
async fn node_describe(
    node: &dyn na::Node,
    at: &na::RootPath,
) -> Result<(String, String, bool), Miss> {
    let names = node_list(node, at).await?;
    let list = names
        .iter()
        .map(|(n, _)| n.as_str())
        .filter(|f| !f.starts_with('.'))
        .take(60)
        .collect::<Vec<_>>()
        .join(", ");
    let mut readme = vec![];
    for f in README {
        let Some(file) = below(at, f) else { continue };
        // asked by name, as the Hub opens it: a case-insensitive disk finds `readme.md` too
        if let Some(text) = node_head(node, file, README_CHARS * 4).await? {
            readme.push(format!(
                "{f}:\n{}",
                js::slice(&String::from_utf8_lossy(&text), README_CHARS)
            ));
        }
    }
    Ok((list, readme.join("\n\n"), has_git(&names)))
}

/// What `dirs` lists of `base` on the Node: its directories, not hidden, the first 200.
async fn node_dirs(
    node: &dyn na::Node,
    at: &na::RootPath,
    base: &str,
) -> Result<Vec<String>, Miss> {
    let home = home_dir().to_string_lossy().into_owned();
    Ok(node_list(node, at)
        .await?
        .into_iter()
        .filter(|(n, d)| *d && !n.starts_with('.'))
        .take(200)
        .map(|(n, _)| join(base, &n).replacen(&home, "~", 1))
        .collect())
}

/// What `dirs` lists of a `base` no root holds: the way down to the roots below it, one directory
/// at a time (only configured roots are browsed, D11).
fn toward_roots(roots: &[PathBuf], base: &str) -> Vec<String> {
    let home = home_dir().to_string_lossy().into_owned();
    let from = PathBuf::from(normalize(base));
    let next: BTreeSet<String> = roots
        .iter()
        .filter_map(|r| r.strip_prefix(&from).ok()?.components().next())
        .filter_map(|c| c.as_os_str().to_str().map(String::from))
        .filter(|n| !n.starts_with('.'))
        .collect();
    next.into_iter()
        .take(200)
        .map(|n| join(base, &n).replacen(&home, "~", 1))
        .collect()
}

/// Whether `path` is a directory, and a git checkout, as the Hub's disk says.
async fn hub_dir_and_git(path: &str) -> Result<(bool, bool), ApiError> {
    let p = path.to_string();
    blocking(move || Ok((exists(&p) && is_dir(&p), exists(&join(&p, ".git"))))).await
}

pub async fn create(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    b: Bytes,
) -> ApiResult {
    let v = body(&b)?;
    let mut k = Check::object(&v);
    let name = k.str("name", Str::len(1, 40));
    let raw = k.str("path", Str::min(1));
    let description = k.opt_str("description", Str::max(1000)).unwrap_or_default();
    let worktree = k.opt_bool("worktree");
    let default_bot = match k.opt_nullable_str("default_bot", Str::ANY) {
        Some(Some(b)) => Some(b),
        _ => None,
    };
    k.finish()?;
    if let Some(bot) = default_bot.clone().filter(|b| !b.is_empty()) {
        let ws = c.ws.clone();
        st.read(move |r| Ok(own_bot(r, &bot, &ws)?)).await?;
    }
    let path = expand(&raw);
    let (dir_ok, git) = match via(&st, &path).await? {
        Via::Hub | Via::Outside(_) => hub_dir_and_git(&path).await?,
        Via::Node(node, at) => match node_list(&*node, &at).await {
            Ok(names) => (true, has_git(&names)),
            Err(Miss::Gone) => (false, false),
            Err(Miss::Unsupported) => hub_dir_and_git(&path).await?,
            Err(Miss::Fail(e)) => return Err(e),
        },
    };
    if !dir_ok {
        return Err(ApiError::bad_request(steve_i18n::t!(
            "hub.project.dir_missing",
            path = raw
        )));
    }
    st.write(move |r| {
        if r.project_by_path(&c.ws, &path)?.is_some() {
            return Err(ApiError::bad_request(steve_i18n::t!(
                "hub.project.already_project"
            )));
        }
        let x = r.project_create(
            &c.ws,
            &NewProject {
                name,
                description,
                device_id: "hub".into(),
                path: path.clone(),
                kind: if git { "git" } else { "dir" }.into(),
                worktree: git && worktree.unwrap_or(true),
                default_bot,
                release: None,
            },
        )?;
        let id = x
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        r.audit(
            &c.me,
            "project.create",
            &id,
            &json!({ "path": path }),
            &c.ws,
        )?;
        ok(Value::Object(x))
    })
    .await
}

pub async fn patch_one(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    b: Bytes,
) -> ApiResult {
    let (i, ws) = (id.clone(), c.ws.clone());
    st.read(move |r| Ok(own(r.project_get(&i)?, &ws, Noun::Project)?))
        .await?;
    let v = body(&b)?;
    let mut k = Check::object(&v);
    let mut p = Obj::new();
    if let Some(x) = k.opt_str("name", Str::len(1, 40)) {
        p.insert("name".into(), json!(x));
    }
    if let Some(x) = k.opt_str("description", Str::max(1000)) {
        p.insert("description".into(), json!(x));
    }
    if let Some(x) = k.opt_bool("worktree") {
        p.insert("worktree".into(), json!(x));
    }
    if let Some(x) = k.opt_nullable_str("default_bot", Str::ANY) {
        p.insert("default_bot".into(), json!(x));
    }
    if let Some(x) = k.opt_bool("archived") {
        p.insert("archived".into(), json!(x));
    }
    // what counts as finished for an accepted result (`merge` | `tag` | none: a release by the
    // project's own command). Only this part of the release settings: the command stays as set up
    let done_when = k.opt_nullable_str("done_when", Str::ANY);
    k.finish()?;
    if let Some(Some(v)) = &done_when
        && ![steve_store::DONE_MERGE, steve_store::DONE_TAG].contains(&v.as_str())
    {
        return Err(ApiError::Invalid(vec!["done_when".into()]));
    }
    st.write(move |r| {
        if let Some(Value::String(bot)) = p.get("default_bot")
            && !bot.is_empty()
        {
            own_bot(r, bot, &c.ws)?;
        }
        if let Some(dw) = done_when {
            let mut release: steve_store::Release = r
                .project_get(&id)?
                .and_then(|o| o.get("release").cloned())
                .filter(|v| !v.is_null())
                .and_then(|v| serde_json::from_value(v).ok())
                .ok_or_else(|| ApiError::bad_request(steve_i18n::t!("hub.project.no_release")))?;
            release.done_when = dw;
            p.insert("release".into(), json!(release));
        }
        ok(Value::Object(r.project_patch(&id, &p, &c.me)?))
    })
    .await
}

/// The project `id` of the caller's workspace whose merge lane is asked about: its directory on this
/// machine and its release setting. Refused for a project on another device and for a non-git one.
async fn lane_project(
    st: &AppState,
    c: &Ctx,
    id: String,
) -> Result<(String, Option<steve_store::Release>), ApiError> {
    let ws = c.ws.clone();
    let i = id.clone();
    let p = st
        .read(move |r| Ok(own(r.project_get(&i)?, &ws, Noun::Project)?))
        .await?;
    // a project on another device is never read off the Hub's disk: its `path` is a place on that
    // device. Only the local Node's own device is read here (the same machine); a remote Node's
    // lane waits for the profile to be read through its git face (E5b)
    if let Some(ws) = st.workspaces.clone()
        && p.get("device_id").and_then(Value::as_str) != Some(crate::wire::workspaces::HUB_DEVICE)
    {
        let placed = blocking(move || {
            Ok(match ws.placement_of(Subject::Project(&id)) {
                Ok(pl) => Ok(ws.is_local_placement(&pl)),
                Err(e) => Err(e),
            })
        })
        .await?;
        match placed {
            Ok(true) => {}
            Ok(false) => {
                return Err(ApiError::bad_request(steve_i18n::t!(
                    "hub.project.lane_remote"
                )));
            }
            Err(e @ (PlacementError::Offline { .. } | PlacementError::Store(_))) => {
                return Err(placement_error(e));
            }
            Err(e) => return Err(ApiError::bad_request(e.to_string())),
        }
    }
    if p.get("kind").and_then(Value::as_str) != Some("git") {
        return Err(ApiError::bad_request(steve_i18n::t!(
            "hub.project.lanes_git_only"
        )));
    }
    let path = p
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let release: Option<steve_store::Release> = p
        .get("release")
        .filter(|v| !v.is_null())
        .and_then(|v| serde_json::from_value(v.clone()).ok());
    Ok((path, release))
}

/// The project's directory, which has to be there.
fn lane_dir(path: &str) -> Result<&FsPath, ApiError> {
    let dir = FsPath::new(path);
    if !dir.is_dir() {
        return Err(ApiError::bad_request(steve_i18n::t!(
            "hub.project.project_dir_missing",
            path = path
        )));
    }
    Ok(dir)
}

/// The target the project at `path` merges into (local git, off the runtime).
fn lane_target(path: &str) -> Result<steve_core::lane::target::Target, ApiError> {
    steve_core::lane::target::target_of(lane_dir(path)?, None).map_err(ApiError::bad_request)
}

/// `GET /api/projects/:id/lane`: what Steve recognizes about the target the project merges into (a
/// read-only profile with its evidence), and the lane itself: your limits, the unit in flight, the batch
/// collecting. Fetches first; a fetch that fails is said and the profile
/// is of the branch as last fetched.
pub async fn lane(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
) -> ApiResult {
    let (path, release) = lane_project(&st, &c, id).await?;
    let gh = st.cfg.gh.clone().unwrap_or_else(steve_core::config::gh_cli);
    let mut v = blocking(move || {
        let dir = lane_dir(&path)?;
        let fetch = steve_core::lane::precheck::fetch(dir).err();
        let prof = steve_core::lane::profile::profile(dir, None, release.as_ref(), &gh)
            .map_err(ApiError::bad_request)?;
        let mut v = json!(prof);
        v["fetch_error"] = json!(fetch);
        Ok(v)
    })
    .await?;
    let (up, br) = (
        js::string(&v["target"]["upstream"]),
        js::string(&v["target"]["branch"]),
    );
    v["lane"] = json!(st.domain.lane_view(&up, &br).await?);
    ok(v)
}

/// The longest merge window you may set, in minutes (a day).
const LANE_WINDOW_MAX_MIN: f64 = 1440.0;
/// The most requests a batch may take.
const LANE_MEMBERS_MAX: f64 = 50.0;

/// `PATCH /api/projects/:id/lane` `{window_min, max_members}` (`null`: the default): how long a batch
/// of the project's lane collects and how many requests it takes. The lane as shown after.
pub async fn lane_policy(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    b: Bytes,
) -> ApiResult {
    let v = body(&b)?;
    let mut k = Check::object(&v);
    let window = k.nullable_num(
        "window_min",
        Num {
            int: true,
            min: Some(1.0),
            max: Some(LANE_WINDOW_MAX_MIN),
            gt: None,
        },
    );
    let members = k.nullable_num(
        "max_members",
        Num {
            int: true,
            min: Some(1.0),
            max: Some(LANE_MEMBERS_MAX),
            gt: None,
        },
    );
    k.finish()?;
    let (path, _) = lane_project(&st, &c, id).await?;
    let t = blocking(move || lane_target(&path)).await?;
    let lane = st
        .domain
        .lane_policy(
            &t.upstream,
            &t.branch,
            window.map(|m| m as i64 * 60_000),
            members.map(|m| m as i64),
        )
        .await?;
    ok(json!({ "lane": lane }))
}

/// `POST /api/projects/:id/lane/now`: merge and release now. The lane's collecting batch closes at
/// once and starts as soon as nothing is in flight (its approval card still asks you). `closed`:
/// whether a batch was collecting.
pub async fn lane_now(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
) -> ApiResult {
    let (path, _) = lane_project(&st, &c, id).await?;
    let t = blocking(move || lane_target(&path)).await?;
    let closed = st.domain.lane_now(&t.upstream, &t.branch).await?;
    let lane = st.domain.lane_view(&t.upstream, &t.branch).await?;
    ok(json!({ "closed": closed, "lane": lane }))
}

/// What `describe` shows the model: the file list and the start of the usual readme files.
fn describe_input(path: &str) -> Result<(String, String), ApiError> {
    let mut names: Vec<String> = std::fs::read_dir(path)
        .map_err(|e| fs_error(&e, "scandir", path))?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    let list = names
        .into_iter()
        .filter(|f| !f.starts_with('.'))
        .take(60)
        .collect::<Vec<_>>()
        .join(", ");
    let mut readme = vec![];
    for f in ["README.md", "readme.md", "AGENTS.md", "package.json"] {
        let p = join(path, f);
        if !exists(&p) {
            continue;
        }
        let text = std::fs::read(&p).map_err(|e| fs_error(&e, "open", &p))?;
        readme.push(format!(
            "{f}:\n{}",
            js::slice(&String::from_utf8_lossy(&text), 2500)
        ));
    }
    Ok((list, readme.join("\n\n")))
}

/// Let Steve draft a description from what is in the directory.
pub async fn describe(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    b: Bytes,
) -> ApiResult {
    let v = body(&b)?;
    let mut k = Check::object(&v);
    let raw = k.str("path", Str::min(1));
    k.finish()?;
    let path = expand(&raw);
    let missing = || ApiError::bad_request(steve_i18n::t!("hub.project.dir_missing", path = raw));
    // the Node's answer has the git check in it; the Hub's own disk is asked after, as always
    let from_node = match via(&st, &path).await? {
        Via::Hub | Via::Outside(_) => None,
        Via::Node(node, at) => match node_describe(&*node, &at).await {
            Ok(v) => Some(v),
            Err(Miss::Gone) => return Err(missing()),
            Err(Miss::Unsupported) => None,
            Err(Miss::Fail(e)) => return Err(e),
        },
    };
    let (list, readme, node_git) = match from_node {
        Some((list, readme, git)) => (list, readme, Some(git)),
        None => {
            let p = path.clone();
            if !blocking(move || Ok(exists(&p))).await? {
                return Err(missing());
            }
            let p = path.clone();
            let (list, readme) = blocking(move || describe_input(&p)).await?;
            (list, readme, None)
        }
    };
    let prompt = format!(
        "Describe this project directory for an assistant that routes tasks to projects. Chinese, one or two sentences: what it is and what kind of work happens there. ONLY JSON {{\"name\":\"<= 12 chars\",\"description\":\"\"}}\nPath: {path}\nFiles: {list}\n\n{readme}"
    );
    let reply = st
        .domain
        .claude_print("title", &c.ws, &prompt, FAST, 60_000)
        .await?;
    let r = js::first_json(&reply)?.unwrap_or_else(|| json!({}));
    let get = |k: &str| r.get(k).filter(|x| !x.is_null()).map(js::string);
    let base = FsPath::new(&path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let git = match node_git {
        Some(git) => git,
        None => {
            let p = path.clone();
            blocking(move || Ok(exists(&join(&p, ".git")))).await?
        }
    };
    ok(
        json!({ "name": get("name").unwrap_or(base), "description": get("description").unwrap_or_default(), "git": git }),
    )
}

pub async fn dirs(
    State(st): State<AppState>,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult {
    let raw = q
        .get("path")
        .filter(|p| !p.is_empty())
        .cloned()
        .unwrap_or_else(|| "~/".into());
    let base = expand(&raw);
    match via(&st, &base).await? {
        Via::Hub => {}
        Via::Outside(roots) => return ok(json!(toward_roots(&roots, &base))),
        Via::Node(node, at) => match node_dirs(&*node, &at, &base).await {
            Ok(out) => return ok(json!(out)),
            Err(Miss::Gone) => return ok(json!([])),
            Err(Miss::Unsupported) => {}
            Err(Miss::Fail(e)) => return Err(e),
        },
    }
    let out = blocking(move || {
        if !exists(&base) {
            return Ok(vec![]);
        }
        let mut ents: Vec<(String, bool)> = std::fs::read_dir(&base)
            .map_err(|e| fs_error(&e, "scandir", &base))?
            .filter_map(|e| e.ok())
            // Dirent.isDirectory(): symlinks are not followed
            .map(|e| {
                (
                    e.file_name().to_string_lossy().into_owned(),
                    e.file_type().is_ok_and(|t| t.is_dir()),
                )
            })
            .collect();
        ents.sort();
        let home = home_dir().to_string_lossy().into_owned();
        Ok(ents
            .into_iter()
            .filter(|(n, d)| *d && !n.starts_with('.'))
            .take(200)
            .map(|(n, _)| join(&base, &n).replacen(&home, "~", 1))
            .collect::<Vec<_>>())
    })
    .await?;
    ok(json!(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_paths() {
        assert_eq!(normalize("/a//b/../c/"), "/a/c/");
        assert_eq!(normalize("/a/./b"), "/a/b");
        assert_eq!(normalize("/.."), "/");
        assert_eq!(join("/home/user", ""), "/home/user");
        assert_eq!(join("/home/user", "x/"), "/home/user/x/");
        assert_eq!(join("/home/user/", "x"), "/home/user/x");
        assert_eq!(expand("/abs"), "/abs");
        assert_eq!(expand("~x"), "~x");
    }

    #[test]
    fn describe_reads_list_and_readme() {
        let d = tempfile::tempdir().expect("tmp");
        std::fs::write(d.path().join("README.md"), "# Hi").expect("w");
        std::fs::write(d.path().join(".hidden"), "").expect("w");
        std::fs::create_dir(d.path().join("src")).expect("d");
        let (list, readme) = describe_input(&d.path().to_string_lossy()).expect("ok");
        assert_eq!(list, "README.md, src");
        assert!(readme.starts_with("README.md:\n# Hi"));
        let f = d.path().join("README.md");
        let e = describe_input(&f.to_string_lossy()).unwrap_err();
        assert_eq!(
            e,
            ApiError::internal(format!(
                "ENOTDIR: not a directory, scandir '{}'",
                f.display()
            ))
        );
    }

    /* ── through a Node (FakeNode: what the local Node answers once it has a files face) ── */

    use steve_node_api::testkit::FakeNode;

    fn fake(files: &[(&str, &[u8])]) -> (Arc<FakeNode>, na::RootPath) {
        let n = FakeNode::new("n1");
        n.set_roots(vec![na::RootInfo {
            id: na::RootId::new("r1").unwrap(),
            display: "~/code".into(),
            access: na::RootAccess::ReadWrite,
        }]);
        for (rel, bytes) in files {
            n.put_file("r1", rel, bytes);
        }
        let at = na::RootPath {
            root: na::RootId::new("r1").unwrap(),
            rel: na::RelPath::new("app").unwrap(),
        };
        (n, at)
    }

    #[tokio::test]
    async fn describe_through_the_node_reads_what_the_hub_did() {
        let (n, at) = fake(&[
            ("app/README.md", b"# Hi"),
            ("app/.hidden", b""),
            ("app/src/main.rs", b""),
        ]);
        let (list, readme, git) = node_describe(&*n, &at).await.unwrap();
        // the same as describe_reads_list_and_readme on the Hub's disk
        assert_eq!(list, "README.md, src");
        assert_eq!(readme, "README.md:\n# Hi");
        assert!(!git);
        n.put_file("r1", "app/.git/HEAD", b"ref: refs/heads/main\n");
        assert!(node_describe(&*n, &at).await.unwrap().2);
    }

    #[tokio::test]
    async fn a_long_readme_is_cut_where_the_hub_cut_it() {
        // three bytes a character, past what the model sees
        let long = "说明".repeat(2000);
        let (n, at) = fake(&[
            ("app/AGENTS.md", long.as_bytes()),
            ("app/package.json", b"{}"),
        ]);
        let (_, readme, _) = node_describe(&*n, &at).await.unwrap();
        assert_eq!(
            readme,
            format!(
                "AGENTS.md:\n{}\n\npackage.json:\n{{}}",
                js::slice(&long, README_CHARS)
            )
        );
    }

    #[tokio::test]
    async fn dirs_through_the_node_lists_directories_not_hidden() {
        let (n, at) = fake(&[
            ("app/b/x", b""),
            ("app/a/y", b""),
            ("app/.cache/z", b""),
            ("app/file.txt", b""),
        ]);
        let out = node_dirs(&*n, &at, "/w/app").await.unwrap();
        assert_eq!(out, vec!["/w/app/a", "/w/app/b"]);
    }

    #[tokio::test]
    async fn a_root_the_node_does_not_allow_is_gone_not_a_failure() {
        let (n, mut at) = fake(&[("app/README.md", b"x")]);
        at.root = na::RootId::new("elsewhere").unwrap();
        assert_eq!(node_list(&*n, &at).await.unwrap_err(), Miss::Gone);
        assert_eq!(node_describe(&*n, &at).await.unwrap_err(), Miss::Gone);
    }

    #[tokio::test]
    async fn an_offline_node_is_a_503_device_offline() {
        let e = na::ApiError::new(na::ErrorCode::Offline, "link down");
        assert_eq!(miss(e), Miss::Fail(ApiError::Json(503, "设备离线".into())));
        let en = steve_i18n::scope("en", async {
            miss(na::ApiError::new(na::ErrorCode::Offline, "x"))
        })
        .await;
        assert_eq!(en, Miss::Fail(ApiError::Json(503, "Device offline".into())));
        assert_eq!(
            placement_error(PlacementError::Offline { device: "n".into() }),
            ApiError::Json(503, "设备离线".into())
        );
        assert_eq!(
            miss(na::ApiError::new(na::ErrorCode::Unsupported, "files")),
            Miss::Unsupported
        );
        assert_eq!(
            miss(na::ApiError::new(na::ErrorCode::Denied, "outside")),
            Miss::Gone
        );
    }

    #[test]
    fn outside_the_roots_dirs_shows_the_way_down_to_them() {
        let roots = [
            PathBuf::from("/w/code/steve"),
            PathBuf::from("/w/code/app"),
            PathBuf::from("/w/notes"),
            PathBuf::from("/w/.secret/x"),
            PathBuf::from("/elsewhere"),
        ];
        assert_eq!(toward_roots(&roots, "/w/"), vec!["/w/code", "/w/notes"]);
        assert_eq!(
            toward_roots(&roots, "/w/code"),
            vec!["/w/code/app", "/w/code/steve"]
        );
        assert!(toward_roots(&roots, "/nowhere").is_empty());
    }
}
