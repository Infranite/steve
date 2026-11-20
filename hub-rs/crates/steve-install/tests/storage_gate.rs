use rusqlite::Connection;
use serde_json::Value;
use std::{fs, path::Path, process::Command};

fn matrix() -> Value {
    serde_json::from_str(include_str!("storage_matrix.json")).unwrap()
}
fn manifest(data: &Value, case: &Value) -> Value {
    let mut value = data["manifest"].clone();
    if let Some(patch) = case["patch"].as_object() {
        for (path, replacement) in patch {
            let (parent, key) = path.rsplit_once('/').unwrap();
            let object = value.pointer_mut(parent).unwrap().as_object_mut().unwrap();
            if replacement.is_null() {
                object.remove(key);
            } else {
                object.insert(key.into(), replacement.clone());
            }
        }
    }
    value
}
#[test]
fn same_storage_matrix_as_python_and_packaged_manifests() {
    let data = matrix();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let mut failures = Vec::new();
    for case in data["cases"].as_array().unwrap() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("odd ?#% 数据.sqlite3");
        if case["missing"] != true {
            let c = Connection::open(&db).unwrap();
            let base = format!("{}_sql", case["base"].as_str().unwrap());
            c.execute_batch(&format!(
                "{}{}",
                data[&base].as_str().unwrap_or(""),
                case["sql"].as_str().unwrap()
            ))
            .unwrap();
        }
        fs::write(
            temp.path().join("release.json"),
            serde_json::to_vec(&manifest(&data, case)).unwrap(),
        )
        .unwrap();
        let before = fs::read(&db).ok();
        let native = steve_install::release::check_schema(&db, temp.path());
        let allowed = case["allowed"].as_bool().unwrap();
        if native.is_ok() != allowed {
            failures.push(format!(
                "{}: native={native:?}, allowed={allowed}",
                case["name"]
            ));
        }
        if let Some(expected) = case.get("state") {
            match steve_install::release::schema_state(&db) {
                Ok(actual) if &actual == expected => {}
                actual => failures.push(format!(
                    "{}: state={actual:?}, expected={expected}",
                    case["name"]
                )),
            }
        }
        let python = Command::new("python3")
            .arg(root.join("ops/schema_gate.py"))
            .arg("check")
            .arg(&db)
            .arg(temp.path())
            .output()
            .unwrap();
        if python.status.success() != native.is_ok() {
            failures.push(format!(
                "{}: Python/native mismatch: {}",
                case["name"],
                String::from_utf8_lossy(&python.stdout)
            ));
        }
        if fs::read(&db).ok() != before {
            failures.push(format!("source changed: {}", case["name"]));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn compiled_converter_accepts_both_supported_legacy_inputs() {
    use steve_store_sqlite::compat::{StorageCapabilities, StorageState};
    let caps = StorageCapabilities::compiled();
    assert_eq!(caps.legacy.unwrap().min_read, 113);
    assert_eq!(caps.legacy.unwrap().target, 114);
    for schema_version in [112, 113, 114, 115] {
        assert_eq!(
            caps.can_open(&StorageState::Legacy { schema_version }, false)
                .is_ok(),
            (113..=114).contains(&schema_version)
        );
    }
}

#[test]
fn read_only_inspection_sees_live_wal_without_checkpointing_or_changing_journal_mode() {
    let data = matrix();
    let temp = tempfile::tempdir().unwrap();
    let db = temp.path().join("live.sqlite3");
    let writer = Connection::open(&db).unwrap();
    writer
        .execute_batch(data["current_sql"].as_str().unwrap())
        .unwrap();
    writer.pragma_update(None, "journal_mode", "WAL").unwrap();
    writer
        .execute("UPDATE domain_schema SET version=2 WHERE domain='core'", [])
        .unwrap();
    let mut manifest = data["manifest"].clone();
    manifest["storage"]["core"]["target"] = 2.into();
    fs::write(
        temp.path().join("release.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    // Keep the writer open: this is a live WAL database, not an immutable snapshot.
    let wal = temp.path().join("live.sqlite3-wal");
    let before_db = fs::read(&db).unwrap();
    let before_wal = fs::read(&wal).unwrap();
    assert!(!before_wal.is_empty());
    assert_eq!(
        steve_install::release::schema_state(&db).unwrap()["core"],
        2
    );
    steve_install::release::check_schema(&db, temp.path()).unwrap();
    assert_eq!(
        writer
            .pragma_query_value(None, "journal_mode", |r| r.get::<_, String>(0))
            .unwrap(),
        "wal"
    );
    assert_eq!(fs::read(&db).unwrap(), before_db);
    assert_eq!(fs::read(&wal).unwrap(), before_wal);
    // SQLite's shared-memory sidecar is deliberately not asserted byte-for-byte:
    // read-only WAL access may maintain -shm while leaving database/WAL contents alone.
}
