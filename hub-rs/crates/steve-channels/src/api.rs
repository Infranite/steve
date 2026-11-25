//! REST of spaces and messages, relative to `/api`:
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
//! | `GET /chan-messages/:id/open-url` | `{url}`: the link that opens it (or its chat) in its platform's own client, `null` when there is none or its space is not yours |

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
        .route("/chan-messages/{id}/open-url", get(open_url))
        .route("/chan-messages/{id}/authorization", post(authorization))
        .route("/chan-messages/{id}/resources/{n}", get(resource))
        .merge(crate::personas_api::routes())
        .layer(Extension(ch))
}

fn viewer(ch: &Channels, who: &Caller) -> ApiResult<Viewer> {
    ch.modules().viewer(who)
}

/// Asked when a card about a chat message is shown, not with every list of cards.
async fn open_url(
    Extension(ch): Cx,
    who: Caller,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let v = viewer(&ch, &who)?;
    let ids = [id.clone()];
    let mut links = crate::open_link::message_links(ch.store(), &v, &ids)?;
    let link = links.remove(&id);
    Ok(Json(json!({
        "url": link.as_ref().map(|l| l.url.as_str()),
        "provider": link.as_ref().map(|l| l.provider.as_str()),
    })))
}

async fn authorization(
    Extension(ch): Cx,
    who: Caller,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    Ok(Json(serde_json::to_value(crate::recovery::propose(
        &ch, &who, &id,
    )?)?))
}

#[derive(Debug, Default, Deserialize)]
struct ListQ {
    connection: Option<String>,
    q: Option<String>,
}

async fn list(Extension(ch): Cx, who: Caller, Query(q): Query<ListQ>) -> ApiResult<Json<Value>> {
    let v = viewer(&ch, &who)?;
    let store = ch.store();
    let out = store.read(|r| -> ApiResult<Vec<spaces::SpaceOut>> {
        steve_store::channel_spaces::rows::list(
            r,
            &v.ws,
            q.connection.as_deref().filter(|s| !s.is_empty()),
            q.q.as_deref(),
        )?
        .iter()
        .map(|s| spaces::out(r, s))
        .collect()
    })?;
    Ok(Json(serde_json::to_value(out)?))
}

fn space_json(ch: &Channels, v: &Viewer, id: &str, recent: bool) -> ApiResult<Value> {
    let store = ch.store();
    store.read(|r| {
        let s = steve_store::channel_spaces::rows::get_in_workspace(r, &v.ws, id)?
            .ok_or_else(|| ApiError::not_found("空间"))?;
        let mut out = serde_json::to_value(spaces::out(r, &s)?)?;
        if recent {
            out["recent"] = serde_json::to_value(messages::page(
                store,
                &s.id,
                &v.user,
                None,
                messages::RECENT,
            )?)?;
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
    ch.store().write(|tx| spaces::patch(tx, &v, &id, &p, now))?;
    Ok(Json(space_json(&ch, &v, &id, false)?))
}

async fn to_flow(Extension(ch): Cx, who: Caller, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let v = viewer(&ch, &who)?;
    let now = ch.now();
    let nodes = ch.flow().map(|f| &f.nodes);
    ch.store()
        .write(|tx| crate::rules::to_flow(tx, &v, &id, nodes, now))?;
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
        .write(|tx| crate::rules::back_to_rules(tx, &v, &id, now))?;
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
    let store = ch.store();
    let rows = store.read(|r| -> ApiResult<_> {
        let s = steve_store::channel_spaces::rows::get_in_workspace(r, &v.ws, &id)?
            .ok_or_else(|| ApiError::not_found("空间"))?;
        messages::page(store, &s.id, &v.user, before, limit)
    })?;
    Ok(Json(json!(rows)))
}

/// Who sent a stored message, for people: the contact's name, else the account's display name.
fn sender_name(
    db: &mut impl steve_store::Read,
    party: Option<&str>,
    account: Option<&str>,
) -> ApiResult<String> {
    let by_party = match party {
        Some(id) => steve_store::parties::Parties::new(db).name(id)?,
        None => None,
    };
    let by_account = match account {
        Some(id) => steve_store::accounts::display_name(db, id)?,
        None => None,
    };
    let visible_name = |n: &String| !crate::context::raw_name(n);
    Ok(by_party
        .filter(visible_name)
        .or(by_account.filter(visible_name))
        .unwrap_or_else(|| "某人".into()))
}

/// `GET /chan-messages/:id/context`: an outbound message with what it answers (the message it
/// replies to and the three before it), where it goes and as whom, so its approval card says what
/// you are approving.
async fn context(Extension(ch): Cx, who: Caller, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    use steve_store::{channel_message_repo::approval_context as repo, channel_personas};
    let v = viewer(&ch, &who)?;
    let store = ch.store();
    let out = store.read(|db| -> ApiResult<Value> {
        let draft = repo::draft(db, &id)?.ok_or_else(|| ApiError::not_found("消息"))?;
        let s = steve_store::channel_spaces::rows::get_in_workspace(db, &v.ws, &draft.space_id)?
            .ok_or_else(|| ApiError::not_found("空间"))?;
        let space = spaces::out(db, &s)?;
        let as_you = draft.on_behalf == "proxy"
            || (draft.persona_id.is_none()
                && spaces::identities(db, &s.id)?
                    .iter()
                    .any(|i| i.connection_id == s.connection_id && i.mode == spaces::Mode::Own));
        let persona = match &draft.persona_id {
            Some(id) => channel_personas::name(db, id)?,
            None => None,
        };
        let replied = match draft.reply_to.as_deref() {
            Some(reference) => repo::replied_to(db, &s.id, reference)?,
            None => None,
        };
        let mut before = Vec::new();
        let mut reply_to_sender: Option<String> = None;
        let reply = match replied {
            Some(replied) => {
                for msg in repo::preceding(db, &s.id, &replied, 3)?.into_iter().rev() {
                    let name = if msg.out {
                        "Steve".to_string()
                    } else {
                        sender_name(
                            db,
                            msg.sender_party_id.as_deref(),
                            msg.sender_account_id.as_deref(),
                        )?
                    };
                    before.push(json!({"sender": name, "text": msg.text, "at": msg.created}));
                }
                let sender = sender_name(
                    db,
                    replied.sender_party_id.as_deref(),
                    replied.sender_account_id.as_deref(),
                )?;
                reply_to_sender = Some(sender.clone());
                Some(json!({
                    "id": replied.id,
                    "sender": sender,
                    "text": replied.text,
                    "at": replied.created,
                }))
            }
            None => None,
        };
        let ai_mark = if as_you {
            if draft.on_behalf == "proxy" {
                spaces::channel_settings(db, &v.ws, &s.connection_id)?
                    .map(|cs| crate::rules::Trigger::from_value(&cs.trigger).mark)
                    .unwrap_or(true)
            } else {
                spaces::effective_ai_mark(db, &v.ws, &s.id, &s.connection_id).unwrap_or(true)
            }
        } else {
            false
        };
        let composed = steve_store::channel_message_repo::outbound::composer_result(db, &id)?;
        let why = composed["why"]
            .as_str()
            .map(str::trim)
            .filter(|w| !w.is_empty())
            .map(String::from)
            .or_else(|| {
                composed["reason"]
                    .as_str()
                    .and_then(crate::decide::Reason::parse)
                    .map(|r| r.label().to_string())
            })
            .or_else(|| (draft.on_behalf == "proxy").then(|| "按你的触发词代发".into()));
        let basis_row = crate::basis::of_message(db, &v.ws, &id)?;
        let disclosure = basis_row.map(|b| {
            json!({
                "verdict": b.verdict,
                "spans": b.spans,
            })
        });
        let burst_id = steve_store::channel_message_repo::outbound::burst_id_of(db, &id)?;
        let open = crate::open_link::space_link(db, &s)?;
        Ok(json!({
            "space": {
                "id": s.id,
                "name": space.title,
                "kind": s.kind,
                "external": s.has_external,
                "member_count": space.member_count,
            },
            "recipient": {
                "space_id": s.id,
                "space_name": space.title,
                "kind": s.kind,
                "member_count": space.member_count,
                "has_external": s.has_external,
                "reply_to_sender": reply_to_sender,
            },
            "speaker": {
                "as_you": as_you,
                "mode": if as_you { "self" } else { "bot" },
                "name": persona.as_deref().unwrap_or(if as_you { "你本人" } else { "Bot" }),
            },
            "as_you": as_you,
            "persona": persona,
            "text": draft.text,
            "ai_mark": ai_mark,
            "why": why,
            "disclosure": disclosure,
            "burst_id": burst_id,
            "open_url": open.as_ref().map(|l| l.url.as_str()),
            "open_provider": open.as_ref().map(|l| l.provider.as_str()),
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
