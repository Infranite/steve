//! Extra prompt context (B1): what modules add to an Agent session's opening prompt and to Steve's
//! main-thread answers (knowledge snippets, contact cards). steve-core calls
//! [`PromptContexts::session`] in `startSession` and [`PromptContexts::chat`] /
//! [`PromptContexts::chat_done`] in `answer` (integrator).
//!
//! With nothing registered, or nothing found, `session` is the empty string and `chat` is `None`, so the
//! prompts stay byte for byte what they are today.

use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use steve_store::Item;
use steve_store::base::{Level, Viewer};

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

/// One provider. All methods default to adding nothing.
pub trait PromptContext: Send + Sync {
    /// Text for the opening prompt of a session working on `item` (`""` or `None` = nothing).
    fn session(&self, c: &Connection, ws: &str, item: &Item) -> Option<String> {
        let _ = (c, ws, item);
        None
    }

    /// Context for answering `text` from `viewer` in the main thread.
    fn chat(&self, c: &Connection, ws: &str, viewer: &Viewer, text: &str) -> Option<ChatCtx> {
        let _ = (c, ws, viewer, text);
        None
    }

    /// Called with the answer when this provider's [`ChatCtx`] was used; `message_id` is the stored reply.
    fn chat_done(
        &self,
        c: &Connection,
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

    /// The non-empty session texts joined by a blank line; `""` when there are none.
    pub fn session(&self, c: &Connection, ws: &str, item: &Item) -> String {
        self.0
            .iter()
            .filter_map(|p| p.session(c, ws, item))
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    /// The first provider with something to say. Its index goes back into [`Self::chat_done`]; answers
    /// cite one numbering, so contexts are not merged.
    pub fn chat(
        &self,
        c: &Connection,
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
        c: &Connection,
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
