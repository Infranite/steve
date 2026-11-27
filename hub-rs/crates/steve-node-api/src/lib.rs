//! The contract between the Hub and a Node.
//!
//! The Hub is the brain: queue, decisions, model choice and retries, permission rulings, Steve's tools.
//! A Node hosts harnesses: it runs attempts, owns the project directories on its machine and reports
//! facts. This crate is the system protocol between the two, in Steve's own words. Agent protocols
//! (ACP, MCP, a CLI's stream-json) end inside a Node's drivers and never appear here: no ACP payload,
//! no MCP URL, no CLI argument, no local path.
//!
//! Everything is owned data that serializes (the same types later go over the wire); the ports are
//! async and object safe. A dropped future only stops waiting, it never cancels an operation: cancel
//! is its own command.

pub mod blob;
pub mod common;
pub mod discovery;
pub mod env;
pub mod events;
pub mod glob;
pub mod harness_updates;
pub mod ids;
pub mod intent;
pub mod oneshot;
pub mod ports;
pub mod run;
pub mod stop;
pub mod tools;
pub mod upgrade_bridge;
pub mod upgrade_v1;
pub mod workspace;

#[cfg(any(test, feature = "testkit"))]
pub mod testkit;

pub use blob::*;
pub use common::*;
pub use discovery::*;
pub use events::*;
pub use harness_updates::*;
pub use ids::*;
pub use intent::{Intent, IntentKey};
pub use oneshot::*;
pub use ports::*;
pub use run::*;
pub use steve_types::devices::{
    DEFAULT_DISK_FLOOR_BYTES, FileAccessReport, FileAccessStatus, LOCAL_SERVICE_LINK_FILE,
    LOCAL_SERVICE_PID_FILE, LOGIN_SHELL_PROBE_TIMEOUT_SECS, LocalServiceLink, LocalServicePid,
    MissingCommandDiagnostic, MissingCommandKind, NODE_BOOT_SERVICE_ENV, NODE_FILE_ACCESS_SOCKET,
    ProtectedAccess, ProtectedLocation, extract_login_shell_path, login_shell_path_script,
    shell_supports_interactive_login,
};
pub use steve_types::gateway::{GatewayFailure, TimeoutPhase};
pub use steve_types::{
    Code, EffortMapping, EffortMode, EffortStrategy, Error, ReasoningEffort, Retry,
    devices::NodeReason,
};
pub use stop::*;
pub use tools::*;
pub use workspace::*;

/// Additive RPCs that the Hub calls only when the Node advertises them through
/// [`Capabilities::features`] (or [`Capacity`]) are capability-negotiated and keep `PROTOCOL`
/// unchanged, so running Nodes stay connected across upgrades.
///
/// Exact ordinary wire contract between the Hub and a Node.
///
/// Peers compare `PROTOCOL` for exact equality in [`upgrade_v1::Mode::between`]: any mismatch
/// drops the link into `Mode::Upgrade`, refusing all ordinary RPCs until the Hub and Nodes are
/// upgraded together. Because bumping `PROTOCOL` interrupts remote Nodes, increment it (by `+1`
/// only) **strictly for breaking wire-format or semantic changes**:
///
/// - `remove-or-rename`: removing or renaming an RPC method, request/response field, or JSON tag.
/// - `type-narrowing`: changing an existing field's type or wire encoding, removing or narrowing
///   variants of a closed enum, or adding a closed-enum variant sent to an older peer without
///   `#[serde(other)]`.
/// - `required-field`: adding a required field without `#[serde(default)]`, or changing any
///   `#[serde(deny_unknown_fields)]` struct (`InstallHarness`, `UpgradeHarness`, `Envelope`,
///   `OrdinaryIntro`).
/// - `semantic-break`: changing security, permission, sandbox, idempotency (`Command::digest`),
///   or data-integrity semantics where an older peer ignoring unknown fields or using defaults
///   would silently drop data, widen permissions, or cause wrong side effects (and capability
///   negotiation cannot safely fall back).
/// - `transport-frame`: changing transport framing (`Envelope`), post-handshake `OrdinaryIntro`,
///   chunking, or flow-control rules.
///
/// **Do not increment `PROTOCOL`** for backwards-compatible additive changes on non-strict
/// structs (those without `#[serde(deny_unknown_fields)]`):
///
/// - Appending fields with `#[serde(default)]` (and `skip_serializing_if` where appropriate) on
///   read-only observation, diagnostic, or display payloads (`NodeVitals`, `AdapterInfo`,
///   `HarnessSetup`, `MissingCommandDiagnostic`, `HarnessUpdateReport`, `SessionView`, etc.) when
///   the default value has clear, safe semantics.
/// - Appending optional request fields (`OpenSession`, `RunSpec`, etc.) with `#[serde(default)]`
///   and `skip_serializing_if` when the Hub only populates them after the Node advertises support
///   in `Capabilities.features` or `AdapterInfo`, keeping older Nodes' `Command::digest` intact.
/// - Adding a new RPC method called only when advertised, or enabling an RPC method the peer
///   already implements.
///
/// `upgrade_v1::UPGRADE_PROTOCOL` is permanently frozen and must never be changed.
///
/// Any PR that increments `PROTOCOL` must include both `Protocol-Bump: <category> — <reason>`
/// and `Upgrade:` trailers in its PR description (enforced by `ops/cut-release.py lint`), and
/// update the baseline fixtures under `tests/fixtures/ordinary_compat/`.
pub const PROTOCOL: u32 = 17;

/// Why an execution a Node held is `unknown` after the Node restarted (its process, the harness
/// with it, is gone): what the Hub gives up and continues on its own (B6), unlike other unknowns.
pub const RESTARTED: &str = "the node restarted while it ran";
/// The `stop_reason` of a turn given up because its Node restarted, its harness provably gone:
/// the agent is continued once, told to check first.
pub const NODE_RESTARTED: &str = "node_restarted";
/// The code of the [`EventBody::Diagnostic`] for an attempt whose session's harness was given
/// Steve's tools and never listed them: its agent ran without them.
pub const STEVE_TOOLS_MISSING: &str = "steve_tools_missing";
/// The code of the [`EventBody::Diagnostic`] when a harness's native auto-mode safety classifier
/// repeatedly returns no verdict (`Severity::Warning`) or halts auto mode for the turn
/// (`Severity::Error`).
pub const AUTO_MODE_CLASSIFIER_UNAVAILABLE: &str = "auto_mode_classifier_unavailable";
/// Version of [`events::NodeEvent`] bodies, separate from the transport.
pub const EVENT_SCHEMA: u16 = 1;

pub mod usage;
pub use usage::{UsageMeter, accounting_usage, accumulate_usage, merged_usage};
