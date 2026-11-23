//! steve-core and the Hub as the module crates need them: the
//! session sink of agent-facing tools, the item wrap-up of GitHub and mail, the attachment store,
//! and the hooks that turn item and Bot changes into module events.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use steve_core::Core;
use steve_modules::hooks::ItemHooks;
use steve_store::Item;

use crate::backend::Backend;

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| e.to_string())?
}

// ---------------------------------------------------------------- sessions

/// steve-core's “send to the item's session” (§6.5) for the vault's `secret_use` answers and the
/// GitHub tools' answers.
pub struct CoreSessions(pub Core);

#[async_trait]
impl steve_modules::vault::SessionSink for CoreSessions {
    async fn send(&self, _ws: &str, item_id: Option<&str>, session_id: Option<&str>, text: &str) {
        let (c, item, sid, text) = (
            self.0.clone(),
            item_id.map(String::from),
            session_id.map(String::from),
            text.to_string(),
        );
        let r = blocking(move || {
            c.send_to_session(item.as_deref(), sid.as_deref(), &text)
                .map_err(|e| e.to_string())
        })
        .await;
        if let Err(e) = r {
            tracing::warn!("send to session: {e}");
        }
    }
}

/// Marks a session as waiting for the user's approval (after a module tool opened one).
pub fn awaiting_approval(core: &Core, session_id: &str) {
    let (c, sid) = (core.clone(), session_id.to_string());
    let run = move || {
        if let Err(e) = c.mark_awaiting_approval(&sid) {
            tracing::warn!(session = %sid, "awaiting approval: {e}");
        }
    };
    // decision handlers call this inside their transaction: do it after, off the caller's lock
    match tokio::runtime::Handle::try_current() {
        Ok(h) => {
            h.spawn_blocking(run);
        }
        Err(_) => run(),
    }
}

/// E3/E5: what steve-core does for drafts.
pub struct MailDraftHooks(pub Core);

impl steve_mail::DraftHooks for MailDraftHooks {
    /// The model's disclosure check runs as a draft is written only while YOLO decides for it.
    fn yolo_on(&self, ws: &str, item_id: Option<&str>) -> bool {
        self.0.yolo_applies(ws, item_id)
    }
    fn rejected(&self, ws: &str, session_id: &str, item_id: Option<&str>, message: &str) {
        let sink = CoreSessions(self.0.clone());
        let (ws, sid, item, msg) = (
            ws.to_string(),
            session_id.to_string(),
            item_id.map(String::from),
            message.to_string(),
        );
        if let Ok(h) = tokio::runtime::Handle::try_current() {
            h.spawn(async move {
                use steve_modules::vault::SessionSink;
                sink.send(&ws, item.as_deref(), Some(&sid), &msg).await;
            });
        }
    }
    fn sent(
        &self,
        ws: &str,
        item_id: &str,
        _subject: &str,
        version: Option<steve_types::DeliveryVersion>,
    ) {
        let (c, ws, id) = (self.0.clone(), ws.to_string(), item_id.to_string());
        let run = move || {
            if let Err(e) = c.module_complete(&ws, &id, version, "mail.sent") {
                tracing::warn!(item = %id, "mail sent, completion: {e}");
            }
        };
        match tokio::runtime::Handle::try_current() {
            Ok(h) => {
                h.spawn_blocking(run);
            }
            Err(_) => run(),
        }
    }
    fn awaiting_approval(&self, _ws: &str, session_id: &str, _item_id: Option<&str>) {
        awaiting_approval(&self.0, session_id);
    }
}

// ---------------------------------------------------------------- items

/// Applies verified GitHub delivery to its current item cycle.
pub struct GhItems(pub Core);

#[async_trait]
impl steve_github::core::ItemCore for GhItems {
    async fn delivery_verified(&self, proof: steve_types::DeliveryProof) -> Result<(), String> {
        let core = self.0.clone();
        blocking(move || {
            core.delivery_verified(&proof)
                .map_err(|error| error.to_string())
        })
        .await
    }
}

/// E5 `to-item`: an item from a mail thread.
pub struct MailItems(pub Core);

#[async_trait]
impl steve_mail::items::MailItems for MailItems {
    async fn create_item(&self, req: steve_mail::items::ItemRequest) -> Result<String, String> {
        let c = self.0.clone();
        blocking(move || {
            c.create_item(steve_core::CreateItem {
                ws: req.ws,
                goal: req.goal,
                title: req.title,
                bot: req.bot,
                source: Some(req.source),
                propose: req.propose,
                actor: Some(req.by),
                ..Default::default()
            })
            .map(|i| i.id)
            .map_err(|e| e.to_string())
        })
        .await
    }
}

/// E5: a mail attachment landed as a Hub attachment (`files` + blob), like an upload.
pub struct HubFiles {
    pub backend: Arc<dyn Backend>,
    pub data_dir: PathBuf,
}

impl steve_mail::attach::FileStore for HubFiles {
    fn store(
        &self,
        ws: &str,
        bytes: &[u8],
        name: &str,
        by: &str,
    ) -> Result<steve_store::FileRef, steve_mail::attach::Refused> {
        let refused = |status: u16, m: String| steve_mail::attach::Refused::new(status, m);
        let w = self.backend.writer().map_err(|e| refused(500, e.message))?;
        let f = crate::files::store(w.as_ref(), &self.data_dir, ws, bytes, Some(name), by)
            .map_err(|e| refused(e.status, e.message))?;
        serde_json::from_value(crate::files::file_ref(&f)).map_err(|e| refused(500, e.to_string()))
    }
}

// ---------------------------------------------------------------- item and Bot events

/// S10: a dropped item becomes an `item.dropped` event (flows can follow up), spawned after the
/// lifecycle step. `item.done` comes through the outbox instead (`steve_flow::ItemTrigger`).
#[derive(Clone, Default)]
pub struct FlowItemEvents(pub Arc<OnceLock<Arc<steve_flow::FlowCx>>>);

impl FlowItemEvents {
    pub fn bind(&self, flow: Arc<steve_flow::FlowCx>) {
        let _ = self.0.set(flow);
    }

    fn spawn(&self, item: &Item, ev: steve_flow::ItemEvent) {
        let Some(flow) = self.0.get().cloned() else {
            return;
        };
        let (ws, id) = (item.workspace_id.clone(), item.id.clone());
        if let Ok(h) = tokio::runtime::Handle::try_current() {
            h.spawn(async move {
                if let Err(e) = steve_flow::ingest_item_event(&flow, &ws, &id, ev).await {
                    tracing::warn!(item = %id, "item event: {e}");
                }
            });
        }
    }
}

#[async_trait]
impl ItemHooks for FlowItemEvents {
    fn name(&self) -> &'static str {
        "flow"
    }
    async fn on_dropped(&self, item: &Item) {
        self.spawn(item, steve_flow::ItemEvent::Dropped);
    }
}

/// C2 + S10: personas and flow `bot` nodes follow a deleted Bot to its heir.
pub struct BotHeirs;

impl steve_modules::BotDeleted for BotHeirs {
    fn on_bot_deleted(
        &self,
        tx: &mut steve_store::W<'_>,
        ws: &str,
        bot: &str,
        heir: &str,
        now: i64,
    ) -> steve_modules::ApiResult<()> {
        steve_store::channel_personas::reassign_bot(tx, ws, bot, heir, now)?;
        steve_flow::on_bot_deleted(tx, ws, bot, heir, now)?;
        Ok(())
    }
}

/// S10: the flow module's report notes for `/汇报`.
#[derive(Clone, Default)]
pub struct FlowDigest(pub Arc<OnceLock<Arc<steve_flow::FlowCx>>>);

impl steve_modules::DigestNotes for FlowDigest {
    fn take(&self, ws: &str) -> Vec<String> {
        let Some(flow) = self.0.get() else {
            return Vec::new();
        };
        match steve_flow::digest::take_now(flow, ws, 0) {
            Ok(v) => v.into_iter().map(|e| e.text).collect(),
            Err(e) => {
                tracing::warn!("digest notes: {e}");
                Vec::new()
            }
        }
    }
}
