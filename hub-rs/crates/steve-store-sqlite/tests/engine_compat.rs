use rusqlite::Connection;
use steve_store_sqlite::{
    compat::{StorageCapabilities, StorageState},
    layout::{CORE_FILE, Layout},
    migrate, open,
};
#[test]
fn candidate_matrix_is_the_same_rule_used_by_opens() {
    let caps = StorageCapabilities::compiled();
    let encoded = caps.encode_build_info_draft().unwrap();
    assert_eq!(encoded["writable_layout"], 2);
    assert_eq!(encoded["legacy_input"], 113);
    assert_eq!(
        serde_json::from_value::<StorageCapabilities>(encoded.clone()).unwrap(),
        caps
    );
    let mut missing = encoded.clone();
    missing.as_object_mut().unwrap().remove("contract_schema");
    assert!(serde_json::from_value::<StorageCapabilities>(missing).is_err());
    let mut unknown = encoded;
    unknown["unknown"] = serde_json::json!(1);
    assert!(serde_json::from_value::<StorageCapabilities>(unknown).is_err());
    for (state, runtime, admin) in [
        (StorageState::Empty, true, false),
        (
            StorageState::Legacy {
                schema_version: 112,
            },
            false,
            false,
        ),
        (
            StorageState::Legacy {
                schema_version: 113,
            },
            true,
            false,
        ),
        (
            StorageState::Legacy {
                schema_version: 114,
            },
            false,
            false,
        ),
    ] {
        assert_eq!(caps.can_open(&state, false).is_ok(), runtime);
        assert_eq!(caps.can_open(&state, true).is_ok(), admin);
    }
    let mut c = Connection::open_in_memory().unwrap();
    migrate::initialize(&mut c, &Layout::single_file()).unwrap();
    let current = open::inspect(&c).unwrap();
    assert!(caps.can_open(&current, false).is_ok());
    assert!(caps.can_open(&current, true).is_ok());
    for sql in [
        "UPDATE domain_schema SET version=0 WHERE domain='core'",
        "UPDATE domain_schema SET version=2 WHERE domain='core'",
        "UPDATE meta SET value='2' WHERE key='contract_schema'",
        "UPDATE meta SET value='3' WHERE key='min_reader'",
        "UPDATE meta SET value='3' WHERE key='layout'",
    ] {
        let tx = c.transaction().unwrap();
        tx.execute_batch(sql).unwrap();
        let state = open::inspect(&tx).unwrap(); // Observation can describe future headers.
        assert!(caps.can_open(&state, true).is_err());
        assert_eq!(
            caps.can_open(&state, false).is_ok(),
            sql.contains("version=0")
        );
    }
    let mut candidate = caps.clone();
    candidate.domains.remove("comms");
    assert!(candidate.can_open(&StorageState::Empty, false).is_err());
    candidate = caps;
    candidate.contract_schema.target = candidate.contract_schema.max_read + 1;
    assert!(candidate.can_open(&current, false).is_err());
}
#[test]
fn malformed_metadata_is_rejected_before_persistent_pragmas() {
    for mutation in [
        "DROP TABLE cursor",
        "DELETE FROM domain_schema WHERE domain='comms'",
        "INSERT INTO domain_schema VALUES('unknown',1)",
        "DELETE FROM schema_compat",
        "UPDATE schema_compat SET min_reader=113",
        "CREATE TABLE schema_version(v); INSERT INTO schema_version VALUES(112)",
        "UPDATE meta SET value='wrong' WHERE key='file_id' AND domain='core'",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CORE_FILE);
        let mut c = Connection::open(&path).unwrap();
        migrate::initialize(&mut c, &Layout::single_file()).unwrap();
        c.execute_batch(mutation).unwrap();
        drop(c);
        let before = std::fs::read(&path).unwrap();
        for result in [open::open_runtime(dir.path()), open::open_admin(dir.path())] {
            assert!(result.is_err(), "{mutation}");
            assert_eq!(std::fs::read(&path).unwrap(), before, "{mutation}");
            assert!(!dir.path().join("steve.sqlite3-wal").exists());
        }
    }
}
#[test]
fn empty_partial_unknown_and_readonly_observation_never_guess_a_legacy_database() {
    for sql in [
        "",
        "CREATE TABLE cursor(domain)",
        "CREATE VIEW meta AS SELECT 1",
        "CREATE TABLE mystery(x)",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CORE_FILE);
        Connection::open(&path).unwrap().execute_batch(sql).unwrap();
        let before = std::fs::read(&path).unwrap();
        let ro = open::open_readonly(&path).unwrap();
        let state = open::inspect(&ro);
        if sql.is_empty() {
            assert_eq!(state.unwrap(), StorageState::Empty);
        } else {
            assert!(state.is_err());
        }
        drop(ro);
        assert!(open::open_admin(dir.path()).is_err());
        if !sql.is_empty() {
            assert!(open::open_runtime(dir.path()).is_err());
        }
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
}
