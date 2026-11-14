//! `/api/plugins`: the libraries, what they offer, what is installed and connecting it.
//!
//! | Route | Who | What |
//! |---|---|---|
//! | `GET /plugins` | anyone signed in | libraries, their plugins, what is installed, the caller's own connections |
//! | `GET /plugins/themes` | anyone signed in | the manifests of the installed theme plugins |
//! | `POST /plugins/stores` `{address}` | owner | add a library (its index is read first) |
//! | `POST /plugins/stores/{store}/refresh` | owner | read its index again |
//! | `DELETE /plugins/stores/{store}` | owner | remove a library nothing is installed from |
//! | `POST /plugins/installed/{store}/{plugin}` | owner | install the version the index names (also an upgrade) |
//! | `DELETE /plugins/installed/{store}/{plugin}` | owner | uninstall a plugin nobody is connected through |
//! | `POST /plugins/installed/{store}/{plugin}/connect` `{config}` | anyone signed in | start it with these settings and connect it as the caller |

use std::collections::BTreeMap;

use axum::extract::Path;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Extension, Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use steve_modules::accounts::connections::Connections;
use steve_modules::{Body, Caller, Modules};

use super::{Plugins, Refused};
use crate::api::Ctx;
use crate::error::ApiError;

impl IntoResponse for Refused {
    fn into_response(self) -> Response {
        match self {
            Refused::NotFound(msg) => ApiError::Json(404, msg),
            Refused::Rule(status, msg) => ApiError::Json(status, msg),
            Refused::Connection(e) => return e.into_response(),
        }
        .into_response()
    }
}

pub fn routes(plugins: Plugins, conns: Connections) -> Router<Modules> {
    Router::new()
        .route("/plugins", get(list))
        .route("/plugins/themes", get(themes))
        .route("/plugins/stores", post(add_store))
        .route("/plugins/stores/{store}", delete(remove_store))
        .route("/plugins/stores/{store}/refresh", post(refresh))
        .route(
            "/plugins/installed/{store}/{plugin}",
            post(install).delete(uninstall),
        )
        .route("/plugins/installed/{store}/{plugin}/connect", post(connect))
        .layer(Extension(plugins))
        .layer(Extension(conns))
}

fn is_owner(cx: &Ctx) -> bool {
    cx.user.get("role").and_then(Value::as_str) == Some("owner")
}

/// Installing a plugin runs a program as the Hub's user, and a library is where programs come
/// from: both are the instance owner's.
fn owner(cx: &Ctx) -> Result<(), Refused> {
    if is_owner(cx) {
        Ok(())
    } else {
        Err(Refused::Rule(403, steve_i18n::t!("hub.plugins.owner_only")))
    }
}

async fn list(
    Extension(p): Extension<Plugins>,
    Extension(cx): Extension<Ctx>,
    who: Caller,
) -> Result<Json<Value>, Refused> {
    p.list(&who, is_owner(&cx))
        .map(Json)
        .map_err(|e| Refused::Rule(500, format!("{e:#}")))
}

async fn themes(Extension(p): Extension<Plugins>, _who: Caller) -> Result<Json<Value>, Refused> {
    p.themes()
        .map(|t| Json(json!({"themes": t})))
        .map_err(|e| Refused::Rule(500, format!("{e:#}")))
}

#[derive(Deserialize)]
struct AddStore {
    address: String,
}

async fn add_store(
    Extension(p): Extension<Plugins>,
    Extension(cx): Extension<Ctx>,
    Body(b): Body<AddStore>,
) -> Result<Json<Value>, Refused> {
    owner(&cx)?;
    let rec = p.add_store(&b.address).await?;
    Ok(Json(json!({"id": rec.id})))
}

async fn refresh(
    Extension(p): Extension<Plugins>,
    Extension(cx): Extension<Ctx>,
    Path(store): Path<String>,
) -> Result<Json<Value>, Refused> {
    owner(&cx)?;
    let rec = p.refresh(&store).await?;
    Ok(Json(
        json!({"id": rec.id, "checked": rec.checked, "error": rec.error}),
    ))
}

async fn remove_store(
    Extension(p): Extension<Plugins>,
    Extension(cx): Extension<Ctx>,
    Path(store): Path<String>,
) -> Result<Json<Value>, Refused> {
    owner(&cx)?;
    p.remove_store(&store).await?;
    Ok(Json(json!({"ok": true})))
}

async fn install(
    Extension(p): Extension<Plugins>,
    Extension(cx): Extension<Ctx>,
    Path((store, plugin)): Path<(String, String)>,
) -> Result<Json<Value>, Refused> {
    owner(&cx)?;
    let done = p.install(&store, &plugin).await?;
    Ok(Json(
        json!({"version": done.plugin.version, "installed": done.installed}),
    ))
}

async fn uninstall(
    Extension(p): Extension<Plugins>,
    Extension(cx): Extension<Ctx>,
    Path((store, plugin)): Path<(String, String)>,
) -> Result<Json<Value>, Refused> {
    owner(&cx)?;
    p.uninstall(&store, &plugin).await?;
    Ok(Json(json!({"ok": true})))
}

#[derive(Deserialize)]
struct Connect {
    #[serde(default)]
    config: BTreeMap<String, String>,
}

async fn connect(
    Extension(p): Extension<Plugins>,
    Extension(conns): Extension<Connections>,
    who: Caller,
    Path((store, plugin)): Path<(String, String)>,
    Body(b): Body<Connect>,
) -> Result<Json<Value>, Refused> {
    p.connect(&conns, &who, &store, &plugin, b.config)
        .await
        .map(Json)
}
