//! Management-only routes: never merged into the public Router.
use crate::access::{self, CliOutput};
use axum::{
    Json, Router,
    extract::{Path, Query, Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get},
};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use steve_store::Store;

#[derive(Deserialize, Serialize)]
pub(super) struct Create {
    /// Original argv preserves legacy flag ordering, duplicate flags and values beginning
    /// with '--'; no shell or process is involved in executing these arguments.
    pub args: Vec<String>,
    pub bootstrap_user: Option<String>,
}

#[derive(Default, Deserialize)]
struct Format {
    #[serde(default)]
    json: bool,
}
async fn deadline(State(limit): State<Duration>, request: Request, next: Next) -> Response {
    match tokio::time::timeout(limit, next.run(request)).await {
        Ok(response) => response,
        Err(_) => (StatusCode::GATEWAY_TIMEOUT, Json(CliOutput::result(Err(anyhow::anyhow!(
            "management request deadline exceeded; a write may already have completed; inspect before retrying"))))).into_response(),
    }
}
pub(super) fn router(store: Store, limit: Duration) -> Router {
    Router::new()
        .route("/admin/v1/access/tokens", get(tokens).post(create))
        .route("/admin/v1/access/tokens/{id}", delete(revoke_token))
        .route("/admin/v1/access/clients", get(clients))
        .route("/admin/v1/access/clients/{id}", delete(revoke_client))
        .route("/admin/v1/storage/status", get(status))
        .route("/admin/v1/backup", get(backups).post(make_backup))
        .with_state(store)
        .layer(middleware::from_fn_with_state(limit, deadline))
}
async fn command(store: Store, args: Vec<String>) -> Response {
    command_with_user(store, args, None).await
}
async fn command_with_user(
    store: Store,
    args: Vec<String>,
    bootstrap_user: Option<String>,
) -> Response {
    match tokio::task::spawn_blocking(move || match bootstrap_user {
        Some(user) => access::command_with_default_user(&store, &args, &user),
        None => access::command_on(&store, &args),
    })
    .await
    {
        Ok(result) => {
            let output = CliOutput::result(result);
            let status = if output.code == 0 {
                StatusCode::OK
            } else {
                StatusCode::BAD_REQUEST
            };
            (status, Json(output)).into_response()
        }
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(CliOutput::result(Err(anyhow::anyhow!(
                "management worker failed"
            )))),
        )
            .into_response(),
    }
}
async fn create(State(store): State<Store>, Json(input): Json<Create>) -> Response {
    let words: Vec<_> = input
        .args
        .iter()
        .map(String::as_str)
        .take_while(|s| !s.starts_with("--"))
        .collect();
    if words != ["token", "create"] {
        return (
            StatusCode::BAD_REQUEST,
            Json(CliOutput::result(Err(access::usage()))),
        )
            .into_response();
    }
    command_with_user(store, input.args, input.bootstrap_user).await
}

async fn tokens(State(store): State<Store>, Query(format): Query<Format>) -> Response {
    let mut args = vec!["token".into(), "list".into()];
    if format.json {
        args.push("--json".into());
    }
    command(store, args).await
}
async fn clients(State(store): State<Store>, Query(format): Query<Format>) -> Response {
    let mut args = vec!["client".into(), "list".into()];
    if format.json {
        args.push("--json".into());
    }
    command(store, args).await
}
async fn revoke_token(State(store): State<Store>, Path(id): Path<String>) -> Response {
    command(store, vec!["token".into(), "revoke".into(), id]).await
}
async fn revoke_client(State(store): State<Store>, Path(id): Path<String>) -> Response {
    command(store, vec!["client".into(), "revoke".into(), id]).await
}
async fn status(State(store): State<Store>) -> Response {
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        let schema: i64 = store.reader(|c| Ok::<_, steve_store::StoreError>(c.query_row("SELECT v FROM schema_version LIMIT 1", [], |r| r.get(0))?))?;
        Ok(serde_json::json!({"path": store.path(), "schema_version": schema, "file_size": std::fs::metadata(store.path())?.len()}))
    }).await;
    match result {
        Ok(Ok(status)) => Json(status).into_response(),
        _ => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(CliOutput::result(Err(anyhow::anyhow!(
                "cannot read storage status"
            )))),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;
    #[tokio::test(start_paused = true)]
    async fn deadline_reports_why_without_waiting_for_wall_time() {
        let router = Router::new()
            .route(
                "/slow",
                get(|| async {
                    std::future::pending::<()>().await;
                    "done"
                }),
            )
            .layer(middleware::from_fn_with_state(
                Duration::from_secs(30),
                deadline,
            ));
        let response = router
            .oneshot(Request::builder().uri("/slow").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        let reply: CliOutput = serde_json::from_slice(&body).unwrap();
        assert_eq!(reply.code, 2);
        assert!(reply.stderr.contains("deadline exceeded"));
    }
}

#[derive(Default, Deserialize, Serialize)]
pub(super) struct BackupRequest {
    pub reason: Option<String>,
}
async fn make_backup(State(store): State<Store>, Json(input): Json<BackupRequest>) -> Response {
    let args = vec![
        "backup".into(),
        "--reason".into(),
        input.reason.unwrap_or_else(|| "diagnostic".into()),
    ];
    let result = tokio::task::spawn_blocking(move || {
        let db = store.path();
        super::client::backup::command(&super::client::backup::home(db), db, &args)
    })
    .await;
    let output =
        CliOutput::result(result.unwrap_or_else(|_| Err(anyhow::anyhow!("backup worker failed"))));
    (
        if output.code == 0 {
            StatusCode::OK
        } else {
            StatusCode::BAD_REQUEST
        },
        Json(output),
    )
        .into_response()
}
async fn backups(State(store): State<Store>) -> Response {
    let result = tokio::task::spawn_blocking(move || {
        steve_store_sqlite::backup::usage(
            &super::client::backup::home(store.path()).join("backups"),
        )
    })
    .await;
    match result {
        Ok(Ok(usage)) => Json(usage).into_response(),
        _ => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}
