//! Durable snapshots, independent of the lossy live bus. The storage envelope (workspace, sequence,
//! schema version and creation time) remains Wave 2's responsibility. Topic/payload are explicit here.
//! Wave 2 will type the opaque source, mail event and GitHub signal snapshots; they are never read
//! through a JSON tree in this crate. Polymorphic object/owner/subject ids remain strings.

use crate::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, strum::EnumCount)]
#[serde(tag = "topic", content = "payload")]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum DurableFact {
    #[serde(rename = "work.item.lifecycle")]
    WorkItemLifecycle {
        item_id: ItemId,
        cycle: i64,
        revision: i64,
        transition: String,
        phase: String,
        outcome: Option<String>,
        acceptance: Option<String>,
        reason: String,
    },
    #[serde(rename = "work.item.done")]
    WorkItemDone {
        item_id: ItemId,
        title: String,
        status: String,
        bot_id: Option<BotId>,
        project_id: Option<ProjectId>,
        source: RawJson,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cycle: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        revision: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        acceptance: Option<String>,
    },
    #[serde(rename = "mail.event.ready")]
    MailEventReady {
        // Mail message ids and main-conversation MessageId have different domains.
        message_id: String,
        connection_id: ConnectionId,
        event: RawJson,
        how: MailDelivery,
    },
    #[serde(rename = "github.after")]
    GithubAfter {
        connection: ConnectionId,
        finish: Vec<(ItemId, String)>,
        signals: Vec<RawJson>,
        #[serde(default)]
        supplements: Vec<PrSupplement>,
    },
    #[serde(rename = "kb.object.changed")]
    KnowledgeObjectChanged { obj_type: String, obj_id: String },
    #[serde(rename = "todo.due")]
    TodoDue(AlarmFired),
    #[serde(rename = "snooze.wake")]
    SnoozeWake(AlarmFired),
    #[serde(rename = "briefing.ready")]
    BriefingReady(AlarmFired),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "lowercase")]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum MailDelivery {
    Ingest,
    Fixed { outcome: String, label: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct PrSupplement {
    pub item: ItemId,
    pub external_id: String,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct AlarmFired {
    pub owner: String,
    // Existing alarm kind, not its topic (todo / snooze / brief).
    pub kind: String,
    pub alarms: Vec<String>,
    pub lines: Vec<AlarmLine>,
    pub late: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<Millis>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct AlarmLine {
    pub subject: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub planned_for: Option<Millis>,
}

impl<'de> Deserialize<'de> for DurableFact {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        crate::raw::deserialize_with(d, |text| {
            #[derive(Deserialize)]
            struct Tag {
                topic: String,
            }
            #[derive(Deserialize)]
            struct Payload<T> {
                payload: T,
            }
            let tag: Tag = serde_json::from_str(text)?;
            match tag.topic.as_str() {
                "work.item.lifecycle" => {
                    #[derive(Deserialize)]
                    struct Fields {
                        item_id: ItemId,
                        cycle: i64,
                        revision: i64,
                        transition: String,
                        phase: String,
                        outcome: Option<String>,
                        acceptance: Option<String>,
                        reason: String,
                    }
                    let f: Payload<Fields> = serde_json::from_str(text)?;
                    Ok(Self::WorkItemLifecycle {
                        item_id: f.payload.item_id,
                        cycle: f.payload.cycle,
                        revision: f.payload.revision,
                        transition: f.payload.transition,
                        phase: f.payload.phase,
                        outcome: f.payload.outcome,
                        acceptance: f.payload.acceptance,
                        reason: f.payload.reason,
                    })
                }
                "work.item.done" => {
                    #[derive(Deserialize)]
                    struct Fields {
                        item_id: ItemId,
                        title: String,
                        status: String,
                        bot_id: Option<BotId>,
                        project_id: Option<ProjectId>,
                        source: RawJson,
                        #[serde(default, skip_serializing_if = "Option::is_none")]
                        cycle: Option<i64>,
                        #[serde(default, skip_serializing_if = "Option::is_none")]
                        revision: Option<i64>,
                        #[serde(default, skip_serializing_if = "Option::is_none")]
                        acceptance: Option<String>,
                    }
                    let f: Payload<Fields> = serde_json::from_str(text)?;
                    Ok(Self::WorkItemDone {
                        item_id: f.payload.item_id,
                        title: f.payload.title,
                        status: f.payload.status,
                        bot_id: f.payload.bot_id,
                        project_id: f.payload.project_id,
                        source: f.payload.source,
                        cycle: f.payload.cycle,
                        revision: f.payload.revision,
                        acceptance: f.payload.acceptance,
                    })
                }
                "mail.event.ready" => {
                    #[derive(Deserialize)]
                    struct Fields {
                        // Mail message ids and main-conversation MessageId have different domains.
                        message_id: String,
                        connection_id: ConnectionId,
                        event: RawJson,
                        how: MailDelivery,
                    }
                    let f: Payload<Fields> = serde_json::from_str(text)?;
                    Ok(Self::MailEventReady {
                        message_id: f.payload.message_id,
                        connection_id: f.payload.connection_id,
                        event: f.payload.event,
                        how: f.payload.how,
                    })
                }
                "github.after" => {
                    #[derive(Deserialize)]
                    struct Fields {
                        connection: ConnectionId,
                        finish: Vec<(ItemId, String)>,
                        signals: Vec<RawJson>,
                        #[serde(default)]
                        supplements: Vec<PrSupplement>,
                    }
                    let f: Payload<Fields> = serde_json::from_str(text)?;
                    Ok(Self::GithubAfter {
                        connection: f.payload.connection,
                        finish: f.payload.finish,
                        signals: f.payload.signals,
                        supplements: f.payload.supplements,
                    })
                }
                "kb.object.changed" => {
                    #[derive(Deserialize)]
                    struct Fields {
                        obj_type: String,
                        obj_id: String,
                    }
                    let f: Payload<Fields> = serde_json::from_str(text)?;
                    Ok(Self::KnowledgeObjectChanged {
                        obj_type: f.payload.obj_type,
                        obj_id: f.payload.obj_id,
                    })
                }
                "todo.due" => serde_json::from_str::<Payload<AlarmFired>>(text)
                    .map(|f| Self::TodoDue(f.payload)),
                "snooze.wake" => serde_json::from_str::<Payload<AlarmFired>>(text)
                    .map(|f| Self::SnoozeWake(f.payload)),
                "briefing.ready" => serde_json::from_str::<Payload<AlarmFired>>(text)
                    .map(|f| Self::BriefingReady(f.payload)),
                other => Err(<serde_json::Error as serde::de::Error>::custom(format!(
                    "unknown fact topic: {other}"
                ))),
            }
        })
    }
}
