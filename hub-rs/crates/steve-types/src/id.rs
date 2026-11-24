//! Contract labels only: IDs do not validate prefixes or generate new identifiers.

use serde::{Deserialize, Serialize};
use std::fmt;

macro_rules! id {
    ($name:ident) => {
        #[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
        #[serde(transparent)]
        #[cfg_attr(test, derive(ts_rs::TS))]
        pub struct $name(String);

        impl $name {
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }
        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_owned())
            }
        }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }
    };
}

id!(WorkspaceId);
id!(UserId);
id!(ItemId);
id!(SessionId);
id!(DecisionId);
id!(MessageId);
id!(BotId);
id!(ProjectId);
id!(TagId);
id!(DeviceId);
id!(NodeId);
id!(ClientId);
id!(PartyId);
id!(MemoryId);
id!(ChangesetId);
id!(AccountId);
id!(ConnectionId);
id!(VaultItemId);
id!(SignalId);
id!(FlowId);
id!(FlowRunId);
id!(SpaceId);
id!(ChanMessageId);
id!(PersonaId);
id!(CalendarId);
id!(KbSourceId);
id!(KbDocId);
id!(MailThreadId);
id!(NotificationId);

// Process identity used by the SSE cursor; the per-frame cursor number is not a Rev.
id!(Epoch);

/// A committed transaction's revision, carried as frame data rather than a cursor.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
#[serde(transparent)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct Rev(pub u64);

/// Unix time in milliseconds.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
#[serde(transparent)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct Millis(pub i64);

/// A session's event sequence number, independent of revisions and SSE cursors.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
#[serde(transparent)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct Seq(pub i64);
