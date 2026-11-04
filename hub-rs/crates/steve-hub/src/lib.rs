//! Steve Hub: the binary that replaces hub/src/{main,api,bus,config,system,housekeeping}.ts.
//!
//! The HTTP layer is written against [`backend::Backend`] (storage) and [`backend::Domain`] (steve-core);
//! [`wire`] is where the integrator plugs in steve-store, steve-core, steve-auth and steve-mcp.

pub mod api;
pub mod app;
pub mod backend;
pub mod bus;
pub mod catalog;
pub mod config;
pub mod error;
pub mod files;
pub mod jobs;
pub mod js;
pub mod notify;
pub mod recovery;
pub mod resume;
pub mod settings;
pub mod system;
pub mod validate;
pub mod web;
pub mod wire;

pub mod upgrades;

/// Release identity is compiled into the executable; runtime environment cannot forge it.
/// Source/development builds retain the legacy runtime version and have no release identity.
pub fn build_identity() -> Option<(&'static str, &'static str)> {
    option_env!("STEVE_BUILD_VERSION").zip(option_env!("STEVE_BUILD_COMMIT"))
}
