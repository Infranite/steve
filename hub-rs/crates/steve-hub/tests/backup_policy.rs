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
    for reason in [
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
        assert_eq!(backup::plan(home.path(), reason).dest_root, root);
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
    assert!(manifests.iter().any(|m| m.reason
        == Reason::PreDestructive {
            what: "purge".into()
        }));
    assert!(manifests.iter().any(|m| m.reason == Reason::Daily));
    drop(store);
}

#[test]
fn channel_backup_does_not_acquire_the_legacy_writer_mutex() {
    let home = tempfile::tempdir().unwrap();
    let store =
        steve_store::Store::open(home.path().join("steve.sqlite3"), steve_store::NoPublisher)
            .unwrap();
    let port = ChannelBackup::new(home.path().to_owned(), store.path().to_owned());
    let (held, ready) = mpsc::channel();
    let (release, wait) = mpsc::channel();
    let locked = store.clone();
    let owner = std::thread::spawn(move || {
        locked.with_conn(|_| {
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
