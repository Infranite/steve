//! Audience resolution and persistence for a single model answer; no extra model calls.
use super::*;

pub(super) struct PendingReply {
    pub all: bool,
    pub to: Vec<crate::repo::Rcpt>,
    pub cc: Vec<crate::repo::Rcpt>,
    pub subject: String,
    pub body: String,
    pub md: ModelDraft,
    pub answer_model: String,
}

impl Mail {
    pub(super) async fn quick_audience(
        &self,
        ws: String,
        rc: Vec<crate::repo::Rcpt>,
    ) -> MailResult<Audience> {
        let m = self.clone();
        tokio::task::spawn_blocking(move || {
            m.0.m.store.read(|c| {
                Ok((|| -> MailResult<Audience> {
                    let mut a = Audience::default();
                    let mut names = Vec::new();
                    for r in &rc {
                        let (party, internal) = audience_of(c, &ws, &r.addr)?;
                        a.external |= !internal;
                        if let Some(p) = party {
                            a.parties.push(p);
                        }
                        names.push(if r.name.is_empty() {
                            r.addr.clone()
                        } else {
                            r.name.clone()
                        });
                    }
                    a.description = format!(
                        "邮件收件人：{}，{}",
                        names.join("、"),
                        if a.external { "外部" } else { "内部" }
                    );
                    Ok(a)
                })())
            })?
        })
        .await
        .map_err(|e| MailError::Other(format!("task failed: {e}")))?
    }

    pub(super) fn persist_quick_reply(
        &self,
        who: &Caller,
        q: &QuickDraft,
        g: &Gathered,
        reply: PendingReply,
        check: &steve_modules::outbound::CheckResult,
    ) -> MailResult<DraftOut> {
        let PendingReply {
            all,
            to,
            cc,
            subject,
            body,
            md,
            answer_model,
        } = reply;
        let now = self.now();
        let d = DraftRow {
            id: uid("md"),
            workspace_id: who.ws.clone(),
            connection_id: g.connection_id.clone(),
            thread_id: Some(q.thread_id.clone()),
            reply_to_message: Some(g.orig.id.clone()),
            mode: if all { "reply_all" } else { "reply" }.into(),
            to,
            cc,
            bcc: Vec::new(),
            subject,
            body,
            files: Vec::new(),
            author: g.author.clone(),
            item_id: q.item_id.clone(),
            session_id: q.session_id.clone(),
            run_id: q.run_id.clone(),
            decision_id: None,
            basis: None,
            content_hash: String::new(),
            status: "draft".into(),
            send_at: None,
            sent_message: None,
            error: None,
            created: now,
            updated: now,
            sent: None,
        };
        let mut d = self.insert_pending(d, check)?;
        // why / needs_attention / cites ride along on the basis (the card shows them with it), and the
        // model that wrote it (a later check uses another one)
        if let Some(Value::Object(b)) = d.basis.as_mut() {
            b.insert("why".into(), Value::String(md.why.clone()));
            b.insert("answer_model".into(), Value::String(answer_model.clone()));
            b.insert("needs_attention".into(), Value::Bool(md.needs_attention));
            b.insert(
                "cites".into(),
                serde_json::to_value(&md.cites).unwrap_or(Value::Null),
            );
        }
        let (hits, query) = (g.hits.clone(), g.query.clone());
        self.0.m.store.transaction(|tx| -> MailResult<()> {
            tx.prepare_cached("UPDATE mail_drafts SET basis = ? WHERE id = ?")?
                .execute(rusqlite::params![
                    d.basis.as_ref().map(Value::to_string),
                    d.id
                ])?;
            if !hits.is_empty() {
                tx.write(|w| record_use(w, &d.workspace_id, ("draft", &d.id), &query, &hits))?;
            }
            Ok(())
        })?;
        self.get_draft(who, &d.id)
    }
}
