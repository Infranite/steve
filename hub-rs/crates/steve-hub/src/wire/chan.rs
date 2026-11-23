//! The two channel-message routes the channel crate has the service for but no router:
//! `POST /chan-messages/:id/recall` (C6) and `GET /chan-messages/:id/basis` (C5), relative to `/api`
//! behind the `Caller` layer. A message of a space the caller can't see is `404 没有这个消息`.

use std::sync::Arc;

use axum::extract::{Extension, Path, Query};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use serde_json::{Value, json};
use steve_channels::{Channels, Outbox};
use steve_modules::{ApiError, ApiResult, Caller, Modules};

pub fn routes(ch: Channels, outbox: Arc<Outbox>) -> Router<Modules> {
    Router::new()
        .route(
            "/me/notification-policy",
            get(notification_policy).put(set_notification_policy),
        )
        .route("/me/notification-targets", get(notification_targets))
        .route("/me/notifications", get(personal_notifications))
        .route("/me/notifications/{id}/seen", post(notification_seen))
        .route("/deliveries", post(submit_delivery))
        .route("/deliveries/{id}", get(delivery))
        .route(
            "/people/{party}/output-preference",
            patch(output_preference),
        )
        .route("/chan-messages/{id}/recall", post(recall))
        .route("/chan-messages/{id}/basis", get(basis))
        .route("/conversations", get(conversations))
        .route("/conversations/channels", get(conversation_channels))
        .route(
            "/conversations/{space}/preferences",
            patch(conversation_preferences),
        )
        .route(
            "/conversations/{space}/messages",
            get(conversation_messages),
        )
        .route("/conversations/{space}/record", get(conversation_record))
        .route("/conversations/{space}/read", post(conversation_read))
        .layer(Extension(outbox))
        .layer(Extension(ch))
}

/// The message exists in the caller's workspace and its space is visible to them.
fn visible(ch: &Channels, who: &Caller, id: &str) -> ApiResult<()> {
    let viewer = ch.modules().viewer(who)?;
    ch.store().read(|r| {
        let space = steve_store::channel_message_repo::space_of(r, &who.ws, id)?
            .ok_or_else(|| ApiError::not_found("消息"))?;
        steve_store::channel_spaces::rows::get_in_workspace(r, &viewer.ws, &space)?
            .ok_or_else(|| ApiError::not_found("消息"))?;
        Ok(())
    })
}

async fn recall(
    Extension(ch): Extension<Channels>,
    Extension(outbox): Extension<Arc<Outbox>>,
    who: Caller,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let (c2, w2, i2) = (ch.clone(), who.clone(), id.clone());
    ch.modules()
        .blocking(move |_| visible(&c2, &w2, &i2))
        .await?;
    outbox.recall(&ch, &who.ws, &who.me, &id).await?;
    Ok(Json(json!({"ok": true})))
}

async fn basis(
    Extension(ch): Extension<Channels>,
    who: Caller,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let c2 = ch.clone();
    ch.modules()
        .blocking(move |_| {
            let b = c2.store().read(|r| {
                visible(&c2, &who, &id)?;
                steve_channels::basis::of_message(r, &who.ws, &id)
            })?;
            Ok(Json(serde_json::to_value(b).map_err(ApiError::from)?))
        })
        .await
}

/// steve-core's thread as the channel module's conversation port (channels §2.12).
pub struct CoreThread(pub steve_core::Core);

impl steve_channels::conversations::Thread for CoreThread {
    fn say(&self, ws: &str, turn: steve_channels::conversations::Turn) -> Result<String, String> {
        let conv = steve_core::thread::Conv {
            space: turn.space,
            research: turn.research,
            items: turn.items,
            owner_only: turn.owner_only,
            start: turn.start,
            hands_on: turn.hands_on,
        };
        self.0
            .say_in_as(ws, conv, &turn.text, &turn.meta, turn.user.as_deref())
            .map_err(|e| e.to_string())
    }
}

/// steve-core's YOLO switch for the channel module: a channel card has no item, the workspace's decides.
pub struct ChanYolo(pub steve_core::Core);

impl steve_channels::YoloSwitch for ChanYolo {
    fn on(&self, ws: &str) -> bool {
        self.0.yolo_applies(ws, None)
    }
}

/// The channel module as the owner of steve-core's guest conversations.
pub struct ChanConvHost(pub steve_channels::WeakChannels);

#[async_trait::async_trait]
impl steve_core::thread::ConvHost for ChanConvHost {
    async fn answer(&self, ws: &str, _space: &str, origin: &str) -> Result<String, String> {
        let ch = self
            .0
            .upgrade()
            .ok_or_else(|| "渠道模块已停止".to_string())?;
        ch.conv_answer(ws, origin).await
    }

    fn research_context(&self, ws: &str, space: &str, origin: &str) -> String {
        self.0
            .upgrade()
            .map(|ch| ch.conv_research_context(ws, space, origin))
            .unwrap_or_default()
    }

    async fn research_answer(
        &self,
        ws: &str,
        _space: &str,
        origin: &str,
        text: &str,
    ) -> Result<String, String> {
        let ch = self
            .0
            .upgrade()
            .ok_or_else(|| "渠道模块已停止".to_string())?;
        ch.conv_research_answer(ws, origin, text).await
    }

    fn said(&self, ws: &str, space: &str, id: &str) {
        if let Some(ch) = self.0.upgrade() {
            ch.conv_said(ws, space, id);
        }
    }
}

/// `GET /conversations`: the IM conversations of the caller's workspace (spaces they can see),
/// newest first, with their badge (provider, space, group or DM).
async fn conversations(
    Extension(ch): Extension<Channels>,
    who: Caller,
    Query(query): Query<steve_channels::conversations::ListQuery>,
) -> ApiResult<Json<Value>> {
    let c2 = ch.clone();
    ch.modules()
        .blocking(move |_| {
            let viewer = c2.modules().viewer(&who)?;
            let list = steve_channels::conversations::list_page(
                c2.store(),
                &who.ws,
                &viewer,
                &query,
                c2.now(),
            )?;
            Ok(Json(serde_json::to_value(list).map_err(ApiError::from)?))
        })
        .await
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ConversationPreferences {
    closed: bool,
}

async fn conversation_preferences(
    Extension(ch): Extension<Channels>,
    who: Caller,
    Path(space): Path<String>,
    Json(body): Json<ConversationPreferences>,
) -> ApiResult<Json<Value>> {
    let c2 = ch.clone();
    ch.modules()
        .blocking(move |_| {
            let viewer = c2.modules().viewer(&who)?;
            c2.store().write(|tx| {
                steve_channels::conversations::set_closed(tx, &who.ws, &viewer, &space, body.closed)
            })?;
            Ok(Json(
                json!({"ok": true, "space": space, "closed": body.closed}),
            ))
        })
        .await
}

/// `GET /conversations/:space/messages`: one IM conversation (`chan:<space>`), oldest first, each
/// inbound turn with the reaction state of its chat message (`reaction`).
async fn conversation_messages(
    Extension(ch): Extension<Channels>,
    who: Caller,
    Path(space): Path<String>,
) -> ApiResult<Json<Value>> {
    let c2 = ch.clone();
    ch.modules()
        .blocking(move |_| {
            let viewer = c2.modules().viewer(&who)?;
            let v = steve_channels::conversations::messages_of(&c2, &who.ws, &viewer, &space)?;
            Ok(Json(v))
        })
        .await
}

/// `GET /conversations/channels`: the channel switcher of 对话, the channel accounts the caller sees
/// with how many of their conversations have new messages.
async fn conversation_channels(
    Extension(ch): Extension<Channels>,
    who: Caller,
) -> ApiResult<Json<Value>> {
    let c2 = ch.clone();
    ch.modules()
        .blocking(move |_| {
            let viewer = c2.modules().viewer(&who)?;
            let list = steve_channels::conv_record::channels(&c2, c2.store(), &viewer, c2.now())?;
            Ok(Json(json!({ "channels": list })))
        })
        .await
}

/// `GET /conversations/:space/record`: the space's channel messages with what Steve did with each,
/// a page at a time (`before`, `before_id`, `limit`), oldest first.
async fn conversation_record(
    Extension(ch): Extension<Channels>,
    who: Caller,
    Path(space): Path<String>,
    Query(query): Query<steve_channels::conv_record::RecordQuery>,
) -> ApiResult<Json<Value>> {
    let c2 = ch.clone();
    ch.modules()
        .blocking(move |_| {
            let viewer = c2.modules().viewer(&who)?;
            let v =
                steve_channels::conv_record::record(c2.store(), &who.ws, &viewer, &space, &query)?;
            Ok(Json(v))
        })
        .await
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ConversationRead {
    /// the newest message time the caller was shown
    at: i64,
}

/// `POST /conversations/:space/read`: the caller's read marker of the conversation moves to `at`.
async fn conversation_read(
    Extension(ch): Extension<Channels>,
    who: Caller,
    Path(space): Path<String>,
    Json(body): Json<ConversationRead>,
) -> ApiResult<Json<Value>> {
    let c2 = ch.clone();
    ch.modules()
        .blocking(move |_| {
            let viewer = c2.modules().viewer(&who)?;
            let now = c2.now();
            c2.store().write(|tx| {
                steve_channels::conv_record::mark_read(tx, &who.ws, &viewer, &space, body.at, now)
            })?;
            Ok(Json(json!({"ok": true})))
        })
        .await
}

/// YOLO's `recall` undo: the same recall as `POST /chan-messages/:id/recall`, from the executor's
/// blocking thread.
pub struct ChanRecall {
    pub ch: Channels,
    pub outbox: Arc<Outbox>,
}

impl steve_core::yolo::undo::Recall for ChanRecall {
    fn recall(&self, ws: &str, actor: &str, message_id: &str) -> ApiResult<()> {
        let h = tokio::runtime::Handle::try_current()
            .map_err(|_| ApiError::Internal("no runtime for the recall".into()))?;
        h.block_on(self.outbox.recall(&self.ch, ws, actor, message_id))?;
        Ok(())
    }
}

/// Same pipeline used by the existing source-reply tool; keys belong to the authenticated user.
async fn submit_delivery(
    Extension(ch): Extension<Channels>,
    Extension(outbox): Extension<Arc<Outbox>>,
    who: Caller,
    Json(intent): Json<steve_modules::outbound::DeliveryIntent>,
) -> ApiResult<Json<Value>> {
    Ok(Json(serde_json::to_value(
        outbox.submit(&ch, &who, intent).await?,
    )?))
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct OutputPreference {
    space_id: Option<String>,
}

async fn output_preference(
    Extension(ch): Extension<Channels>,
    who: Caller,
    Path(party): Path<String>,
    Json(pref): Json<OutputPreference>,
) -> ApiResult<Json<Value>> {
    let c2 = ch.clone();
    ch.modules()
        .blocking(move |_| {
            steve_channels::pipeline::set_preference(&c2, &who, &party, pref.space_id.as_deref())?;
            Ok(Json(json!({"ok":true})))
        })
        .await
}

async fn delivery(
    Extension(ch): Extension<Channels>,
    who: Caller,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let c2 = ch.clone();
    ch.modules()
        .blocking(move |_| {
            Ok(Json(serde_json::to_value(steve_channels::pipeline::get(
                &c2, &who, &id,
            )?)?))
        })
        .await
}

async fn notification_policy(
    Extension(ch): Extension<Channels>,
    who: Caller,
) -> ApiResult<Json<Value>> {
    Ok(Json(serde_json::to_value(
        steve_channels::notifications::get_policy(&ch, &who)?,
    )?))
}

async fn set_notification_policy(
    Extension(ch): Extension<Channels>,
    who: Caller,
    Json(policy): Json<steve_channels::notifications::Policy>,
) -> ApiResult<Json<Value>> {
    steve_channels::notifications::put_policy(&ch, &who, &policy)?;
    Ok(Json(json!({"ok":true})))
}

async fn notification_targets(
    Extension(ch): Extension<Channels>,
    who: Caller,
) -> ApiResult<Json<Value>> {
    Ok(Json(
        json!({"targets": steve_channels::notifications::targets(&ch, &who)?}),
    ))
}

async fn personal_notifications(
    Extension(ch): Extension<Channels>,
    who: Caller,
) -> ApiResult<Json<Value>> {
    Ok(Json(serde_json::to_value(
        steve_channels::notifications::list(&ch, &who)?,
    )?))
}

async fn notification_seen(
    Extension(ch): Extension<Channels>,
    who: Caller,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    steve_channels::notifications::seen(&ch, &who, &id)?;
    Ok(Json(json!({"ok":true})))
}
