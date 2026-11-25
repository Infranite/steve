//! Steve's shared contract types: types only, no production IO or internal crate dependencies.
//! Use concrete payloads or opaque `RawJson`, never general-purpose JSON trees or object maps.
//!
//! TypeScript generation is test-only. Add every new public data type to the explicit list in
//! `gen.rs`. Default `cargo test -p steve-types` compares all bindings and the complete directory.
//! To rewrite, run from `hub-rs`: `STEVE_GUARD_BLESS=1 cargo test -p steve-types gen`.
//! Generated files live in `web/src/gen`, are committed, and must never be edited or merged by hand.
//! The generator prepends the Steve provenance header to ts-rs's own generated header, and uses
//! TypeScript `number` for JSON integer fields (not ts-rs's default `bigint`).
//!
//! Reasons live in their domain file: declare a `reasons!` enum with a stable prefix, error code
//! and named display parameters, then include its `SET` in that file's `REASONS`. New domains also
//! need a row in `ALL_REASONS`. Regenerate the TS bindings and add both languages' `reason` texts.
//! Published names, meanings and error codes never change. To retire a cause, remove its variant,
//! keep its suffix in `retired { "old_code" }`, regenerate TS and remove both translations. Unknown
//! and retired wire strings remain readable and display the stored safe detail as a fallback.
//! Branch only with `r.is(..)` or `known()`, never by comparing strings or using `code` to distinguish
//! reasons. Adding a reason needs no minor release; a new prefix requires a contract change request.
//!
//! `Reason` belongs only in `Error`, for display and branches that need no data. `params` is only
//! for display and diagnostics. Facts whose data drives logic belong in typed protocol fields
//! (including `NodeStop`), never in `params` or parsed safe detail. Each fact takes exactly one
//! route: a typed protocol field or an error reason, never parallel representations in both.

mod item_delivery;
pub use item_delivery::{
    ClosePolicy, CloseRequest, DeliveryBaseline, DeliveryProof, DeliveryRef, DeliveryVersion,
    PullProof,
};
pub mod automation;
pub mod autonomy;
pub mod backup;
pub mod channels;
pub mod config;
pub mod devices;
pub mod error;
pub mod event;
pub mod fact;
pub mod gateway;
pub mod id;
pub mod integrations;
pub mod knowledge;
pub mod orchestration;
pub mod people;
pub mod placement;
pub mod raw;
pub mod settings;
pub mod stats;
pub mod stream;
pub mod vault;
pub mod view;
pub mod wait;
pub mod work;

pub use error::{
    ALL_REASONS, Code, Error, HttpErrorBody, NOW_MAX_ATTEMPTS, Param, Params, Reason, ReasonCode,
    ReasonDef, ReasonSet, Retry,
};
pub use event::{
    BusEvent, DomainEvent, Envelope, LiveEvent, Notification, Scope, Scoped, WIRE_TYPES,
};
pub use fact::{AlarmFired, AlarmLine, DurableFact, MailDelivery, PrSupplement};
pub use id::*;
pub use placement::*;
pub use raw::RawJson;
pub use stream::StreamControl;
pub use view::{
    ById, CloseKind, ClosedBy, DecisionPrompt, DecisionStatus, DecisionTier, DecisionView,
    FoldReason, ItemOutcome, ItemParent, ItemPhase, ItemSnooze, ItemState, ItemView, ItemWait,
    MessageView, Op, OperationBlocker, PageOf, Patch, PatchOp, PatchTag, Said, SessionView,
    SessionWaiting, Snapshot, SnapshotViews, View, ViewKind,
};
pub use wait::{WaitContract, WaitKind, WaitLimits, WaitOwner, WaitTimeoutAction};

#[cfg(test)]
mod r#gen;

pub use automation::AutomationEvent;
pub use autonomy::{ApprovalMode, ApprovalPolicy, AutonomyEvent};
pub use channels::ChannelsEvent;
pub use config::{ConfigEvent, ConfigReason};
pub use devices::DevicesEvent;
pub use integrations::IntegrationsEvent;
pub use knowledge::KnowledgeEvent;
pub use people::{AvatarRef, MemberIdentity, PeopleEvent};
pub use stats::StatsEvent;
pub use vault::{VaultEvent, VaultReason};
pub use work::WorkEvent;
pub use work::WorkLiveEvent;
