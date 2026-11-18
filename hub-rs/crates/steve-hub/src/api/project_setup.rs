//! Preview and provision a project directory on the Hub's own Node. Unlike registering an
//! existing directory, setup creates something on the host and is limited to the instance owner.

use std::convert::Infallible;
use std::path::{Path as FsPath, PathBuf};

use axum::response::{
    IntoResponse, Sse,
    sse::{Event, KeepAlive},
};
use axum::{Extension, body::Bytes, extract::State};
use serde_json::{Value, json};
use steve_node::project_setup::{self, Branch, Remote, SetupError, Staging};
use tokio::sync::{Semaphore, mpsc};

use super::projects::{Registration, already_project, expand, on_builtin_node, require_fs_owner};
use super::{ApiResult, AppState, Ctx, blocking, body, ok};
use crate::error::ApiError;
use crate::validate::{Check, Str};

fn setup_error(e: SetupError) -> ApiError {
    use SetupError::*;
    let (code, msg) = match e {
        DirectoryName => (400, steve_i18n::t!("hub.project.setup_name")),
        Parent => (400, steve_i18n::t!("hub.project.setup_parent")),
        Conflict => (409, steve_i18n::t!("hub.project.setup_conflict")),
        Filesystem => (500, steve_i18n::t!("hub.project.setup_filesystem")),
        Remote => (400, steve_i18n::t!("hub.project.setup_remote")),
        Branch => (400, steve_i18n::t!("hub.project.setup_branch")),
        DefaultBranch => (400, steve_i18n::t!("hub.project.setup_default_branch")),
        Git => (503, steve_i18n::t!("hub.project.setup_git")),
        Authentication => (400, steve_i18n::t!("hub.project.setup_auth")),
        Network => (502, steve_i18n::t!("hub.project.setup_network")),
        Timeout => (504, steve_i18n::t!("hub.project.setup_timeout")),
        Clone => (400, steve_i18n::t!("hub.project.setup_clone")),
    };
    ApiError::Json(code, msg)
}

async fn setup_target(
    st: &AppState,
    c: &Ctx,
    parent: String,
    directory: String,
) -> Result<String, ApiError> {
    let target = blocking(move || {
        project_setup::destination(FsPath::new(&expand(&parent)), &directory)
            .map(|p| p.into_os_string().into_string().expect("UTF-8 destination"))
            .map_err(setup_error)
    })
    .await?;
    let (lookup, ws) = (target.clone(), c.ws.clone());
    if st
        .read(move |r| Ok(r.project_by_path(&ws, &lookup)?.is_some()))
        .await?
    {
        return Err(already_project());
    }
    Ok(target)
}

pub async fn setup_preview(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    b: Bytes,
) -> ApiResult {
    require_fs_owner(&c)?;
    let v = body(&b)?;
    let mut k = Check::object(&v);
    let parent = k.str("parent", Str::len(1, 4096));
    let directory = k.str("directory", Str::len(1, 255));
    let node = k.opt_str("node", Str::ANY);
    k.finish()?;
    on_builtin_node(node.as_deref())?;
    ok(json!({ "path": setup_target(&st, &c, parent, directory).await? }))
}

// The filesystem is shared by all workspaces of this installation, so bound Git jobs globally.
static CLONES: Semaphore = Semaphore::const_new(4);

pub async fn setup(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    b: Bytes,
) -> ApiResult {
    require_fs_owner(&c)?;
    let v = body(&b)?;
    let mut k = Check::object(&v);
    let registration = Registration::parse(&mut k);
    let node = k.opt_str("node", Str::ANY);
    let parent = k.str("parent", Str::len(1, 4096));
    let directory = k.str("directory", Str::len(1, 255));
    let preview = k.str("preview", Str::len(1, 8192));
    let mode = k.str("mode", Str::ANY);
    let remote = k.opt_str("remote", Str::len(1, 4096));
    let branch = k.opt_str("branch", Str::max(255)).filter(|b| !b.is_empty());
    k.finish()?;
    on_builtin_node(node.as_deref())?;
    let clone = match (mode.as_str(), remote, branch) {
        ("empty", None, None) => None,
        ("clone", Some(remote), branch) => Some((
            Remote::new(remote).map_err(setup_error)?,
            branch.map(Branch::new).transpose().map_err(setup_error)?,
        )),
        _ => {
            return Err(ApiError::Invalid(vec![
                "mode".into(),
                "remote".into(),
                "branch".into(),
            ]));
        }
    };
    registration.validate(&st, &c).await?;
    let target = setup_target(&st, &c, parent, directory).await?;
    if target != preview {
        return Err(ApiError::Json(
            409,
            steve_i18n::t!("hub.project.setup_preview_changed"),
        ));
    }
    let permit = if clone.is_some() {
        Some(
            CLONES
                .try_acquire()
                .map_err(|_| ApiError::Json(503, steve_i18n::t!("hub.project.setup_busy")))?,
        )
    } else {
        None
    };

    // A lost client connection isn't a rollback signal. In particular it must not interrupt the
    // transition from a complete directory to a registered project.
    let (tx, rx) = mpsc::channel::<Value>(32);
    tokio::spawn(steve_i18n::bind(async move {
        let _permit = permit;
        let git = clone.is_some();
        let path = PathBuf::from(&target);
        let result = if let Some((remote, branch)) = clone {
            clone_to(path, remote, branch, &tx).await
        } else {
            blocking(move || project_setup::create_empty(&path).map_err(setup_error)).await
        };
        let result = match result {
            Err(e) => json!({ "error": e.to_string() }),
            Ok(()) => {
                if git {
                    let _ = tx.try_send(json!({ "phase": "registering" }));
                }
                match registration.save(&st, c, target, git).await {
                    Ok(project) => json!({ "project": project }),
                    Err(_) => json!({ "error": steve_i18n::t!("hub.project.setup_registration") }),
                }
            }
        };
        let _ = tx.send(result).await;
    }));
    let events = futures::stream::unfold(rx, |mut rx| async move {
        let value = rx.recv().await?;
        Some((
            Ok::<_, Infallible>(Event::default().data(value.to_string())),
            rx,
        ))
    });
    Ok((
        [("x-accel-buffering", "no")],
        Sse::new(events).keep_alive(KeepAlive::default()),
    )
        .into_response())
}

async fn clone_to(
    path: PathBuf,
    remote: Remote,
    branch: Option<Branch>,
    tx: &mpsc::Sender<Value>,
) -> Result<(), ApiError> {
    let temp = path.clone();
    let staging = blocking(move || Staging::new(&temp).map_err(setup_error)).await?;
    let _ = tx.try_send(json!({ "phase": "connecting" }));
    let r = staging
        .clone_repo(remote, branch, |phase, percent| {
            let _ = tx.try_send(json!({ "phase": phase, "percent": percent }));
        })
        .await;
    blocking(move || {
        r.map_err(setup_error)?;
        staging.publish(&path).map_err(setup_error)
    })
    .await
}

/// Routes mounted behind the shared API login and body-limit layers.
pub(crate) fn routes() -> axum::Router<super::AppState> {
    use super::*;
    Router::new()
        .route(
            "/projects/setup/preview",
            post(project_setup::setup_preview),
        )
        .route("/projects/setup", post(project_setup::setup))
}
