//! Steve Hub: the HTTP API, the event stream, configuration, system checks and housekeeping.
//!
//! The HTTP layer is written against [`backend::Backend`] (storage) and [`backend::Domain`] (steve-core);
//! [`wire`] is where the integrator plugs in steve-store, steve-core, steve-auth and steve-mcp;
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
pub mod backend;
pub mod backup;
pub mod bus;
pub mod cache_tags;
pub mod caps_api;
pub mod catalog;
pub mod check;
pub mod config;
pub mod daily_backup;
pub mod decision_text;
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
pub mod recovery;
pub mod resume;
pub mod settings;
pub mod stats_snapshot;
pub mod system;
pub mod validate;
pub mod web;
pub mod wire;

pub mod upgrades;

pub use extension::{Assembly, Extension, run};

/// Release identity is compiled into the executable; runtime environment cannot forge it.
/// Source/development builds retain the legacy runtime version and have no release identity.
pub fn build_identity() -> Option<(&'static str, &'static str)> {
    option_env!("STEVE_BUILD_VERSION").zip(option_env!("STEVE_BUILD_COMMIT"))
}

/// One build-info payload for the executable and extension entry points. Storage
/// ranges come solely from the embedded catalogs; the integer is an old-parser shell.
pub fn build_info() -> anyhow::Result<serde_json::Value> {
    let storage = steve_store_sqlite::compat::StorageCapabilities::compiled();
    let legacy = storage
        .legacy
        .ok_or_else(|| anyhow::anyhow!("compiled legacy capability missing"))?;
    Ok(serde_json::json!({
        "version": option_env!("STEVE_BUILD_VERSION"),
        "commit": option_env!("STEVE_BUILD_COMMIT"),
        "target": env!("STEVE_BUILD_TARGET"),
        "schema": legacy.target,
        "storage": storage.encode_build_info_draft()?,
        "check_config": true,
        "upgrade_runner": {"journal": 1, "kinds": ["release_bundle", "local_release"], "native": true},
        "native_install": 1
    }))
}

#[cfg(test)]
mod build_info_tests {
    #[test]
    fn storage_and_legacy_shell_share_the_compiled_catalog() {
        let info = super::build_info().unwrap();
        let caps = steve_store_sqlite::compat::StorageCapabilities::compiled();
        assert_eq!(info["storage"], serde_json::to_value(&caps).unwrap());
        assert_eq!(info["schema"], caps.legacy.unwrap().target);
        assert_eq!(info["native_install"], 1);
        assert_eq!(info["upgrade_runner"]["native"], true);
        assert_eq!(info["target"], env!("STEVE_BUILD_TARGET"));
        assert_eq!(
            info["version"],
            serde_json::json!(option_env!("STEVE_BUILD_VERSION"))
        );
        assert_eq!(
            info["commit"],
            serde_json::json!(option_env!("STEVE_BUILD_COMMIT"))
        );
    }
}
