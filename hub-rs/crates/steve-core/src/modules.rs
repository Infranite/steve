//! The B1 seams into steve-core: item lifecycle hooks, extra prompt context,
//! `before_undo` of module decisions, and the shared model gateway. Nothing here changes what the core
//! does while no [`steve_modules::Modules`] is attached or nothing is registered.
//!
//! Hooks are async and the core's lifecycle runs synchronously under its state lock, so a critical
//! section only records what happened ([`HookEvent`]); when the section ends ([`Tx`]'s `Drop`) the
//! events go to one worker task per core, which calls the hooks in order, after the writes are
//! committed and without the lock.

use crate::error::Result;
use crate::gateway::SharedGateway;
use crate::repo::{Decision, Item};
use crate::{Core, Inner, Tx};
use std::sync::{Arc, Weak};
use steve_modules::Modules;
use steve_modules::context::{ChatCtx, ChatDone};
use tokio::sync::mpsc;

/// One lifecycle point, with the item as it was right after the step's own writes.
#[derive(Debug, Clone)]
pub(crate) enum HookEvent {
    Created(Item),
    Finished(Item),
    Dropped(Item),
}

/// Which lifecycle point to record for an item (see [`Tx::hook`]).
#[derive(Debug, Clone, Copy)]
pub(crate) enum Point {
    Created,
    Finished,
    Dropped,
}

/// The hook worker's queue; set the first time a section records an event.
pub(crate) type HookQueue = std::sync::OnceLock<mpsc::UnboundedSender<HookEvent>>;

impl Core {
    /// Attaches the module registries (once, at startup, before traffic). Later calls are ignored with a
    /// warning: the registries are immutable once built.
    pub fn attach_modules(&self, m: Modules) {
        if self.0.modules.set(m).is_err() {
            tracing::warn!("modules already attached");
        }
    }

    /// The attached modules, if any.
    pub fn modules(&self) -> Option<&Modules> {
        self.0.modules.get()
    }

    /// This core's model gateway as the modules' [`steve_modules::Gateway`]: build [`Modules`] with it so
    /// core and modules share one gateway (tiers, fallback, reported model and cost).
    pub fn module_gateway(&self) -> Arc<dyn steve_modules::Gateway> {
        Arc::new(SharedGateway(self.0.gateway.clone()))
    }

    /// Extra context for a session's opening prompt (`""` = none).
    pub(crate) fn session_context(&self, item: &Item) -> String {
        let Some(m) = self.modules() else {
            return String::new();
        };
        let cx = m.registry.contexts();
        cx.session(self.store().store(), &item.workspace_id, item)
    }

    /// Extra context for a main-thread answer to `user`'s `text`: the first provider's, with its index.
    pub(crate) fn chat_context(
        &self,
        ws: &str,
        user: Option<&str>,
        text: &str,
    ) -> Option<(usize, ChatCtx)> {
        let m = self.modules()?;
        let cx = m.registry.contexts();
        // Without a known user the answer sees what the workspace may see.
        let viewer = steve_store::base::Viewer::bare(ws, user.unwrap_or_default());
        cx.chat(self.store().store(), ws, &viewer, text)
    }

    /// Hands the stored reply `message_id` back to the provider whose context it used, and applies what
    /// the provider made of it (text, extra `meta` keys) to the stored message.
    pub(crate) fn chat_done(
        &self,
        ws: &str,
        provider: usize,
        cx: &ChatCtx,
        message_id: &str,
        answer: &str,
    ) -> Result<()> {
        let Some(m) = self.modules() else {
            return Ok(());
        };
        let contexts = m.registry.contexts();
        let store = self.store().store();
        let ChatDone { text, meta } =
            contexts.chat_done(provider, store, ws, message_id, answer, cx);
        if text == answer && meta.is_none() {
            return Ok(());
        }
        store.write(|w| {
            let Some(message) = self.store().message(message_id)? else {
                return Ok(());
            };
            let mut stored = message.meta;
            if let (Some(extra), serde_json::Value::Object(object)) = (meta, &mut stored) {
                object.extend(extra);
            }
            let metadata = serde_json::to_string(&stored)?;
            steve_store::core_queue::edit_message(w, ws, message_id, &text, Some(&metadata))?;
            w.emit(steve_types::DomainEvent::from(
                steve_types::WorkEvent::Message {
                    space: message.space.into(),
                    ws: ws.into(),
                    id: steve_types::MessageId::from(message_id.to_owned()),
                },
            ));
            Ok(())
        })
    }

    /// Calls the hooks for one event; runs on the hook worker.
    async fn run_hook(&self, e: HookEvent) {
        let Some(m) = self.modules() else {
            return;
        };
        let hooks = m.registry.hooks();
        match e {
            HookEvent::Created(i) => hooks.created(&i).await,
            HookEvent::Finished(i) => hooks.finished(&i).await,
            HookEvent::Dropped(i) => hooks.dropped(&i).await,
        }
    }
}

/// Starts the hook worker on `rt`. It holds the core weakly, so it ends with the core.
fn worker(rt: &tokio::runtime::Handle, inner: Weak<Inner>) -> mpsc::UnboundedSender<HookEvent> {
    let (tx, mut rx) = mpsc::unbounded_channel::<HookEvent>();
    rt.spawn(async move {
        while let Some(e) = rx.recv().await {
            let Some(inner) = inner.upgrade() else {
                return;
            };
            // A hook that panics loses its call. The worker still delivers the next committed event.
            let core = Core(inner);
            let run = tokio::spawn(async move { core.run_hook(e).await });
            if let Err(err) = run.await {
                tracing::error!("item hook: {err}");
            }
        }
    });
    tx
}

impl Tx<'_> {
    /// Some module registered item hooks.
    pub(crate) fn hooked(&self) -> bool {
        self.c
            .modules()
            .is_some_and(|m| !m.registry.hooks().is_empty())
    }

    /// Records a lifecycle point of `item_id` for the hooks (nothing without hooks).
    pub(crate) fn hook(&mut self, p: Point, item_id: &str) -> Result<()> {
        if !self.hooked() {
            return Ok(());
        }
        let Some(i) = self.db().item(item_id)? else {
            return Ok(());
        };
        self.st.hooks.push(match p {
            Point::Created => HookEvent::Created(i),
            Point::Finished => HookEvent::Finished(i),
            Point::Dropped => HookEvent::Dropped(i),
        });
        Ok(())
    }

    /// `undoDecision` of a decision whose `subject_type` a module handles: the module's `before_undo`,
    /// the reopen and the audit in one transaction ([`steve_modules::decisions::undo`]); a refusal rolls
    /// it back and comes out as [`crate::CoreError::Module`] with its own status. Returns the decision as
    /// it was, or `None` when no module handles it (core undoes it itself).
    pub(crate) fn module_undo(
        &self,
        d: &Decision,
        actor: &str,
        undoable: impl FnOnce(&Decision) -> bool,
    ) -> Result<Option<Decision>> {
        let Some(m) = self.c.modules() else {
            return Ok(None);
        };
        if !m.registry.handles_decision(&d.subject_type) {
            return Ok(None);
        }
        match steve_modules::decisions::undo(m, &d.id, actor, undoable) {
            Some(r) => Ok(Some(r?)),
            None => Ok(None),
        }
    }
}

impl Drop for Tx<'_> {
    /// Hands the section's hook events to the worker (in order, after the section's writes).
    fn drop(&mut self) {
        /* lifecycle stops: cancel outside the lock, then look at the closing again */
        let cancels = std::mem::take(&mut self.st.cancels);
        if !cancels.is_empty() {
            let core = self.c.clone();
            let run = move || {
                let mut items = vec![];
                for (turn, item) in cancels {
                    // the turn frozen when the stop was asked for: a newer one is left running
                    if let Err(e) = core.0.runtime.stop(&turn) {
                        tracing::warn!(session = turn.session, item, "stopping the turn: {e}");
                    }
                    if !items.contains(&item) {
                        items.push(item);
                    }
                }
                for item in items {
                    if let Err(e) = core.tx().evaluate_cycle_close(&item) {
                        tracing::warn!(item, "closing after cancel: {e}");
                    }
                }
            };
            match tokio::runtime::Handle::try_current() {
                Ok(rt) => {
                    rt.spawn_blocking(run);
                }
                // not joined: the thread waits for the lock this section still holds
                Err(_) => {
                    std::thread::spawn(run);
                }
            }
        }
        if self.st.hooks.is_empty() {
            return;
        }
        let events = std::mem::take(&mut self.st.hooks);
        let Ok(rt) = tokio::runtime::Handle::try_current() else {
            tracing::warn!("item hooks need a tokio runtime; {} skipped", events.len());
            return;
        };
        let q = self
            .c
            .0
            .hook_queue
            .get_or_init(|| worker(&rt, Arc::downgrade(&self.c.0)));
        for e in events {
            if q.send(e).is_err() {
                tracing::warn!("item hook worker is gone; event dropped");
            }
        }
    }
}
