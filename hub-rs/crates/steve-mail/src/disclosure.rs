//! Shared preview/send checks. A preview is informational; every send checks again on its final
//! audience and wire body. Nothing here authorizes a send or caches a passing verdict.

use std::time::Duration;

use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use steve_modules::outbound::{self, Audience, CheckCx, CheckResult, KnowledgeUse, Span, Verdict};

use crate::Mail;
use crate::drafts::audience_of_draft;
use crate::error::MailResult;
use crate::repo::DraftRow;

/// Bound the registered checker as well as its model gateway. No provider submission has started
/// when this expires; a timeout is a definite hold, never an ambiguous send or permission to send.
pub const CHECK_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, PartialEq, Serialize)]
pub(crate) struct Inputs {
    audience: Audience,
    knowledge: Vec<KnowledgeUse>,
    complete: bool,
    draft_model: Option<String>,
}

pub(crate) fn inputs(
    store: &steve_store::Store,
    c: &mut impl steve_store::Read,
    d: &DraftRow,
) -> MailResult<Inputs> {
    let entries = d
        .basis
        .as_ref()
        .and_then(|b| b["entries"].as_array())
        .cloned()
        .unwrap_or_default();
    let (knowledge, complete) =
        store.read(|c| steve_knowledge::retrieve::knowledge_again(c, &d.workspace_id, &entries))?;
    let mut draft_model = d
        .basis
        .as_ref()
        .and_then(|b| b["answer_model"].as_str())
        .filter(|m| !m.trim().is_empty())
        .map(str::to_string);
    // MCP drafts are written by an agent rather than the compose gateway. Use its recorded model
    // when available; an absent session/model is not evidence of a particular drafting model.
    if draft_model.is_none()
        && let Some(session) = &d.session_id
    {
        draft_model = steve_store::session_repo::effective_model(c, &d.workspace_id, session)?;
    }
    Ok(Inputs {
        audience: audience_of_draft(c, d)?,
        knowledge,
        complete,
        draft_model,
    })
}

/// Includes display names and every envelope recipient, the final AI mark, attachment references
/// and check inputs. This is separate from the existing approval hash (whose contract is unchanged).
pub(crate) fn checked_hash(d: &DraftRow, inputs: &Inputs) -> String {
    let value = json!([
        d.workspace_id,
        d.connection_id,
        d.thread_id,
        d.reply_to_message,
        d.mode,
        d.to,
        d.cc,
        d.bcc,
        d.subject,
        d.body,
        d.files,
        inputs
    ]);
    hex::encode(Sha256::digest(value.to_string().as_bytes()))
}

/// Keep drafting metadata and source references, including references that could not be read this
/// time. Losing one must not make the next check/retry silently pass with less knowledge.
pub(crate) fn basis(d: &DraftRow, check: &CheckResult) -> Value {
    let mut b = json!(check);
    if let Some(old) = &d.basis {
        for key in ["why", "needs_attention", "cites", "answer_model"] {
            if let Some(value) = old.get(key) {
                b[key] = value.clone();
            }
        }
        if let Some(old_entries) = old["entries"].as_array() {
            let entries = b["entries"]
                .as_array_mut()
                .expect("serialized check entries");
            for e in old_entries {
                if !entries.iter().any(|n| {
                    n["source_type"] == e["source_type"]
                        && n["source_id"] == e["source_id"]
                        && n["seq"] == e["seq"]
                }) {
                    entries.push(e.clone());
                }
            }
        }
    }
    b
}

impl Mail {
    pub(crate) async fn full_check(&self, d: &DraftRow) -> MailResult<(CheckResult, Inputs)> {
        let inputs = self.0.m.store.read(|c| inputs(&self.0.m.store, c, d))?;
        let cx = CheckCx {
            ws: d.workspace_id.clone(),
            actor: d.author.clone(),
            draft_model: inputs.draft_model.clone(),
            skip_model: false,
            internal_skip: false,
            ..Default::default()
        };
        let text = format!("{}\n\n{}", d.subject, d.body);
        let mut check = match tokio::time::timeout(
            CHECK_TIMEOUT,
            outbound::check(&self.0.m, &cx, &inputs.audience, &text, &inputs.knowledge),
        )
        .await
        {
            Ok(r) => r,
            Err(_) => {
                let mut r = CheckResult::unchecked();
                r.spans.push(Span {
                    text: String::new(),
                    reason: steve_i18n::t!("mail.disclosure.timeout").to_string(),
                });
                r
            }
        };
        if check.verdict == Verdict::Pass {
            // A local-only pass or a check run on the known drafting model is not a second-model
            // disclosure check, even if a faulty/unavailable integration labels it "pass".
            let model = check
                .check_model
                .as_deref()
                .filter(|m| !m.trim().is_empty());
            if model.is_none() || model == inputs.draft_model.as_deref() {
                check.verdict = Verdict::Unchecked;
                check.spans.push(Span {
                    text: String::new(),
                    reason: steve_i18n::t!("mail.disclosure.incomplete").to_string(),
                });
            } else if !check.spans.is_empty() {
                check.verdict = Verdict::Leak;
            }
        }
        if !inputs.complete {
            check.verdict = Verdict::Leak;
            check.spans.push(Span {
                text: String::new(),
                reason: outbound::R_BASIS_GONE.into(),
            });
        }
        Ok((check, inputs))
    }
}
