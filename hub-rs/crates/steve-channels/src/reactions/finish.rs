use super::*;
use steve_store::channel_message_repo::reactions::Reaction;

pub(super) struct FinishedReaction {
    pub(super) id: Option<String>,
    pub(super) emoji: Option<String>,
    pub(super) as_user: bool,
    pub(super) error: Option<String>,
}

pub(super) fn allowed(state: &str, how: Finish) -> bool {
    match (state, how) {
        (s, h) if s == h.state() => false,
        ("ack", _) => true,
        ("failed", Finish::Done) => true,
        // a discarded draft taken back and sent after all (or failing then): the asker still
        // learns how it ended
        ("cleared", Finish::Done | Finish::Failed) => true,
        _ => false,
    }
}

impl Channels {
    pub(super) async fn finish_provider(
        &self,
        message_id: &str,
        how: Finish,
        t: &Target,
        row: &Reaction,
    ) -> FinishedReaction {
        // Clearing an acknowledgement that the provider never accepted performs no successful
        // provider action. Retain its refusal instead of erasing the only persisted evidence.
        let mut err = if how == Finish::Clear && row.reaction_id.is_none() {
            row.error.clone()
        } else {
            None
        };
        // the space's connections were removed: nothing can be put on or taken off at the
        // provider any more, the state ends here only
        let kind = match how {
            Finish::Done => Some(ReactionKind::Done),
            Finish::Failed => Some(ReactionKind::Failed),
            Finish::Clear => None,
        };
        let added = match &t.via {
            Some((conn, msg)) => {
                if let Some(rid) = &row.reaction_id
                    && let Err(e) =
                        timed(
                            self.reactor()
                                .remove(self, &t.ws, conn, msg, rid, row.as_user),
                        )
                        .await
                {
                    tracing::info!(message = message_id, "reaction not removed: {e}");
                    err = Some(e);
                }
                match kind {
                    Some(k) => Some(timed(self.reactor().add(self, &t.ws, conn, msg, k)).await),
                    None => None,
                }
            }
            None => {
                tracing::debug!(
                    message = message_id,
                    "{} reaction recorded only: the space has no connection",
                    how.state()
                );
                None
            }
        };
        finish_result(message_id, how, row, added, err)
    }
}

fn finish_result(
    message_id: &str,
    how: Finish,
    row: &Reaction,
    added: Option<Result<Added, String>>,
    mut err: Option<String>,
) -> FinishedReaction {
    let (id, emoji, as_user) = match &added {
        Some(Ok(a)) => (
            Some(a.reaction_id.clone()),
            Some(a.emoji.clone()),
            a.as_user,
        ),
        Some(Err(e)) => {
            tracing::info!(
                message = message_id,
                "{} reaction not added: {e}",
                how.state()
            );
            err = Some(e.clone());
            (None, None, row.as_user)
        }
        None => (None, None, row.as_user),
    };
    FinishedReaction {
        id,
        emoji,
        as_user,
        error: err,
    }
}
