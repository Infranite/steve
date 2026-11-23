//! Isolated acceptance for the daily backup hotfix; never uses the installed home.
use std::{
    fs, io,
    path::Path,
    sync::{Arc, Mutex},
};
use steve_hub::daily_backup;

fn write(home: &Path, name: &str, value: &str) {
    let path = home.join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, value).unwrap();
}

fn assert_missing(path: &Path) {
    assert!(
        matches!(fs::symlink_metadata(path), Err(e) if e.kind() == io::ErrorKind::NotFound),
        "unexpected snapshot entry: {}",
        path.display()
    );
}

struct LogWriter(Arc<Mutex<Vec<u8>>>);
impl io::Write for LogWriter {
    fn write(&mut self, value: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(value);
        Ok(value.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn daily_backup_preserves_cache_named_or_marked_directories_outside_workspaces() {
    let home = tempfile::tempdir().unwrap();
    let signature = "Signature: 8a477f597d28d172789f06886806bc55";
    // the signing part copies only its installed identity (see the signing test below)
    let parts = ["data/files", "secrets", "deliveries"];
    for part in parts {
        write(
            home.path(),
            &format!("{part}/node_modules/CACHEDIR.TAG"),
            signature,
        );
        write(home.path(), &format!("{part}/node_modules/payload"), part);
        write(
            home.path(),
            &format!("{part}/marked/CACHEDIR.TAG"),
            signature,
        );
        write(home.path(), &format!("{part}/marked/payload"), part);
    }
    let db = home.path().join("database.sqlite3");
    steve_store::testkit::hub::backup::seed_evidence(&db, "kept").unwrap();
    let made = daily_backup::make(home.path(), &db, "20261120").unwrap();
    let snapshot = made.files.unwrap();
    for part in parts {
        for dir in ["node_modules", "marked"] {
            let kept = snapshot.join(part).join(dir);
            assert_eq!(
                fs::read_to_string(kept.join("CACHEDIR.TAG")).unwrap(),
                signature
            );
            assert_eq!(fs::read_to_string(kept.join("payload")).unwrap(), part);
        }
    }
}

#[test]
fn daily_backup_skips_shared_caches_at_any_depth_and_logs_paths_and_bytes() {
    let home = tempfile::tempdir().unwrap();
    let prefix = "workspaces/item";
    let tagged = format!("{prefix}/outputs/a/b/c/d/target");
    let named = format!("{prefix}/node_modules");
    let tool = format!(
        "{prefix}/{}/custom-tool",
        steve_core::workspace::ITEM_BUILD_DIR
    );
    let signature = "Signature: 8a477f597d28d172789f06886806bc55";
    write(home.path(), &format!("{tagged}/CACHEDIR.TAG"), signature);
    let large = fs::File::create(home.path().join(&tagged).join("large")).unwrap();
    large.set_len(32 << 20).unwrap();
    write(home.path(), &format!("{named}/cache"), "abc");
    write(home.path(), &format!("{tool}/artifact"), "tool");
    write(
        home.path(),
        &format!("{prefix}/outputs/report.md"),
        "report",
    );
    write(home.path(), &format!("{prefix}/build/user.txt"), "user");
    for path in [&tagged, &named, &tool] {
        assert!(steve_core::workspace::is_disposable_dir(
            &home.path().join(path),
            None
        ));
    }
    assert!(!steve_core::workspace::is_disposable_dir(
        &home.path().join(prefix).join("build"),
        None
    ));
    std::os::unix::fs::symlink(
        home.path().join(&tagged),
        home.path().join(prefix).join("cache-link"),
    )
    .unwrap();
    let db = home.path().join("database.sqlite3");
    steve_store::testkit::hub::backup::seed_evidence(&db, "kept").unwrap();
    let logs = Arc::new(Mutex::new(Vec::new()));
    let output = logs.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .without_time()
        .with_target(false)
        .with_max_level(tracing::Level::DEBUG)
        .with_writer(move || LogWriter(output.clone()))
        .finish();
    let made = tracing::subscriber::with_default(subscriber, || {
        daily_backup::make(home.path(), &db, "20261120")
    })
    .unwrap();
    assert!(made.database.is_file());
    let snapshot = made.files.unwrap();
    assert_eq!(snapshot.file_name().unwrap(), "files");
    for path in [&tagged, &named, &tool] {
        assert_missing(&snapshot.join(path));
    }
    for (path, content) in [("build/user.txt", "user"), ("outputs/report.md", "report")] {
        assert_eq!(
            fs::read_to_string(snapshot.join(prefix).join(path)).unwrap(),
            content
        );
    }
    assert_eq!(
        fs::read_link(snapshot.join(prefix).join("cache-link")).unwrap(),
        home.path().join(&tagged)
    );
    let logs = String::from_utf8(logs.lock().unwrap().clone()).unwrap();
    for (path, bytes) in [
        (&tagged, (32 << 20) + signature.len() as u64),
        (&named, 3),
        (&tool, 4),
    ] {
        assert!(
            logs.lines().any(|line| line.contains("DEBUG")
                && line.contains(&format!("path={path}"))
                && line.contains(&format!("bytes={bytes}"))),
            "{logs}"
        );
    }
    assert!(
        logs.lines()
            .any(|line| line.contains("INFO") && line.contains("skipped_dirs=3")),
        "{logs}"
    );
    assert!(
        logs.contains(&format!(
            "skipped_bytes={}",
            (32 << 20) + signature.len() as u64 + 7
        )),
        "{logs}"
    );
    assert!(!logs.contains("report.md"));
    let database_bytes = fs::metadata(&made.database).unwrap().len();
    let status = daily_backup::status(home.path(), &db, true, 7);
    assert!(status["last"].get("database_bytes").is_none());
    assert_eq!(status["last"]["files_bytes"], "user".len() + "report".len());
    assert_eq!(status["last"]["bytes"], database_bytes);
    fs::remove_dir_all(snapshot).unwrap();
    let status = daily_backup::status(home.path(), &db, true, 7);
    assert_eq!(status["last"]["files_bytes"], 0);
    assert_eq!(status["last"]["bytes"], database_bytes);
}

#[test]
fn daily_backup_follows_part_roots_skips_unreadable_caches_and_copies_only_the_signing_identity() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let home = tempfile::tempdir().unwrap();
    let external = tempfile::tempdir().unwrap();
    write(external.path(), "item/outputs/report.md", "report");
    write(external.path(), "item/node_modules/cache", "unreadable");
    let cache = external.path().join("item/node_modules");
    symlink(external.path(), home.path().join("workspaces")).unwrap();
    symlink(&cache, external.path().join("item/cache-link")).unwrap();
    // Also exercise the special signing layout when its PARTS root is a link.
    let signing = tempfile::tempdir().unwrap();
    write(signing.path(), "identity.json", "{}");
    write(signing.path(), "steve-signing.keychain-db", "keychain");
    write(signing.path(), "keys/id", "staging");
    write(signing.path(), ".setup-password", "secret");
    symlink(signing.path(), home.path().join("signing")).unwrap();
    let db = home.path().join("database.sqlite3");
    steve_store::testkit::hub::backup::seed_evidence(&db, "kept").unwrap();
    fs::set_permissions(&cache, fs::Permissions::from_mode(0o000)).unwrap();
    assert!(fs::read_dir(&cache).is_err(), "fixture must be unreadable");
    let made = daily_backup::make(home.path(), &db, "20261120");
    fs::set_permissions(&cache, fs::Permissions::from_mode(0o700)).unwrap();
    let snapshot = made.unwrap().files.unwrap();
    for part in ["workspaces", "signing"] {
        assert!(fs::symlink_metadata(snapshot.join(part)).unwrap().is_dir());
    }
    assert_eq!(
        fs::read_to_string(snapshot.join("workspaces/item/outputs/report.md")).unwrap(),
        "report"
    );
    assert_eq!(
        fs::read_link(snapshot.join("workspaces/item/cache-link")).unwrap(),
        cache
    );
    assert_eq!(
        fs::read_to_string(snapshot.join("signing/identity.json")).unwrap(),
        "{}"
    );
    assert_missing(&snapshot.join("workspaces/item/node_modules"));
    assert_eq!(
        fs::read_to_string(snapshot.join("signing/steve-signing.keychain-db")).unwrap(),
        "keychain"
    );
    assert_missing(&snapshot.join("signing/keys"));
    assert_missing(&snapshot.join("signing/.setup-password"));
}
