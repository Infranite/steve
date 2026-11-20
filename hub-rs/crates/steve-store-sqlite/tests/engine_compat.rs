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
    assert_eq!(encoded["layout"]["target"], 2);
    assert_eq!(
        encoded["legacy"],
        serde_json::json!({"min_read":113,"target":114})
    );
    assert_eq!(
        serde_json::from_value::<StorageCapabilities>(encoded.clone()).unwrap(),
        caps
    );
    let mut missing = encoded.clone();
    missing.as_object_mut().unwrap().remove("contract");
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
            true,
            false,
        ),
        (
            StorageState::Legacy {
                schema_version: 115,
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
        assert!(caps.can_open(&state, false).is_err());
    }
    let mut candidate = caps;
    candidate.layout.min_read = candidate.layout.target + 1;
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
        "UPDATE schema_compat SET min_reader=114",
        "UPDATE schema_compat SET min_reader=1001",
        "INSERT INTO schema_compat(min_reader) VALUES(1000)",
        "ALTER TABLE schema_compat ADD COLUMN unexpected TEXT",
        "DROP TABLE schema_compat; CREATE TABLE schema_compat(min_reader INTEGER NOT NULL DEFAULT 1000); INSERT INTO schema_compat(min_reader) VALUES(1000)",
        "CREATE TABLE schema_version(v); INSERT INTO schema_version VALUES(113)",
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

#[test]
fn candidate_declarations_are_independent_of_the_inspecting_binary() {
    use steve_store_sqlite::compat::SchemaRange;
    let mut candidate = StorageCapabilities::compiled();
    candidate.legacy = Some(SchemaRange {
        min_read: 114,
        target: 115,
    });
    for (version, allowed) in [(113, false), (114, true), (115, true), (116, false)] {
        assert_eq!(
            candidate
                .can_open(
                    &StorageState::Legacy {
                        schema_version: version
                    },
                    false
                )
                .is_ok(),
            allowed
        );
    }
    candidate.legacy = None;
    assert!(
        candidate
            .can_open(
                &StorageState::Legacy {
                    schema_version: 114
                },
                false
            )
            .is_err()
    );
    let mut conn = Connection::open_in_memory().unwrap();
    migrate::initialize(&mut conn, &Layout::single_file()).unwrap();
    conn.execute(
        "UPDATE meta SET value='3' WHERE key IN ('layout','min_reader')",
        [],
    )
    .unwrap();
    candidate.layout.min_read = 3;
    candidate.layout.target = 3;
    let state = open::inspect(&conn).unwrap();
    assert!(candidate.can_open(&state, true).is_ok());
    assert!(
        StorageCapabilities::compiled()
            .can_open(&state, false)
            .is_err()
    );
}
#[test]
fn future_legacy_version_and_reader_request_a_newer_program_without_writing() {
    for sql in [
        "UPDATE schema_version SET v=115",
        "UPDATE schema_compat SET min_reader=1000",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CORE_FILE);
        let c = Connection::open(&path).unwrap();
        steve_store_sqlite::legacy::final_structure(&c).unwrap();
        c.execute_batch(sql).unwrap();
        drop(c);
        let before = std::fs::read(&path).unwrap();
        let e = open::open_runtime(dir.path()).err().unwrap();
        assert!(e.safe_detail.contains("需要更新的程序"));
        assert!(!e.safe_detail.contains("先升级到上一个发布版"));
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
}

#[test]
fn encoded_storage_is_flat_uniform_strict_and_matches_the_embedded_catalog() {
    let caps = StorageCapabilities::compiled();
    let json = caps.encode_build_info_draft().unwrap();
    let mut keys = json
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>();
    keys.sort_unstable();
    assert_eq!(
        keys,
        ["comms", "contract", "core", "layout", "legacy", "stream"]
    );
    for name in keys {
        assert_eq!(
            json[name]
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["min_read", "target"]
        );
    }
    for set in migrate::CATALOG {
        assert_eq!(
            json[set.domain]["min_read"],
            set.migrations.first().unwrap().version
        );
        assert_eq!(
            json[set.domain]["target"],
            set.migrations.last().unwrap().version
        );
    }
    let legacy_head = include_str!("../legacy/migrations/LOCK")
        .lines()
        .filter_map(|line| {
            line.split_whitespace()
                .next()?
                .strip_suffix(".sql")?
                .parse::<u32>()
                .ok()
        })
        .max()
        .unwrap();
    assert_eq!(json["legacy"]["min_read"], 113);
    assert_eq!(json["legacy"]["target"], legacy_head);
    assert_eq!(
        json["layout"]["target"],
        steve_store_sqlite::layout::LAYOUT_GENERATION
    );
    for field in ["layout", "contract", "core", "stream", "comms"] {
        let mut missing = json.clone();
        missing.as_object_mut().unwrap().remove(field);
        assert!(
            serde_json::from_value::<StorageCapabilities>(missing).is_err(),
            "{field}"
        );
    }
    let mut no_legacy = json.clone();
    no_legacy.as_object_mut().unwrap().remove("legacy");
    let candidate = serde_json::from_value::<StorageCapabilities>(no_legacy.clone()).unwrap();
    assert_eq!(candidate.encode_build_info_draft().unwrap(), no_legacy);
    assert!(
        candidate
            .can_open(
                &StorageState::Legacy {
                    schema_version: 113
                },
                false
            )
            .is_err()
    );
    for old in ["format_version", "domains", "contract_schema"] {
        let mut json = json.clone();
        json[old] = serde_json::json!(1);
        assert!(serde_json::from_value::<StorageCapabilities>(json).is_err());
    }
    for (field, key) in [
        ("core", "max_read"),
        ("legacy", "max_read"),
        ("legacy", "convert"),
    ] {
        let mut json = json.clone();
        json[field][key] = serde_json::json!(1);
        assert!(serde_json::from_value::<StorageCapabilities>(json).is_err());
    }
}
#[test]
fn every_item_uses_the_same_inclusive_bounds_and_exact_target_rule() {
    use steve_store_sqlite::compat::SchemaRange;
    let c = &mut Connection::open_in_memory().unwrap();
    migrate::initialize(c, &Layout::single_file()).unwrap();
    let current = open::inspect(c).unwrap();
    for item in ["layout", "contract", "core", "stream", "comms"] {
        let mut candidate = StorageCapabilities::compiled();
        let range = match item {
            "layout" => &mut candidate.layout,
            "contract" => &mut candidate.contract,
            "core" => &mut candidate.core,
            "stream" => &mut candidate.stream,
            _ => &mut candidate.comms,
        };
        let actual = range.target;
        *range = SchemaRange {
            min_read: actual,
            target: actual + 1,
        };
        assert!(candidate.can_open(&current, false).is_ok(), "{item}");
        assert!(candidate.can_open(&current, true).is_err(), "{item}");
        range_for(&mut candidate, item).min_read = actual + 1;
        assert!(candidate.can_open(&current, false).is_err(), "{item}");
        *range_for(&mut candidate, item) = SchemaRange {
            min_read: actual,
            target: actual,
        };
        assert!(candidate.can_open(&current, true).is_ok(), "{item}");
    }
    fn range_for<'a>(c: &'a mut StorageCapabilities, n: &str) -> &'a mut SchemaRange {
        match n {
            "layout" => &mut c.layout,
            "contract" => &mut c.contract,
            "core" => &mut c.core,
            "stream" => &mut c.stream,
            _ => &mut c.comms,
        }
    }
}

#[path = "common/legacy_input.rs"]
mod legacy_input;

fn legacy_window(version: i64, accepted: bool, detail: &str) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(CORE_FILE);
    let c = Connection::open(&path).unwrap();
    legacy_input::at(&c, version.min(114));
    c.execute("UPDATE schema_version SET v=?1", [version])
        .unwrap();
    c.execute_batch("INSERT INTO workspaces(id,name,created) VALUES('w','fixture',0); INSERT INTO users(id,workspace_id,subject,name,role,created) VALUES('o','w','fixture','fixture','owner',0)").unwrap();
    // Candidate declarations can be tested even when this binary refuses observation.
    let state = StorageState::Legacy {
        schema_version: version,
    };
    assert_eq!(
        StorageCapabilities::compiled()
            .can_open(&state, false)
            .is_ok(),
        accepted
    );
    drop(c);
    let before = std::fs::read(&path).unwrap();
    let opened = open::open_runtime(dir.path());
    if accepted {
        let engine = opened.unwrap();
        let c = engine.read_conn().unwrap();
        assert!(matches!(
            open::inspect(&c).unwrap(),
            StorageState::Current { .. }
        ));
        assert_eq!(
            c.query_row("SELECT count(*) FROM users WHERE role='owner'", [], |r| r
                .get::<_, u32>(
                0
            ))
            .unwrap(),
            1
        );
    } else {
        let error = opened.err().unwrap();
        assert!(error.safe_detail.contains(detail), "{}", error.safe_detail);
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let mut c = Connection::open(&path).unwrap();
        let error = steve_store_sqlite::convert::convert(&mut c).unwrap_err();
        assert!(error.safe_detail.contains(detail), "{}", error.safe_detail);
        drop(c);
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
}
#[test]
fn legacy_113_replays_114_then_becomes_current() {
    legacy_window(113, true, "");
}
#[test]
fn legacy_114_becomes_current_without_replaying_114() {
    legacy_window(114, true, "");
}
#[test]
fn legacy_112_is_refused_without_changing_file_bytes() {
    legacy_window(112, false, "先升级到上一个发布版");
}
#[test]
fn legacy_115_requires_newer_program_without_changing_file_bytes() {
    legacy_window(115, false, "需要更新的程序");
}
