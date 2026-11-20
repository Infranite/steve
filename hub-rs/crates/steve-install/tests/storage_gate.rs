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
        assert_eq!(
            native.is_ok(),
            case["allowed"].as_bool().unwrap(),
            "{}: {native:?}",
            case["name"]
        );
        if let Some(state) = case.get("state") {
            assert_eq!(
                &steve_install::release::schema_state(&db).unwrap(),
                state,
                "{}",
                case["name"]
            );
        }
        let python = Command::new("python3")
            .arg(root.join("ops/schema_gate.py"))
            .arg("check")
            .arg(&db)
            .arg(temp.path())
            .output()
            .unwrap();
        assert_eq!(
            python.status.success(),
            native.is_ok(),
            "{}: {}",
            case["name"],
            String::from_utf8_lossy(&python.stdout)
        );
        assert_eq!(
            fs::read(&db).ok(),
            before,
            "source changed: {}",
            case["name"]
        );
    }
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
