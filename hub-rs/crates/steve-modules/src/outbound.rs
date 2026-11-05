//! The disclosure check before anything leaves Steve. steve-channels'
//! C5 implements [`OutboundCheck`]; email drafts and GitHub comments call [`check`]. Until C5 is
//! registered the answer is [`CheckResult::unchecked`] (email shows “没有做披露检查”), never a pass.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use steve_store::base::Level;

use crate::Modules;

/// The mark the outbound path puts in front of a message YOLO approved in the user's name (YOLO user
/// decision 2026-11-01 §2): Feishu as the user and proxy sends get it at the start of the text, mail
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
}

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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Pass,
    /// Possibly reveals something (see `spans`).
    Leak,
    /// Contains a registered secret value: must not be sent at all.
    Blocked,
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
