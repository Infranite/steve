//! Typed bus envelopes and domain dispatch; revisions are data, never SSE cursors.

use crate::{Rev, UserId, WorkspaceId};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct Envelope<E> {
    #[serde(flatten)]
    pub event: E,
    /// Data, never the SSE cursor: frames of one transaction share it and live events reuse it.
    pub rev: Rev,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Deserialize)]
pub enum Scope {
    Ws(WorkspaceId),
    User(UserId),
    Hub,
}

pub trait Scoped {
    fn scope(&self) -> Scope;
}

impl<E: Scoped> Scoped for Envelope<E> {
    fn scope(&self) -> Scope {
        self.event.scope()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, strum::EnumCount)]
#[serde(untagged)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum DomainEvent {
    Work(crate::WorkEvent),
    Config(crate::ConfigEvent),
    Devices(crate::DevicesEvent),
    Channels(crate::ChannelsEvent),
    People(crate::PeopleEvent),
    Vault(crate::VaultEvent),
    Automation(crate::AutomationEvent),
    Autonomy(crate::AutonomyEvent),
    Stats(crate::StatsEvent),
    Knowledge(crate::KnowledgeEvent),
    Integrations(crate::IntegrationsEvent),
}
impl Scoped for DomainEvent {
    fn scope(&self) -> Scope {
        match self {
            Self::Work(event) => event.scope(),
            Self::Config(event) => event.scope(),
            Self::Devices(event) => event.scope(),
            Self::Channels(event) => event.scope(),
            Self::People(event) => event.scope(),
            Self::Vault(event) => event.scope(),
            Self::Automation(event) => event.scope(),
            Self::Autonomy(event) => event.scope(),
            Self::Stats(event) => event.scope(),
            Self::Knowledge(event) => event.scope(),
            Self::Integrations(event) => event.scope(),
        }
    }
}
impl From<crate::WorkEvent> for DomainEvent {
    fn from(event: crate::WorkEvent) -> Self {
        Self::Work(event)
    }
}
impl From<crate::ConfigEvent> for DomainEvent {
    fn from(event: crate::ConfigEvent) -> Self {
        Self::Config(event)
    }
}
impl From<crate::DevicesEvent> for DomainEvent {
    fn from(event: crate::DevicesEvent) -> Self {
        Self::Devices(event)
    }
}
impl From<crate::ChannelsEvent> for DomainEvent {
    fn from(event: crate::ChannelsEvent) -> Self {
        Self::Channels(event)
    }
}
impl From<crate::PeopleEvent> for DomainEvent {
    fn from(event: crate::PeopleEvent) -> Self {
        Self::People(event)
    }
}
impl From<crate::VaultEvent> for DomainEvent {
    fn from(event: crate::VaultEvent) -> Self {
        Self::Vault(event)
    }
}
impl From<crate::AutomationEvent> for DomainEvent {
    fn from(event: crate::AutomationEvent) -> Self {
        Self::Automation(event)
    }
}
impl From<crate::AutonomyEvent> for DomainEvent {
    fn from(event: crate::AutonomyEvent) -> Self {
        Self::Autonomy(event)
    }
}
impl From<crate::StatsEvent> for DomainEvent {
    fn from(event: crate::StatsEvent) -> Self {
        Self::Stats(event)
    }
}
impl From<crate::KnowledgeEvent> for DomainEvent {
    fn from(event: crate::KnowledgeEvent) -> Self {
        Self::Knowledge(event)
    }
}
impl From<crate::IntegrationsEvent> for DomainEvent {
    fn from(event: crate::IntegrationsEvent) -> Self {
        Self::Integrations(event)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, strum::EnumCount)]
#[serde(untagged)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum LiveEvent {
    Work(crate::WorkLiveEvent),
}
impl Scoped for LiveEvent {
    fn scope(&self) -> Scope {
        match self {
            Self::Work(event) => event.scope(),
        }
    }
}
impl From<crate::WorkLiveEvent> for LiveEvent {
    fn from(event: crate::WorkLiveEvent) -> Self {
        Self::Work(event)
    }
}

/// Distinguishes a missing key (`None`) from an explicit JSON null (`Some(None)`).
pub(crate) mod double_option {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    pub fn serialize<S: Serializer, T: Serialize>(
        value: &Option<Option<T>>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        value.serialize(s)
    }
    pub fn deserialize<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
        d: D,
    ) -> Result<Option<Option<T>>, D::Error> {
        Option::<T>::deserialize(d).map(Some)
    }
}

/// Migration placeholder: every invocation warns, then panics if accidentally executed.
#[deprecated(note = "W1.3b: fill the workspace from the write transaction")]
pub fn ws_todo() -> WorkspaceId {
    panic!("W1.3b: fill the workspace from the write transaction")
}

impl<'de, E: serde::de::DeserializeOwned> Deserialize<'de> for Envelope<E> {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        crate::raw::deserialize_with(d, |text| {
            #[derive(Deserialize)]
            struct Revision {
                rev: Rev,
            }
            let revision: Revision = serde_json::from_str(text)?;
            Ok(Self {
                event: serde_json::from_str(text)?,
                rev: revision.rev,
            })
        })
    }
}

impl<'de> Deserialize<'de> for DomainEvent {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        crate::raw::deserialize_with(d, |text| {
            #[derive(Deserialize)]
            struct Tag {
                r#type: String,
            }
            let tag: Tag = serde_json::from_str(text)?;
            if crate::WorkEvent::WIRE_TYPES.contains(&tag.r#type.as_str()) {
                return serde_json::from_str::<crate::WorkEvent>(text).map(Self::Work);
            }
            if crate::ConfigEvent::WIRE_TYPES.contains(&tag.r#type.as_str()) {
                return serde_json::from_str::<crate::ConfigEvent>(text).map(Self::Config);
            }
            if crate::DevicesEvent::WIRE_TYPES.contains(&tag.r#type.as_str()) {
                return serde_json::from_str::<crate::DevicesEvent>(text).map(Self::Devices);
            }
            if crate::ChannelsEvent::WIRE_TYPES.contains(&tag.r#type.as_str()) {
                return serde_json::from_str::<crate::ChannelsEvent>(text).map(Self::Channels);
            }
            if crate::PeopleEvent::WIRE_TYPES.contains(&tag.r#type.as_str()) {
                return serde_json::from_str::<crate::PeopleEvent>(text).map(Self::People);
            }
            if crate::VaultEvent::WIRE_TYPES.contains(&tag.r#type.as_str()) {
                return serde_json::from_str::<crate::VaultEvent>(text).map(Self::Vault);
            }
            if crate::AutomationEvent::WIRE_TYPES.contains(&tag.r#type.as_str()) {
                return serde_json::from_str::<crate::AutomationEvent>(text).map(Self::Automation);
            }
            if crate::AutonomyEvent::WIRE_TYPES.contains(&tag.r#type.as_str()) {
                return serde_json::from_str::<crate::AutonomyEvent>(text).map(Self::Autonomy);
            }
            if crate::KnowledgeEvent::WIRE_TYPES.contains(&tag.r#type.as_str()) {
                return serde_json::from_str::<crate::KnowledgeEvent>(text).map(Self::Knowledge);
            }
            if crate::StatsEvent::WIRE_TYPES.contains(&tag.r#type.as_str()) {
                return serde_json::from_str::<crate::StatsEvent>(text).map(Self::Stats);
            }
            if crate::IntegrationsEvent::WIRE_TYPES.contains(&tag.r#type.as_str()) {
                return serde_json::from_str::<crate::IntegrationsEvent>(text)
                    .map(Self::Integrations);
            }
            Err(<serde_json::Error as serde::de::Error>::custom(format!(
                "unknown event type: {}",
                tag.r#type
            )))
        })
    }
}

impl<'de> Deserialize<'de> for LiveEvent {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        crate::raw::deserialize_with(d, |text| {
            #[derive(Deserialize)]
            struct Tag {
                r#type: String,
            }
            let tag: Tag = serde_json::from_str(text)?;
            if crate::WorkLiveEvent::WIRE_TYPES.contains(&tag.r#type.as_str()) {
                return serde_json::from_str::<crate::WorkLiveEvent>(text).map(Self::Work);
            }
            Err(<serde_json::Error as serde::de::Error>::custom(format!(
                "unknown event type: {}",
                tag.r#type
            )))
        })
    }
}

// Keep fields in one place. Serde's tagged/untagged Deserialize buffers raw payloads into Content,
// which cannot deserialize RawJson. The helper structs below read directly from the original text;
// Serialize and ts-rs still use the normal internally tagged enum and its unchanged wire attributes.
macro_rules! event_enum {
    ($name:ident { $(#[serde(rename = $tag:literal)] $variant:ident {
        $($(#[$attr:meta])* $field:ident: $ty:ty),* $(,)?
    }),* $(,)? }) => {
        #[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, strum::EnumCount)]
        #[serde(tag = "type")]
        #[cfg_attr(test, derive(ts_rs::TS))]
        pub enum $name {
            $(#[serde(rename = $tag)] $variant { $($(#[$attr])* $field: $ty),* }),*
        }
        impl $name {
            /// Generated from the variants: adding an event never requires editing aggregate dispatch.
            pub const WIRE_TYPES: &'static [&'static str] = &[$($tag),*];
        }
        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                $crate::raw::deserialize_with(d, |text| {
                    #[derive(serde::Deserialize)] struct Tag { r#type: String }
                    let tag: Tag = serde_json::from_str(text)?;
                    match tag.r#type.as_str() {
                        $($tag => {
                            #[derive(serde::Deserialize)]
                            #[cfg_attr(test, derive(ts_rs::TS))]
                            struct Fields { $($(#[$attr])* $field: $ty),* }
                            let fields: Fields = serde_json::from_str(text)?;
                            Ok(Self::$variant { $($field: fields.$field),* })
                        }),*,
                        other => Err(<serde_json::Error as serde::de::Error>::custom(
                            format!("unknown event type: {other}"))),
                    }
                })
            }
        }
    };
}
pub(crate) use event_enum;
