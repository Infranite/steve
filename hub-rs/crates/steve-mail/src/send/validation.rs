//! Recheck final disclosure and authority in the existing store transaction.
use super::*;

impl Mail {
    pub(super) fn revalidate_send(
        &self,
        d: &DraftRow,
        stored: &DraftRow,
        approval: &Decision,
        check: &steve_modules::outbound::CheckResult,
        inputs: &crate::disclosure::Inputs,
    ) -> MailResult<Option<SendOutcome>> {
        let checked_hash = crate::disclosure::checked_hash(d, inputs);
        let mut basis = crate::disclosure::basis(d, check);
        basis["checked_content_hash"] = json!(checked_hash);
        self.0.m.store.transaction(|tx| -> MailResult<Option<SendOutcome>> {
            let current = repo::draft(tx, &d.id)?;
            let decision = base::decisions::get(tx, &approval.id)?;
            let unchanged = current.as_ref() == Some(stored)
                && decision.as_ref().is_some_and(|a| {
                    a.status == "resolved" && a.result == approval.result
                })
                && crate::disclosure::inputs(&self.0.m.store, tx, d)? == *inputs;
            if !unchanged {
                self.blocked(tx, d, "disclosure_changed", json!({"checked_content_hash": checked_hash}))?;
                return Ok(Some(SendOutcome::Blocked {
                    reason: steve_i18n::t!("mail.disclosure.changed").to_string(),
                }));
            }
            tx.execute("UPDATE mail_drafts SET basis = ? WHERE id = ?", rusqlite::params![basis.to_string(), d.id])?;
            base::audit(tx, &d.workspace_id, "steve", "mail.send.check", &d.id,
                &json!({"verdict": check.verdict, "check_model": check.check_model, "content_hash": d.content_hash, "checked_content_hash": checked_hash, "decision": approval.id}))?;
            if check.verdict != Verdict::Pass {
                self.blocked(tx, d, "disclosure", json!({"verdict": check.verdict, "checked_content_hash": checked_hash}))?;
                return Ok(Some(SendOutcome::Blocked {
                    reason: steve_i18n::t!("mail.disclosure.held").to_string(),
                }));
            }
            // Authorization may change while the model is running. A paused connection waits as it
            // did before; a revoked capability/connection cannot use the earlier permission.
            match may_write_in(tx, &d.workspace_id, &d.connection_id, Some(SEND_CAP))? {
                Ok(()) => Ok(None),
                Err(WriteRefusal::Later(_)) => {
                    repo::draft_transition(tx, &d.id, &["sending"], "approved", self.now())?;
                    Ok(Some(SendOutcome::Skipped))
                }
                Err(WriteRefusal::Never(why)) => {
                    self.blocked(tx, d, "connection", json!({"why": why}))?;
                    Ok(Some(SendOutcome::Blocked { reason: why }))
                }
            }
        })
    }
}
