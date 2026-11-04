//! `mail.events`: the consumer of [`EVENT_TOPIC`] (a stored mail's signal event, emitted in the
//! transaction that stored the mail). Delivered at least once after the commit; what makes a
//! repetition harmless is the delivery itself: steve-flow answers the same connection and
//! `external_id` (the mail's `ml_…`) with the event it already has, and the mail's `event_id` is
//! written again with that id.

use std::sync::Arc;

use steve_store::outbox::{Applied, ConsumeError, Consumer, Event, Since};

use crate::Mail;
use crate::repo;
use crate::store::{EVENT_TOPIC, EVENT_V, PendingEvent};

pub struct MailEventsOut {
    mail: Mail,
}

impl MailEventsOut {
    pub const ID: &'static str = "mail.events";

    pub fn new(mail: Mail) -> Arc<Self> {
        Arc::new(Self { mail })
    }
}

fn pending(ev: &Event) -> Result<PendingEvent, ConsumeError> {
    if ev.version != EVENT_V {
        return Err(ConsumeError::Reject(format!(
            "{} version {}",
            ev.topic, ev.version
        )));
    }
    serde_json::from_value(ev.payload.clone())
        .map_err(|e| ConsumeError::Reject(format!("{}: {e}", ev.topic)))
}

impl Consumer for MailEventsOut {
    fn id(&self) -> &'static str {
        Self::ID
    }

    fn topic(&self) -> &'static str {
        EVENT_TOPIC
    }

    /// Shipped with its topic: nothing emitted before the first registration is skipped.
    fn since(&self) -> Since {
        Since::Start
    }

    /// The delivery (async: the dispatcher runs consumers on a blocking thread of the Hub's
    /// runtime) and the mail's link to its event.
    fn effect(&self, ev: &Event) -> Result<(), ConsumeError> {
        let p = pending(ev)?;
        let rt = tokio::runtime::Handle::try_current()
            .map_err(|_| ConsumeError::Retry("没有运行时".into()))?;
        let id = rt
            .block_on(self.mail.0.events.deliver(
                &ev.workspace_id,
                &p.connection_id,
                p.event,
                p.how,
            ))
            .map_err(ConsumeError::Retry)?;
        if let Some(id) = id {
            self.mail
                .0
                .m
                .store
                .transaction(|tx| repo::set_event(tx, &p.message_id, &id))
                .map_err(|e| ConsumeError::Retry(e.to_string()))?;
        }
        Ok(())
    }

    fn apply(&self, _tx: &steve_store::Tx, ev: &Event) -> Result<Applied, ConsumeError> {
        Ok(Applied::noted(pending(ev)?.message_id))
    }
}
