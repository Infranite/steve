//! `steve-hub --check-config`, the binary itself: the exit status and the JSON line the deploy scripts
//! and the upgrade supervisor read, on a throwaway STEVE_HOME. It must not start a Hub: no database
//! appears and nothing listens on the configured port.

use std::process::Command;

fn check(env: &[(&str, &str)]) -> (i32, serde_json::Value, std::path::PathBuf) {
    let tmp = tempfile::tempdir().expect("tmp").keep();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .expect("free port")
        .port();
    let mut c = Command::new(env!("CARGO_BIN_EXE_steve-hub"));
    c.arg("--check-config")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", &tmp)
        .env("STEVE_HOME", &tmp)
        .env("STEVE_PORT", port.to_string())
        .env("STEVE_ADAPTER_CLAUDE", "/usr/bin/false")
        .env("STEVE_ADAPTER_CODEX", "/usr/bin/false")
        .env("STEVE_VAULT_KEYSTORE", "file");
    for (k, v) in env {
        c.env(k, v);
    }
    let out = c.output().expect("run steve-hub");
    let report = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "{e}: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    });
    assert!(
        std::net::TcpStream::connect(("127.0.0.1", port)).is_err(),
        "--check-config must not listen"
    );
    for d in ["data", "worktrees", "scratch", "updates"] {
        assert!(!tmp.join(d).exists(), "{d}/ is not made");
    }
    let code = out.status.code().expect("exit code");
    (code, report, tmp)
}

#[test]
fn a_wildcard_host_without_the_opt_in_is_refused_with_the_fix() {
    let (code, r, tmp) = check(&[("STEVE_HOST", "0.0.0.0")]);
    assert_eq!(code, 2, "{r}");
    assert_eq!(r["ok"], false);
    let e = r["errors"][0].as_str().expect("error");
    assert!(e.starts_with("STEVE_HOST: "), "{e}");
    assert!(
        e.contains("STEVE_ALLOW_ANY_HOST=1") && e.contains("127.0.0.1"),
        "{e}"
    );
    let _ = std::fs::remove_dir_all(tmp);

    let (code, r, tmp) = check(&[("STEVE_HOST", "0.0.0.0"), ("STEVE_ALLOW_ANY_HOST", "1")]);
    assert_eq!((code, &r["ok"]), (0, &serde_json::json!(true)), "{r}");
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn a_good_environment_passes_and_build_info_says_the_check_exists() {
    let (code, r, tmp) = check(&[("STEVE_HOST", "127.0.0.1")]);
    assert_eq!(code, 0, "{r}");
    assert_eq!(r["errors"], serde_json::json!([]));
    let _ = std::fs::remove_dir_all(tmp);

    let out = Command::new(env!("CARGO_BIN_EXE_steve-hub"))
        .arg("--build-info")
        .output()
        .expect("build info");
    let info: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(info["check_config"], true, "{info}");
}
