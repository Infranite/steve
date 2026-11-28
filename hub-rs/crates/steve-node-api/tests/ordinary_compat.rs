//! Ordinary Node wire-protocol baseline compatibility tests.
//!
//! These tests guard the other half of the `PROTOCOL` bump policy: when `PROTOCOL` does NOT
//! increment, every minimal baseline fixture from the current protocol generation must still
//! deserialize into the latest Rust types (catching forgotten `#[serde(default)]`, narrowed
//! field types, or removed enum variants), and non-strict structs must still ignore unknown
//! additive fields sent by a newer peer (catching accidental `#[serde(deny_unknown_fields)]`).
//!
//! Existing fixtures in `tests/fixtures/ordinary_compat/` are locked by `ops/cut-release.py lint`
//! unless `PROTOCOL` increments by `+1`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use steve_node_api::*;

const FIXTURE_DIR: &str = "tests/fixtures/ordinary_compat";
const PROTOCOL_FILE: &str = "protocol.txt";
const UNKNOWN_ADDITIVE_KEY: &str = "__future_additive_field";

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE_DIR)
}

fn load_fixture_json(name: &str) -> Value {
    let path = fixture_dir().join(name);
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read fixture {}: {e}", path.display()));
    serde_json::from_str(&raw)
        .unwrap_or_else(|e| panic!("fixture {} is not valid JSON: {e}", path.display()))
}

fn json_pointer(segments: &[&str]) -> String {
    let mut pointer = String::new();
    for segment in segments {
        pointer.push('/');
        pointer.push_str(&segment.replace('~', "~0").replace('/', "~1"));
    }
    pointer
}

fn inject_unknown_field(value: &mut Value, pointer: &str, fixture: &str) {
    let target = if pointer.is_empty() {
        value
    } else {
        value
            .pointer_mut(pointer)
            .unwrap_or_else(|| panic!("fixture {fixture}: JSON pointer {pointer:?} not found"))
    };
    let obj = target
        .as_object_mut()
        .unwrap_or_else(|| panic!("fixture {fixture}: JSON pointer {pointer:?} is not an object"));
    obj.insert(UNKNOWN_ADDITIVE_KEY.to_string(), json!(1));
}

fn assert_compat<T>(name: &str, unknown_pointers: &[&str], covered: &mut BTreeSet<String>)
where
    T: DeserializeOwned + PartialEq + std::fmt::Debug,
{
    assert!(
        covered.insert(name.to_string()),
        "duplicate fixture entry in test manifest: {name}"
    );
    let baseline_json = load_fixture_json(name);
    let baseline: T = serde_json::from_value(baseline_json.clone()).unwrap_or_else(|e| {
        panic!(
            "baseline fixture {name} failed to deserialize into {} at PROTOCOL {}: {e}\n\
             If this is an additive change, add `#[serde(default)]` instead of editing the baseline.\n\
             If this is a breaking change, increment `PROTOCOL` by +1, add `Protocol-Bump:` and `Upgrade:`, \
             and update `tests/fixtures/ordinary_compat/` together.",
            std::any::type_name::<T>(),
            PROTOCOL
        )
    });

    let mut with_unknown = baseline_json;
    for pointer in unknown_pointers {
        inject_unknown_field(&mut with_unknown, pointer, name);
    }
    let decoded_with_unknown: T = serde_json::from_value(with_unknown).unwrap_or_else(|e| {
        panic!(
            "fixture {name} rejected unknown additive field `{UNKNOWN_ADDITIVE_KEY}` on {}: {e}\n\
             Ordinary payloads without `#[serde(deny_unknown_fields)]` must ignore unknown fields \
             for rolling forward compatibility.",
            std::any::type_name::<T>()
        )
    });
    assert_eq!(
        decoded_with_unknown,
        baseline,
        "fixture {name}: unknown additive field changed decoded {}",
        std::any::type_name::<T>()
    );
}

#[test]
fn protocol_txt_matches_current_protocol() {
    let path = fixture_dir().join(PROTOCOL_FILE);
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
    let recorded: u32 = raw
        .trim()
        .parse()
        .unwrap_or_else(|e| panic!("{} must contain a u32 protocol number: {e}", path.display()));
    assert_eq!(
        recorded, PROTOCOL,
        "{PROTOCOL_FILE} records PROTOCOL {recorded}, but steve_node_api::PROTOCOL is {PROTOCOL}; \
         update {FIXTURE_DIR} alongside any PROTOCOL increment"
    );
}

#[test]
fn every_baseline_fixture_deserializes_and_ignores_unknown_fields() {
    let mut covered = BTreeSet::new();

    assert_compat::<HarnessChoice>("harness_choice.json", &[""], &mut covered);
    assert_compat::<OpenSession>(
        "open_session.json",
        &["", "/workspace", "/choice", "/policy"],
        &mut covered,
    );
    assert_compat::<RunSpec>(
        "run_spec.json",
        &["", "/choice", "/directory", "/policy", "/limits"],
        &mut covered,
    );
    assert_compat::<SessionView>("session_view.json", &["", "/effective"], &mut covered);
    assert_compat::<Capabilities>(
        "capabilities.json",
        &["", "/harnesses/0", "/roots/0", "/capacity"],
        &mut covered,
    );
    assert_compat::<AdapterInfo>(
        "adapter_info_ready.json",
        &["", "/session", "/session/models/0", "/capabilities"],
        &mut covered,
    );
    assert_compat::<AdapterInfo>(
        "adapter_info_missing_cmd.json",
        &["", "/missing_command"],
        &mut covered,
    );
    assert_compat::<MissingCommandDiagnostic>(
        "missing_command_diagnostic.json",
        &[""],
        &mut covered,
    );
    assert_compat::<HarnessSetup>("harness_setup.json", &["", "/components/0"], &mut covered);
    assert_compat::<HarnessUpdateReport>(
        "harness_update_report.json",
        &["", "/harnesses/0", "/harnesses/0/components/0"],
        &mut covered,
    );
    assert_compat::<NodeVitals>("node_vitals.json", &[""], &mut covered);
    assert_compat::<NodeVitals>(
        "node_vitals_populated.json",
        &[
            "",
            "/roots/0",
            "/session_vitals/0",
            "/meters",
            "/meters/process",
        ],
        &mut covered,
    );
    assert_compat::<HostInfo>("host_info.json", &["", "/file_access"], &mut covered);
    assert_compat::<NodeEvent>(
        "node_event.json",
        &["", "/cursor", "/body", "/body/completion"],
        &mut covered,
    );
    assert_compat::<EventBatch>(
        "event_batch.json",
        &[
            "",
            "/events/0",
            "/events/0/body",
            "/events/0/body/view",
            "/events/0/body/view/effort_mapping",
            "/events/1/body",
            "/events/1/body/tool",
            "/events/2/body",
            "/events/2/body/subtask",
            "/events/3/body",
            "/events/3/body/request",
            "/events/4/body",
            "/events/4/body/report",
            "/events/4/body/report/tokens",
            "/events/5/body",
        ],
        &mut covered,
    );
    assert_compat::<Error>("error.json", &["", "/retry"], &mut covered);
    // Build these JSON pointers from field segments so this test never embeds
    // the retired HTTP aggregate route literal guarded by `web/test/guard-read-side.test.mjs`.
    let receipt_state = json_pointer(&["state"]);
    let receipt_error = json_pointer(&["state", "error"]);
    let receipt_retry = json_pointer(&["state", "error", "retry"]);
    assert_compat::<Receipt>(
        "receipt_error.json",
        &["", &receipt_state, &receipt_error, &receipt_retry],
        &mut covered,
    );
    assert_compat::<Command<OpenSession>>(
        "command_open_session.json",
        &["", "/body", "/body/choice", "/body/policy"],
        &mut covered,
    );

    // Every file in the baseline directory must be either protocol.txt or an exercised JSON fixture.
    let mut on_disk = BTreeSet::new();
    for entry in std::fs::read_dir(fixture_dir()).unwrap() {
        let name = entry.unwrap().file_name().into_string().unwrap();
        if name == PROTOCOL_FILE {
            continue;
        }
        on_disk.insert(name);
    }
    assert_eq!(
        on_disk, covered,
        "every baseline fixture under {FIXTURE_DIR} must be exercised by ordinary_compat.rs"
    );
}

#[test]
fn strict_ordinary_commands_reject_unknown_fields() {
    let install_err = serde_json::from_value::<InstallHarness>(json!({
        "harness": "claude-code",
        UNKNOWN_ADDITIVE_KEY: 1,
    }));
    assert!(
        install_err.is_err(),
        "InstallHarness is strict (deny_unknown_fields) and changes require a PROTOCOL bump"
    );

    let upgrade_err = serde_json::from_value::<UpgradeHarness>(json!({
        "harness": "claude-code",
        UNKNOWN_ADDITIVE_KEY: 1,
    }));
    assert!(
        upgrade_err.is_err(),
        "UpgradeHarness is strict (deny_unknown_fields) and changes require a PROTOCOL bump"
    );
}

#[test]
fn host_info_confinement_is_additive_and_round_trips() {
    let baseline: HostInfo = serde_json::from_value(load_fixture_json("host_info.json")).unwrap();
    assert_eq!(baseline.confinement, None);

    let confined = HostInfo {
        confinement: Some(ConfinementStatus::Confined {
            backend: "seatbelt".into(),
        }),
        ..baseline.clone()
    };
    let round: HostInfo = serde_json::from_value(serde_json::to_value(&confined).unwrap()).unwrap();
    assert_eq!(round, confined);

    let unconfined = HostInfo {
        confinement: Some(ConfinementStatus::Unconfined {
            reason: "disabled by policy".into(),
        }),
        ..baseline
    };
    let round_unconfined: HostInfo =
        serde_json::from_value(serde_json::to_value(&unconfined).unwrap()).unwrap();
    assert_eq!(round_unconfined, unconfined);
}
