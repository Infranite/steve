use super::*;
use std::ops::ControlFlow;

impl Channels {
    pub(super) async fn respond_without_grant(
        &self,
        plan: &Plan,
        mut r: Response,
    ) -> ChannelResult<Response> {
        r.act = Act::Record;
        r.label = NO_SEND_AS_YOU.into();
        if let Some(i) = &plan.trigger {
            self.react_ack(&plan.message_id).await;
            match self.converse_web(plan, i).await {
                Ok(_) => r.label = NO_SEND_AS_YOU_COMMAND.into(),
                Err(e) => {
                    tracing::warn!(
                        space = plan.space_id,
                        "command to the web conversation: {e}"
                    );
                    self.react_finish(&plan.message_id, crate::reactions::Finish::Failed)
                        .await;
                    self.conv_failed(plan, &not_answered(&format!("{NO_SEND_AS_YOU}（{e}）")));
                }
            }
        }
        return Ok(r);
    }

    pub(super) fn apply_owner_gates(
        &self,
        plan: &Plan,
        r: &mut Response,
    ) -> ChannelResult<ControlFlow<()>> {
        // you are there yourself, or the sender's later message took this one into its turn: nothing
        // is drafted or asked in your name (crate::presence, gate 1)
        if let Some(e) = self.owner_stop(plan)? {
            tracing::info!(
                space = plan.space_id,
                reason = e.reason(),
                "not drafted in your name"
            );
            r.act = Act::Record;
            r.label = e.label().into();
            return Ok(ControlFlow::Break(()));
        }
        // nothing to answer, or a nudge onto the card still open (crate::decide, G4 and G5)
        if let Some(g) = self.owner_gate(plan)? {
            r.act = Act::Record;
            match g {
                crate::decide::Gate::NoContent => r.label = crate::decide::LABEL_NO_CONTENT.into(),
                crate::decide::Gate::Nudge { decision } => {
                    self.store()
                        .transaction(|tx| crate::decide::nudge(tx, &decision, &plan.text))?;
                    r.label = crate::decide::LABEL_NUDGE.into();
                    r.decision_id = Some(decision);
                }
            }
            return Ok(ControlFlow::Break(()));
        }
        Ok(ControlFlow::Continue(()))
    }

    pub(super) async fn respond_handoff(
        &self,
        plan: &Plan,
        mut r: Response,
    ) -> ChannelResult<Response> {
        let d = self.store().transaction(|tx| open_handoff(tx, plan))?;
        r.decision_id = Some(d);
        if !plan.muted {
            let owner = self.store().read(|r| {
                Ok(self
                    .store()
                    .reader(|c| personas::owner_name_for_space(c, r, &plan.ws, &plan.space_id)))
            })??;
            let text = crate::rules::handoff_ack(&owner);
            match self.responder().acknowledge(self, plan, &text).await {
                Ok(_) => {}
                Err(e) => tracing::warn!(space = plan.space_id, "handoff ack not sent: {e}"),
            }
        }
        return Ok(r);
    }

    pub(super) async fn route_conversation(
        &self,
        plan: &Plan,
        command: bool,
        r: &mut Response,
        failed: &mut Vec<String>,
    ) -> ControlFlow<()> {
        if matches!(plan.act, Act::Reply | Act::Draft) {
            match self.converse(plan, command).await {
                Ok(Some(_)) => {
                    r.label = if command {
                        "你的指令，交给对话回答".into()
                    } else {
                        "交给对话回答".into()
                    };
                    return ControlFlow::Break(());
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!(space = plan.space_id, "conversation hand-off failed: {e}");
                    failed.push(e.to_string());
                }
            }
        }
        ControlFlow::Continue(())
    }

    pub(super) async fn respond_failed_trigger(
        &self,
        plan: &Plan,
        mut r: Response,
        failed: Vec<String>,
    ) -> ChannelResult<Response> {
        r.label = "触发语未接入，只记录".into();
        r.act = Act::Record;
        self.react_finish(&plan.message_id, crate::reactions::Finish::Failed)
            .await;
        let why = if !failed.is_empty() {
            failed.join("；")
        } else if self.thread().is_none() {
            "渠道模块没有接上 Steve 的对话".to_string()
        } else {
            "渠道的回复器没有接手这条触发词".to_string()
        };
        let said = self.conv_failed(plan, &not_answered(&why));
        // said in its thread too, as you, like any failed trigger word answered as you
        if plan.mode == Mode::Own
            && let Some(m) = said
            && let Some(out) = self.responder().outbound()
        {
            let op = crate::conversations::conv_operation(
                &m,
                m.meta["origin"].as_str().unwrap_or_default(),
                crate::conversations::OP_TRIGGER_FAILED,
            );
            match self.trigger_failed_note(&plan.ws, plan, out, op).await {
                Ok(d) => {
                    if let Err(e) = self.mark_out(&m, &d) {
                        tracing::warn!(space = plan.space_id, "failure note not marked: {e}");
                    }
                }
                Err(e) => tracing::warn!(space = plan.space_id, "failure note not sent: {e}"),
            }
        }
        return Ok(r);
    }

    pub(super) async fn respond_fallback(
        &self,
        plan: &Plan,
        mut r: Response,
        now: i64,
    ) -> ChannelResult<Response> {
        match plan.act {
            Act::Reply | Act::Draft => {
                let (m, d) = self.store().transaction(|tx| {
                    let (m, d) = open_draft(tx, plan, "", now)?;
                    // the reply you write and approve answers the message
                    crate::reactions::expect_answer(tx, &plan.message_id, &m)?;
                    Ok::<_, crate::ChannelError>((m, d))
                })?;
                r.act = Act::Draft;
                r.label = "等你确认回复".into();
                r.out_message_id = Some(m);
                r.decision_id = Some(d);
            }
            Act::CreateItem(_) => {
                let d = self.store().transaction(|tx| open_handoff(tx, plan))?;
                r.act = Act::Handoff;
                r.label = "转给你，可以转成事项".into();
                r.decision_id = Some(d);
                self.react_finish(&plan.message_id, crate::reactions::Finish::Clear)
                    .await;
            }
            Act::Record | Act::Handoff => {}
        }
        Ok(r)
    }
}
