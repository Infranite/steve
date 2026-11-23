//! Installation paths belong to the Hub, independently of the database's directory.
use std::{sync::mpsc, time::Duration};
use steve_channels::PreDestructiveBackup;
use steve_hub::backup::{self, ChannelBackup};
use steve_store_sqlite::backup::Reason;

#[test]
fn cleanup_daily_and_management_plans_share_the_configured_backup_root() {
    let home = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let db = data.path().join("steve.sqlite3");
    let store = steve_store::Store::open(&db, steve_store::NoPublisher).unwrap();
    let root = home.path().join("backups");
    std::fs::write(home.path().join("steve.env"), "fixture").unwrap();
    for reason in [
        Reason::Manual,
        Reason::Daily,
        Reason::Diagnostic,
        Reason::PreUpgrade {
            from: "old".into(),
            to: "new".into(),
        },
        Reason::PreDestructive {
            what: "purge".into(),
        },
    ] {
        let plan = backup::plan(home.path(), reason.clone());
        assert_eq!(plan.dest_root, root);
        assert_eq!(
            plan.with_files.is_some(),
            reason.rule().files != steve_store_sqlite::backup::FileScope::None
        );
    }
    let port = ChannelBackup::new(home.path().to_owned(), db.clone());
    let relative = port.backup("purge").unwrap();
    assert!(!relative.is_absolute());
    assert!(root.join(&relative).is_file());
    assert_eq!(steve_hub::daily_backup::dir(home.path()), root);
    let daily = steve_hub::daily_backup::make(home.path(), &db, "20261119").unwrap();
    assert!(daily.database.starts_with(&root));
    assert!(
        !data.path().join("backups").exists(),
        "never infer home from database location"
    );
    let manifests = steve_store_sqlite::backup::list(&root).unwrap();
    assert_eq!(manifests.len(), 2);
    assert!(manifests.iter().all(|m| !m.files.artifacts.is_empty()));
    assert!(manifests.iter().any(|m| m.reason
        == Reason::PreDestructive {
            what: "purge".into()
        }));
    assert!(manifests.iter().any(|m| m.reason == Reason::Daily));
    drop(store);
}

#[test]
fn channel_backup_does_not_acquire_the_store_writer_mutex() {
    let home = tempfile::tempdir().unwrap();
    let store =
        steve_store::Store::open(home.path().join("steve.sqlite3"), steve_store::NoPublisher)
            .unwrap();
    let port = ChannelBackup::new(home.path().to_owned(), store.path().to_owned());
    let (held, ready) = mpsc::channel();
    let (release, wait) = mpsc::channel();
    let locked = store.clone();
    let owner = std::thread::spawn(move || {
        locked.write(|_| -> steve_store::Result<()> {
            held.send(()).unwrap();
            wait.recv().unwrap();
            Ok(())
        })
    });
    ready.recv().unwrap();
    let (finished, result) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        finished.send(port.backup("purge")).unwrap();
    });
    let completed = result.recv_timeout(Duration::from_secs(5));
    release.send(()).unwrap();
    owner.join().unwrap().unwrap();
    worker.join().unwrap();
    let relative = completed
        .expect("backup must use an independent connection")
        .unwrap();
    assert!(home.path().join("backups").join(relative).is_file());
}

#[test]
fn daily_backup_off_keeps_the_real_channel_cleanup_backup_capability_active() {
    let home = tempfile::tempdir().unwrap();
    let store =
        steve_store::Store::open(home.path().join("db.sqlite3"), steve_store::NoPublisher).unwrap();
    std::fs::write(home.path().join("steve.env"), "fixture").unwrap();
    backup::save_daily_settings(
        &store,
        backup::DailySettings {
            enabled: false,
            keep_days: 7,
        },
        "usr_dev",
    )
    .unwrap();
    assert!(
        steve_hub::daily_backup::make_if_enabled(&store, home.path(), store.path(), "today")
            .unwrap()
            .is_none()
    );
    let port = ChannelBackup::new(home.path().to_owned(), store.path().to_owned());
    let relative = port.backup("purge").unwrap();
    assert!(backup::root(home.path()).join(relative).is_file());
    let manifests = steve_store_sqlite::backup::list(&backup::root(home.path())).unwrap();
    assert_eq!(manifests.len(), 1);
    assert!(matches!(manifests[0].reason, Reason::PreDestructive { .. }));
    assert!(!manifests[0].files.artifacts.is_empty());
}
