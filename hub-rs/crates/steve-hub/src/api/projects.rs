//! `/api/projects*` and `/api/fs/browse`.
//!
//! All host directory browsing, existence checks, git detection, and README reads go through the
//! colocated Node over `steve-node-api`; the Hub never touches project directories on disk.

use std::collections::HashMap;
use std::path::Path as FsPath;
use std::sync::Arc;

use axum::Extension;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use serde_json::{Value, json};
use steve_node_api as na;

use super::misc::FAST;
use super::{ApiResult, AppState, Ctx, Noun, Patch, check_len, json_body, ok, own, own_bot};
use crate::config::home_dir;
use crate::error::ApiError;
use crate::js;
use crate::wire::store::{NewProject, Obj};
use crate::wire::workspaces::PlacementError;

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

/// `~` and `~/x` → `$HOME` and `$HOME/x`; anything else unchanged.
pub fn expand(p: &str) -> String {
    match p.strip_prefix("~/") {
        Some(rest) => join(&home_dir().to_string_lossy(), rest),
        None if p == "~" => home_dir().to_string_lossy().into_owned(),
        None => p.to_string(),
    }
}

/// `设备离线` is a 503: the device may come back, the request was fine.
pub(super) fn offline() -> ApiError {
    ApiError::Json(503, steve_i18n::t!("hub.workspaces.offline"))
}

pub(super) fn placement_error(e: PlacementError) -> ApiError {
    match e {
        PlacementError::Offline { .. } | PlacementError::LocalOff => offline(),
        e @ PlacementError::NotOwners { .. } => ApiError::Json(403, e.to_string()),
        e => ApiError::internal(e.to_string()),
    }
}

fn local_node(st: &AppState) -> Result<Arc<dyn na::Node>, ApiError> {
    st.workspaces
        .as_ref()
        .and_then(|ws| ws.local_node())
        .ok_or_else(offline)
}

fn map_node_err(e: na::Error) -> ApiError {
    match e.code {
        na::Code::Unavailable => offline(),
        na::Code::Forbidden => ApiError::Json(403, e.safe_detail),
        na::Code::Invalid => ApiError::bad_request(e.safe_detail),
        _ => ApiError::internal(e.safe_detail),
    }
}

#[derive(serde::Deserialize)]
pub(super) struct Registration {
    name: String,
    #[serde(default)]
    description: String,
    worktree: Option<bool>,
    default_bot: Option<String>,
}

impl Registration {
    pub(super) fn check(&self, bad: &mut Vec<String>) {
        if !check_len(&self.name, 1, 40) {
            bad.push("name".into());
        }
        if !check_len(&self.description, 0, 1000) {
            bad.push("description".into());
        }
    }

    pub(super) async fn validate(&self, st: &AppState, c: &Ctx) -> Result<(), ApiError> {
        if let Some(bot) = self.default_bot.clone().filter(|b| !b.is_empty()) {
            let ws = c.ws.clone();
            st.read(move |r| Ok(own_bot(r, &bot, &ws)?)).await?;
        }
        Ok(())
    }

    pub(super) async fn save(
        self,
        st: &AppState,
        c: Ctx,
        path: String,
        git: bool,
    ) -> Result<Value, ApiError> {
        let Self {
            name,
            description,
            worktree,
            default_bot,
        } = self;
        st.write(move |r| {
            if r.project_by_path(&c.ws, &path)?.is_some() {
                return Err(already_project());
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
            let id = x.get("id").and_then(Value::as_str).unwrap_or_default();
            r.audit(&c.me, "project.create", id, &json!({ "path": path }), &c.ws)?;
            Ok(Value::Object(x))
        })
        .await
    }
}

pub(super) fn already_project() -> ApiError {
    ApiError::bad_request(steve_i18n::t!("hub.project.already_project"))
}

#[derive(serde::Deserialize)]
struct CreateBody {
    #[serde(flatten)]
    registration: Registration,
    path: String,
    node: Option<String>,
}

#[derive(serde::Deserialize)]
struct PatchBody {
    name: Option<String>,
    description: Option<String>,
    worktree: Option<bool>,
    #[serde(default)]
    default_bot: Patch<String>,
    archived: Option<bool>,
    #[serde(default)]
    release: Patch<Value>,
}

#[derive(serde::Deserialize)]
struct ReleaseIn {
    kind: String,
    cmd: String,
}

#[derive(serde::Deserialize)]
struct DescribeBody {
    path: String,
}

pub async fn create(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    b: Bytes,
) -> ApiResult {
    let req: CreateBody = json_body(&b)?;
    let mut bad = Vec::new();
    req.registration.check(&mut bad);
    if req.path.is_empty() {
        bad.push("path".into());
    }
    if !bad.is_empty() {
        return Err(ApiError::Invalid(bad));
    }
    let CreateBody {
        registration,
        path: raw,
        node,
    } = req;
    on_builtin_node(node.as_deref())?;
    registration.validate(&st, &c).await?;
    let local = local_node(&st)?;
    let info = local
        .describe_dir(na::DescribeDirRequest {
            path: raw.clone(),
            with_readme: false,
        })
        .await
        .map_err(map_node_err)?;
    if !info.exists || !info.is_dir {
        return Err(ApiError::bad_request(steve_i18n::t!(
            "hub.project.dir_missing",
            path = raw
        )));
    }
    let path = if info.resolved_path.is_empty() {
        expand(&raw)
    } else {
        info.resolved_path
    };
    ok(registration.save(&st, c, path, info.git).await?)
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
    let req: PatchBody = json_body(&b)?;
    let mut bad = Vec::new();
    let mut p = Obj::new();
    if let Some(x) = req.name {
        if !check_len(&x, 1, 40) {
            bad.push("name".into());
        } else {
            p.insert("name".into(), json!(x));
        }
    }
    if let Some(x) = req.description {
        if !check_len(&x, 0, 1000) {
            bad.push("description".into());
        } else {
            let x = steve_store::seeded::stored(&x, steve_store::seeded::PROJECT[0].1).unwrap_or(x);
            p.insert("description".into(), json!(x));
        }
    }
    if let Some(x) = req.worktree {
        p.insert("worktree".into(), json!(x));
    }
    if let Some(x) = req.default_bot.into_option() {
        p.insert("default_bot".into(), json!(x));
    }
    if let Some(x) = req.archived {
        p.insert("archived".into(), json!(x));
    }
    match req.release {
        Patch::Missing => {}
        Patch::Value(None) => {
            p.insert("release".into(), Value::Null);
        }
        Patch::Value(Some(v)) => match serde_json::from_value::<ReleaseIn>(v) {
            Ok(r) if r.kind == "script" && check_len(&r.cmd, 1, 2000) => {
                p.insert(
                    "release".into(),
                    json!(steve_store::Release {
                        kind: r.kind,
                        cmd: r.cmd,
                    }),
                );
            }
            _ => bad.push("release".into()),
        },
    }
    if !bad.is_empty() {
        return Err(ApiError::Invalid(bad));
    }
    st.write(move |r| {
        if let Some(Value::String(bot)) = p.get("default_bot")
            && !bot.is_empty()
        {
            own_bot(r, bot, &c.ws)?;
        }
        ok(Value::Object(r.project_patch(&id, &p, &c.me)?))
    })
    .await
}

/// Let Steve draft a description from what is in the directory (read on the colocated Node).
pub async fn describe(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    b: Bytes,
) -> ApiResult {
    let req: DescribeBody = json_body(&b)?;
    if req.path.is_empty() {
        return Err(ApiError::Invalid(vec!["path".into()]));
    }
    let raw = req.path;
    let missing = || ApiError::bad_request(steve_i18n::t!("hub.project.dir_missing", path = raw));
    let local = local_node(&st)?;
    let info = local
        .describe_dir(na::DescribeDirRequest {
            path: raw.clone(),
            with_readme: true,
        })
        .await
        .map_err(map_node_err)?;
    if !info.exists || !info.is_dir {
        return Err(missing());
    }
    let path = if info.resolved_path.is_empty() {
        expand(&raw)
    } else {
        info.resolved_path
    };
    let list = info.files.join(", ");
    let readme = info
        .readmes
        .into_iter()
        .map(|(name, text)| format!("{name}:\n{text}"))
        .collect::<Vec<_>>()
        .join("\n\n");
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
    ok(
        json!({ "name": get("name").unwrap_or(base), "description": get("description").unwrap_or_default(), "git": info.git }),
    )
}

/// The Node a project is being made on: only the Hub's built-in one runs sessions so far.
pub(super) fn on_builtin_node(node: Option<&str>) -> Result<(), ApiError> {
    match node {
        None | Some("" | "hub") => Ok(()),
        Some(_) => Err(ApiError::bad_request(steve_i18n::t!(
            "hub.project.node_remote_unsupported"
        ))),
    }
}

/// The directory chooser of a new project: delegated to the colocated Node (`browse_dir` RPC),
/// for the instance owner only.
pub async fn browse(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult {
    require_fs_owner(&c)?;
    on_builtin_node(q.get("node").map(String::as_str))?;
    let raw = q.get("path").cloned().unwrap_or_default();
    let local = local_node(&st)?;
    let resp = local
        .browse_dir(na::BrowseDirRequest { path: raw })
        .await
        .map_err(map_node_err)?;
    ok(json!(resp))
}

pub(super) fn require_fs_owner(c: &Ctx) -> Result<(), ApiError> {
    if c.user.get("role").and_then(Value::as_str) == Some("owner") {
        Ok(())
    } else {
        Err(ApiError::Json(
            403,
            steve_i18n::t!("hub.project.browse_owner_only"),
        ))
    }
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
    fn an_offline_node_is_a_503_device_offline() {
        assert_eq!(
            map_node_err(na::Error::new(na::Code::Unavailable, "link down")),
            ApiError::Json(503, "设备离线".into())
        );
        assert_eq!(
            placement_error(PlacementError::Offline { device: "n".into() }),
            ApiError::Json(503, "设备离线".into())
        );
        assert_eq!(
            placement_error(PlacementError::LocalOff),
            ApiError::Json(503, "设备离线".into())
        );
    }

    #[test]
    fn a_project_is_made_on_the_built_in_node_only() {
        for node in [None, Some(""), Some("hub")] {
            assert!(on_builtin_node(node).is_ok(), "{node:?}");
        }
        assert!(matches!(
            on_builtin_node(Some("nod_laptop")),
            Err(ApiError::Json(400, _))
        ));
    }
}

/// Routes mounted behind the shared API login and body-limit layers.
pub(crate) fn routes() -> axum::Router<super::AppState> {
    use super::*;
    Router::new()
        .route("/projects", post(projects::create))
        .route("/projects/describe", post(projects::describe))
        .route("/projects/{id}", patch(projects::patch_one))
        .route("/fs/browse", get(projects::browse))
}
