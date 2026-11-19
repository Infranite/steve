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
    assert_eq!(info["upgrade_runner"]["journal"], 1, "{info}");
    assert!(
        info["upgrade_runner"]["kinds"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("release_bundle")),
        "{info}"
    );
}

#[test]
fn local_cli_requests_use_the_hubs_operation_contract() {
    let script =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../ops/test-upgrade.py");
    let out = Command::new("python3")
        .arg(script)
        .arg("--contract")
        .output()
        .expect("isolated CLI fixture");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let op: steve_hub::upgrades::Operation =
        serde_json::from_slice(&out.stdout).expect("Hub Operation contract");
    assert_eq!(op.phase, "staging");
    assert_eq!(op.approved_by.as_deref(), Some(op.requested_by.as_str()));
    assert!(op.requested_by.starts_with("local-os:"));
    let runner = op.runner.unwrap();
    assert_eq!(runner.kind, "release_bundle");
    assert!(runner.repo.is_empty());
    assert_eq!(
        runner.commit.as_deref(),
        Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
    );
}

#[test]
fn removed_and_malformed_variables_fail_the_preflight() {
    for (name, value) in [
        ("STEVE_ITEM_PREFIX", "ST"),
        ("STEVE_OPENVIKING_KEY", "private-test-sentinel"),
        ("STEVE_PUBLIC_URL_FILE", ""),
        ("STEVE_PUBLIC_URLS_FILE", ""),
        ("STEVE_CLIENT_ORIGINS_FILE", ""),
        ("STEVE_OIDC_ISSUER_FILE", ""),
        ("STEVE_OPENVIKING_URL_FILE", ""),
        ("STEVE_PORT", ""),
        ("STEVE_PORT", "0x10"),
        ("STEVE_PORT", "65536"),
        ("STEVE_TAKEOVER_CONTINUES", "typo"),
        ("STEVE_TAKEOVER_SAME_REFUSALS", "-1"),
        ("STEVE_IDLE_RELEASE_MS", "NaN"),
        ("STEVE_DISK_FLOOR_GB", "unknown"),
        ("STEVE_BACKUP", "of"),
        ("STEVE_DYNAMIC_WORKFLOWS", "yes"),
    ] {
        let (code, r, tmp) = check(&[(name, value)]);
        assert_eq!(code, 2, "{name}: {r}");
        assert_eq!(r["ok"], false);
        assert!(!r.to_string().contains("private-test-sentinel"));
        assert!(r["errors"][0].as_str().unwrap().starts_with(name), "{r}");
        if name == "STEVE_ITEM_PREFIX" {
            assert!(
                r["errors"][0]
                    .as_str()
                    .unwrap()
                    .contains("unknown variable")
            );
        }
        let _ = std::fs::remove_dir_all(tmp);
    }
}

#[test]
fn secret_files_are_checked_for_conflicts_and_permissions_before_startup() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("secret");
    std::fs::write(&path, "private-test-sentinel\n").unwrap();
    let path_str = path.to_str().unwrap();
    for mode in [0o644, 0o600] {
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        let (code, r, tmp) = check(&[("STEVE_OIDC_CLIENT_SECRET_FILE", path_str)]);
        assert_eq!(code, if mode == 0o600 { 0 } else { 2 }, "{r}");
        assert!(!r.to_string().contains("private-test-sentinel"), "{r}");
        if mode == 0o644 {
            assert!(r["errors"][0].as_str().unwrap().contains("0600"));
        }
        let _ = std::fs::remove_dir_all(tmp);
    }
    let (code, r, tmp) = check(&[
        ("STEVE_OIDC_CLIENT_SECRET_FILE", path_str),
        ("STEVE_OIDC_CLIENT_SECRET", "inline"),
    ]);
    assert_eq!(code, 2, "{r}");
    assert!(
        r["errors"][0]
            .as_str()
            .unwrap()
            .contains("cannot be set together"),
        "{r}"
    );
    assert!(!r.to_string().contains("private-test-sentinel"));
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn removed_variables_also_stop_normal_startup_before_creating_data() {
    let tmp = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_steve-hub"))
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", tmp.path())
        .env("STEVE_HOME", tmp.path())
        .env("STEVE_NO_RESUME", "1")
        .env("STEVE_ADAPTER_CLAUDE", "/usr/bin/false")
        .env("STEVE_ADAPTER_CODEX", "/usr/bin/false")
        .env("STEVE_PORT", "0")
        .env("STEVE_ITEM_PREFIX", "ST")
        .output()
        .unwrap();
    assert!(!out.status.success());
    let error = String::from_utf8_lossy(&out.stderr);
    assert!(
        error.contains("STEVE_ITEM_PREFIX") && error.contains("unknown variable"),
        "{error}"
    );
    assert!(!tmp.path().join("data").exists());
}
