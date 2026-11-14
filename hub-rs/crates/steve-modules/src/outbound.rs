//! The disclosure check before anything leaves Steve. steve-channels'
//! C5 implements [`OutboundCheck`]; email drafts and GitHub comments call [`check`]. Until C5 is
//! registered the answer is [`CheckResult::unchecked`] (email shows “没有做披露检查”), never a pass.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use steve_store::base::Level;

use crate::Modules;

/// The mark the outbound path puts in front of a message YOLO approved in the user's name:
/// Feishu as the user and proxy sends get it at the start of the text, mail
/// on the first line of the body; the subject is not changed.
pub const AI_PREFIX: &str = "[AI] ";

/// The actor of everything YOLO decides (`Caller.me`, `decisions.result.decided_by`, audit).
pub const YOLO_ACTOR: &str = "yolo";

/// `[AI] ` + the text, exactly once.
pub fn with_ai_prefix(text: &str) -> String {
    format!("{AI_PREFIX}{}", strip_ai_prefix(text))
}

/// Removes a leading `[AI] ` the text brought with it, so the mark only ever comes from the outbound
/// path: a model can neither fake it on a message the user approved nor double it.
pub fn strip_ai_prefix(text: &str) -> &str {
    let mut t = text;
    while let Some(rest) = t.trim_start().strip_prefix("[AI]") {
        t = rest.strip_prefix(' ').unwrap_or(rest);
    }
    t
}

/// Opens text somebody other than the user wrote, carried into an agent's goal (a mail, an issue,
/// other people's messages in a group).
pub const UNTRUSTED_OPEN: &str = "<<<外部内容开始>>>";
/// Closes it.
pub const UNTRUSTED_CLOSE: &str = "<<<外部内容结束>>>";

/// `text` between [`UNTRUSTED_OPEN`] and [`UNTRUSTED_CLOSE`], on lines of their own; markers inside
/// the text are defused first, so it cannot close the quote itself.
pub fn quote_untrusted(text: &str) -> String {
    let t = text
        .replace(UNTRUSTED_OPEN, "<外部内容开始>")
        .replace(UNTRUSTED_CLOSE, "<外部内容结束>");
    format!("{UNTRUSTED_OPEN}\n{t}\n{UNTRUSTED_CLOSE}")
}

/// The settled decision's `result` says YOLO answered it (`decided_by: "yolo"`).
pub fn decided_by_yolo(result: &serde_json::Value) -> bool {
    result.get("decided_by").and_then(serde_json::Value::as_str) == Some(YOLO_ACTOR)
}

/// Who will read the text.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Audience {
    /// Anyone outside the company or the user (a customer group, an outside recipient, a public repo).
    pub external: bool,
    /// The readers' accounts, when known.
    #[serde(default)]
    pub accounts: Vec<String>,
    /// Their contacts / organizations, when known.
    #[serde(default)]
    pub parties: Vec<String>,
    /// One line for the check model (“Acme 的两位联系人，外部”).
    #[serde(default)]
    pub description: String,
    /// The visibility scopes the audience is inside (`space:<id>`, `team:<id>`, `workspace`): knowledge
    /// visible only within these counts as public for this audience.
    #[serde(default)]
    pub scopes: Vec<String>,
}

/// A piece of knowledge the text was written with.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KnowledgeUse {
    pub source_type: String,
    pub source_id: String,
    pub source_label: String,
    pub level: Level,
    /// The visibility code of the source (`workspace`, `private`, `team:<id>`, `space:<id>`).
    pub visibility: String,
    pub text: String,
    /// The index piece (`kb_chunks.seq`) of a retrieved hit, so the check can read the same text again
    /// later ([`BasisEntry::seq`]); `None` for what was not retrieved (a memory shown by id).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<i64>,
}

/// Reason of the span a later check adds when knowledge the basis names cannot be read again: the text
/// is not checked against all of what it was written with, so it does not pass.
pub const R_BASIS_GONE: &str = "原来用到的知识有的已经读不到，没能全部重新检查";

/// Who asks, for the basis record and the models' cost.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CheckCx {
    pub ws: String,
    /// `me`, `steve` or a bot id.
    pub actor: String,
    /// The model that wrote the text; the check uses a different one.
    pub draft_model: Option<String>,
    /// What the audience itself just said to the sender (the message this text answers), as context
    /// for the check only: it tells what they already know, it is not knowledge, grants no level and
    /// changes no rule. `None` when the text answers nothing (email, comments, a new message).
    pub audience_said: Option<String>,
    /// Only the local checks (secrets, never-level knowledge, the rule check), no check model: a fixed
    /// receipt no model wrote, or an email draft the model checks only on request. A result without
    /// spans then has no `check_model`.
    pub skip_model: bool,
    /// The text goes out under a bot's own identity (not yours, not on your behalf) and the knowledge
    /// given is everything its writer was given: then, for an audience made only of the workspace's own
    /// people to whom every piece of it (at least one) is public, the model check is skipped. Off, the
    /// model looks whatever the knowledge.
    pub internal_skip: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Pass,
    /// Possibly reveals something (see `spans`).
    Leak,
    /// No check is installed.
    Unchecked,
}

/// A suspicious part of the text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Span {
    pub text: String,
    pub reason: String,
}

/// How one knowledge entry ended up in the text (`reply_basis.entries`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BasisEntry {
    pub source_type: String,
    pub source_id: String,
    pub source_label: String,
    pub level: Level,
    /// The level for this audience (public when the audience is inside the source's scope).
    pub effective: Level,
    pub used: bool,
    /// `verbatim|conclusion|reasoning|blocked`.
    pub out: String,
    /// [`KnowledgeUse::seq`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<i64>,
}

/// The result; serialized as the `basis` of an email draft (`{verdict, entries, spans}`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CheckResult {
    pub verdict: Verdict,
    #[serde(default)]
    pub entries: Vec<BasisEntry>,
    #[serde(default)]
    pub spans: Vec<Span>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check_model: Option<String>,
}

impl CheckResult {
    pub fn unchecked() -> Self {
        Self {
            verdict: Verdict::Unchecked,
            entries: Vec::new(),
            spans: Vec::new(),
            check_model: None,
        }
    }
}

#[async_trait]
pub trait OutboundCheck: Send + Sync {
    /// Checks `text` for `audience`. Never rewrites: rewriting is the caller's choice (channels do,
    /// email doesn't). Failures of the check model come back as `Leak` with a span saying so, so a
    /// broken check never lets text through unchecked.
    async fn check(
        &self,
        cx: &CheckCx,
        audience: &Audience,
        text: &str,
        knowledge: &[KnowledgeUse],
    ) -> CheckResult;
}

/// Runs the registered check, or answers [`CheckResult::unchecked`].
pub async fn check(
    m: &Modules,
    cx: &CheckCx,
    audience: &Audience,
    text: &str,
    knowledge: &[KnowledgeUse],
) -> CheckResult {
    match &m.registry.outbound {
        Some(c) => c.check(cx, audience, text, knowledge).await,
        None => CheckResult::unchecked(),
    }
}

/// Platform-neutral content. Rich cards/HTML belong to the connector adapter; all adapters can
/// receive the lossless Markdown fallback, including actions as labelled links. No model rewrites
/// or content masking happen here.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Content {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub markdown: String,
    #[serde(default)]
    pub fields: Vec<(String, String)>,
    #[serde(default)]
    pub actions: Vec<(String, String)>,
    #[serde(default)]
    pub references: Vec<(String, String)>,
}

impl Content {
    pub fn markdown(&self) -> String {
        let mut parts = Vec::new();
        if !self.title.is_empty() {
            parts.push(format!("# {}", self.title));
        }
        if !self.markdown.is_empty() {
            parts.push(self.markdown.clone());
        }
        parts.extend(self.fields.iter().map(|(k, v)| format!("{k}: {v}")));
        parts.extend(
            self.actions
                .iter()
                .chain(&self.references)
                .map(|(label, url)| format!("{label}: {url}")),
        );
        parts.join("\n\n")
    }
}

/// Routing never chooses an identity for a caller. `Bot` must resolve to a bot account; `AsYou`
/// to a personal account owned by the live requester, then the existing outbound approvals apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Identity {
    Bot,
    AsYou,
}

/// One semantic send, independent of its platform representation. `source_message_id` defaults a
/// reply to its original channel. `space_id` is an explicit channel switch, not a hint to a model.
/// For a person, routing only considers known active DMs containing that person's platform alias.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryIntent {
    pub key: String,
    pub identity: Identity,
    pub content: Content,
    #[serde(default)]
    pub source_message_id: Option<String>,
    #[serde(default)]
    pub space_id: Option<String>,
    #[serde(default)]
    pub party_id: Option<String>,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub urgency: String,
}
