//! Change events (hub/src/bus.ts `HubEvent`). They are the SSE payloads, so the JSON shape is the contract.

use std::sync::Arc;

use serde::Serialize;
use serde_json::Value;

/// One domain change. Serializes exactly like the TS `HubEvent` union (`{"type": "...", ...}`).
///
/// `item_id` on `decision`/`session` is a double option because TS distinguishes an absent key
/// (published from `queued.*`, imports) from an explicit `null` (a session without an item).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type")]
pub enum ChangeEvent {
    #[serde(rename = "item")]
    Item { id: String },
    #[serde(rename = "decision")]
    Decision {
        id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        item_id: Option<Option<String>>,
    },
    #[serde(rename = "session")]
    Session {
        id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        item_id: Option<Option<String>>,
    },
    /// `data` is the object as stored, not re-read from the database.
    #[serde(rename = "event")]
    Event {
        session_id: String,
        seq: i64,
        kind: String,
        data: Value,
    },
    #[serde(rename = "message")]
    Message { id: String },
    /// Not published by the store; defined here so the whole union lives in one place.
    #[serde(rename = "steve.thinking")]
    SteveThinking {
        on: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        ws: Option<String>,
        /// `route` → `answer` | `config` | `item` | `supplement` while one message is handled (TS 3169ae7).
        #[serde(skip_serializing_if = "Option::is_none")]
        phase: Option<String>,
    },
    /// A chat answer while it is written (Rust Hub only): the reasoning and the answer so far, whole each
    /// time, for the user message `mid`. The stored reply replaces it.
    #[serde(rename = "steve.draft")]
    SteveDraft {
        ws: String,
        mid: String,
        thought: String,
        text: String,
    },
    /// One tool call of an inline research run, for the user message `mid`.
    #[serde(rename = "steve.tool")]
    SteveTool {
        ws: String,
        mid: String,
        run: String,
        name: String,
        brief: String,
        ok: bool,
        /// The tool's category (`read`, `search`, `exec`, …); empty when the executor did not say.
        #[serde(skip_serializing_if = "String::is_empty")]
        kind: String,
        /// The call has begun and has not ended: `ok` says nothing yet.
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        running: bool,
        /// Opaque call number within this run; absent in older events.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        call: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        started_at: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        at: Option<i64>,
    },
    /// An inline research run was queued, started or ended.
    #[serde(rename = "thread.run")]
    ThreadRun { id: String, ws: String },
    /// The Hub holds for a release (Rust Hub only): `{item, running, since}` while new turns wait for
    /// the restart, `null` once the hold ends. Hub-wide, like the hold itself.
    #[serde(rename = "release.hold")]
    ReleaseHold { hold: Option<Value> },
    /// `id` may be `"*"` (all memories of a workspace cleared).
    #[serde(rename = "bot")]
    Bot { id: String },
    #[serde(rename = "project")]
    Project { id: String },
    #[serde(rename = "tag")]
    Tag { id: String },
    #[serde(rename = "workspace")]
    Workspace { id: String },
    #[serde(rename = "settings")]
    Settings { id: String },
    #[serde(rename = "device")]
    Device { id: String },
    /// A paired Node changed (G5): its link came up or went down, its state, limit or settings, an
    /// upgrade's phase, what its machine is. `user`: its owner; the stream sends it to them (and
    /// the Hub's owners) only.
    #[serde(rename = "node")]
    Node { id: String, user: String },
    /// A signed-in client of `user` connected to the live stream, left it, or was signed out
    /// (G6). Sent to that user (and the Hub's owners) only.
    #[serde(rename = "client")]
    Client { id: String, user: String },

    // The new modules. Every one carries `ws`: the stream
    // sends them only to subscribers of that workspace. Payloads are ids only; the web refetches.
    /// Contacts, organizations and teams, and their attributes, jobs and members.
    #[serde(rename = "party")]
    Party { id: String, ws: String },
    /// A memory (`mems`).
    #[serde(rename = "mm")]
    Mm { id: String, ws: String },
    #[serde(rename = "changeset")]
    Changeset { id: String, ws: String },
    #[serde(rename = "account")]
    Account { id: String, ws: String },
    #[serde(rename = "connection")]
    Connection { id: String, ws: String },
    /// Vault item metadata, grants and uses; `id` is the item id.
    #[serde(rename = "vault")]
    Vault { id: String, ws: String },
    #[serde(rename = "signal")]
    Signal { id: String, ws: String },
    /// Flows and schedules.
    #[serde(rename = "flow")]
    Flow { id: String, ws: String },
    #[serde(rename = "run")]
    Run {
        id: String,
        flow_id: String,
        ws: String,
    },
    #[serde(rename = "space")]
    Space { id: String, ws: String },
    #[serde(rename = "chan_message")]
    ChanMessage {
        id: String,
        space_id: String,
        ws: String,
    },
    #[serde(rename = "persona")]
    Persona { id: String, ws: String },
    /// The digest base changed: `id` is a conversation (`space:…`), matter, todo or packet.
    #[serde(rename = "digest")]
    Digest { id: String, ws: String },
    /// The calendar cache changed: `id` is a calendar (a sync wrote to it, or its settings changed).
    #[serde(rename = "calendar")]
    Calendar { id: String, ws: String },
    /// `id` is the connection id.
    #[serde(rename = "channel")]
    Channel { id: String, ws: String },
    /// YOLO state of a workspace changed: the switch, a project or item scope, the breaker.
    #[serde(rename = "yolo")]
    Yolo { ws: String },
    /// A YOLO ledger row was written or changed.
    #[serde(rename = "yolo_action")]
    YoloAction {
        id: String,
        decision_id: Option<String>,
        ws: String,
    },
    /// Statistics of the workspace changed; `day` is the local day written, when known.
    /// The Hub coalesces these to one per workspace every 5 seconds before they reach a browser.
    #[serde(rename = "stats")]
    Stats {
        ws: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        day: Option<String>,
    },
}

impl ChangeEvent {
    /// The workspace of a module event. `/api/stream` sends events with a workspace only to that
    /// workspace's subscribers; `None` (the old types) keeps the old broadcast behaviour.
    pub fn ws(&self) -> Option<&str> {
        match self {
            Self::Party { ws, .. }
            | Self::Mm { ws, .. }
            | Self::Changeset { ws, .. }
            | Self::Account { ws, .. }
            | Self::Connection { ws, .. }
            | Self::Vault { ws, .. }
            | Self::Signal { ws, .. }
            | Self::Flow { ws, .. }
            | Self::Run { ws, .. }
            | Self::Space { ws, .. }
            | Self::ChanMessage { ws, .. }
            | Self::Persona { ws, .. }
            | Self::Digest { ws, .. }
            | Self::Calendar { ws, .. }
            | Self::Channel { ws, .. }
            | Self::Yolo { ws }
            | Self::YoloAction { ws, .. }
            | Self::Stats { ws, .. }
            | Self::SteveDraft { ws, .. }
            | Self::SteveTool { ws, .. }
            | Self::ThreadRun { ws, .. } => Some(ws),
            _ => None,
        }
    }
}

/// Transitional publication: legacy producers keep their wire contract until the sweep finishes.
#[derive(Debug, Clone, PartialEq)]
pub enum Published {
    Legacy(ChangeEvent),
    Domain(steve_types::DomainEvent),
    Live(steve_types::LiveEvent),
}

impl From<ChangeEvent> for Published {
    fn from(event: ChangeEvent) -> Self {
        Self::Legacy(event)
    }
}
impl From<steve_types::DomainEvent> for Published {
    fn from(event: steve_types::DomainEvent) -> Self {
        Self::Domain(event)
    }
}
impl From<steve_types::LiveEvent> for Published {
    fn from(event: steve_types::LiveEvent) -> Self {
        Self::Live(event)
    }
}

impl Published {
    /// Compatibility for old test collectors and optional legacy subscribers only. The Hub overrides
    /// publish_batch and never downgrades typed events. Removed with ChangeEvent in the final W1.3b commit.
    fn into_legacy(self) -> ChangeEvent {
        match self {
            Self::Legacy(event) => event,
            Self::Domain(steve_types::DomainEvent::Work(steve_types::WorkEvent::Item {
                id,
                ..
            })) => ChangeEvent::Item { id: id.to_string() },
            Self::Domain(steve_types::DomainEvent::Work(steve_types::WorkEvent::Decision {
                id,
                item_id,
                ..
            })) => ChangeEvent::Decision {
                id: id.to_string(),
                item_id: item_id.map(|value| value.map(|id| id.to_string())),
            },
            Self::Domain(steve_types::DomainEvent::Work(steve_types::WorkEvent::Session {
                id,
                item_id,
                ..
            })) => ChangeEvent::Session {
                id: id.to_string(),
                item_id: item_id.map(|value| value.map(|id| id.to_string())),
            },
            Self::Live(steve_types::LiveEvent::Work(steve_types::WorkLiveEvent::Event {
                session_id,
                seq,
                kind,
                data,
                ..
            })) => ChangeEvent::Event {
                session_id: session_id.to_string(),
                seq: seq.0,
                kind: kind,
                data: serde_json::to_value(data).expect("validated JSON"),
            },
            Self::Domain(steve_types::DomainEvent::Work(steve_types::WorkEvent::Message {
                id,
                ..
            })) => ChangeEvent::Message { id: id.to_string() },
            Self::Live(steve_types::LiveEvent::Work(
                steve_types::WorkLiveEvent::SteveThinking { on, ws, phase },
            )) => ChangeEvent::SteveThinking {
                on: on,
                ws: Some(ws.to_string()),
                phase: phase,
            },
            Self::Live(steve_types::LiveEvent::Work(steve_types::WorkLiveEvent::SteveDraft {
                ws,
                mid,
                thought,
                text,
            })) => ChangeEvent::SteveDraft {
                ws: ws.to_string(),
                mid: mid.to_string(),
                thought: thought,
                text: text,
            },
            Self::Live(steve_types::LiveEvent::Work(steve_types::WorkLiveEvent::SteveTool {
                ws,
                mid,
                run,
                name,
                brief,
                ok,
                kind,
                running,
                call,
                started_at,
                at,
            })) => ChangeEvent::SteveTool {
                ws: ws.to_string(),
                mid: mid.to_string(),
                run: run.to_string(),
                name: name,
                brief: brief,
                ok: ok,
                kind: kind,
                running: running,
                call: call,
                started_at: started_at.map(|value| value.0),
                at: at.map(|value| value.0),
            },
            Self::Domain(steve_types::DomainEvent::Work(steve_types::WorkEvent::ThreadRun {
                id,
                ws,
            })) => ChangeEvent::ThreadRun {
                id: id.to_string(),
                ws: ws.to_string(),
            },
            Self::Domain(steve_types::DomainEvent::Devices(
                steve_types::DevicesEvent::ReleaseHold { hold },
            )) => ChangeEvent::ReleaseHold {
                hold: hold.map(|value| serde_json::to_value(value).expect("validated JSON")),
            },
            Self::Domain(steve_types::DomainEvent::Config(steve_types::ConfigEvent::Bot {
                id,
                ..
            })) => ChangeEvent::Bot { id: id.to_string() },
            Self::Domain(steve_types::DomainEvent::Config(steve_types::ConfigEvent::Project {
                id,
                ..
            })) => ChangeEvent::Project { id: id.to_string() },
            Self::Domain(steve_types::DomainEvent::Config(steve_types::ConfigEvent::Tag {
                id,
                ..
            })) => ChangeEvent::Tag { id: id.to_string() },
            Self::Domain(steve_types::DomainEvent::Config(
                steve_types::ConfigEvent::Workspace { id, .. },
            )) => ChangeEvent::Workspace { id: id.to_string() },
            Self::Domain(steve_types::DomainEvent::Config(
                steve_types::ConfigEvent::Settings { id, .. },
            )) => ChangeEvent::Settings { id: id.to_string() },
            Self::Domain(steve_types::DomainEvent::Devices(
                steve_types::DevicesEvent::Device { id },
            )) => ChangeEvent::Device { id: id.to_string() },
            Self::Domain(steve_types::DomainEvent::Devices(steve_types::DevicesEvent::Node {
                id,
                user,
            })) => ChangeEvent::Node {
                id: id.to_string(),
                user: user.to_string(),
            },
            Self::Domain(steve_types::DomainEvent::Devices(
                steve_types::DevicesEvent::Client { id, user },
            )) => ChangeEvent::Client {
                id: id.to_string(),
                user: user.to_string(),
            },
            Self::Domain(steve_types::DomainEvent::People(steve_types::PeopleEvent::Party {
                id,
                ws,
            })) => ChangeEvent::Party {
                id: id.to_string(),
                ws: ws.to_string(),
            },
            Self::Domain(steve_types::DomainEvent::Config(steve_types::ConfigEvent::Mm {
                id,
                ws,
            })) => ChangeEvent::Mm {
                id: id.to_string(),
                ws: ws.to_string(),
            },
            Self::Domain(steve_types::DomainEvent::Config(
                steve_types::ConfigEvent::Changeset { id, ws },
            )) => ChangeEvent::Changeset {
                id: id.to_string(),
                ws: ws.to_string(),
            },
            Self::Domain(steve_types::DomainEvent::People(steve_types::PeopleEvent::Account {
                id,
                ws,
            })) => ChangeEvent::Account {
                id: id.to_string(),
                ws: ws.to_string(),
            },
            Self::Domain(steve_types::DomainEvent::Channels(
                steve_types::ChannelsEvent::Connection { id, ws },
            )) => ChangeEvent::Connection {
                id: id.to_string(),
                ws: ws.to_string(),
            },
            Self::Domain(steve_types::DomainEvent::Vault(steve_types::VaultEvent::Vault {
                id,
                ws,
            })) => ChangeEvent::Vault {
                id: id.to_string(),
                ws: ws.to_string(),
            },
            Self::Domain(steve_types::DomainEvent::Automation(
                steve_types::AutomationEvent::Signal { id, ws },
            )) => ChangeEvent::Signal {
                id: id.to_string(),
                ws: ws.to_string(),
            },
            Self::Domain(steve_types::DomainEvent::Automation(
                steve_types::AutomationEvent::Flow { id, ws },
            )) => ChangeEvent::Flow {
                id: id.to_string(),
                ws: ws.to_string(),
            },
            Self::Domain(steve_types::DomainEvent::Automation(
                steve_types::AutomationEvent::Run { id, flow_id, ws },
            )) => ChangeEvent::Run {
                id: id.to_string(),
                flow_id: flow_id.to_string(),
                ws: ws.to_string(),
            },
            Self::Domain(steve_types::DomainEvent::Channels(
                steve_types::ChannelsEvent::Space { id, ws },
            )) => ChangeEvent::Space {
                id: id.to_string(),
                ws: ws.to_string(),
            },
            Self::Domain(steve_types::DomainEvent::Channels(
                steve_types::ChannelsEvent::ChanMessage { id, space_id, ws },
            )) => ChangeEvent::ChanMessage {
                id: id.to_string(),
                space_id: space_id.to_string(),
                ws: ws.to_string(),
            },
            Self::Domain(steve_types::DomainEvent::Channels(
                steve_types::ChannelsEvent::Persona { id, ws },
            )) => ChangeEvent::Persona {
                id: id.to_string(),
                ws: ws.to_string(),
            },
            Self::Domain(steve_types::DomainEvent::Channels(
                steve_types::ChannelsEvent::Digest { id, ws },
            )) => ChangeEvent::Digest {
                id: id,
                ws: ws.to_string(),
            },
            Self::Domain(steve_types::DomainEvent::Automation(
                steve_types::AutomationEvent::Calendar { id, ws },
            )) => ChangeEvent::Calendar {
                id: id.to_string(),
                ws: ws.to_string(),
            },
            Self::Domain(steve_types::DomainEvent::Channels(
                steve_types::ChannelsEvent::Channel { id, ws },
            )) => ChangeEvent::Channel {
                id: id.to_string(),
                ws: ws.to_string(),
            },
            Self::Domain(steve_types::DomainEvent::Autonomy(
                steve_types::AutonomyEvent::Yolo { ws },
            )) => ChangeEvent::Yolo { ws: ws.to_string() },
            Self::Domain(steve_types::DomainEvent::Autonomy(
                steve_types::AutonomyEvent::YoloAction {
                    id,
                    decision_id,
                    ws,
                },
            )) => ChangeEvent::YoloAction {
                id: id.to_string(),
                decision_id: decision_id.map(|value| value.to_string()),
                ws: ws.to_string(),
            },
            Self::Domain(steve_types::DomainEvent::Stats(steve_types::StatsEvent::Stats {
                ws,
                day,
            })) => ChangeEvent::Stats {
                ws: ws.to_string(),
                day: day,
            },
            Self::Domain(
                steve_types::DomainEvent::Knowledge(_) | steve_types::DomainEvent::Integrations(_),
            ) => panic!("new event families require a typed Publisher"),
        }
    }
}

/// Where the store sends change events. Called synchronously while the write lock is held, so
/// events arrive in commit order; implementations must be cheap and must not call back into the store.
pub trait Publisher: Send + Sync {
    fn publish(&self, event: ChangeEvent);

    /// A committed transaction, still under the write lock. Typed-capable publishers override this
    /// to allocate one revision for the batch. Legacy-only collectors must never silently lose events.
    fn publish_batch(&self, events: Vec<Published>) {
        for event in events {
            self.publish(event.into_legacy());
        }
    }
}

/// No subscribers is normal (no browser open), so send errors are ignored, like EventEmitter.
impl Publisher for tokio::sync::broadcast::Sender<ChangeEvent> {
    fn publish(&self, event: ChangeEvent) {
        let _ = self.send(event);
    }
}

impl Publisher for tokio::sync::broadcast::Sender<Arc<ChangeEvent>> {
    fn publish(&self, event: ChangeEvent) {
        let _ = self.send(Arc::new(event));
    }
}

impl<P: Publisher + ?Sized> Publisher for Arc<P> {
    fn publish(&self, event: ChangeEvent) {
        (**self).publish(event)
    }
    fn publish_batch(&self, events: Vec<Published>) {
        (**self).publish_batch(events)
    }
}

/// Drops every event (tools, migrations-only opens).
#[derive(Debug, Default, Clone, Copy)]
pub struct NoPublisher;

impl Publisher for NoPublisher {
    fn publish(&self, _: ChangeEvent) {}
    fn publish_batch(&self, _: Vec<Published>) {}
}

/// Adapts a closure, handy for tests and for bridging into another bus type.
pub struct FnPublisher<F>(pub F);

impl<F: Fn(ChangeEvent) + Send + Sync> Publisher for FnPublisher<F> {
    fn publish(&self, event: ChangeEvent) {
        (self.0)(event)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn old_collectors_accept_migrated_producers_without_changing_their_callback() {
        let events = std::sync::Mutex::new(Vec::new());
        let publisher = FnPublisher(|e: ChangeEvent| events.lock().unwrap().push(e));
        publisher.publish_batch(vec![
            steve_types::DomainEvent::from(steve_types::WorkEvent::Item {
                id: "item".into(),
                ws: "workspace".into(),
            })
            .into(),
            steve_types::LiveEvent::from(steve_types::WorkLiveEvent::SteveThinking {
                on: true,
                ws: "workspace".into(),
                phase: None,
            })
            .into(),
        ]);
        assert_eq!(
            *events.lock().unwrap(),
            vec![
                ChangeEvent::Item { id: "item".into() },
                ChangeEvent::SteveThinking {
                    on: true,
                    ws: Some("workspace".into()),
                    phase: None
                },
            ]
        );
    }

    #[test]
    fn json_matches_hub_event() {
        let j = |e: ChangeEvent| serde_json::to_value(e).unwrap();
        assert_eq!(
            j(ChangeEvent::Item { id: "ST-1".into() }),
            json!({"type": "item", "id": "ST-1"})
        );
        assert_eq!(
            j(ChangeEvent::Session {
                id: "s".into(),
                item_id: None
            }),
            json!({"type": "session", "id": "s"})
        );
        assert_eq!(
            j(ChangeEvent::Session {
                id: "s".into(),
                item_id: Some(None)
            }),
            json!({"type": "session", "id": "s", "item_id": null})
        );
        assert_eq!(
            j(ChangeEvent::Decision {
                id: "d".into(),
                item_id: Some(Some("ST-2".into()))
            }),
            json!({"type": "decision", "id": "d", "item_id": "ST-2"})
        );
        assert_eq!(
            j(ChangeEvent::Event {
                session_id: "s".into(),
                seq: 3,
                kind: "user".into(),
                data: json!({"text": "hi"})
            }),
            json!({"type": "event", "session_id": "s", "seq": 3, "kind": "user", "data": {"text": "hi"}})
        );
        assert_eq!(
            j(ChangeEvent::SteveThinking {
                on: true,
                ws: None,
                phase: None
            }),
            json!({"type": "steve.thinking", "on": true})
        );
        assert_eq!(
            j(ChangeEvent::SteveThinking {
                on: true,
                ws: Some("default".into()),
                phase: Some("route".into())
            }),
            json!({"type": "steve.thinking", "on": true, "ws": "default", "phase": "route"})
        );
        assert_eq!(
            j(ChangeEvent::Settings {
                id: "default".into()
            }),
            json!({"type": "settings", "id": "default"})
        );
        // a finished call as before; one that runs says so, with its category
        let tool = |kind: &str, running| ChangeEvent::SteveTool {
            ws: "default".into(),
            mid: "msg_1".into(),
            run: "tr_1".into(),
            name: "Bash".into(),
            brief: "du -sh ~".into(),
            ok: true,
            kind: kind.into(),
            running,
            call: None,
            started_at: None,
            at: None,
        };
        assert_eq!(
            j(tool("", false)),
            json!({"type": "steve.tool", "ws": "default", "mid": "msg_1", "run": "tr_1", "name": "Bash", "brief": "du -sh ~", "ok": true})
        );
        assert_eq!(
            j(tool("exec", true)),
            json!({"type": "steve.tool", "ws": "default", "mid": "msg_1", "run": "tr_1", "name": "Bash", "brief": "du -sh ~", "ok": true, "kind": "exec", "running": true})
        );
        assert_eq!(
            serde_json::to_string(&ChangeEvent::Bot { id: "*".into() }).unwrap(),
            r#"{"type":"bot","id":"*"}"#
        );
    }

    #[test]
    fn module_events_carry_ws() {
        let p = ChangeEvent::Party {
            id: "ct_1".into(),
            ws: "w".into(),
        };
        assert_eq!(
            serde_json::to_string(&p).unwrap(),
            r#"{"type":"party","id":"ct_1","ws":"w"}"#
        );
        assert_eq!(p.ws(), Some("w"));
        let r = ChangeEvent::Run {
            id: "run_1".into(),
            flow_id: "flw_1".into(),
            ws: "w".into(),
        };
        assert_eq!(
            serde_json::to_value(&r).unwrap(),
            json!({"type": "run", "id": "run_1", "flow_id": "flw_1", "ws": "w"})
        );
        let m = ChangeEvent::ChanMessage {
            id: "cm_1".into(),
            space_id: "spc_1".into(),
            ws: "w".into(),
        };
        assert_eq!(
            serde_json::to_value(&m).unwrap(),
            json!({"type": "chan_message", "id": "cm_1", "space_id": "spc_1", "ws": "w"})
        );
        for (e, t) in [
            (
                ChangeEvent::Mm {
                    id: "x".into(),
                    ws: "w".into(),
                },
                "mm",
            ),
            (
                ChangeEvent::Changeset {
                    id: "x".into(),
                    ws: "w".into(),
                },
                "changeset",
            ),
            (
                ChangeEvent::Account {
                    id: "x".into(),
                    ws: "w".into(),
                },
                "account",
            ),
            (
                ChangeEvent::Connection {
                    id: "x".into(),
                    ws: "w".into(),
                },
                "connection",
            ),
            (
                ChangeEvent::Vault {
                    id: "x".into(),
                    ws: "w".into(),
                },
                "vault",
            ),
            (
                ChangeEvent::Signal {
                    id: "x".into(),
                    ws: "w".into(),
                },
                "signal",
            ),
            (
                ChangeEvent::Flow {
                    id: "x".into(),
                    ws: "w".into(),
                },
                "flow",
            ),
            (
                ChangeEvent::Space {
                    id: "x".into(),
                    ws: "w".into(),
                },
                "space",
            ),
            (
                ChangeEvent::Persona {
                    id: "x".into(),
                    ws: "w".into(),
                },
                "persona",
            ),
            (
                ChangeEvent::Channel {
                    id: "x".into(),
                    ws: "w".into(),
                },
                "channel",
            ),
        ] {
            assert_eq!(e.ws(), Some("w"));
            assert_eq!(
                serde_json::to_value(&e).unwrap(),
                json!({"type": t, "id": "x", "ws": "w"})
            );
        }
        // the old types keep broadcasting to everyone
        assert_eq!(ChangeEvent::Item { id: "ST-1".into() }.ws(), None);
        assert_eq!(
            ChangeEvent::SteveThinking {
                on: true,
                ws: Some("w".into()),
                phase: None
            }
            .ws(),
            None
        );
    }
}
