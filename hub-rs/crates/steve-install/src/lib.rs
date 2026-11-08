//! Operator-owned installation and maintenance. No task or harness execution lives here.
pub mod common;
pub(crate) mod env_file;
pub mod release;
pub mod service;
pub mod setup;
pub mod signing;

pub fn build_info() -> serde_json::Value {
    serde_json::json!({
        "version": option_env!("STEVE_BUILD_VERSION").unwrap_or("dev"),
        "commit": option_env!("STEVE_BUILD_COMMIT").unwrap_or("dev"),
        "target": env!("STEVE_BUILD_TARGET"),
        "cli_protocol": 2,
        "native_install": true,
    })
}
