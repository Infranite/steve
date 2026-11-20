use rusqlite::{
    Connection,
    hooks::{AuthContext, Authorization},
};
use std::sync::{Arc, Mutex};
use steve_store_sqlite::{
    convert, legacy,
    migrate::{self, Migration, MigrationSet, Observer, Phase, Progress},
};
fn owner(c: &Connection) {
    c.execute_batch("INSERT INTO workspaces(id,name,created) VALUES('w','fixture',0); INSERT INTO users(id,workspace_id,subject,name,role,created) VALUES('u','w','fixture','fixture','owner',0);").unwrap();
}
fn observer() -> (Arc<Mutex<Vec<Progress>>>, Observer) {
    let events = Arc::new(Mutex::new(Vec::new()));
    let log = events.clone();
    (
        events,
        Arc::new(move |p| {
            log.lock().unwrap().push(p);
        }),
    )
}
fn assert_events(events: &[Progress], complete: bool) {
    assert!(!events.is_empty());
    assert!(events.windows(2).all(|p| p[1].vm_steps >= p[0].vm_steps));
    assert!(events.windows(2).all(|p| p[1].phase >= p[0].phase));
    assert!(events[..events.len() - 1].iter().all(|p| !p.complete));
    assert_eq!(events.last().unwrap().complete, complete);
}
#[path = "common/legacy_input.rs"]
mod legacy_input;
fn prefix(c: &Connection) {
    legacy_input::at(c, 113);
    owner(c);
}

#[test]
fn phases_and_cumulative_vm_work_end_only_after_contract_commit() {
    let mut c = Connection::open_in_memory().unwrap();
    prefix(&c);
    // A long statement in legacy stamping before the final hook; TEMP objects
    // do not change the main schema under validation.
    c.execute_batch("CREATE TEMP TRIGGER progress_legacy BEFORE DELETE ON main.schema_compat BEGIN SELECT (WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<20000) SELECT sum(x) FROM n); END;").unwrap();
    let (events, observe) = observer();
    convert::convert_observed(&mut c, observe).unwrap();
    let log = events.lock().unwrap();
    assert_events(&log, true);
    for phase in [
        Phase::LegacyReplay,
        Phase::Validation,
        Phase::InstallContract,
    ] {
        assert!(log.iter().any(|p| p.phase == phase));
    }
    assert_eq!(log.last().unwrap().phase, Phase::InstallContract);
    assert!(
        log.iter()
            .filter(|p| p.phase == Phase::LegacyReplay && p.vm_steps > 0)
            .count()
            > 2
    );
    drop(log);
    migrate::check_versions(&c, true).unwrap();
    let (domain_events, observe) = observer();
    migrate::migrate_observed(&mut c, observe).unwrap();
    let log = domain_events.lock().unwrap();
    assert_events(&log, true);
    assert!(log.iter().all(|p| p.phase == Phase::DomainMigration));
    for name in ["contract", "core", "stream", "comms"] {
        assert!(log.iter().any(|p| p.domain == Some(name)));
    }
}
#[test]
fn failed_final_commit_never_reports_complete_and_handler_is_removed() {
    let mut c = Connection::open_in_memory().unwrap();
    legacy_input::at(&c, 114);
    owner(&c);
    // Deny the sole atomic replay + installation commit.
    c.commit_hook(Some(|| true)).unwrap();
    let (events, observe) = observer();
    assert!(convert::convert_observed(&mut c, observe).is_err());
    assert_events(&events.lock().unwrap(), false);
    assert_eq!(
        c.query_row(
            "SELECT count(*) FROM sqlite_schema WHERE name='meta'",
            [],
            |r| r.get::<_, u32>(0)
        )
        .unwrap(),
        0
    );
    c.commit_hook(None::<fn() -> bool>).unwrap();
    let count = events.lock().unwrap().len();
    c.query_row("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<10000) SELECT sum(x) FROM n",[],|r|r.get::<_,i64>(0)).unwrap();
    assert_eq!(events.lock().unwrap().len(), count);
    let (_, observe) = observer();
    convert::convert_observed(&mut c, observe).unwrap();
}
#[test]
fn domain_backfill_has_vm_progress_inside_long_sql_and_failed_commit_is_not_complete() {
    const SQL: &str = "CREATE TABLE progress_rows(x INTEGER); WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<20000) INSERT INTO progress_rows(x) SELECT x FROM n;";
    let set = MigrationSet {
        domain: "core",
        migrations: &[Migration {
            version: 1,
            name: "0001_progress.sql",
            sql: SQL,
        }],
        lock: "0001_progress.sql e455630a7e6e5f0ded6e56822af6c8492d71b3713fb9498d9a5056b148ad3e31\n",
    };
    for fail in [false, true] {
        let mut c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE domain_schema(domain TEXT PRIMARY KEY,version INTEGER);")
            .unwrap();
        if fail {
            c.commit_hook(Some(|| true)).unwrap();
        }
        let (events, observe) = observer();
        let result = migrate::apply_set_observed(&mut c, &set, observe);
        assert_eq!(result.is_err(), fail);
        let log = events.lock().unwrap();
        assert_events(&log, !fail);
        assert!(
            log.iter()
                .filter(|p| p.domain == Some("core") && p.vm_steps > 0 && !p.complete)
                .count()
                > 2
        );
        drop(log);
        assert!(c.is_autocommit());
        assert_eq!(
            c.query_row(
                "SELECT count(*) FROM sqlite_schema WHERE name='progress_rows'",
                [],
                |r| r.get::<_, u32>(0)
            )
            .unwrap(),
            u32::from(!fail)
        );
    }
}
#[test]
fn validation_failure_after_legacy_work_preserves_error_and_never_completes() {
    let mut c = Connection::open_in_memory().unwrap();
    prefix(&c);
    c.execute_batch("ALTER TABLE users ADD COLUMN unwanted TEXT")
        .unwrap();
    let (events, observe) = observer();
    let error = convert::convert_observed(&mut c, observe).unwrap_err();
    assert_eq!(error.code, steve_types::Code::Incompatible);
    assert!(error.safe_detail.contains("users"));
    assert_events(&events.lock().unwrap(), false);
    assert_eq!(legacy::version(&c).unwrap(), 113);
    c.authorizer(None::<fn(AuthContext<'_>) -> Authorization>)
        .unwrap();
}

#[test]
fn supervisor_stream_finishes_once_after_all_four_stages_and_domain_commits() {
    let mut c = Connection::open_in_memory().unwrap();
    prefix(&c);
    let (events, observe) = observer();
    convert::convert_and_migrate_observed(&mut c, observe).unwrap();
    let log = events.lock().unwrap();
    assert_events(&log, true);
    for phase in [
        Phase::LegacyReplay,
        Phase::Validation,
        Phase::InstallContract,
        Phase::DomainMigration,
    ] {
        assert!(log.iter().any(|p| p.phase == phase && !p.complete));
    }
    assert_eq!(log.iter().filter(|p| p.complete).count(), 1);
    assert_eq!(log.last().unwrap().phase, Phase::DomainMigration);
}
