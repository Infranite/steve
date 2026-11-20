use rusqlite::Connection;
use steve_store_sqlite::{
    compat::{CurrentStorage, StorageCapabilities},
    layout::Layout,
    migrate,
};

#[test]
fn current_identity_roundtrips_for_admin_snapshots_and_backups() {
    let mut c = Connection::open_in_memory().unwrap();
    migrate::initialize(&mut c, &Layout::single_file()).unwrap();
    let state = CurrentStorage::inspect(&c).unwrap();
    let mut json = serde_json::to_value(&state).unwrap();
    assert_eq!(state.domains.len(), 3);
    assert_eq!(state.layout.files.len(), 1);
    assert!(!state.layout.store_generation.is_empty());
    assert_eq!(
        serde_json::from_value::<CurrentStorage>(json.clone()).unwrap(),
        state
    );
    json["unknown"] = serde_json::json!(1);
    assert!(serde_json::from_value::<CurrentStorage>(json).is_err());
}

#[test]
fn current_business_metadata_refuses_empty_legacy_and_future_without_writes() {
    let c = Connection::open_in_memory().unwrap();
    assert!(CurrentStorage::inspect(&c).is_err());
    c.execute_batch(include_str!("../legacy/113_fixture.sql"))
        .unwrap();
    let before = c.total_changes();
    assert!(CurrentStorage::inspect(&c).is_err());
    assert_eq!(c.total_changes(), before);
    assert_eq!(steve_store_sqlite::legacy::version(&c).unwrap(), 113);
    let mut c = Connection::open_in_memory().unwrap();
    migrate::initialize(&mut c, &Layout::single_file()).unwrap();
    c.execute(
        "UPDATE domain_schema SET version=999 WHERE domain='core'",
        [],
    )
    .unwrap();
    let before = c.total_changes();
    assert!(CurrentStorage::inspect(&c).is_err());
    assert_eq!(c.total_changes(), before);
}

#[test]
fn release_capabilities_share_the_compiled_matrix_and_reject_null_legacy() {
    let caps = StorageCapabilities::compiled();
    let mut manifest = serde_json::json!({"storage":caps.encode_build_info_draft().unwrap()});
    assert_eq!(StorageCapabilities::from_manifest(&manifest).unwrap(), caps);
    manifest["storage"]["legacy"] = serde_json::Value::Null;
    assert!(StorageCapabilities::from_manifest(&manifest).is_err());
    manifest["storage"]
        .as_object_mut()
        .unwrap()
        .remove("legacy");
    assert_eq!(
        StorageCapabilities::from_manifest(&manifest)
            .unwrap()
            .legacy,
        None
    );
}
