//! Isolated acceptance for the daily backup hotfix; never uses the installed home.
use std::{
    collections::BTreeMap,
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
            &home.path().join(path)
        ));
    }
    assert!(!steve_core::workspace::is_disposable_dir(
        &home.path().join(prefix).join("build")
    ));
    std::os::unix::fs::symlink(
        home.path().join(&tagged),
        home.path().join(prefix).join("cache-link"),
    )
    .unwrap();
    let db = home.path().join("database.sqlite3");
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute_batch("CREATE TABLE evidence(v); INSERT INTO evidence VALUES('kept');")
        .unwrap();
    let logs = Arc::new(Mutex::new(Vec::new()));
    let output = logs.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .without_time()
        .with_target(false)
        .with_max_level(tracing::Level::INFO)
        .with_writer(move || LogWriter(output.clone()))
        .finish();
    let made = tracing::subscriber::with_default(subscriber, || {
        daily_backup::make(home.path(), &db, "20261120")
    })
    .unwrap();
    assert!(made.database.is_file());
    let archive_path = made.files.unwrap();
    assert!(fs::metadata(&archive_path).unwrap().len() < 32 << 20);
    let mut archive = tar::Archive::new(fs::File::open(&archive_path).unwrap());
    let entries: BTreeMap<_, _> = archive
        .entries()
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (
                entry.path().unwrap().to_string_lossy().into_owned(),
                entry.header().entry_type(),
            )
        })
        .collect();
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
    for (path, bytes) in [
        (&tagged, (32 << 20) + signature.len() as u64),
        (&named, 3),
        (&tool, 4),
    ] {
        assert!(
            logs.lines()
                .any(|line| line.contains(&format!("path={path}"))
                    && line.contains(&format!("bytes={bytes}"))),
            "{logs}"
        );
    }
    assert!(logs.contains("skipped_dirs=3"), "{logs}");
    assert!(
        logs.contains(&format!(
            "skipped_bytes={}",
            (32 << 20) + signature.len() as u64 + 7
        )),
        "{logs}"
    );
    assert!(!logs.contains("report.md"));
    let database_bytes = fs::metadata(&made.database).unwrap().len();
    let files_bytes = fs::metadata(&archive_path).unwrap().len();
    let status = daily_backup::status(home.path(), &db, true);
    assert_eq!(status["last"]["database_bytes"], database_bytes);
    assert_eq!(status["last"]["files_bytes"], files_bytes);
    assert_eq!(status["last"]["bytes"], database_bytes + files_bytes);
    fs::remove_file(archive_path).unwrap();
    let status = daily_backup::status(home.path(), &db, true);
    assert_eq!(status["last"]["files_bytes"], 0);
    assert_eq!(status["last"]["bytes"], database_bytes);
}
