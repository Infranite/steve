//! The real Hub CLI with the verified release engine resolved deterministically via
//! `STEVE_BACKUP_ENGINE` (or build-time dev discovery), never via runtime `PATH`.
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::{Output, Stdio},
    time::{Duration, Instant},
};
use steve_runtime::testkit::process::Command;
use steve_store::testkit::hub::backup as fixture;
use steve_types::backup::{BackupPurpose, BackupReceipt};

/// Upper bound for a single `steve-hub --admin` invocation in this integration suite (covers cold
/// restic repo init/backup/restore on loaded CI runners while failing fast on hangs).
const BACKUP_CLI_TIMEOUT: Duration = Duration::from_secs(60);

fn home() -> tempfile::TempDir {
    let home = tempfile::tempdir_in("/tmp").unwrap();
    fs::create_dir_all(home.path().join("data/files")).unwrap();
    fixture::seed_evidence(&db(home.path()), "original").unwrap();
    home
}

fn db(home: &Path) -> PathBuf {
    home.join("data/steve.sqlite3")
}

fn hub_command(home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_steve-hub"));
    command
        .env_clear()
        .env("HOME", home)
        .env("STEVE_HOME", home)
        .env("STEVE_PORT", "0")
        .env("STEVE_HOST", "127.0.0.1")
        .env("STEVE_VAULT_KEYSTORE", "file")
        .env("STEVE_FORBID_REAL_HOSTS", "1")
        .env("STEVE_UPDATE_CHECK", "0")
        .env("STEVE_NO_RESUME", "1")
        .env("STEVE_NODE_LISTEN", "0")
        .env("STEVE_ADAPTER_CLAUDE", "/usr/bin/false")
        .env("STEVE_ADAPTER_CODEX", "/usr/bin/false");
    if let Some(engine) = std::env::var_os("STEVE_BACKUP_ENGINE").filter(|v| !v.is_empty()) {
        command.env("STEVE_BACKUP_ENGINE", engine);
    }
    command
}

fn command(home: &Path) -> Command {
    let mut command = hub_command(home);
    command.arg("--admin");
    command
}

fn run_bounded(mut cmd: Command, label: &str) -> Output {
    cmd.output_within(BACKUP_CLI_TIMEOUT)
        .unwrap_or_else(|diag| panic!("backup_v2 command '{label}' failed: {diag}"))
}

fn success(output: Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}
fn copy(home: &Path, purpose: BackupPurpose) -> BackupReceipt {
    let mut cmd = command(home);
    cmd.args(["backup", "--reason", purpose.code()]);
    let out = success(run_bounded(
        cmd,
        &format!("backup --reason {}", purpose.code()),
    ));
    let receipt: BackupReceipt = serde_json::from_str(&out).unwrap();
    assert_eq!(receipt.purpose, purpose);
    receipt
}

#[test]
fn one_engine_and_repository_for_all_purposes_with_private_verified_restore() {
    let home = home();
    let h = home.path();
    fs::create_dir(h.join("backups")).unwrap();
    fs::set_permissions(h.join("backups"), fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(h.join("backups/old-copy"), b"keep").unwrap();
    for part in [
        "workspaces/item/node_modules",
        "deliveries/objects",
        "signing",
        "secrets",
    ] {
        fs::create_dir_all(h.join(part)).unwrap();
    }
    for path in [
        "data/files/asset",
        "deliveries/objects/sample",
        "workspaces/item/output",
        "workspaces/item/node_modules/cache",
        "signing/identity.json",
        "signing/steve-signing.keychain-db",
        "signing/keychain-pass",
        "signing/other",
        "secrets/token",
    ] {
        fs::write(h.join(path), path).unwrap();
    }
    fs::write(h.join("steve.env"), "STEVE_UPDATE_CHECK=0\n").unwrap();
    symlink(h.join("backups"), h.join("workspaces/item/backups-link")).unwrap();

    let first = copy(h, BackupPurpose::Daily);
    let foreign = tempfile::tempdir_in("/tmp").unwrap();
    let trap = foreign.path().join("restic");
    fs::write(&trap, "#!/bin/sh\nexit 97\n").unwrap();
    fs::set_permissions(&trap, fs::Permissions::from_mode(0o700)).unwrap();
    let mut second_cmd = command(h);
    second_cmd
        .args(["backup", "--reason", "daily"])
        .env("PATH", foreign.path())
        .env("RESTIC_REPOSITORY", foreign.path().join("repo"))
        .env("RESTIC_REPOSITORY_FILE", foreign.path().join("repo-file"))
        .env("RESTIC_PASSWORD", "wrong")
        .env("RESTIC_PASSWORD_FILE", foreign.path().join("password"))
        .env("RESTIC_PASSWORD_COMMAND", &trap)
        .env("RESTIC_CACHE_DIR", foreign.path().join("cache"));
    let out = success(run_bounded(
        second_cmd,
        "backup --reason daily (with PATH trap)",
    ));
    let second: BackupReceipt = serde_json::from_str(&out).unwrap();
    assert_eq!(second.database.sha256, first.database.sha256);
    assert_ne!(first.snapshot_id, second.snapshot_id);
    assert!(
        second.written_bytes < first.written_bytes && second.written_bytes < first.database.bytes,
        "first={first:?} second={second:?}"
    );
    assert_eq!(fs::read_dir(foreign.path()).unwrap().count(), 1);
    assert!(h.join("backups/.restic-cache").is_dir());

    let receipts = [
        first,
        second,
        copy(h, BackupPurpose::PreUpgrade),
        copy(h, BackupPurpose::PreDestructive),
        copy(h, BackupPurpose::Manual),
        copy(h, BackupPurpose::Diagnostic),
    ];
    let mut list_cmd = command(h);
    list_cmd.arg("backup-list");
    let inventory: Vec<serde_json::Value> =
        serde_json::from_str(&success(run_bounded(list_cmd, "backup-list"))).unwrap();
    assert_eq!(inventory.len(), receipts.len());
    for receipt in &receipts {
        let target = tempfile::tempdir_in("/tmp").unwrap();
        let mut restore_cmd = command(h);
        restore_cmd
            .args(["restore", receipt.snapshot_id.as_str(), "--to"])
            .arg(target.path());
        let path = PathBuf::from(success(run_bounded(restore_cmd, "restore --to")).trim());
        assert!(path.starts_with(target.path().canonicalize().unwrap()));
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o077, 0);
        assert_eq!(
            fs::read_to_string(path.join("data/steve.sqlite3.sha256")).unwrap(),
            receipt.database.sha256
        );
        assert_eq!(fixture::evidence(&db(&path)).unwrap(), "original");
        if let Ok(meta) = fs::symlink_metadata(path.join("files/workspaces/item/backups-link")) {
            assert!(meta.is_symlink());
        }
        let state = receipt.purpose != BackupPurpose::Diagnostic;
        let all = state && receipt.purpose != BackupPurpose::PreUpgrade;
        assert_eq!(path.join("files/data/files/asset").is_file(), all);
        assert_eq!(path.join("files/deliveries/objects/sample").is_file(), all);
        assert_eq!(path.join("files/signing/identity.json").is_file(), state);
        assert_eq!(
            path.join("files/signing/steve-signing.keychain-db")
                .is_file(),
            state
        );
        assert_eq!(path.join("files/secrets/token").is_file(), state);
        assert_eq!(path.join("files/steve.env").is_file(), state);
        assert_eq!(path.join("files/workspaces/item/output").is_file(), all);
        for excluded in [
            "signing/keychain-pass",
            "signing/other",
            "vault-keys",
            "workspaces/item/node_modules",
        ] {
            assert!(!path.join("files").join(excluded).exists());
        }
    }
    assert_eq!(fs::read(h.join("backups/old-copy")).unwrap(), b"keep");
    fixture::set_probe_evidence(&db(h), "probe").unwrap();
    assert_eq!(fixture::evidence(&db(h)).unwrap(), "probe");
    let mut db_restore = command(h);
    db_restore.args([
        "restore",
        receipts[2].snapshot_id.as_str(),
        "--database-only",
    ]);
    success(run_bounded(db_restore, "restore --database-only"));
    assert_eq!(fixture::evidence(&db(h)).unwrap(), "original");
    assert!(fs::read_dir(h.join("data")).unwrap().flatten().any(|p| {
        p.file_name()
            .to_string_lossy()
            .starts_with("steve-before-restore-")
    }));
}

#[test]
fn busy_installation_reports_lock_conflict_and_can_retry() {
    let home = home();
    let h = home.path();
    let original = copy(h, BackupPurpose::Manual);
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(h.join("backups/.restic.lock"))
        .unwrap();
    lock.try_lock().unwrap();
    let mut refused_cmd = command(h);
    refused_cmd.arg("backup");
    let refused = run_bounded(refused_cmd, "backup (locked)");
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("backup_repository_locked"));
    drop(lock);
    let retry = copy(h, BackupPurpose::Manual);
    assert_eq!(retry.database.sha256, original.database.sha256);

    let _unsupported = std::os::unix::net::UnixListener::bind(h.join("data/files/socket")).unwrap();
    let mut incomplete_cmd = command(h);
    incomplete_cmd.arg("backup");
    let incomplete = run_bounded(incomplete_cmd, "backup (socket)");
    assert!(!incomplete.status.success());
    assert!(String::from_utf8_lossy(&incomplete.stderr).contains("backup_source_incomplete"));
    let mut list_cmd = command(h);
    list_cmd.arg("backup-list");
    let snapshots: Vec<serde_json::Value> =
        serde_json::from_str(&success(run_bounded(list_cmd, "backup-list"))).unwrap();
    assert_eq!(snapshots.len(), 2);
}

#[test]
fn backup_uses_live_management_socket_and_offline_respects_instance_lock() {
    use std::{
        io::{Read, Write},
        os::unix::net::UnixStream,
        thread,
    };

    let home = tempfile::tempdir_in("/tmp").unwrap();
    let h = home.path();
    let mut hub = hub_command(h)
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let socket = h.join("run/hub.sock");
    let deadline = Instant::now() + Duration::from_secs(20);
    while UnixStream::connect(&socket).is_err() {
        assert!(hub.try_wait().unwrap().is_none(), "temporary Hub exited");
        assert!(Instant::now() < deadline, "temporary Hub did not start");
        thread::sleep(Duration::from_millis(20));
    }
    let mut denied_cmd = command(h);
    denied_cmd.args(["backup", "--offline"]);
    let denied = run_bounded(denied_cmd, "backup --offline");
    assert!(
        !denied.status.success(),
        "offline call bypassed instance lock"
    );
    assert!(!h.join("backups/restic").exists());
    let receipt = copy(h, BackupPurpose::Daily);
    let mut stream = UnixStream::connect(&socket).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    stream
        .write_all(b"GET /admin/v1/backup HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .unwrap();
    let mut text = String::new();
    stream.read_to_string(&mut text).unwrap();
    let (headers, body) = text.split_once("\r\n\r\n").unwrap();
    assert!(headers.starts_with("HTTP/1.1 200"), "{text}");
    let info: serde_json::Value = serde_json::from_str(body).unwrap();
    assert_eq!(info["last"]["snapshot_id"], receipt.snapshot_id.as_str());
}

#[test]
fn offline_backup_does_not_migrate_the_source_database() {
    let home = tempfile::tempdir_in("/tmp").unwrap();
    let h = home.path();
    fs::create_dir_all(h.join("data")).unwrap();
    fixture::seed_old_backup(&db(h)).unwrap();
    let before = fs::read(db(h)).unwrap();
    let receipt = copy(h, BackupPurpose::PreUpgrade);
    assert_eq!(receipt.purpose, BackupPurpose::PreUpgrade);
    assert_eq!(fs::read(db(h)).unwrap(), before);
}
