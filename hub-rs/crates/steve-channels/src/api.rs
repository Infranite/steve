//! REST of spaces and messages (channels-personas.md §5.2), relative to `/api`:
//!
//! | route | |
//! | --- | --- |
//! | `GET /spaces?connection=&q=` | `Space[]` |
//! | `GET /spaces/:id` | `Space` + `recent: ChanMessage[]` (latest 30) |
//! | `PATCH /spaces/:id` | `{persona_id? (null = follow the account), duty?, rules? (null = follow the account), muted?, collab?, memory_scope?, disclosure?, item_defaults?}` → `Space` |
//! | `POST /spaces/:id/rules/to-flow` | the effective rules become a `kind=response` flow → `Space` (`flow_id`) |
//! | `DELETE /spaces/:id/flow` | back to the rules (the flow is disabled, kept) → `Space` |
//! | `GET /spaces/:id/messages?before=&limit=` | `ChanMessage[]` (limit ≤100; purged text is `null`) |
//! | `GET /chan-messages/:id/resources/:n?download=1` | the bytes of attachment `n` (an image or file of an inbound message in a space you see; [`crate::media`]); errors `{error, reason}` |
//! | `GET /chan-messages/:id/context` | what an outbound message answers, for its approval card: `{space, as_you, text, reply_to?, before[]}` |

use axum::extract::{Path, Query};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Extension, Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use steve_modules::{ApiError, ApiResult, Body, Caller, Modules};
use steve_store::base::Viewer;

use crate::spaces::{self, SpacePatch};
use crate::{Channels, messages};

type Cx = Extension<Channels>;

/// The space routes. Mount on the `/api` router behind the layer that inserts [`Caller`].
pub fn routes(ch: Channels) -> Router<Modules> {
    Router::new()
        .route("/spaces", get(list))
        .route("/spaces/{id}", get(detail).patch(patch))
        .route("/spaces/{id}/messages", get(page))
        .route("/spaces/{id}/rules/to-flow", post(to_flow))
        .route("/spaces/{id}/flow", delete(back_to_rules))
        .route("/chan-messages/{id}/context", get(context))
        .route("/chan-messages/{id}/resources/{n}", get(resource))
        .merge(crate::personas_api::routes())
        .layer(Extension(ch))
}

fn viewer(ch: &Channels, who: &Caller) -> ApiResult<Viewer> {
    ch.modules().viewer(who)
}

#[derive(Debug, Default, Deserialize)]
struct ListQ {
    connection: Option<String>,
    q: Option<String>,
}

async fn list(Extension(ch): Cx, who: Caller, Query(q): Query<ListQ>) -> ApiResult<Json<Value>> {
    let v = viewer(&ch, &who)?;
    let out = ch.store().reader(|c| -> ApiResult<Vec<spaces::SpaceOut>> {
        let trigger = spaces::your_trigger(c, &v.ws)?;
        spaces::list(
            c,
            &v,
            q.connection.as_deref().filter(|s| !s.is_empty()),
            q.q.as_deref(),
        )?
        .iter()
        .map(|s| spaces::out_with(c, s, &trigger))
        .collect()
    })?;
    Ok(Json(serde_json::to_value(out)?))
}

fn space_json(ch: &Channels, v: &Viewer, id: &str, recent: bool) -> ApiResult<Value> {
    ch.store().reader(|c| -> ApiResult<Value> {
        let s = spaces::own(c, v, id)?;
        let mut out = serde_json::to_value(spaces::out(c, &s)?)?;
        if recent {
            out["recent"] =
                serde_json::to_value(messages::page(c, &s.id, None, messages::RECENT)?)?;
        }
        Ok(out)
    })
}

async fn detail(Extension(ch): Cx, who: Caller, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let v = viewer(&ch, &who)?;
    Ok(Json(space_json(&ch, &v, &id, true)?))
}

async fn patch(
    Extension(ch): Cx,
    who: Caller,
    Path(id): Path<String>,
    Body(p): Body<SpacePatch>,
) -> ApiResult<Json<Value>> {
    let v = viewer(&ch, &who)?;
    let now = ch.now();
    ch.store()
        .transaction(|tx| spaces::patch(tx, &v, &id, &p, now))?;
    Ok(Json(space_json(&ch, &v, &id, false)?))
}

async fn to_flow(Extension(ch): Cx, who: Caller, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let v = viewer(&ch, &who)?;
    let now = ch.now();
    let nodes = ch.flow().map(|f| &f.nodes);
    ch.store()
        .transaction(|tx| crate::rules::to_flow(tx, &v, &id, nodes, now))?;
    Ok(Json(space_json(&ch, &v, &id, false)?))
}

async fn back_to_rules(
    Extension(ch): Cx,
    who: Caller,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let v = viewer(&ch, &who)?;
    let now = ch.now();
    ch.store()
        .transaction(|tx| crate::rules::back_to_rules(tx, &v, &id, now))?;
    Ok(Json(space_json(&ch, &v, &id, false)?))
}

#[derive(Debug, Default, Deserialize)]
struct PageQ {
    before: Option<String>,
    limit: Option<String>,
}

fn num(s: Option<String>, field: &str, ok: impl Fn(i64) -> bool) -> ApiResult<Option<i64>> {
    match s.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(s) => s
            .parse::<i64>()
            .ok()
            .filter(|n| ok(*n))
            .map(Some)
            .ok_or_else(|| ApiError::invalid(field)),
    }
}

async fn page(
    Extension(ch): Cx,
    who: Caller,
    Path(id): Path<String>,
    Query(q): Query<PageQ>,
) -> ApiResult<Json<Value>> {
    let before = num(q.before, "before", |n| n > 0)?;
    let limit = num(q.limit, "limit", |n| {
        (1..=messages::PAGE_MAX as i64).contains(&n)
    })?
    .map_or(messages::PAGE, |n| n as usize);
    let v = viewer(&ch, &who)?;
    let rows = ch.store().reader(|c| -> ApiResult<_> {
        let s = spaces::own(c, &v, &id)?;
        messages::page(c, &s.id, before, limit)
    })?;
    Ok(Json(json!(rows)))
}

/// `(space_id, text, reply_to, on_behalf, persona_id)` of an outbound message.
type OutRow = (
    String,
    Option<String>,
    Option<String>,
    String,
    Option<String>,
);
/// `(id, text, sender_party_id, sender_account_id, created)` of the message it answers.
type Replied = (String, Option<String>, Option<String>, Option<String>, i64);

/// Who sent a stored message, for people: the contact's name, else the account's display name.
fn sender_name(
    c: &rusqlite::Connection,
    party: Option<&str>,
    account: Option<&str>,
) -> ApiResult<String> {
    use rusqlite::OptionalExtension;
    let by_party: Option<String> = match party {
        Some(p) => c
            .query_row("SELECT name FROM parties WHERE id = ?", [p], |r| r.get(0))
            .optional()?,
        None => None,
    };
    let by_account: Option<String> = match account {
        Some(a) => c
            .query_row("SELECT display_name FROM accounts WHERE id = ?", [a], |r| {
                r.get(0)
            })
            .optional()?,
        None => None,
    };
    Ok(by_party
        .filter(|n| !n.trim().is_empty() && !n.starts_with("ou_"))
        .or(by_account.filter(|n| !n.trim().is_empty() && !n.starts_with("ou_")))
        .unwrap_or_else(|| "某人".into()))
}

/// `GET /chan-messages/:id/context`: an outbound message with what it answers (the message it
/// replies to and the three before it), where it goes and as whom, so its approval card says what
/// you are approving.
async fn context(Extension(ch): Cx, who: Caller, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    use rusqlite::OptionalExtension;
    let v = viewer(&ch, &who)?;
    let out = ch.store().reader(|c| -> ApiResult<Value> {
        let row: Option<OutRow> = c
            .query_row(
                "SELECT space_id, text, reply_to, on_behalf, persona_id FROM chan_messages WHERE id = ? AND direction = 'out'",
                [&id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()?;
        let Some((space_id, text, reply_to, on_behalf, persona_id)) = row else {
            return Err(ApiError::not_found("消息"));
        };
        let s = spaces::own(c, &v, &space_id)?;
        let space = spaces::out(c, &s)?;
        let as_you = on_behalf == "proxy"
            || (persona_id.is_none()
                && spaces::identities(c, &s.id)?
                    .iter()
                    .any(|i| i.connection_id == s.connection_id && i.mode == spaces::Mode::Own));
        let persona: Option<String> = match &persona_id {
            Some(p) => c
                .query_row("SELECT name FROM personas WHERE id = ?", [p], |r| r.get(0))
                .optional()?,
            None => None,
        };
        // the message it answers: by provider id in the same space
        let replied: Option<Replied> = match &reply_to {
            Some(r) => c
                .query_row(
                    "SELECT id, text, sender_party_id, sender_account_id, created FROM chan_messages
                      WHERE space_id = ? AND (external_id = ? OR id = ?) ORDER BY created LIMIT 1",
                    rusqlite::params![s.id, r, r],
                    |x| Ok((x.get(0)?, x.get(1)?, x.get(2)?, x.get(3)?, x.get(4)?)),
                )
                .optional()?,
            None => None,
        };
        let mut before = Vec::new();
        let reply = match &replied {
            Some((rid, rtext, party, account, created)) => {
                let mut st = c.prepare_cached(
                    "SELECT text, sender_party_id, sender_account_id, created, direction FROM chan_messages
                      WHERE space_id = ? AND created < ? AND id != ? AND text IS NOT NULL
                      ORDER BY created DESC LIMIT 3",
                )?;
                let rows = st
                    .query_map(rusqlite::params![s.id, created, rid], |x| {
                        Ok((
                            x.get::<_, Option<String>>(0)?,
                            x.get::<_, Option<String>>(1)?,
                            x.get::<_, Option<String>>(2)?,
                            x.get::<_, i64>(3)?,
                            x.get::<_, String>(4)?,
                        ))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                for (t, p, a, at, dir) in rows.into_iter().rev() {
                    let name = if dir == "out" {
                        "Steve".to_string()
                    } else {
                        sender_name(c, p.as_deref(), a.as_deref())?
                    };
                    before.push(json!({"sender": name, "text": t, "at": at}));
                }
                Some(json!({
                    "id": rid,
                    "sender": sender_name(c, party.as_deref(), account.as_deref())?,
                    "text": rtext,
                    "at": created,
                }))
            }
            None => None,
        };
        Ok(json!({
            "space": {
                "id": s.id,
                "name": space.title,
                "kind": s.kind,
                "external": s.has_external,
            },
            "as_you": as_you,
            "persona": persona,
            "text": text,
            "reply_to": reply,
            "before": before,
        }))
    })?;
    Ok(Json(out))
}

#[derive(Debug, Default, Deserialize)]
struct ResourceQ {
    download: Option<String>,
}

/// `GET /chan-messages/:id/resources/:n`: inline only for the raster types a browser shows
/// (png/jpeg/gif/webp, sniffed), else an `octet-stream` download; never cached, never sniffed,
/// sandboxed.
async fn resource(
    Extension(ch): Cx,
    who: Caller,
    Path((id, n)): Path<(String, String)>,
    Query(q): Query<ResourceQ>,
) -> Response {
    let v = match viewer(&ch, &who) {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    let fetched = match n.parse::<usize>() {
        Ok(n) => {
            crate::media::fetch_for_viewer(&ch, &v, &id, n, crate::media::Limits::default()).await
        }
        Err(_) => Err(crate::media::MediaError::NotFound),
    };
    match fetched {
        Ok(f) => resource_response(f, q.download.is_some_and(|d| d != "0")),
        Err(e) => {
            let status = StatusCode::from_u16(e.status()).unwrap_or(StatusCode::BAD_GATEWAY);
            let mut r = (
                status,
                Json(json!({"error": e.message(), "reason": e.reason()})),
            )
                .into_response();
            r.headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            r
        }
    }
}

fn resource_response(f: crate::media::Fetched, download: bool) -> Response {
    let inline = !download && f.mime.starts_with("image/");
    let disposition = format!(
        "{}; filename*=UTF-8''{}",
        if inline { "inline" } else { "attachment" },
        pct(&f.name)
    );
    let len = f.bytes.len();
    let mut r = (StatusCode::OK, f.bytes).into_response();
    let h = r.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(f.mime));
    if let Ok(v) = HeaderValue::from_str(&disposition) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    h.insert(header::CONTENT_LENGTH, HeaderValue::from(len));
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; sandbox"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    r
}

/// RFC 5987 percent-encoding of a file name.
fn pct(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}
