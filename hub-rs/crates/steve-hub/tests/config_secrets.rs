//! Shared secret sources and Hub validation: no credentials or running installation required.
use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;

use steve_auth::{AuthConfig, SecretSource};
use steve_hub::config::Config;

fn config(vars: &[(&str, &str)]) -> Result<Config, steve_hub::config::ConfigError> {
    Config::from_vars(|k| match k {
        "STEVE_HOME" => Some("/nonexistent/steve-config-test".into()),
        v if v.starts_with("STEVE_ADAPTER_") => Some("/usr/bin/false".into()),
        _ => vars
            .iter()
            .find(|(key, _)| *key == k)
            .map(|(_, v)| v.to_string()),
    })
}

#[test]
fn secrets_preserve_spaces_remove_only_final_newlines_and_reject_conflicts() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("secret");
    std::fs::write(&path, "  test-secret  \r\n\n").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let path = path.to_str().unwrap();
    for name in ["STEVE_OIDC_CLIENT_SECRET"] {
        let file = format!("{name}_FILE");
        let read = |vars: &[(&str, &str)]| {
            AuthConfig::secret(
                |k| {
                    vars.iter()
                        .find(|(key, _)| *key == k)
                        .map(|(_, v)| v.to_string())
                },
                name,
                SecretSource::FileOrDirect,
            )
        };
        assert_eq!(read(&[]).unwrap(), None);
        assert_eq!(
            read(&[(name, " inline \n")]).unwrap().as_deref(),
            Some(" inline \n")
        );
        assert_eq!(
            read(&[(&file, path)]).unwrap().as_deref(),
            Some("  test-secret  ")
        );
        for value in ["", "inline"] {
            let error = read(&[(&file, path), (name, value)])
                .unwrap_err()
                .to_string();
            assert!(
                error.contains(&file) && error.contains("cannot be set together"),
                "{error}"
            );
            assert!(!error.contains("test-secret"));
        }
        for path in ["", "/nonexistent/steve-test-secret"] {
            assert!(
                read(&[(&file, path)])
                    .unwrap_err()
                    .to_string()
                    .contains(&file)
            );
        }
    }
    let c = config(&[("STEVE_OIDC_CLIENT_SECRET_FILE", path)]).unwrap();
    let auth =
        AuthConfig::from_vars(|k| (k == "STEVE_OIDC_CLIENT_SECRET_FILE").then(|| path.into()))
            .unwrap();
    assert_eq!(c.oidc.client_secret, auth.oidc.client_secret);
    assert_eq!(c.oidc.client_secret.as_deref(), Some("  test-secret  "));
}

#[test]
fn hub_and_auth_reject_unsafe_secret_files_without_exposing_contents() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("secret");
    std::fs::write(&path, "test-private-sentinel").unwrap();
    let path_str = path.to_str().unwrap();
    for mode in [0o644, 0o640, 0o601, 0o700] {
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        let hub = config(&[("STEVE_OIDC_CLIENT_SECRET_FILE", path_str)])
            .unwrap_err()
            .to_string();
        let auth = AuthConfig::from_vars(|k| {
            (k == "STEVE_OIDC_CLIENT_SECRET_FILE").then(|| path_str.into())
        })
        .unwrap_err()
        .to_string();
        for error in [hub, auth] {
            assert!(error.contains("0600"), "{error}");
            assert!(!error.contains("test-private-sentinel"), "{error}");
            assert_eq!(
                error.matches("STEVE_OIDC_CLIENT_SECRET").count(),
                1,
                "{error}"
            );
        }
    }
    for mode in [0o600, 0o400] {
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        assert!(config(&[("STEVE_OIDC_CLIENT_SECRET_FILE", path_str)]).is_ok());
    }
}

#[test]
fn openviking_requires_a_safe_file_even_when_disabled() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("key");
    std::fs::write(&path, "test-key\n").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let file = ("STEVE_OPENVIKING_KEY_FILE", path.to_str().unwrap());
    let c = config(&[
        ("STEVE_KB_STORE", "openviking"),
        file,
        ("STEVE_OPENVIKING_URL", "https://viking.example.test/"),
    ])
    .unwrap();
    let backend = c.openviking.unwrap();
    assert_eq!(backend.key, "test-key");
    assert_eq!(backend.url, "https://viking.example.test/");
    assert!(config(&[("STEVE_KB_STORE", "openviking")]).is_err());
    for direct in ["", "inline"] {
        assert!(config(&[("STEVE_OPENVIKING_KEY", direct)]).is_err());
        assert!(config(&[file, ("STEVE_OPENVIKING_KEY", direct)]).is_err());
        assert!(
            AuthConfig::secret(
                |k| (k == "STEVE_OPENVIKING_KEY").then(|| direct.into()),
                "STEVE_OPENVIKING_KEY",
                SecretSource::FileOnly,
            )
            .is_err()
        );
    }
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    let error = config(&[file]).unwrap_err().to_string();
    assert!(error.contains("0600"));
    assert_eq!(error.matches("STEVE_OPENVIKING_KEY").count(), 1, "{error}");
}

#[test]
fn public_addresses_use_direct_values() {
    let vars = [
        ("STEVE_PUBLIC_URL", "https://service.example.test/"),
        ("STEVE_PUBLIC_URLS", "https://service.example.test/"),
        ("STEVE_CLIENT_ORIGINS", "https://service.example.test/"),
        ("STEVE_OIDC_ISSUER", "https://service.example.test/"),
        ("STEVE_OPENVIKING_URL", "https://service.example.test/"),
    ];
    let c = config(&vars).unwrap();
    assert_eq!(c.public_url, "https://service.example.test");
    assert_eq!(c.public_urls, ["https://service.example.test"]);
    assert_eq!(c.client_origins, ["https://service.example.test"]);
    assert_eq!(
        c.oidc.issuer.as_deref(),
        Some("https://service.example.test/")
    );
    let vars: BTreeMap<_, _> = vars.into_iter().collect();
    let auth = AuthConfig::from_vars(|k| vars.get(k).map(|v| v.to_string())).unwrap();
    assert_eq!(auth.public_url, c.public_url);
    assert_eq!(auth.public_urls, c.public_urls);
    assert_eq!(auth.oidc.issuer, c.oidc.issuer);
}

#[test]
fn fifo_secret_sources_are_rejected_before_open() {
    use std::os::unix::ffi::OsStrExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fifo");
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: c_path is a valid NUL-terminated path inside the test's private directory.
    assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = AuthConfig::secret(
            |k| (k == "STEVE_OIDC_CLIENT_SECRET_FILE").then(|| path.to_str().unwrap().into()),
            "STEVE_OIDC_CLIENT_SECRET",
            SecretSource::FileOrDirect,
        );
        tx.send(result).unwrap();
    });
    let error = rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("FIFO must not wait for a writer")
        .unwrap_err()
        .to_string();
    assert!(error.contains("regular file"), "{error}");
    assert_eq!(
        error.matches("STEVE_OIDC_CLIENT_SECRET").count(),
        1,
        "{error}"
    );
}

#[test]
fn invalid_values_are_errors_and_do_not_fall_back_to_defaults() {
    for (name, bad) in [
        ("STEVE_ITEM_PREFIX", ""),
        ("STEVE_PORT", ""),
        ("STEVE_PORT", " "),
        ("STEVE_PORT", " 9001 "),
        ("STEVE_PORT", "0x10"),
        ("STEVE_PORT", "1e3"),
        ("STEVE_PORT", "+9001"),
        ("STEVE_PORT", "9001.0"),
        ("STEVE_PORT", "65536"),
        ("STEVE_PORT", "-1"),
        ("STEVE_IDLE_RELEASE_MS", "not-a-number"),
        ("STEVE_IDLE_RELEASE_MS", ""),
        ("STEVE_IDLE_RELEASE_MS", "-1"),
        ("STEVE_IDLE_RELEASE_MS", "Infinity"),
        ("STEVE_IDLE_RELEASE_MS", "1.5"),
        ("STEVE_IDLE_RELEASE_MS", "9223372036854775808"),
        ("STEVE_TAKEOVER_CONTINUES", "bad"),
        ("STEVE_TAKEOVER_SAME_REFUSALS", "4294967296"),
        ("STEVE_BACKUP", "of"),
        ("STEVE_DEV_LOGIN", "true"),
        ("STEVE_NO_RESUME", "yes"),
        ("STEVE_ALLOW_ANY_HOST", "true"),
        ("STEVE_DEBUG_ROUTE", "yes"),
        ("STEVE_DISK_FLOOR_GB", "NaN"),
        ("STEVE_DISK_LOW_GB", "-2"),
        ("STEVE_DISK_LOW_GB", "1"),
        ("STEVE_WORKTREE_IDLE_HOURS", "1e99"),
        ("STEVE_KB_STORE", "other"),
        ("STEVE_PUBLIC_URL", "relative"),
        ("STEVE_PUBLIC_URLS", "https://good.test,relative"),
        ("STEVE_CLIENT_ORIGINS", "bad"),
        ("STEVE_OIDC_ISSUER", "bad"),
        ("STEVE_OPENVIKING_URL", "bad"),
    ] {
        let error = config(&[(name, bad)]).unwrap_err().to_string();
        assert!(error.starts_with(name), "{name}: {error}");
        if name == "STEVE_ITEM_PREFIX" {
            assert!(error.contains("unknown variable"));
        }
    }
    let c = config(&[
        ("STEVE_TAKEOVER_CONTINUES", "0"),
        ("STEVE_TAKEOVER_SAME_REFUSALS", "12"),
        ("STEVE_IDLE_RELEASE_MS", "0"),
        ("STEVE_DISK_FLOOR_GB", "1.5"),
        ("STEVE_DISK_LOW_GB", "2"),
    ])
    .unwrap();
    assert_eq!(c.takeover_continues, Some(0));
    assert_eq!(c.takeover_same_refusals, Some(12));
    assert_eq!(c.idle_release_ms, 0);
    assert_eq!(
        config(&[("STEVE_IDLE_RELEASE_MS", "9223372036854775807")])
            .unwrap()
            .idle_release_ms,
        i64::MAX as u64
    );
    assert_eq!(c.disk.floor, steve_core::workspace::GB * 3 / 2);
    for port in ["0", "9001", "65535"] {
        assert_eq!(
            config(&[("STEVE_PORT", port)]).unwrap().listen.port,
            port.parse::<u16>().unwrap()
        );
    }
    for value in ["", "true", "yes", " 1 "] {
        assert!(AuthConfig::from_vars(|k| (k == "STEVE_DEV_LOGIN").then(|| value.into())).is_err());
    }
    for (value, expected) in [("0", false), ("1", true)] {
        assert_eq!(
            AuthConfig::from_vars(|k| (k == "STEVE_DEV_LOGIN").then(|| value.into()))
                .unwrap()
                .dev_login,
            expected
        );
        assert_eq!(
            config(&[("STEVE_DEV_LOGIN", value)]).unwrap().dev_login,
            expected
        );
    }
}

#[test]
fn secret_files_require_utf8_regular_files_and_preserve_embedded_newlines() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("secret");
    let read = |path: &std::path::Path| {
        AuthConfig::secret(
            |k| (k == "STEVE_OIDC_CLIENT_SECRET_FILE").then(|| path.to_str().unwrap().into()),
            "STEVE_OIDC_CLIENT_SECRET",
            SecretSource::FileOrDirect,
        )
    };
    assert!(read(dir.path()).is_err());
    std::fs::write(&path, [0xff, 0xfe]).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(read(&path).is_err());
    std::fs::write(&path, "first\n second \r\n").unwrap();
    assert_eq!(read(&path).unwrap().as_deref(), Some("first\n second "));
}
