use rusqlite::Connection;
use std::time::Duration;
use steve_store_sqlite::{
    checkpoint::{Action, CheckpointPolicy, FORCE_BYTES, INTERVAL, Mode, PASSIVE_BYTES},
    open, pragmas,
};

#[test]
fn default_thresholds_and_elapsed_time_are_explicit() {
    let policy = CheckpointPolicy::default();
    assert_eq!(policy.passive_bytes, 64 * 1024 * 1024);
    assert_eq!(policy.force_bytes, 512 * 1024 * 1024);
    assert_eq!(
        (policy.passive_bytes, policy.force_bytes, policy.interval),
        (PASSIVE_BYTES, FORCE_BYTES, INTERVAL)
    );
    assert_eq!(
        policy.decide(0, false, Duration::from_secs(29)),
        Action::None
    );
    assert_eq!(
        policy.decide(0, false, Duration::from_secs(30)),
        Action::Passive
    );
    assert_eq!(
        policy.decide(PASSIVE_BYTES, false, Duration::ZERO),
        Action::Passive
    );
    assert_eq!(
        policy.decide(FORCE_BYTES, false, Duration::ZERO),
        Action::ForcedPassive { alert: true }
    );
    assert_eq!(policy.decide(1, true, Duration::ZERO), Action::Truncate);
}

#[test]
fn pinned_snapshot_forces_pressure_alert_then_release_allows_truncate_to_zero() {
    let dir = tempfile::tempdir().unwrap();
    let engine = open::open_runtime(dir.path()).unwrap();
    let writer = Connection::open(engine.path()).unwrap();
    pragmas::connection(&writer, true).unwrap();
    writer.execute_batch("CREATE TABLE engine_wal(id INTEGER PRIMARY KEY, payload BLOB); INSERT INTO engine_wal VALUES(0,x'00');").unwrap();
    engine.checkpoint(Mode::Truncate).unwrap();
    let snapshot = engine.read_conn().unwrap();
    snapshot.execute_batch("BEGIN").unwrap();
    assert_eq!(
        snapshot
            .query_row("SELECT count(*) FROM engine_wal", [], |r| r
                .get::<_, u32>(0))
            .unwrap(),
        1
    );
    let policy = CheckpointPolicy {
        passive_bytes: 4 * 1024 * 1024,
        force_bytes: 32 * 1024 * 1024,
        interval: Duration::from_secs(30),
    };
    let wal = dir.path().join("steve.sqlite3-wal");
    let mut forced = false;
    let mut saw_passive = false;
    for _ in 0..40 {
        writer
            .execute(
                "INSERT INTO engine_wal(payload) VALUES(zeroblob(1048576))",
                [],
            )
            .unwrap();
        let bytes = std::fs::metadata(&wal).unwrap().len();
        match policy.decide(bytes, false, Duration::ZERO) {
            Action::Passive => {
                saw_passive = true;
                let (_, log, done) = engine.checkpoint(Mode::Passive).unwrap();
                assert!(
                    done < log,
                    "pinned snapshot must prevent a complete checkpoint"
                );
            }
            Action::ForcedPassive { alert } => {
                assert!(alert);
                assert!(bytes >= 32 * 1024 * 1024);
                let (_, log, done) = engine.checkpoint(Mode::Passive).unwrap();
                assert!(done < log);
                forced = true;
                break;
            }
            Action::None => {}
            Action::Truncate => panic!("writer is not idle"),
        }
    }
    assert!(saw_passive && forced);
    let (busy, log, done) = engine.checkpoint(Mode::Truncate).unwrap();
    assert_eq!(busy, 1, "pinned reader requires a later retry");
    assert!(done < log);
    assert_eq!(
        snapshot
            .query_row("SELECT count(*) FROM engine_wal", [], |r| r
                .get::<_, u32>(0))
            .unwrap(),
        1
    );
    snapshot.execute_batch("ROLLBACK").unwrap();
    drop(snapshot);
    assert_eq!(
        policy.decide(std::fs::metadata(&wal).unwrap().len(), true, Duration::ZERO),
        Action::Truncate
    );
    let (busy, _, _) = engine.checkpoint(Mode::Truncate).unwrap();
    assert_eq!(busy, 0);
    assert_eq!(std::fs::metadata(&wal).unwrap().len(), 0);
}
