//! Preview and provision a project directory on the colocated Node via `steve-node-api` RPCs.
//! Unlike registering an existing directory, setup creates something on the host and is limited
//! to the instance owner. The Hub never touches the host filesystem or runs `git clone` directly.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::response::{
    IntoResponse, Sse,
    sse::{Event, KeepAlive},
};
use axum::{Extension, body::Bytes, extract::State};
use serde_json::{Value, json};
use steve_node_api as na;
use tokio::sync::mpsc;

use super::projects::{Registration, already_project, offline, on_builtin_node, require_fs_owner};
use super::{ApiResult, AppState, Ctx, check_len, json_body, ok};
use crate::error::ApiError;

/// How often the Hub polls the colocated Node for `git clone` progress while streaming SSE.
const SETUP_POLL_INTERVAL: Duration = Duration::from_millis(150);

#[derive(serde::Deserialize)]
struct PreviewBody {
    parent: String,
    directory: String,
    node: Option<String>,
}

#[derive(serde::Deserialize)]
struct SetupBody {
    #[serde(flatten)]
    registration: Registration,
    node: Option<String>,
    parent: String,
    directory: String,
    preview: String,
    mode: String,
    remote: Option<String>,
    branch: Option<String>,
}

fn setup_error(e: na::ProjectSetupError) -> ApiError {
    use na::ProjectSetupError::*;
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

fn node_rpc_error(e: na::Error) -> ApiError {
    match e.code {
        na::Code::Unavailable => offline(),
        na::Code::Busy => ApiError::Json(503, steve_i18n::t!("hub.project.setup_busy")),
        na::Code::Conflict => {
            ApiError::Json(409, steve_i18n::t!("hub.project.setup_preview_changed"))
        }
        na::Code::Invalid => ApiError::bad_request(e.safe_detail),
        _ => ApiError::internal(e.safe_detail),
    }
}

fn local_node(st: &AppState) -> Result<Arc<dyn na::Node>, ApiError> {
    st.workspaces
        .as_ref()
        .and_then(|ws| ws.local_node())
        .ok_or_else(offline)
}

async fn setup_target(
    st: &AppState,
    c: &Ctx,
    node: &dyn na::Node,
    parent: String,
    directory: String,
) -> Result<String, ApiError> {
    let preview = node
        .project_setup_preview(na::ProjectSetupPreviewRequest { parent, directory })
        .await
        .map_err(node_rpc_error)?;
    let target = match preview {
        na::ProjectSetupPreview::Ready { path } => path,
        na::ProjectSetupPreview::Failed { error } => return Err(setup_error(error)),
    };
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
    let req: PreviewBody = json_body(&b)?;
    let mut bad = Vec::new();
    if !check_len(&req.parent, 1, 4096) {
        bad.push("parent".into());
    }
    if !check_len(&req.directory, 1, 255) {
        bad.push("directory".into());
    }
    if !bad.is_empty() {
        return Err(ApiError::Invalid(bad));
    }
    on_builtin_node(req.node.as_deref())?;
    let node = local_node(&st)?;
    let path = setup_target(&st, &c, node.as_ref(), req.parent, req.directory).await?;
    ok(json!({ "path": path }))
}

pub async fn setup(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    b: Bytes,
) -> ApiResult {
    require_fs_owner(&c)?;
    let req: SetupBody = json_body(&b)?;
    let mut bad = Vec::new();
    req.registration.check(&mut bad);
    if !check_len(&req.parent, 1, 4096) {
        bad.push("parent".into());
    }
    if !check_len(&req.directory, 1, 255) {
        bad.push("directory".into());
    }
    if !check_len(&req.preview, 1, 8192) {
        bad.push("preview".into());
    }
    if req
        .remote
        .as_deref()
        .is_some_and(|r| !check_len(r, 1, 4096))
    {
        bad.push("remote".into());
    }
    if req
        .branch
        .as_deref()
        .is_some_and(|br| !check_len(br, 0, 255))
    {
        bad.push("branch".into());
    }
    if !bad.is_empty() {
        return Err(ApiError::Invalid(bad));
    }
    let SetupBody {
        registration,
        node: node_arg,
        parent,
        directory,
        preview,
        mode,
        remote,
        branch,
    } = req;
    let branch = branch.filter(|b| !b.is_empty());
    on_builtin_node(node_arg.as_deref())?;
    let setup_mode = match (mode.as_str(), remote, branch) {
        ("empty", None, None) => na::ProjectSetupMode::Empty,
        ("clone", Some(remote), branch) => na::ProjectSetupMode::Clone { remote, branch },
        _ => {
            return Err(ApiError::Invalid(vec![
                "mode".into(),
                "remote".into(),
                "branch".into(),
            ]));
        }
    };
    let git = matches!(setup_mode, na::ProjectSetupMode::Clone { .. });
    registration.validate(&st, &c).await?;
    let node = local_node(&st)?;
    let target = setup_target(&st, &c, node.as_ref(), parent.clone(), directory.clone()).await?;
    if target != preview {
        return Err(ApiError::Json(
            409,
            steve_i18n::t!("hub.project.setup_preview_changed"),
        ));
    }

    let now = steve_store::now();
    let op = na::OperationId::new(format!("op_setup_{}", uuid::Uuid::new_v4().simple()))
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let cmd = na::Command {
        id: op.clone(),
        issued_at: na::Timestamp(now),
        accept_before: na::Timestamp(now + 60_000),
        grant: na::GrantId::new("hub").expect("constant grant"),
        body: na::ProjectSetupRequest {
            parent,
            directory,
            preview,
            mode: setup_mode,
        },
    };
    let initial = node
        .project_setup_start(cmd)
        .await
        .map_err(node_rpc_error)?;

    // A lost client connection isn't a rollback signal. In particular it must not interrupt the
    // transition from a complete directory to a registered project.
    let (tx, rx) = mpsc::channel::<Value>(32);
    tokio::spawn(steve_i18n::bind(async move {
        let mut status = initial;
        let mut last_event: Option<(String, Option<u8>)> = None;
        let final_result: Result<String, ApiError> = loop {
            if status.is_terminal() {
                if status.phase == "done" {
                    break Ok(status.path.unwrap_or(target));
                }
                let err = status
                    .error
                    .map(setup_error)
                    .unwrap_or_else(|| ApiError::internal("project setup failed"));
                break Err(err);
            }
            if git {
                let key = (status.phase.clone(), status.percent);
                if last_event.as_ref() != Some(&key) {
                    last_event = Some(key);
                    let mut ev = json!({ "phase": status.phase });
                    if let Some(pct) = status.percent {
                        ev["percent"] = json!(pct);
                    }
                    let _ = tx.try_send(ev);
                }
            }
            tokio::time::sleep(SETUP_POLL_INTERVAL).await;
            match node.project_setup_status(op.clone()).await {
                Ok(next) => status = next,
                Err(e) => break Err(node_rpc_error(e)),
            }
        };
        let result = match final_result {
            Err(e) => json!({ "error": e.to_string() }),
            Ok(resolved) => {
                if git {
                    let _ = tx.try_send(json!({ "phase": "registering" }));
                }
                match registration.save(&st, c, resolved, git).await {
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
