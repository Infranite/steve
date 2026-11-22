//! The real Hub CLI with the verified release engine staged next to it by CI.
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::{Command, Output},
};
use steve_types::backup::{BackupPurpose, BackupReceipt};

fn home() -> tempfile::TempDir {
    let home = tempfile::tempdir_in("/tmp").unwrap();
    fs::create_dir_all(home.path().join("data/files")).unwrap();
    let c = rusqlite::Connection::open(db(home.path())).unwrap();
    c.execute_batch("CREATE TABLE sample(value TEXT); INSERT INTO sample VALUES ('original');")
        .unwrap();
    home
}

fn db(home: &Path) -> PathBuf {
    home.join("data/steve.sqlite3")
}

fn command(home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_steve-hub"));
    command
        .env_clear()
        .env("HOME", home)
        .env("STEVE_HOME", home)
        .env("STEVE_PORT", "18700")
        .env("STEVE_HOST", "127.0.0.1")
        .env("STEVE_VAULT_KEYSTORE", "file")
        .env("STEVE_FORBID_REAL_HOSTS", "1")
        .env("STEVE_NO_RESUME", "1")
        .env("STEVE_NODE_MODE", "off")
        .env("STEVE_ADAPTER_CLAUDE", "/usr/bin/false")
        .env("STEVE_ADAPTER_CODEX", "/usr/bin/false")
        .arg("--admin");
    command
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
    let out = success(
        command(home)
            .args(["backup", "--reason", purpose.code()])
            .output()
            .unwrap(),
    );
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
    for part in ["workspaces/item/node_modules", "signing", "secrets"] {
        fs::create_dir_all(h.join(part)).unwrap();
    }
    for path in [
        "data/files/asset",
        "workspaces/item/output",
        "workspaces/item/node_modules/cache",
        "signing/identity.json",
        "signing/steve-signing.keychain-db",
        "signing/keychain-pass",
        "signing/other",
        "secrets/token",
        "steve.env",
    ] {
        fs::write(h.join(path), path).unwrap();
    }
    symlink(h.join("backups"), h.join("workspaces/item/backups-link")).unwrap();

    let first = copy(h, BackupPurpose::Daily);
    let foreign = tempfile::tempdir_in("/tmp").unwrap();
    let trap = foreign.path().join("restic");
    fs::write(&trap, "#!/bin/sh\nexit 97\n").unwrap();
    fs::set_permissions(&trap, fs::Permissions::from_mode(0o700)).unwrap();
    let out = success(
        command(h)
            .args(["backup", "--reason", "daily"])
            .env("PATH", foreign.path())
            .env("RESTIC_REPOSITORY", foreign.path().join("repo"))
            .env("RESTIC_REPOSITORY_FILE", foreign.path().join("repo-file"))
            .env("RESTIC_PASSWORD", "wrong")
            .env("RESTIC_PASSWORD_FILE", foreign.path().join("password"))
            .env("RESTIC_PASSWORD_COMMAND", &trap)
            .env("RESTIC_CACHE_DIR", foreign.path().join("cache"))
            .output()
            .unwrap(),
    );
    let second: BackupReceipt = serde_json::from_str(&out).unwrap();
    assert_eq!(second.database.sha256, first.database.sha256);
    assert_ne!(first.snapshot_id, second.snapshot_id);
    assert!(second.written_bytes < 4096, "{second:?}");
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
    let inventory: Vec<serde_json::Value> =
        serde_json::from_str(&success(command(h).arg("backup-list").output().unwrap())).unwrap();
    assert_eq!(inventory.len(), receipts.len());
    for receipt in &receipts {
        let target = tempfile::tempdir_in("/tmp").unwrap();
        let path = PathBuf::from(
            success(
                command(h)
                    .args(["restore", receipt.snapshot_id.as_str(), "--to"])
                    .arg(target.path())
                    .output()
                    .unwrap(),
            )
            .trim(),
        );
        assert!(path.starts_with(target.path().canonicalize().unwrap()));
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o077, 0);
        assert_eq!(
            fs::read_to_string(path.join("data/steve.sqlite3.sha256")).unwrap(),
            receipt.database.sha256
        );
        assert_eq!(
            rusqlite::Connection::open(db(&path))
                .unwrap()
                .query_row("SELECT value FROM sample", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "original"
        );
        if let Ok(meta) = fs::symlink_metadata(path.join("files/workspaces/item/backups-link")) {
            assert!(meta.is_symlink());
        }
        let state = receipt.purpose != BackupPurpose::Diagnostic;
        let all = state && receipt.purpose != BackupPurpose::PreUpgrade;
        assert_eq!(path.join("files/data/files/asset").is_file(), all);
        assert_eq!(path.join("files/signing/identity.json").is_file(), state);
        assert_eq!(path.join("files/secrets/token").is_file(), state);
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
    rusqlite::Connection::open(db(h))
        .unwrap()
        .execute_batch("PRAGMA journal_mode=WAL; UPDATE sample SET value='probe';")
        .unwrap();
    success(
        command(h)
            .args([
                "restore",
                receipts[2].snapshot_id.as_str(),
                "--database-only",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(
        rusqlite::Connection::open(db(h))
            .unwrap()
            .query_row("SELECT value FROM sample", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "original"
    );
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
    let refused = command(h).arg("backup").output().unwrap();
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("backup_repository_locked"));
    drop(lock);
    let retry = copy(h, BackupPurpose::Manual);
    assert_eq!(retry.database.sha256, original.database.sha256);

    let _unsupported = std::os::unix::net::UnixListener::bind(h.join("data/files/socket")).unwrap();
    let incomplete = command(h).arg("backup").output().unwrap();
    assert!(!incomplete.status.success());
    assert!(String::from_utf8_lossy(&incomplete.stderr).contains("backup_source_incomplete"));
    let snapshots: Vec<serde_json::Value> =
        serde_json::from_str(&success(command(h).arg("backup-list").output().unwrap())).unwrap();
    assert_eq!(snapshots.len(), 2);
}
