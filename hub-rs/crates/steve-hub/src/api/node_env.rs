//! `GET /api/node-env/{node}?principal=`, `PUT|DELETE /api/node-env/{node}/{key}`: a user's own
//! environment variables on a Node (`wire::node_env`). `principal` is the caller (default) or, for
//! the workspace's owner, `svc:<workspace>`. `PUT {value, note?, principal?}`.

use std::collections::HashMap;

use axum::Extension;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};

use super::{ApiResult, AppState, Ctx, body, ok, ok_true};
use crate::validate::{Check, Str};

fn principal_of(c: &Ctx, asked: Option<String>) -> String {
    asked
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| c.me.clone())
}

pub async fn list(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(node): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult {
    let p = principal_of(&c, q.get("principal").cloned());
    let v = st
        .read(move |r| Ok(r.node_env_list(&c.me, &c.ws, &node, &p)?))
        .await?;
    ok(v)
}

pub async fn put(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path((node, key)): Path<(String, String)>,
    b: Bytes,
) -> ApiResult {
    let v = body(&b)?;
    let mut k = Check::object(&v);
    let value = k.str("value", Str::len(0, crate::wire::node_env::MAX_VALUE));
    let note = k.opt_str("note", Str::len(0, 200)).unwrap_or_default();
    let principal = k.opt_str("principal", Str::len(1, 200));
    k.finish()?;
    let p = principal_of(&c, principal);
    st.write(move |r| {
        r.node_env_put(&c.me, &c.ws, &node, &p, &key, Some(&value), &note)?;
        ok_true()
    })
    .await
}

pub async fn delete(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path((node, key)): Path<(String, String)>,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult {
    let p = principal_of(&c, q.get("principal").cloned());
    st.write(move |r| {
        r.node_env_put(&c.me, &c.ws, &node, &p, &key, None, "")?;
        ok_true()
    })
    .await
}
