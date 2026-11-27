//! End-to-end fresh Hub setup and `--admin init-db` contract tests against the real `steve-hub`
//! binary and `steve_install::setup::run`.
//!
//! Every installation home, vault key directory, and service manager stub lives in a disposable
//! temporary directory; no real keychain, launchd/systemd service, or production port is touched.

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use steve_runtime::testkit::process::Command;
use steve_store::Store;

const VERSION: &str = "1.2.3";
const LABEL: &str = "io.github.infranite.steve.test-fresh-hub";

fn commit() -> String {
    "a".repeat(40)
}

fn executable(path: &Path, content: &str) {
    fs::write(path, content).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn private_file(path: &Path, bytes: &[u8]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700)).unwrap();
    }
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

fn sha256_file(path: &Path) -> String {
    hex::encode(Sha256::digest(fs::read(path).unwrap()))
}

fn build_package(root: &Path) -> PathBuf {
    let package = root.join("package");
    for rel in ["bin", "web/dist"] {
        fs::create_dir_all(package.join(rel)).unwrap();
    }
    let schema = steve_store::db::SCHEMA_VERSION;
    let commit = commit();
    let manifest = json!({
        "format_version": 1,
        "version": VERSION,
        "commit": commit,
        "hub_version": VERSION,
        "web_version": VERSION,
        "restic_version": "0.19.1",
        "schema": { "target": schema },
    });
    for rel in ["release.json", "web/dist/release.json"] {
        fs::write(package.join(rel), serde_json::to_vec(&manifest).unwrap()).unwrap();
    }
    fs::write(
        package.join("web/dist/index.html"),
        format!(
            "<html><head><meta name=\"steve-web-version\" content=\"{VERSION}\"></head></html>"
        ),
    )
    .unwrap();

    let target = steve_install::build_info()["target"]
        .as_str()
        .unwrap()
        .to_owned();
    let hub_info = json!({
        "version": VERSION,
        "commit": commit,
        "target": target,
        "schema": schema,
        "check_config": true,
    });
    let real_hub = steve_install::common::shell_quote(env!("CARGO_BIN_EXE_steve-hub")).unwrap();
    // Answer `--build-info` with the fixture package's semver/commit identity; delegate every
    // operational subcommand (`--check-config`, `--admin init-db`, `--access token create`, ...)
    // directly to the real compiled `steve-hub` binary.
    executable(
        &package.join("bin/steve-hub"),
        &format!(
            "#!/bin/sh\nif [ \"$#\" -eq 1 ] && [ \"$1\" = '--build-info' ]; then\n  printf '%s\\n' '{hub_info}'\n  exit 0\nfi\nexec {real_hub} \"$@\"\n"
        ),
    );
    let node_info = json!({
        "version": VERSION,
        "commit": commit,
        "target": target,
    });
    executable(
        &package.join("bin/steve-node"),
        &format!(
            "#!/bin/sh\ncase \"$1\" in\n  --build-info) printf '%s\\n' '{node_info}' ;;\n  adapters) printf '%s\\n' '{{\"adapters\":[],\"npm\":false}}' ;;\n  *) exit 95 ;;\nesac\n"
        ),
    );
    for rel in ["bin/steve-keyhelper", "bin/restic"] {
        executable(&package.join(rel), "#!/bin/sh\nexit 95\n");
    }
    fs::write(package.join("bin/restic.LICENSE"), "BSD-2-Clause\n").unwrap();
    package
}

fn install_service_manager_stub(bin: &Path, calls: &Path) {
    fs::create_dir_all(bin).unwrap();
    let calls_quoted = steve_install::common::shell_quote(calls.to_str().unwrap()).unwrap();
    if cfg!(target_os = "macos") {
        executable(
            &bin.join("launchctl"),
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {calls_quoted}\nif [ \"$1\" = print ]; then\n  printf '%s\\n' 'Could not find service' >&2\n  exit 113\nfi\nexit 97\n"
            ),
        );
    } else {
        executable(
            &bin.join("systemctl"),
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {calls_quoted}\n[ \"$1\" = --user ] || exit 97\ncase \"$2\" in\n  show) printf 'LoadState=not-found\\nActiveState=inactive\\nMainPID=0\\nExecMainStatus=0\\n' ;;\n  daemon-reload|enable) exit 0 ;;\n  *) exit 97 ;;\nesac\n"
            ),
        );
    }
}

/// On macOS, pre-seed only the signing receipt stub (same pattern as `ts5_setup.rs`) so the
/// sandbox never invokes real `/usr/bin/security` or `/usr/bin/codesign`, while still leaving
/// `environment`, `steve.env`, `data/`, and `secrets/` completely absent.
fn seed_macos_signing_stub_if_needed(package: &Path, home: &Path, label: &str) {
    if !cfg!(target_os = "macos") {
        return;
    }
    fs::create_dir_all(home).unwrap();
    fs::set_permissions(home, fs::Permissions::from_mode(0o700)).unwrap();
    private_file(&home.join("bin/steve-keyhelper"), b"fixture helper");
    private_file(
        &home.join("signing/identity.json"),
        &serde_json::to_vec(&json!({ "label": label })).unwrap(),
    );
    let receipt = json!({
        "format": 1,
        "component": "hub",
        "home": home,
        "version": VERSION,
        "commit": commit(),
        "files": {},
        "steps": ["keychain"],
        "keychain_started": true,
        "signed_hub_sha256": sha256_file(&package.join("bin/steve-hub")),
        "helper_sha256": sha256_file(&home.join("bin/steve-keyhelper")),
        "identity_sha256": sha256_file(&home.join("signing/identity.json")),
    });
    private_file(
        &home.join("setup.json"),
        &serde_json::to_vec_pretty(&receipt).unwrap(),
    );
}

fn run_setup_in_child(
    test_name: &str,
    package: &Path,
    account: &Path,
    home: &Path,
    vault_dir: &Path,
    bin: &Path,
    listen: &str,
    label: &str,
) -> Output {
    let exe = std::env::current_exe().expect("test binary");
    let path_env = format!("{}:/usr/bin:/bin", bin.display());
    Command::new(exe)
        .args(["--exact", test_name, "--nocapture", "--test-threads=1"])
        .env_clear()
        .env("HOME", account)
        .env("PATH", path_env)
        .env("TMPDIR", account.parent().unwrap())
        .env("USER", "fresh-owner")
        .env("STEVE_PLAIN", "1")
        .env("STEVE_LANG", "zh-CN")
        .env("STEVE_SETUP_E2E_CHILD", "1")
        .env("STEVE_SETUP_E2E_SOURCE", package)
        .env("STEVE_SETUP_E2E_HOME", home)
        .env("STEVE_SETUP_E2E_LISTEN", listen)
        .env("STEVE_SETUP_E2E_LABEL", label)
        .env("STEVE_SETUP_E2E_VAULT_DIR", vault_dir)
        .stdin(Stdio::null())
        .output()
        .expect("run setup child")
}

fn maybe_run_child() -> bool {
    if std::env::var_os("STEVE_SETUP_E2E_CHILD").is_none() {
        return false;
    }
    steve_i18n::set_process(steve_i18n::from_env());
    let source = PathBuf::from(std::env::var_os("STEVE_SETUP_E2E_SOURCE").unwrap());
    let options = steve_install::setup::Options {
        role: Some("hub".into()),
        home: Some(PathBuf::from(
            std::env::var_os("STEVE_SETUP_E2E_HOME").unwrap(),
        )),
        listen: Some(std::env::var("STEVE_SETUP_E2E_LISTEN").unwrap()),
        label: Some(std::env::var("STEVE_SETUP_E2E_LABEL").unwrap()),
        vault_dir: Some(PathBuf::from(
            std::env::var_os("STEVE_SETUP_E2E_VAULT_DIR").unwrap(),
        )),
        adapters: Some("none".into()),
        ..Default::default()
    };
    let code = steve_install::setup::run(&source, options).expect("steve setup hub");
    assert_eq!(code, 0, "expected setup exit code 0");
    true
}

fn free_listen_port() -> u16 {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    assert!(port < u16::MAX, "need port + 1 for web client");
    port
}

fn parse_owner_token(path: &Path) -> (String, String) {
    let text = fs::read_to_string(path).unwrap();
    let user = text
        .lines()
        .find_map(|l| l.strip_prefix("user="))
        .unwrap()
        .to_owned();
    let token = text
        .lines()
        .find_map(|l| l.strip_prefix("token="))
        .unwrap()
        .to_owned();
    (user, token)
}

#[test]
fn fresh_hub_setup_initializes_db_and_issues_owner_token_in_plain_mode() {
    if maybe_run_child() {
        return;
    }
    let test_name = "fresh_hub_setup_initializes_db_and_issues_owner_token_in_plain_mode";
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let account = root.join("account");
    let home = root.join("fresh-home");
    let vault_dir = root.join("vault-keys");
    let bin = root.join("bin");
    fs::create_dir_all(&account).unwrap();
    let package = build_package(&root);
    install_service_manager_stub(&bin, &root.join("manager-calls"));
    seed_macos_signing_stub_if_needed(&package, &home, LABEL);

    let port = free_listen_port();
    let listen = format!("127.0.0.1:{port}");

    // 1. Fresh run in a clean home with STEVE_PLAIN=1 and no TTY.
    let out = run_setup_in_child(
        test_name, &package, &account, &home, &vault_dir, &bin, &listen, LABEL,
    );
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "fresh setup failed:\n{combined}");
    for expected in [
        "配置检查通过",
        "数据库已就绪",
        "hub 已配置并注册服务",
        "登录用户名：fresh-owner",
        "访问 token：stt_",
        "steve hub start",
    ] {
        assert!(
            combined.contains(expected),
            "missing `{expected}` in setup output:\n{combined}"
        );
    }

    // Verify private directory and file modes.
    let secrets_mode = fs::metadata(home.join("secrets"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(secrets_mode, 0o700, "secrets dir must be 0700");
    let data_mode = fs::metadata(home.join("data"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(data_mode, 0o700, "data dir must be 0700");
    let token_path = home.join("secrets/owner.token");
    assert_eq!(
        fs::metadata(&token_path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let (user, token) = parse_owner_token(&token_path);
    assert_eq!(user, "fresh-owner");
    assert!(token.starts_with("stt_"), "unexpected token: {token}");

    // Verify the database was migrated to head and the issued token actually authenticates.
    let db_path = home.join("data/steve.sqlite3");
    assert_eq!(
        steve_store::db::inspect_schema(&db_path).unwrap().schema,
        steve_store::db::SCHEMA_VERSION
    );
    let store = Store::open_admin(&db_path).unwrap();
    assert_eq!(store.access().tokens(None).unwrap().len(), 1);
    let signed_in = store
        .access()
        .sign_in(
            &user,
            &token,
            &steve_store::access::NewClient {
                kind: "cli",
                name: "setup-e2e",
                platform: "test",
                version: VERSION,
                origin: None,
            },
        )
        .unwrap()
        .expect("issued owner token must sign in");
    assert_eq!(signed_in.0.name, "fresh-owner");
    assert_eq!(signed_in.0.role, "owner");
    drop(store);

    // Verify service registration and symlinks completed.
    assert!(home.join("services/hub.json").is_file());
    assert!(home.join("current/bin/steve-hub").is_file());
    assert!(home.join("web/current/index.html").is_file());
    let receipt: Value =
        serde_json::from_slice(&fs::read(home.join("setup.json")).unwrap()).unwrap();
    assert!(
        receipt["steps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s == "installed"),
        "setup.json must record installed step: {receipt}"
    );

    // 2. Re-running setup on the completed home is idempotent and does not issue a second token.
    let rerun = run_setup_in_child(
        test_name, &package, &account, &home, &vault_dir, &bin, &listen, LABEL,
    );
    let rerun_out = format!(
        "{}\n{}",
        String::from_utf8_lossy(&rerun.stdout),
        String::from_utf8_lossy(&rerun.stderr)
    );
    assert!(rerun.status.success(), "setup rerun failed:\n{rerun_out}");
    assert_eq!(parse_owner_token(&token_path), (user, token));
    let store = Store::open_admin(&db_path).unwrap();
    assert_eq!(
        store.access().tokens(None).unwrap().len(),
        1,
        "rerun must not issue a second owner token"
    );
}

#[test]
fn partial_setup_with_migrated_db_resumes_and_issues_owner_token() {
    if maybe_run_child() {
        return;
    }
    let test_name = "partial_setup_with_migrated_db_resumes_and_issues_owner_token";
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let account = root.join("account");
    let home = root.join("partial-home");
    let vault_dir = root.join("vault-keys");
    let bin = root.join("bin");
    fs::create_dir_all(&account).unwrap();
    let package = build_package(&root);
    install_service_manager_stub(&bin, &root.join("manager-calls"));
    seed_macos_signing_stub_if_needed(&package, &home, LABEL);

    // Pre-create an older database and a hub-identity key (simulating an interrupted setup or
    // an existing older DB) before running `steve setup hub`.
    fs::create_dir_all(home.join("data")).unwrap();
    fs::set_permissions(home.join("data"), fs::Permissions::from_mode(0o700)).unwrap();
    let old_version = (steve_store::db::SCHEMA_VERSION as usize).saturating_sub(2);
    steve_store::db::testkit::migrate_to(&home.join("data/steve.sqlite3"), old_version).unwrap();
    // Simulate Hub having already created `secrets/hub-identity.key` (now 0700).
    let _ = steve_hub::nodes::identity_at(&home).unwrap();

    // Ensure setup.json exists on Linux too if `home` already exists (Wizard::claim never adopts
    // a pre-existing directory without setup.json).
    if !cfg!(target_os = "macos") {
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
        let receipt = json!({
            "format": 1,
            "component": "hub",
            "home": home,
            "version": VERSION,
            "commit": commit(),
            "files": {},
            "steps": [],
        });
        private_file(
            &home.join("setup.json"),
            &serde_json::to_vec_pretty(&receipt).unwrap(),
        );
    }

    let port = free_listen_port();
    let listen = format!("127.0.0.1:{port}");
    let out = run_setup_in_child(
        test_name, &package, &account, &home, &vault_dir, &bin, &listen, LABEL,
    );
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "resumed setup failed:\n{combined}");
    assert_eq!(
        steve_store::db::inspect_schema(&home.join("data/steve.sqlite3"))
            .unwrap()
            .schema,
        steve_store::db::SCHEMA_VERSION
    );
    let (user, token) = parse_owner_token(&home.join("secrets/owner.token"));
    assert_eq!(user, "fresh-owner");
    assert!(token.starts_with("stt_"));
}

#[test]
fn failed_hub_setup_on_loose_secrets_or_phantom_receipt_resumes_with_valid_owner_token() {
    if maybe_run_child() {
        return;
    }
    let test_name =
        "failed_hub_setup_on_loose_secrets_or_phantom_receipt_resumes_with_valid_owner_token";
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let account = root.join("account");
    let home = root.join("resume-home");
    let vault_dir = root.join("vault-keys");
    let bin = root.join("bin");
    fs::create_dir_all(&account).unwrap();
    let package = build_package(&root);
    install_service_manager_stub(&bin, &root.join("manager-calls"));
    seed_macos_signing_stub_if_needed(&package, &home, LABEL);

    // Pre-create `home/secrets` with loose mode 0755 (as produced by older Hub runs).
    // On Linux, also seed a minimal setup.json because Wizard::claim never adopts an unowned dir.
    fs::create_dir_all(home.join("secrets")).unwrap();
    fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
    fs::set_permissions(home.join("secrets"), fs::Permissions::from_mode(0o755)).unwrap();
    if !cfg!(target_os = "macos") {
        let receipt = json!({
            "format": 1,
            "component": "hub",
            "home": home,
            "version": VERSION,
            "commit": commit(),
            "files": {},
            "steps": [],
        });
        private_file(
            &home.join("setup.json"),
            &serde_json::to_vec_pretty(&receipt).unwrap(),
        );
    }

    let port = free_listen_port();
    let listen = format!("127.0.0.1:{port}");

    // 1. First run fails before issuing a token, leaves DB migrated, 0 tokens in SQLite,
    //    and NO phantom `secrets/owner.token` entry in setup.json.
    let first = run_setup_in_child(
        test_name, &package, &account, &home, &vault_dir, &bin, &listen, LABEL,
    );
    let first_out = format!(
        "{}\n{}",
        String::from_utf8_lossy(&first.stdout),
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(
        !first.status.success(),
        "expected setup to fail on 0755 secrets/:\n{first_out}"
    );
    assert!(first_out.contains("0700"), "{first_out}");
    assert!(!home.join("secrets/owner.token").exists());
    let db_path = home.join("data/steve.sqlite3");
    assert_eq!(
        steve_store::db::inspect_schema(&db_path).unwrap().schema,
        steve_store::db::SCHEMA_VERSION
    );
    assert_eq!(
        Store::open_admin(&db_path)
            .unwrap()
            .access()
            .tokens(None)
            .unwrap()
            .len(),
        0,
        "must not leave an unrecorded token in SQLite after secrets/ failure"
    );
    let mut receipt: Value =
        serde_json::from_slice(&fs::read(home.join("setup.json")).unwrap()).unwrap();
    assert!(receipt["files"].get("secrets/owner.token").is_none());

    // 2. Fix `secrets/` to 0700, AND also inject a stale phantom `files["secrets/owner.token"]`
    //    entry (matching the broken state left behind by v0.13.0) to verify resume heals it.
    fs::set_permissions(home.join("secrets"), fs::Permissions::from_mode(0o700)).unwrap();
    receipt["files"]["secrets/owner.token"] = json!("f".repeat(64));
    fs::write(
        home.join("setup.json"),
        serde_json::to_vec_pretty(&receipt).unwrap(),
    )
    .unwrap();

    // 3. Resumed run completes cleanly, writes `secrets/owner.token` (0600), prints non-empty
    //    user + token, updates setup.json digest, and the issued token signs in.
    let second = run_setup_in_child(
        test_name, &package, &account, &home, &vault_dir, &bin, &listen, LABEL,
    );
    let second_out = format!(
        "{}\n{}",
        String::from_utf8_lossy(&second.stdout),
        String::from_utf8_lossy(&second.stderr)
    );
    assert!(
        second.status.success(),
        "resumed setup must succeed:\n{second_out}"
    );
    assert!(
        second_out.contains("登录用户名：fresh-owner"),
        "{second_out}"
    );
    assert!(second_out.contains("访问 token：stt_"), "{second_out}");

    let token_path = home.join("secrets/owner.token");
    assert_eq!(
        fs::metadata(&token_path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let (user, token) = parse_owner_token(&token_path);
    assert_eq!(user, "fresh-owner");
    assert!(token.starts_with("stt_"));
    let updated_receipt: Value =
        serde_json::from_slice(&fs::read(home.join("setup.json")).unwrap()).unwrap();
    assert_eq!(
        updated_receipt["files"]["secrets/owner.token"]
            .as_str()
            .unwrap(),
        sha256_file(&token_path)
    );
    let store = Store::open_admin(&db_path).unwrap();
    assert_eq!(store.access().tokens(None).unwrap().len(), 1);
    let signed_in = store
        .access()
        .sign_in(
            &user,
            &token,
            &steve_store::access::NewClient {
                kind: "cli",
                name: "setup-resume-e2e",
                platform: "test",
                version: VERSION,
                origin: None,
            },
        )
        .unwrap()
        .expect("resumed owner token must sign in");
    assert_eq!(signed_in.0.name, "fresh-owner");
    assert_eq!(signed_in.0.role, "owner");
}

fn run_hub_cli(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_steve-hub"))
        .args(args)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", home.parent().unwrap())
        .env("STEVE_HOME", home)
        .env("STEVE_VAULT_KEYSTORE", "file")
        .output()
        .expect("run steve-hub")
}

#[test]
fn admin_init_db_handles_fresh_old_and_incompatible_dbs() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();

    // 1. Fresh home: creates DB, migrates to head, enables subsequent `--access token create`.
    let fresh = root.join("fresh");
    let init = run_hub_cli(&fresh, &["--admin", "init-db"]);
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );
    let report: Value = serde_json::from_slice(&init.stdout).unwrap();
    assert_eq!(report["ok"], true);
    assert_eq!(report["schema_version"], steve_store::db::SCHEMA_VERSION);
    assert_eq!(
        fs::metadata(fresh.join("data"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    let token_out = run_hub_cli(
        &fresh,
        &["--access", "token", "create", "--user", "alice", "--json"],
    );
    assert!(
        token_out.status.success(),
        "{}",
        String::from_utf8_lossy(&token_out.stderr)
    );
    let created: Value = serde_json::from_slice(&token_out.stdout).unwrap();
    assert_eq!(created["user"], "alice");
    assert!(created["token"].as_str().unwrap().starts_with("stt_"));

    // 2. Older DB: migrates pending versions and preserves existing data.
    let old = root.join("old");
    fs::create_dir_all(old.join("data")).unwrap();
    let old_db = old.join("data/steve.sqlite3");
    let earlier = (steve_store::db::SCHEMA_VERSION as usize).saturating_sub(2);
    steve_store::db::testkit::migrate_to(&old_db, earlier).unwrap();
    let init_old = run_hub_cli(&old, &["--admin", "init-db"]);
    assert!(
        init_old.status.success(),
        "{}",
        String::from_utf8_lossy(&init_old.stderr)
    );
    assert_eq!(
        steve_store::db::inspect_schema(&old_db).unwrap().schema,
        steve_store::db::SCHEMA_VERSION
    );

    // 3. Incompatible newer DB (`min_reader > SCHEMA_VERSION`): refused without touching bytes.
    let newer = root.join("newer");
    fs::create_dir_all(newer.join("data")).unwrap();
    let newer_db = newer.join("data/steve.sqlite3");
    steve_store::db::testkit::set_schema(
        &newer_db,
        steve_store::db::SCHEMA_VERSION + 2,
        Some(steve_store::db::SCHEMA_VERSION + 1),
    )
    .unwrap();
    let before_bytes = fs::read(&newer_db).unwrap();
    let init_newer = run_hub_cli(&newer, &["--admin", "init-db"]);
    assert_eq!(init_newer.status.code(), Some(2));
    assert!(!init_newer.stderr.is_empty());
    assert_eq!(fs::read(&newer_db).unwrap(), before_bytes);
}

#[test]
fn hub_and_node_private_directories_are_created_0700_and_reject_symlinks() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();

    // Fresh Hub identity creates `secrets/` as 0700 and `hub-identity.key` as 0600.
    let hub1 = root.join("hub1");
    fs::create_dir(&hub1).unwrap();
    let _ = steve_hub::nodes::identity_at(&hub1).unwrap();
    assert_eq!(
        fs::metadata(hub1.join("secrets"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(hub1.join("secrets/hub-identity.key"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );

    // Pre-existing loose `secrets/` owned by this user is tightened to 0700.
    let hub2 = root.join("hub2");
    fs::create_dir_all(hub2.join("secrets")).unwrap();
    fs::set_permissions(hub2.join("secrets"), fs::Permissions::from_mode(0o755)).unwrap();
    let _ = steve_hub::nodes::identity_at(&hub2).unwrap();
    assert_eq!(
        fs::metadata(hub2.join("secrets"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    // `steve-install`'s strict `owned` check accepts it.
    steve_install::common::owned(&hub2.join("secrets"), true).unwrap();

    // Symlinked `secrets/` is refused without modifying the symlink target's mode.
    let hub3 = root.join("hub3");
    let external = root.join("external-dir");
    fs::create_dir_all(&hub3).unwrap();
    fs::create_dir_all(&external).unwrap();
    fs::set_permissions(&external, fs::Permissions::from_mode(0o755)).unwrap();
    std::os::unix::fs::symlink(&external, hub3.join("secrets")).unwrap();
    assert!(steve_hub::nodes::identity_at(&hub3).is_err());
    assert_eq!(fs::metadata(&external).unwrap().mode() & 0o777, 0o755);

    // Node identity save creates `node/` with mode 0700.
    let node_home = root.join("node-home");
    let id = steve_node_wire::Identity::generate().unwrap();
    id.save(&node_home.join("node/identity.key")).unwrap();
    assert_eq!(
        fs::metadata(node_home.join("node"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
}
