//! Identity queries must not load/create a Node home or require pairing.
use std::process::Command;

#[test]
fn binary_identity_is_compile_time_release_data_without_a_home() {
    let run = |argument: &str| {
        Command::new(env!("CARGO_BIN_EXE_steve-node"))
            .arg(argument)
            .env_remove("HOME")
            .env_remove("STEVE_HOME")
            .env("STEVE_RELEASE_VERSION", "999.999.999")
            .output()
            .unwrap()
    };
    let info = run("--build-info");
    assert!(
        info.status.success(),
        "{}",
        String::from_utf8_lossy(&info.stderr)
    );
    let info: serde_json::Value = serde_json::from_slice(&info.stdout).unwrap();
    let version = option_env!("STEVE_BUILD_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"));
    assert_eq!(info["version"], version);
    assert_eq!(
        info["commit"],
        option_env!("STEVE_BUILD_COMMIT").unwrap_or("unknown")
    );
    assert_eq!(info["target"], env!("STEVE_BUILD_TARGET"));
    let printed = run("--version");
    assert!(printed.status.success());
    assert_eq!(
        String::from_utf8(printed.stdout).unwrap().trim(),
        format!("steve-node {version}")
    );
}
