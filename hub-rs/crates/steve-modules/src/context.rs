//! Extra prompt context (B1): what modules add to an Agent session's opening prompt and to Steve's
//! main-thread answers (knowledge snippets, contact cards). steve-core calls
//! [`PromptContexts::session`] in `startSession` and [`PromptContexts::chat`] /
//! [`PromptContexts::chat_done`] in `answer` (integrator).
//!
//! With nothing registered, or nothing found, `session` is the empty string and `chat` is `None`, so the
//! prompts stay byte for byte what they are today.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use steve_store::base::{Level, Viewer};
use steve_store::{Item, Store};

/// One numbered piece of knowledge a chat answer may cite as `[n]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cite {
    pub n: u32,
    pub label: String,
    pub href: String,
    pub level: Level,
}

/// What a provider adds to one main-thread answer.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ChatCtx {
    /// Inserted after the main Bot's prompt, before the history.
    pub text: String,
    /// The numbered sources in `text`.
    pub cites: Vec<Cite>,
}

/// What the answer becomes once the provider has seen it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ChatDone {
    /// The answer to store (e.g. with citation marks to sources that don't exist removed).
    pub text: String,
    /// Keys to merge into the reply's `messages.meta` (e.g. `cites`); `None` = add nothing.
    pub meta: Option<serde_json::Map<String, Value>>,
}

/// Extra context assembled for a session: fixed personal user context (delivered through the
/// harness's declared `append_system_context` channel, or under [`Self::USER_CONTEXT_HEADING`] in
/// the opening prompt otherwise) and task-specific context (contact cards and retrieved knowledge).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionPrompt {
    /// Fixed personal context for the requester (`me` memories and address), bounded by budget.
    pub user_context: String,
    /// Task-specific context (contact cards, entity anchors, retrieved knowledge).
    pub task_context: String,
}

impl SessionPrompt {
    /// Standard Markdown heading used when a harness does not declare native
    /// `append_system_context`.
    pub const USER_CONTEXT_HEADING: &'static str = "## User Context";

    pub fn is_empty(&self) -> bool {
        self.user_context.is_empty() && self.task_context.is_empty()
    }

    /// Formats `user_context` (under [`Self::USER_CONTEXT_HEADING`]) and `task_context` for an
    /// opening prompt. Returns `task_context` unchanged when `user_context` is empty.
    pub fn into_protocol_text(self) -> String {
        let user_block = if self.user_context.is_empty() {
            String::new()
        } else {
            format!("{}\n\n{}", Self::USER_CONTEXT_HEADING, self.user_context)
        };
        [user_block, self.task_context]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n")
    }
}

/// One provider. All methods default to adding nothing.
pub trait PromptContext: Send + Sync {
    /// Extra context for a session working on `item` (`None` or empty = nothing).
    fn session(&self, c: &Store, ws: &str, item: &Item) -> Option<SessionPrompt> {
        let _ = (c, ws, item);
        None
    }

    /// Context for answering `text` from `viewer` in the main thread.
    fn chat(&self, c: &Store, ws: &str, viewer: &Viewer, text: &str) -> Option<ChatCtx> {
        let _ = (c, ws, viewer, text);
        None
    }

    /// Called with the answer when this provider's [`ChatCtx`] was used; `message_id` is the stored reply.
    fn chat_done(
        &self,
        c: &Store,
        ws: &str,
        message_id: &str,
        answer: &str,
        cx: &ChatCtx,
    ) -> ChatDone {
        let _ = (c, ws, message_id, cx);
        ChatDone {
            text: answer.into(),
            meta: None,
        }
    }
}

/// The registered providers, asked in registration order.
#[derive(Default)]
pub struct PromptContexts(Vec<Box<dyn PromptContext>>);

impl PromptContexts {
    pub fn push(&mut self, p: Box<dyn PromptContext>) {
        self.0.push(p);
    }

    /// Assembled personal and task contexts across all providers, joined by blank lines.
    pub fn session(&self, c: &Store, ws: &str, item: &Item) -> SessionPrompt {
        let mut user = Vec::new();
        let mut task = Vec::new();
        for cx in self.0.iter().filter_map(|p| p.session(c, ws, item)) {
            if !cx.user_context.is_empty() {
                user.push(cx.user_context);
            }
            if !cx.task_context.is_empty() {
                task.push(cx.task_context);
            }
        }
        SessionPrompt {
            user_context: user.join("\n\n"),
            task_context: task.join("\n\n"),
        }
    }

    /// The first provider with something to say. Its index goes back into [`Self::chat_done`]; answers
    /// cite one numbering, so contexts are not merged.
    pub fn chat(
        &self,
        c: &Store,
        ws: &str,
        viewer: &Viewer,
        text: &str,
    ) -> Option<(usize, ChatCtx)> {
        self.0.iter().enumerate().find_map(|(i, p)| {
            p.chat(c, ws, viewer, text)
                .filter(|x| !x.text.is_empty())
                .map(|x| (i, x))
        })
    }

    /// Hands the answer to the provider that gave the context (`provider` from [`Self::chat`]).
    pub fn chat_done(
        &self,
        provider: usize,
        c: &Store,
        ws: &str,
        message_id: &str,
        answer: &str,
        cx: &ChatCtx,
    ) -> ChatDone {
        match self.0.get(provider) {
            Some(p) => p.chat_done(c, ws, message_id, answer, cx),
            None => ChatDone {
                text: answer.into(),
                meta: None,
            },
        }
    }
}
