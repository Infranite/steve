//! The two channel-message routes the channel crate has the service for but no router:
//! `POST /chan-messages/:id/recall` (C6) and `GET /chan-messages/:id/basis` (C5), relative to `/api`
//! behind the `Caller` layer. A message of a space the caller can't see is `404 没有这个消息`.

use std::sync::Arc;

use axum::extract::{Extension, Path, Query};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use rusqlite::OptionalExtension;
use serde_json::{Value, json};
use steve_channels::{Channels, Outbox};
use steve_modules::{ApiError, ApiResult, Caller, Modules};

pub fn routes(ch: Channels, outbox: Arc<Outbox>) -> Router<Modules> {
    Router::new()
        .route("/chan-messages/{id}/recall", post(recall))
        .route("/chan-messages/{id}/basis", get(basis))
        .route("/conversations", get(conversations))
        .route(
            "/conversations/{space}/preferences",
            patch(conversation_preferences),
        )
        .route(
            "/conversations/{space}/messages",
            get(conversation_messages),
        )
        .layer(Extension(outbox))
        .layer(Extension(ch))
}

/// The message exists in the caller's workspace and its space is visible to them.
fn visible(ch: &Channels, who: &Caller, id: &str) -> ApiResult<()> {
    let viewer = ch.modules().viewer(who)?;
    ch.store().reader(|c| {
        let space: Option<String> = c
            .query_row(
                "SELECT space_id FROM chan_messages WHERE id = ? AND workspace_id = ?",
                [id, &who.ws],
                |r| r.get(0),
            )
            .optional()?;
        let space = space.ok_or_else(|| ApiError::not_found("消息"))?;
        steve_channels::spaces::own(c, &viewer, &space).map_err(|_| ApiError::not_found("消息"))?;
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
            visible(&c2, &who, &id)?;
            let b = c2
                .store()
                .reader(|c| steve_channels::basis::of_message(c, &who.ws, &id))?;
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
        };
        self.0
            .say_in(ws, conv, &turn.text, &turn.meta)
            .map_err(|e| e.to_string())
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
            let list = c2.store().reader(|c| {
                steve_channels::conversations::list_page(c, &who.ws, &viewer, &query)
            })?;
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
            c2.store().transaction(|tx| {
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
