//! Typed bus envelopes. Domain variants and their scopes are added by W1.3a.

use crate::{Rev, UserId, WorkspaceId};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct Envelope<E> {
    #[serde(flatten)]
    pub event: E,
    /// Data, never the SSE cursor: frames of one transaction share it and live events reuse it.
    pub rev: Rev,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
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

/// State changes; W1.3a adds one variant per domain without changing existing wire type tags.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum DomainEvent {}

/// Streaming progress that does not advance the revision; W1.3a fills the domain variants.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum LiveEvent {}
