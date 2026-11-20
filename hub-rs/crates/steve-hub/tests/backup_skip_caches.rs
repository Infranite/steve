//! Isolated acceptance for the daily backup hotfix; never uses the installed home.
use std::{
    collections::BTreeMap,
    fs, io,
    path::Path,
    sync::{Arc, Mutex},
};
use steve_hub::daily_backup;
use steve_store_sqlite::backup::{Files, Manifest, SkippedDir};

fn write(home: &Path, name: &str, value: &str) {
    let path = home.join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, value).unwrap();
}
fn current_database(path: &Path, evidence: &str) {
    let mut c = rusqlite::Connection::open(path).unwrap();
    steve_store_sqlite::migrate::initialize(
        &mut c,
        &steve_store_sqlite::layout::Layout::single_file(),
    )
    .unwrap();
    c.execute_batch(evidence).unwrap();
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

fn saved_files(database: &Path) -> Files {
    let manifest: Manifest = serde_json::from_slice(
        &fs::read(database.parent().unwrap().join("manifest.json")).unwrap(),
    )
    .unwrap();
    manifest.files
}

fn archive_entries(path: &Path) -> BTreeMap<String, tar::EntryType> {
    let mut archive = tar::Archive::new(fs::File::open(path).unwrap());
    archive
        .entries()
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (
                entry.path().unwrap().to_string_lossy().into_owned(),
                entry.header().entry_type(),
            )
        })
        .collect()
}

fn assert_skip_logs(logs: &str, expected: &[SkippedDir], total: u64) {
    for entry in expected {
        assert!(
            logs.lines().any(|line| line.contains("DEBUG")
                && line.contains(&format!("path={}", entry.path.display()))
                && line.contains(&format!("bytes={}", entry.bytes))),
            "{logs}"
        );
    }
    assert!(
        logs.lines().any(|line| line.contains("INFO")
            && line.contains(&format!("skipped_dirs={}", expected.len()))),
        "{logs}"
    );
    assert!(logs.contains(&format!("skipped_bytes={total}")), "{logs}");
    assert!(!logs.contains("report.md"));
}

fn assert_live_bytes(home: &Path, db: &Path, made: &daily_backup::Made, archive: &Path) {
    let database_bytes = fs::metadata(&made.database).unwrap().len();
    let files_bytes = fs::metadata(archive).unwrap().len();
    let status = daily_backup::status(home, db, true, 7);
    assert!(status["last"].get("database_bytes").is_none());
    assert_eq!(status["last"]["files_bytes"], files_bytes);
    assert_eq!(status["last"]["bytes"], database_bytes);
    fs::remove_file(archive).unwrap();
    let status = daily_backup::status(home, db, true, 7);
    assert_eq!(status["last"]["files_bytes"], 0);
    assert_eq!(status["last"]["bytes"], database_bytes);
}

#[test]
fn daily_backup_preserves_cache_named_or_marked_directories_outside_workspaces() {
    use std::io::Read;
    let home = tempfile::tempdir().unwrap();
    let signature = "Signature: 8a477f597d28d172789f06886806bc55";
    let parts = ["data/files", "signing", "secrets", "deliveries"];
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
    current_database(&db, "CREATE TABLE evidence(v);");
    let made = daily_backup::make(home.path(), &db, "20261120").unwrap();
    let files = saved_files(&made.database);
    assert!(files.skipped_dirs.is_empty());
    assert_eq!(files.skipped_bytes, 0);
    let mut archive = tar::Archive::new(fs::File::open(made.files.unwrap()).unwrap());
    let files: BTreeMap<_, _> = archive
        .entries()
        .unwrap()
        .filter_map(|entry| {
            let mut entry = entry.unwrap();
            if !entry.header().entry_type().is_file() {
                return None;
            }
            let path = entry.path().unwrap().to_string_lossy().into_owned();
            let mut content = String::new();
            entry.read_to_string(&mut content).unwrap();
            Some((path, content))
        })
        .collect();
    assert_eq!(files.len(), parts.len() * 4);
    for part in parts {
        for dir in ["node_modules", "marked"] {
            assert_eq!(files[&format!("{part}/{dir}/CACHEDIR.TAG")], signature);
            assert_eq!(files[&format!("{part}/{dir}/payload")], part);
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
    current_database(
        &db,
        "CREATE TABLE evidence(v); INSERT INTO evidence VALUES('kept');",
    );
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
    let archive_path = made.files.as_ref().unwrap();
    assert!(fs::metadata(archive_path).unwrap().len() < 32 << 20);
    let entries = archive_entries(archive_path);
    let files = entries
        .iter()
        .filter(|(_, kind)| kind.is_file())
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        files,
        [
            format!("{prefix}/build/user.txt"),
            format!("{prefix}/outputs/report.md")
        ]
    );
    assert!(entries[&format!("{prefix}/cache-link")].is_symlink());
    let logs = String::from_utf8(logs.lock().unwrap().clone()).unwrap();
    let mut expected: Vec<_> = [
        (&tagged, (32 << 20) + signature.len() as u64),
        (&named, 3),
        (&tool, 4),
    ]
    .into_iter()
    .map(|(path, bytes)| SkippedDir {
        path: path.into(),
        bytes,
    })
    .collect();
    expected.sort_by(|a, b| a.path.cmp(&b.path));
    let total = (32 << 20) + signature.len() as u64 + 7;
    let files = saved_files(&made.database);
    assert_eq!(files.skipped_dirs, expected);
    assert_eq!(files.skipped_bytes, total);
    assert_skip_logs(&logs, &expected, total);
    assert_live_bytes(home.path(), &db, &made, archive_path);
}

#[test]
fn daily_backup_follows_part_roots_and_skips_unreadable_caches_and_signing_password() {
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
    write(signing.path(), "keys/id", "key");
    write(signing.path(), "keychain-pass", "secret");
    symlink(signing.path(), home.path().join("signing")).unwrap();
    let db = home.path().join("database.sqlite3");
    current_database(&db, "CREATE TABLE evidence(v);");
    fs::set_permissions(&cache, fs::Permissions::from_mode(0o000)).unwrap();
    assert!(fs::read_dir(&cache).is_err(), "fixture must be unreadable");
    let made = daily_backup::make(home.path(), &db, "20261120");
    fs::set_permissions(&cache, fs::Permissions::from_mode(0o700)).unwrap();
    let made = made.unwrap();
    let files = saved_files(&made.database);
    assert_eq!(
        files.skipped_dirs,
        vec![SkippedDir {
            path: "workspaces/item/node_modules".into(),
            bytes: 0,
        }]
    );
    assert_eq!(files.skipped_bytes, 0);
    let mut archive = tar::Archive::new(fs::File::open(made.files.unwrap()).unwrap());
    let entries: BTreeMap<_, _> = archive
        .entries()
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (
                entry
                    .path()
                    .unwrap()
                    .to_string_lossy()
                    .trim_end_matches('/')
                    .to_owned(),
                entry.header().entry_type(),
            )
        })
        .collect();
    assert!(entries["workspaces"].is_dir());
    assert!(entries["workspaces/item/outputs/report.md"].is_file());
    assert!(entries["workspaces/item/cache-link"].is_symlink());
    assert!(entries["signing/keys/id"].is_file());
    assert!(
        !entries
            .keys()
            .any(|p| p.starts_with("workspaces/item/node_modules"))
    );
    assert!(!entries.contains_key("signing/keychain-pass"));
}

#[test]
fn daily_backup_without_file_parts_records_no_skips() {
    let home = tempfile::tempdir().unwrap();
    let db = home.path().join("database.sqlite3");
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute_batch("CREATE TABLE evidence(v);")
        .unwrap();
    let made = daily_backup::make(home.path(), &db, "20261120").unwrap();
    assert!(made.files.is_none());
    let files = saved_files(&made.database);
    assert!(files.complete && files.artifacts.is_empty() && files.skipped_dirs.is_empty());
    assert_eq!(files.skipped_bytes, 0);
}

#[cfg(unix)]
#[test]
#[cfg_attr(
    target_os = "macos",
    ignore = "macOS rejects non-UTF-8 filenames; the storage publication regression runs separately"
)]
fn daily_backup_publishes_and_reads_distinct_non_utf8_skipped_paths() {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};
    let home = tempfile::tempdir().unwrap();
    let signature = "Signature: 8a477f597d28d172789f06886806bc55";
    let mut expected = vec![];
    for (name, payload) in [
        (b"cache-\xff".as_slice(), "one"),
        (b"cache-\xfe".as_slice(), "second"),
        ("cache-�".as_bytes(), "readable"),
    ] {
        let relative = Path::new("workspaces/item").join(OsString::from_vec(name.to_vec()));
        let directory = home.path().join(&relative);
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("CACHEDIR.TAG"), signature).unwrap();
        fs::write(directory.join("payload"), payload).unwrap();
        expected.push(SkippedDir {
            path: relative,
            bytes: (signature.len() + payload.len()) as u64,
        });
    }
    expected.sort_by(|a, b| a.path.cmp(&b.path));
    write(home.path(), "workspaces/item/kept.txt", "kept");
    let db = home.path().join("database.sqlite3");
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute_batch("CREATE TABLE evidence(v); INSERT INTO evidence VALUES('kept');")
        .unwrap();
    let made = daily_backup::make(home.path(), &db, "20261120").unwrap();
    assert!(made.database.is_file());
    let files = saved_files(&made.database);
    assert!(files.complete && files.error.is_none());
    assert_eq!(files.artifacts.len(), 1);
    assert_eq!(files.skipped_dirs, expected);
    assert_eq!(
        files.skipped_bytes,
        expected.iter().map(|entry| entry.bytes).sum::<u64>()
    );
    assert_eq!(
        rusqlite::Connection::open(&made.database)
            .unwrap()
            .query_row("SELECT v FROM evidence", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "kept"
    );
    let archive = made.files.as_ref().unwrap();
    assert!(archive.is_file());
    let entries = archive_entries(archive);
    let archived_files: Vec<_> = entries
        .iter()
        .filter(|(_, kind)| kind.is_file())
        .map(|(name, _)| name.as_str())
        .collect();
    assert_eq!(archived_files, ["workspaces/item/kept.txt"]);
    let manifest: serde_json::Value = serde_json::from_slice(
        &fs::read(made.database.parent().unwrap().join("manifest.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(manifest["completed"], true);
    let paths: Vec<_> = manifest["files"]["skipped_dirs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| &entry["path"])
        .collect();
    assert_eq!(paths.iter().filter(|path| path.is_string()).count(), 1);
    assert_eq!(paths.iter().filter(|path| path.is_object()).count(), 2);
    assert!(paths.iter().any(|path| **path == "workspaces/item/cache-�"));
}
