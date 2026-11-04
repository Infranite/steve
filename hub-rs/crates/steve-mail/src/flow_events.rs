//! [`MailEvents`] over steve-flow's `ingest` (email.md §3.5 step 6): the bridge the integrator would
//! otherwise write. [`Mail`](crate::Mail) is built before the flow module (whose mail actions hold
//! it), so the bridge is bound afterwards and holds the module weakly (no reference cycle).
//!
//! ```ignore
//! let events = steve_mail::flow_events::FlowEvents::new();
//! let mail = Mail::builder(..).events(Arc::new(events.clone())).build();
//! let flow = steve_mail::nodes::register(setup, &mail).build(modules, Arc::new(GraphEngine));
//! events.bind(&flow);
//! ```

use std::sync::{Arc, OnceLock, Weak};

use async_trait::async_trait;
use rusqlite::params;
use steve_flow::{ActorRef, FlowCx, RawEvent};

use crate::events::{Delivery, MailEvent, MailEvents};

#[derive(Clone, Default)]
pub struct FlowEvents(Arc<OnceLock<Weak<FlowCx>>>);

impl FlowEvents {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn bind(&self, flow: &Arc<FlowCx>) {
        let _ = self.0.set(Arc::downgrade(flow));
    }
}

/// The steve-flow event of a mail event.
pub fn raw_event(ev: MailEvent, how: &Delivery) -> RawEvent {
    let mut r = RawEvent::new("mail", ev.type_key, ev.external_id, ev.occurred, ev.summary);
    r.also = ev.also;
    r.thread_key = ev.thread_key;
    r.group_key = ev.group_key;
    r.scope_ext_id = ev.scope_ext_id;
    r.payload = ev.payload;
    r.actor = ev.actor.map(|a| match (a.account_id, a.party_id) {
        (Some(account_id), Some(party_id)) => ActorRef::Known {
            account_id,
            party_id,
        },
        _ => ActorRef::Handle {
            system: "mail".into(),
            handle: a.handle,
            display_name: a.display_name,
            signature_org: a.signature_org,
        },
    });
    if let Delivery::Fixed { label, .. } = how {
        r.record_only = Some(label.clone());
    }
    r
}

#[async_trait]
impl MailEvents for FlowEvents {
    async fn deliver(
        &self,
        ws: &str,
        connection_id: &str,
        ev: MailEvent,
        how: Delivery,
    ) -> Result<Option<String>, String> {
        let flow = self
            .0
            .get()
            .and_then(Weak::upgrade)
            .ok_or("flow module not bound")?;
        let raw = raw_event(ev, &how);
        let got = steve_flow::ingest(&flow, ws, Some(connection_id), raw)
            .await
            .map_err(|e| e.to_string())?;
        // `record_only` stores `recorded`; a muted thread's mail is `ignored`. Also on a duplicate:
        // a repeated delivery (the outbox's at-least-once) completes what a crash cut short.
        if let (Delivery::Fixed { outcome, .. }, Some(id)) = (&how, got.id())
            && outcome != "recorded"
        {
            flow.store()
                .with_conn(|c| {
                    // only over the stored default: what happened to the event since stays
                    c.execute(
                        "UPDATE ext_events SET outcome = ? WHERE id = ? AND outcome = 'recorded'",
                        params![outcome, id],
                    )?;
                    Ok(())
                })
                .map_err(|e| e.to_string())?;
        }
        Ok(got.id().map(String::from))
    }
}
