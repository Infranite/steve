//! `POST /channels/feishu/registrations {brand}`, `GET …/current`, `GET …/:id`,
//! `POST …/:id/cancel`, `POST …/:id/retry`.

use axum::extract::Path;
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use serde::Deserialize;
use serde_json::Value;
use steve_modules::vault::VaultResult;
use steve_modules::{ApiError, Caller, Modules};

use super::protocol::Brand;
use super::service::Registrations;

#[derive(Deserialize)]
struct BeginBody {
    brand: String,
}

/// `POST /channels/feishu/registrations {brand}`, `GET …/current`, `GET …/:id`, `POST …/:id/cancel`,
/// `POST …/:id/retry`. Relative to `/api`, behind the `Caller` layer.
pub fn routes(r: Registrations) -> Router<Modules> {
    Router::new()
        .route("/channels/feishu/registrations", post(begin))
        .route("/channels/feishu/registrations/current", get(current))
        .route("/channels/feishu/registrations/{id}", get(status))
        .route("/channels/feishu/registrations/{id}/cancel", post(cancel))
        .route("/channels/feishu/registrations/{id}/retry", post(retry))
        .layer(Extension(r))
}

async fn begin(
    Extension(r): Extension<Registrations>,
    who: Caller,
    body: steve_modules::Body<BeginBody>,
) -> VaultResult<Json<Value>> {
    let brand = Brand::parse(&body.0.brand).ok_or_else(|| ApiError::invalid("brand"))?;
    r.begin(&who, brand).await.map(Json)
}

async fn current(Extension(r): Extension<Registrations>, who: Caller) -> VaultResult<Json<Value>> {
    Ok(Json(r.current(&who)?.unwrap_or(Value::Null)))
}

async fn status(
    Extension(r): Extension<Registrations>,
    who: Caller,
    Path(id): Path<String>,
) -> VaultResult<Json<Value>> {
    r.get(&who, &id).map(Json)
}

async fn cancel(
    Extension(r): Extension<Registrations>,
    who: Caller,
    Path(id): Path<String>,
) -> VaultResult<Json<Value>> {
    r.cancel(&who, &id).map(Json)
}

async fn retry(
    Extension(r): Extension<Registrations>,
    who: Caller,
    Path(id): Path<String>,
) -> VaultResult<Json<Value>> {
    r.retry(&who, &id).map(Json)
}
