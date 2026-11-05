//! Mail → signal events, through [`MailEvents`]: the integrator implements it
//! on steve-flow (`ingest` for live mail; a direct `ext_events` row with a fixed outcome for mail
//! that must not run flows). [`RecordingEvents`] is the fake for tests.
//!
//! Needs `RawEvent.also` in S1: a trigger whose `types` intersect `type ∪ also` matches.

use std::sync::Mutex;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The sender of an event, already identified by the mail store.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Actor {
    /// Handle family (`email`).
    pub family: String,
    pub handle: String,
    pub display_name: Option<String>,
    pub signature_org: Option<String>,
    pub account_id: Option<String>,
    pub party_id: Option<String>,
}

/// One event (S1 `RawEvent` plus `also`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MailEvent {
    /// `mail.reply` > `mail.vip` > `mail.attachment` > `mail.new`.
    pub type_key: String,
    /// The other types that hold; always contains `mail.new`.
    pub also: Vec<String>,
    /// The `mail_messages` id (`ml_…`).
    pub external_id: String,
    /// Unix ms.
    pub occurred: i64,
    /// The thread id (`mt_…`).
    pub thread_key: Option<String>,
    /// The sender organization's main domain, else the sender address.
    pub group_key: Option<String>,
    /// The folder.
    pub scope_ext_id: Option<String>,
    pub actor: Option<Actor>,
    /// `<名字>（<组织>）：<主题>`.
    pub summary: String,
    /// `{mail_message_id, from, to, cc, subject, date, bulk, has_attachments, text}`.
    pub payload: Value,
}

/// How an event enters the signals pipeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "lowercase")]
pub enum Delivery {
    /// `steve_flow::ingest`: triggers flows.
    Ingest,
    /// Written to `ext_events` with this `outcome` and `outcome_label`, no flows: mail synced when a
    /// mailbox is connected (`recorded`, “接入时同步的旧邮件”), a muted thread (`ignored`,
    /// “这串往来已静音”).
    Fixed { outcome: String, label: String },
}

impl Delivery {
    pub fn backfill() -> Self {
        Self::Fixed {
            outcome: "recorded".into(),
            label: "接入时同步的旧邮件".into(),
        }
    }
    pub fn muted() -> Self {
        Self::Fixed {
            outcome: "ignored".into(),
            label: "这串往来已静音".into(),
        }
    }
}

/// The seam to steve-flow (S1). Returns the `ext_events` id, or `None` when the pipeline dropped
/// the event (source paused, type off). Errors are logged; the message stays stored.
#[async_trait]
pub trait MailEvents: Send + Sync {
    async fn deliver(
        &self,
        ws: &str,
        connection_id: &str,
        ev: MailEvent,
        how: Delivery,
    ) -> Result<Option<String>, String>;
}

/// Drops events (until the integrator wires steve-flow).
pub struct NoEvents;

#[async_trait]
impl MailEvents for NoEvents {
    async fn deliver(
        &self,
        _: &str,
        _: &str,
        _: MailEvent,
        _: Delivery,
    ) -> Result<Option<String>, String> {
        Ok(None)
    }
}

/// One call to [`RecordingEvents`].
#[derive(Debug, Clone, PartialEq)]
pub struct Delivered {
    pub ws: String,
    pub connection_id: String,
    pub event: MailEvent,
    pub how: Delivery,
    pub id: String,
}

/// Records every event and answers a fresh `evt_…` id; [`RecordingEvents::fail_next`] makes the
/// next delivery fail (nothing recorded).
#[derive(Debug, Default)]
pub struct RecordingEvents {
    calls: Mutex<Vec<Delivered>>,
    fail: Mutex<Option<String>>,
}

impl RecordingEvents {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn fail_next(&self, error: &str) {
        *self.fail.lock().unwrap_or_else(|e| e.into_inner()) = Some(error.into());
    }
    pub fn calls(&self) -> Vec<Delivered> {
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
    /// The events that went to `ingest`.
    pub fn ingested(&self) -> Vec<Delivered> {
        self.calls()
            .into_iter()
            .filter(|d| d.how == Delivery::Ingest)
            .collect()
    }
}

#[async_trait]
impl MailEvents for RecordingEvents {
    async fn deliver(
        &self,
        ws: &str,
        connection_id: &str,
        ev: MailEvent,
        how: Delivery,
    ) -> Result<Option<String>, String> {
        if let Some(e) = self.fail.lock().unwrap_or_else(|e| e.into_inner()).take() {
            return Err(e);
        }
        let id = steve_store::uid("evt");
        self.calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(Delivered {
                ws: ws.into(),
                connection_id: connection_id.into(),
                event: ev,
                how,
                id: id.clone(),
            });
        Ok(Some(id))
    }
}

/// The type of an incoming message and the other types that hold.
pub fn classify(reply_to_mine: bool, vip: bool, attachment: bool) -> (String, Vec<String>) {
    let mut held = Vec::new();
    if reply_to_mine {
        held.push("mail.reply");
    }
    if vip {
        held.push("mail.vip");
    }
    if attachment {
        held.push("mail.attachment");
    }
    held.push("mail.new");
    let main = held[0].to_string();
    // `also` always lists `mail.new`, also when it is the main type
    let rest = if held.len() > 1 {
        &held[1..]
    } else {
        &held[..]
    };
    let also = rest.iter().map(|s| s.to_string()).collect();
    (main, also)
}

/// Where SSE `{type:"mail", id:<thread>, ws}` goes. The integrator adds the variant to `HubEvent`
/// and publishes it; [`crate::Mail`] already coalesces a sync's changes to one per thread.
pub trait MailNotify: Send + Sync {
    fn thread(&self, ws: &str, thread_id: &str);
}

/// Drops notifications.
pub struct NoNotify;

impl MailNotify for NoNotify {
    fn thread(&self, _: &str, _: &str) {}
}

/// Adapts a closure.
pub struct FnNotify<F>(pub F);

impl<F: Fn(&str, &str) + Send + Sync> MailNotify for FnNotify<F> {
    fn thread(&self, ws: &str, thread_id: &str) {
        (self.0)(ws, thread_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_priority() {
        assert_eq!(
            classify(false, false, false),
            ("mail.new".into(), vec!["mail.new".into()])
        );
        assert_eq!(
            classify(true, true, true),
            (
                "mail.reply".into(),
                vec![
                    "mail.vip".into(),
                    "mail.attachment".into(),
                    "mail.new".into()
                ]
            )
        );
        assert_eq!(
            classify(false, false, true),
            ("mail.attachment".into(), vec!["mail.new".into()])
        );
    }
}
