//! REST of external personas, relative to `/api`; merged into
//! [`crate::api::routes`]:
//!
//! | route | |
//! | --- | --- |
//! | `GET /personas` | `Persona[]` (seeds the default persona “Steve” on first use) |
//! | `GET /personas/:id` | `Persona` |
//! | `POST /personas` | `{name: 1..20, bot_id, intro?: ≤400, soul?: ≤2000, reveal?, signature?: ≤60}` → `Persona` |
//! | `PATCH /personas/:id` | same, all optional → `Persona` |
//! | `DELETE /personas/:id` | `{ok:true, moved:{accounts, spaces}}`; the default persona → `400` |

use axum::extract::Path;
use axum::routing::get;
use axum::{Extension, Json, Router};
use serde_json::{Value, json};
use steve_modules::{ApiResult, Body, Caller, Modules};
use steve_store::base::Viewer;

use crate::Channels;
use crate::personas::{self, NewPersona, PersonaPatch};

type Cx = Extension<Channels>;

pub(crate) fn routes() -> Router<Modules> {
    Router::new()
        .route("/personas", get(list).post(create))
        .route("/personas/{id}", get(detail).patch(patch).delete(delete))
}

fn persona_json(ch: &Channels, v: &Viewer, id: &str) -> ApiResult<Value> {
    ch.store().reader(|c| -> ApiResult<Value> {
        let p = personas::own(c, &v.ws, id)?;
        Ok(serde_json::to_value(personas::out(c, v, &p)?)?)
    })
}

async fn list(Extension(ch): Cx, who: Caller) -> ApiResult<Json<Value>> {
    let v = ch.modules().viewer(&who)?;
    let now = ch.now();
    let out = ch
        .store()
        .transaction(|tx| -> ApiResult<Vec<personas::PersonaOut>> {
            personas::ensure_default(tx, &v.ws, now)?;
            personas::list(tx, &v.ws)?
                .iter()
                .map(|p| personas::out(tx, &v, p))
                .collect()
        })?;
    Ok(Json(serde_json::to_value(out)?))
}

async fn detail(Extension(ch): Cx, who: Caller, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let v = ch.modules().viewer(&who)?;
    Ok(Json(persona_json(&ch, &v, &id)?))
}

async fn create(
    Extension(ch): Cx,
    who: Caller,
    Body(mut n): Body<NewPersona>,
) -> ApiResult<Json<Value>> {
    let v = ch.modules().viewer(&who)?;
    n.intro = personas::sent(n.intro, "intro");
    n.soul = personas::sent(n.soul, "soul");
    let now = ch.now();
    let p = ch
        .store()
        .transaction(|tx| personas::create(tx, &v, &n, now))?;
    Ok(Json(persona_json(&ch, &v, &p.id)?))
}

async fn patch(
    Extension(ch): Cx,
    who: Caller,
    Path(id): Path<String>,
    Body(mut p): Body<PersonaPatch>,
) -> ApiResult<Json<Value>> {
    let v = ch.modules().viewer(&who)?;
    p.intro = personas::sent(p.intro, "intro");
    p.soul = personas::sent(p.soul, "soul");
    let now = ch.now();
    ch.store()
        .transaction(|tx| personas::patch(tx, &v, &id, &p, now))?;
    Ok(Json(persona_json(&ch, &v, &id)?))
}

async fn delete(Extension(ch): Cx, who: Caller, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let v = ch.modules().viewer(&who)?;
    let now = ch.now();
    let moved = ch
        .store()
        .transaction(|tx| personas::delete(tx, &v, &id, now))?;
    Ok(Json(json!({"ok": true, "moved": moved})))
}
