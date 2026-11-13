//! `PUT /api/contacts/:id/avatar` (the picture as the raw body or multipart field `file`) and `DELETE` (back
//! to initials): a contact's own picture (`crate::avatar`). Both answer the contact, as `GET /contacts/:id`
//! does; `me` is “我”. Without the contacts module the route is not there (a JSON `404` without `error`).

use std::collections::HashMap;

use axum::Extension;
use axum::extract::{Path, Query, Request, State};
use axum::response::{IntoResponse, Response};
use serde_json::Value;
use steve_modules::contacts::service as people;

use super::files::read_upload;
use super::{ApiResult, AppState, Ctx, blocking};
use crate::avatar::{self, MAX_UPLOAD};
use crate::error::{ApiError, json_response};
use crate::files as att;

fn too_large() -> ApiError {
    ApiError::Json(
        413,
        steve_i18n::t!("hub.avatar.too_large", mb = MAX_UPLOAD / 1024 / 1024),
    )
}

fn caller(c: &Ctx) -> steve_modules::Caller {
    steve_modules::Caller {
        ws: c.ws.clone(),
        me: c.me.clone(),
    }
}

fn answer(r: steve_modules::ApiResult<people::Contact>) -> ApiResult {
    Ok(match r.and_then(|c| Ok(serde_json::to_value(c)?)) {
        Ok(v) => json_response(200, &v),
        Err(e) => e.into_response(),
    })
}

fn unwired() -> Response {
    json_response(404, &Value::Object(Default::default()))
}

pub async fn put(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
    req: Request,
) -> ApiResult {
    let Some(m) = st.modules.clone() else {
        return Ok(unwired());
    };
    // the contact first: nothing is stored for one that is not there
    let who = caller(&c);
    let (w, i) = (who.clone(), id.clone());
    if let Err(e) = m.blocking(move |m| people::get_contact(m, &w, &i)).await {
        return Ok(e.into_response());
    }
    let (buf, _) = read_upload(&q, req, MAX_UPLOAD + 64 * 1024, too_large).await?;
    let (bytes, ext) = blocking(move || Ok(avatar::square(&buf)?)).await?;
    let data_dir = st.cfg.data_dir.clone();
    let file = st
        .write(move |r| {
            let f = att::store(
                r,
                &data_dir,
                &c.ws,
                &bytes,
                Some(&format!("avatar.{ext}")),
                &c.me,
            )?;
            Ok(f.get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string())
        })
        .await?;
    answer(
        m.blocking(move |m| people::set_avatar_file(m, &who, &id, Some(&file)))
            .await,
    )
}

pub async fn delete(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
) -> ApiResult {
    let Some(m) = st.modules.clone() else {
        return Ok(unwired());
    };
    let who = caller(&c);
    answer(
        m.blocking(move |m| people::set_avatar_file(m, &who, &id, None))
            .await,
    )
}
