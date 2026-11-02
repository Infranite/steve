//! Runs the TS ↔ Rust contract test (tests/contract/contract.mjs) against this crate's binary.
//! Needs node and the TS Hub checkout (`STEVE_TS_REPO`, default ~/Agents/steve with node_modules), so it is
//! ignored by default: `cargo test -p steve-hub --test contract -- --ignored`.

use std::process::Command;

#[test]
#[ignore = "needs node and the TS Hub checkout"]
fn ts_and_rust_hubs_answer_alike() {
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/contract/contract.mjs");
    let status = Command::new("node")
        .args([script, "--a", "ts", "--b", "rust"])
        .env("STEVE_HUB_BIN", env!("CARGO_BIN_EXE_steve-hub"))
        .status()
        .expect("run node");
    assert!(
        status.success(),
        "contract differences (see the report above)"
    );
}
