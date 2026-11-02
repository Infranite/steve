//! The model gateway the modules call (B1): one prompt in, text out, no ACP session. steve-core's
//! `claudePrint` is wrapped into an implementation by the integrator (it reports `total_cost_usd`);
//! tests use [`crate::testkit::FakeGateway`].
//!
//! Statistics (stats.md §2.4): the gateway records every call with a purpose, a workspace and maybe an
//! item. Those come from the caller through task-locals ([`purpose`], [`tagged`], [`for_item`]) so the
//! trait signature stays as it is; a call nobody tagged is booked as `other`.

use std::future::Future;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
pub use steve_store::ModelUsage;

/// Which model list of the gateway to try (`STEVE_MODELS_FAST` / `STEVE_MODELS_SMART`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    /// Triage, checks, extraction.
    Fast,
    /// Answers, drafts, replies.
    Smart,
}

/// One gateway answer.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct GatewayOut {
    pub text: String,
    /// What the call cost, when the harness reports it; flows add it to their run's budget.
    pub cost_usd: Option<f64>,
    /// The model that answered (the first of the tier's list that worked).
    pub model: String,
    /// Tokens per model as the harness reported them (stats); empty when it reported none.
    #[serde(skip)]
    pub usage: Vec<ModelUsage>,
    /// Models of the tier tried, the one that answered included (fallback).
    #[serde(default)]
    pub attempts: u32,
    /// Wall time of the whole call, fallbacks included.
    #[serde(default)]
    pub duration_ms: u64,
}

#[async_trait]
pub trait Gateway: Send + Sync {
    /// Runs `prompt` on `tier` within `timeout`. `Err` carries a message for the log; callers decide
    /// what the user sees.
    async fn run(&self, tier: Tier, prompt: &str, timeout: Duration) -> Result<GatewayOut, String>;

    /// The smart tier's text only (the B0 call shape, kept for existing callers).
    async fn smart(&self, prompt: &str, timeout: Duration) -> Result<String, String> {
        self.run(Tier::Smart, prompt, timeout).await.map(|o| o.text)
    }
}

/// Ends the stable head of a prompt: instructions, examples and workspace context that stay the same
/// from call to call, before the part that changes every time (the message, the history).
///
/// The model gateway sends the head as the system prompt and the rest as the user message. The
/// prompt cache works on whole blocks, so a head inside the user message was never reused: every
/// route call wrote its ~9k tokens to the cache again (prod transcripts, 2026-11-02: 37% hit rate over
/// 64 gateway calls). As the system prompt it is read back at a tenth of the price.
pub const STATIC_END: &str = "\n\n<<steve:static-end>>\n\n";

/// `head` + [`STATIC_END`] + `tail`.
pub fn with_static(head: &str, tail: &str) -> String {
    format!("{head}{STATIC_END}{tail}")
}

/// The stable head (if the prompt has one) and the rest; see [`STATIC_END`].
pub fn split_static(prompt: &str) -> (Option<&str>, &str) {
    match prompt.split_once(STATIC_END) {
        Some((head, tail)) => (Some(head), tail),
        None => (None, prompt),
    }
}

/// What a gateway call is for (stats.md §2.4), set by the caller around the call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallTag {
    /// route | chat | draft | extract | compose | disclosure | intent | flow | yolo | digest | other
    pub purpose: &'static str,
    /// `""` when the caller does not know (the page books it as unassigned).
    pub workspace_id: String,
    pub item_id: Option<String>,
}

impl Default for CallTag {
    fn default() -> Self {
        CallTag {
            purpose: "other",
            workspace_id: String::new(),
            item_id: None,
        }
    }
}

tokio::task_local! {
    static TAG: CallTag;
}

/// The tag of the calls made in the current task; `other` with no workspace when nobody set one.
pub fn current_tag() -> CallTag {
    TAG.try_with(Clone::clone).unwrap_or_default()
}

/// Runs `f` with the outer tag changed by `edit` (inner scopes inherit what they do not set).
async fn with_tag<F: Future>(edit: impl FnOnce(&mut CallTag), f: F) -> F::Output {
    let mut t = current_tag();
    edit(&mut t);
    TAG.scope(t, f).await
}

/// Books the gateway calls made by `f` under `purpose`.
pub async fn purpose<F: Future>(purpose: &'static str, f: F) -> F::Output {
    with_tag(|t| t.purpose = purpose, f).await
}

/// [`purpose`] plus the workspace the calls serve.
pub async fn tagged<F: Future>(purpose: &'static str, ws: &str, f: F) -> F::Output {
    with_tag(
        |t| {
            t.purpose = purpose;
            t.workspace_id = ws.to_string();
        },
        f,
    )
    .await
}

/// Books the gateway calls made by `f` to an item (YOLO answers, a flow's item node).
pub async fn for_item<F: Future>(item_id: &str, f: F) -> F::Output {
    with_tag(|t| t.item_id = Some(item_id.to_string()), f).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn tags_nest_and_default_to_other() {
        assert_eq!(current_tag(), CallTag::default());
        assert_eq!(current_tag().purpose, "other");
        let inner = tagged("flow", "ws1", async {
            for_item("it_1", async {
                purpose("compose", async { current_tag() }).await
            })
            .await
        })
        .await;
        assert_eq!(
            inner,
            CallTag {
                purpose: "compose",
                workspace_id: "ws1".into(),
                item_id: Some("it_1".into()),
            }
        );
        assert_eq!(current_tag(), CallTag::default());
    }
}
