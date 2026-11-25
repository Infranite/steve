//! Steve Hub: the HTTP API, the event stream, configuration, system checks and housekeeping.
//!
//! [`wire`] connects steve-store, steve-core, steve-auth and steve-mcp;
//! [`extension`] is the public entry other binaries assemble a Hub with ([`run`] an [`Assembly`]).

steve_i18n::init!();

// Shared WebSocket/HTTP fixtures are also included by unit tests.
#[cfg(test)]
extern crate self as steve_hub;

pub mod a2a_server;
pub mod access;
mod admin;
pub mod api;
pub mod app;
pub mod avatar;
pub mod backup;
pub mod bus;
pub mod cache_tags;
pub mod caps_api;
pub mod catalog;
pub mod check;
pub mod config;
pub mod daily_backup;
pub mod deliveries;
pub mod error;
pub mod extension;
pub mod files;
pub mod harnesses;
pub mod jobs;
pub mod js;
pub mod metrics;
pub mod nodes;
pub mod notify;
pub mod plugins;
pub mod presence;
pub mod price_sync;
pub mod projection;
pub mod recovery;
pub mod resume;
pub mod stats_snapshot;
pub mod system;
pub mod web;
pub mod wire;
pub mod workflow_step;

pub mod upgrades;

pub use extension::{Assembly, Extension, run};

/// Release identity is compiled into the executable; runtime environment cannot forge it.
/// Source/development builds have no release identity.
pub fn build_identity() -> Option<(&'static str, &'static str)> {
    option_env!("STEVE_BUILD_VERSION").zip(option_env!("STEVE_BUILD_COMMIT"))
}
