//! v1 compiles as a standalone module: crate-local mutable Node types are unavailable here.
#[path = "../src/upgrade_v1.rs"]
#[allow(dead_code)]
mod v1;

use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn value<T: Serialize>(v: T) -> Value {
    serde_json::to_value(v).unwrap()
}

fn frame(kind: v1::Kind, method: &str, p: Value) -> v1::Frame {
    let response = match kind {
        v1::Kind::Res => true,
        v1::Kind::Req | v1::Kind::Ctl | v1::Kind::Unknown => false,
    };
    v1::Frame {
        v: 1,
        t: kind,
        id: if response { "n_1" } else { "h_1" }.into(),
        re: response.then(|| "h_1".into()),
        m: method.into(),
        generation: 7,
        trace: Some("trace-golden".into()),
        deadline_at: 1_700_000_001_000,
        p,
    }
}

fn build() -> v1::BuildIdentity {
    v1::BuildIdentity {
        version: "2.0.0".into(),
        commit: "ab".repeat(20),
        target: "aarch64-apple-darwin".into(),
    }
}

fn state(phase: v1::Phase) -> v1::UpgradeState {
    v1::UpgradeState {
        operation: "op_upgrade".into(),
        phase,
        version: "2.0.0".into(),
        previous: Some("1.9.0".into()),
        error: None,
        updated: 1_700_000_000_000,
    }
}

fn cases() -> BTreeMap<String, Value> {
    let mut cases = BTreeMap::new();
    let challenge = v1::Challenge {
        nonce: "00".repeat(32),
        hub_id: "hub-1".into(),
        conn_id: "conn-1".into(),
        proto: 10,
        upgrade_protocol: 1,
        hub_time: 1_700_000_000_000,
    };
    let hello = v1::Hello {
        node_id: "node-1".into(),
        incarnation: 3,
        proto: 11,
        upgrade_protocol: 1,
        sig: "33".repeat(64),
    };
    let welcome = v1::Welcome {
        generation: 7,
        proto: 10,
        upgrade_protocol: 1,
        mode: v1::Mode::Upgrade,
        paused: false,
        heartbeat_ms: 10_000,
        hub_key: "22".repeat(32),
        pins: vec![format!("sha256:{}", "aa".repeat(32))],
        sig: "44".repeat(64),
    };
    let report = v1::Report {
        node_id: "node-1".into(),
        error: "link failed".into(),
        sig: "55".repeat(64),
    };
    for (name, payload) in [
        ("challenge", value(&challenge)),
        ("hello", value(&hello)),
        ("welcome.upgrade", value(&welcome)),
        (
            "welcome.normal",
            value(v1::Welcome {
                mode: v1::Mode::Normal,
                ..welcome.clone()
            }),
        ),
        ("report", value(&report)),
        ("noted", value(v1::Noted {})),
        (
            "heartbeat",
            value(v1::Heartbeat {
                at: 1_700_000_000_000,
            }),
        ),
        ("fenced", value(v1::Fenced { generation: 8 })),
        (
            "goodbye",
            value(v1::Goodbye {
                reason: "upgrade restart".into(),
            }),
        ),
        (
            "handshake_error",
            value(v1::Error::new(v1::Code::Denied, "not admitted")),
        ),
    ] {
        let method = match name {
            "challenge" => v1::CHALLENGE,
            "hello" => v1::HELLO,
            "welcome.upgrade" | "welcome.normal" => v1::WELCOME,
            "report" => v1::REPORT,
            "noted" => v1::NOTED,
            "handshake_error" => v1::ERROR,
            "heartbeat" => v1::HEARTBEAT,
            "fenced" => v1::FENCED,
            "goodbye" => v1::GOODBYE,
            _ => unreachable!("every control message has a frozen wire name"),
        };
        cases.insert(
            format!("{name}.json"),
            value(frame(v1::Kind::Ctl, method, payload.clone())),
        );
        if matches!(
            method,
            v1::CHALLENGE | v1::HELLO | v1::WELCOME | v1::REPORT | v1::NOTED | v1::ERROR
        ) {
            cases.insert(
                format!("{name}.minimal.json"),
                value(v1::Frame::ctl(format!("hs_{method}"), method, 0, payload)),
            );
        }
    }
    for (name, bytes) in [
        ("hello", v1::hello_message(&challenge, &hello)),
        ("welcome", v1::welcome_message(&challenge, &hello, &welcome)),
        ("report", v1::report_message(&challenge, &report)),
    ] {
        cases.insert(format!("{name}.signing.json"), value(hex::encode(bytes)));
    }

    let stage: v1::StageResponse = state(v1::Phase::Staged);
    let switch: v1::SwitchResponse = state(v1::Phase::Switching);
    let status: v1::StatusResponse = state(v1::Phase::Succeeded);
    let methods = [
        (
            v1::PROBE_BUILD,
            value(v1::ProbeBuildRequest {}),
            value(v1::ProbeBuildResponse {
                build: Some(build()),
                upgradable: true,
                running: 2,
            }),
            v1::Code::Unsupported,
        ),
        (
            v1::UPGRADE_STAGE,
            value(v1::StageRequest {
                operation: "op_upgrade".into(),
                build: build(),
                artifact: "node-upgrade.op_upgrade".into(),
                size: 4096,
                sha256: "cd".repeat(32),
                health_secs: 180,
            }),
            value(stage),
            v1::Code::Invalid,
        ),
        (
            v1::BLOB_FETCH,
            value(v1::FetchRequest {
                access: v1::FileAccess::Operation {
                    operation: "op_upgrade".into(),
                },
                id: "node-upgrade.op_upgrade".into(),
                offset: 0,
                max: 1024,
            }),
            value(v1::FetchResponse {
                offset: 0,
                bytes: vec![0, 1, 254, 255],
                eof: true,
            }),
            v1::Code::Denied,
        ),
        (
            v1::UPGRADE_SWITCH,
            value(v1::SwitchRequest {
                operation: "op_upgrade".into(),
                abandon: false,
                force: true,
            }),
            value(switch),
            v1::Code::Downgrade,
        ),
        (
            v1::UPGRADE_STATUS,
            value(v1::StatusRequest {
                operation: "op_upgrade".into(),
            }),
            value(status),
            v1::Code::Invalid,
        ),
    ];
    for (method, request, response, code) in methods {
        cases.insert(
            format!("{method}.request.json"),
            value(frame(v1::Kind::Req, method, request)),
        );
        cases.insert(
            format!("{method}.response.json"),
            value(frame(v1::Kind::Res, "", value(v1::Outcome::Ok(response)))),
        );
        let error = v1::Error {
            code,
            retry: v1::Retry::Never,
            operation: Some("op_upgrade".into()),
            detail: "request refused".into(),
        };
        cases.insert(
            format!("{method}.error.json"),
            value(frame(v1::Kind::Res, "", value(v1::Outcome::Err(error)))),
        );
    }
    cases.insert(
        "probe_build.unmanaged.response.json".into(),
        value(frame(
            v1::Kind::Res,
            "",
            value(v1::Outcome::Ok(value(v1::ProbeBuildResponse {
                build: None,
                upgradable: false,
                running: 0,
            }))),
        )),
    );
    let mut failed = state(v1::Phase::Failed);
    failed.previous = None;
    failed.error = Some("archive digest mismatch".into());
    cases.insert(
        "upgrade_status.failed.response.json".into(),
        value(frame(
            v1::Kind::Res,
            "",
            value(v1::Outcome::Ok(value(failed))),
        )),
    );
    let mut header = frame(v1::Kind::Unknown, "future", json!({}));
    header.re = Some("h_0".into());
    cases.insert("frame.unknown.json".into(), value(header));
    for code in [
        v1::Code::Incompatible,
        v1::Code::Downgrade,
        v1::Code::Offline,
        v1::Code::Busy,
        v1::Code::Unsupported,
        v1::Code::Denied,
        v1::Code::Expired,
        v1::Code::Stale,
        v1::Code::Conflict,
        v1::Code::Exhausted,
        v1::Code::Invalid,
        v1::Code::OutcomeUnknown,
        v1::Code::Gap,
        v1::Code::Internal,
    ] {
        cases.insert(
            format!("error.{}.json", code_name(code)),
            value(v1::Error::new(code, "golden error")),
        );
    }
    for phase in [
        v1::Phase::Staging,
        v1::Phase::Staged,
        v1::Phase::Switching,
        v1::Phase::Linked,
        v1::Phase::Succeeded,
        v1::Phase::Failed,
        v1::Phase::RolledBack,
        v1::Phase::Abandoned,
    ] {
        cases.insert(format!("phase.{}.json", phase_name(phase)), value(phase));
    }
    for retry in [v1::Retry::Never, v1::Retry::Later, v1::Retry::Reconcile] {
        let name = match retry {
            v1::Retry::Never => "never",
            v1::Retry::Later => "later",
            v1::Retry::Reconcile => "reconcile",
        };
        cases.insert(format!("retry.{name}.json"), value(retry));
    }
    cases
}

// Exhaustive matches also make additions to the permanent enum surfaces a compile failure.
fn code_name(code: v1::Code) -> &'static str {
    match code {
        v1::Code::Incompatible => "incompatible",
        v1::Code::Downgrade => "downgrade",
        v1::Code::Offline => "offline",
        v1::Code::Busy => "busy",
        v1::Code::Unsupported => "unsupported",
        v1::Code::Denied => "denied",
        v1::Code::Expired => "expired",
        v1::Code::Stale => "stale",
        v1::Code::Conflict => "conflict",
        v1::Code::Exhausted => "exhausted",
        v1::Code::Invalid => "invalid",
        v1::Code::OutcomeUnknown => "outcome_unknown",
        v1::Code::Gap => "gap",
        v1::Code::Internal => "internal",
    }
}
fn phase_name(phase: v1::Phase) -> &'static str {
    match phase {
        v1::Phase::Staging => "staging",
        v1::Phase::Staged => "staged",
        v1::Phase::Switching => "switching",
        v1::Phase::Linked => "linked",
        v1::Phase::Succeeded => "succeeded",
        v1::Phase::Failed => "failed",
        v1::Phase::RolledBack => "rolled_back",
        v1::Phase::Abandoned => "abandoned",
    }
}

fn typed<T: Serialize + DeserializeOwned>(value: Value) -> Value {
    serde_json::to_value(serde_json::from_value::<T>(value).unwrap()).unwrap()
}

/// Decode fixture bytes through the actual schema, including the payload behind Frame::p.
fn roundtrip(name: &str, encoded: Value) -> Value {
    if name.ends_with(".signing.json") {
        return typed::<String>(encoded);
    }
    if name.starts_with("error.") {
        return typed::<v1::Error>(encoded);
    }
    if name.starts_with("phase.") {
        return typed::<v1::Phase>(encoded);
    }
    if name.starts_with("retry.") {
        return typed::<v1::Retry>(encoded);
    }
    let mut frame: v1::Frame = serde_json::from_value(encoded).unwrap();
    frame.p = match frame.t {
        v1::Kind::Ctl => match frame.m.as_str() {
            v1::CHALLENGE => typed::<v1::Challenge>(frame.p),
            v1::HELLO => typed::<v1::Hello>(frame.p),
            v1::WELCOME => typed::<v1::Welcome>(frame.p),
            v1::REPORT => typed::<v1::Report>(frame.p),
            v1::NOTED => typed::<v1::Noted>(frame.p),
            v1::ERROR => typed::<v1::Error>(frame.p),
            v1::HEARTBEAT => typed::<v1::Heartbeat>(frame.p),
            v1::FENCED => typed::<v1::Fenced>(frame.p),
            v1::GOODBYE => typed::<v1::Goodbye>(frame.p),
            _ => panic!("uncovered control fixture: {name}"),
        },
        v1::Kind::Req => match frame.m.as_str() {
            v1::PROBE_BUILD => typed::<v1::ProbeBuildRequest>(frame.p),
            v1::UPGRADE_STAGE => typed::<v1::StageRequest>(frame.p),
            v1::BLOB_FETCH => typed::<v1::FetchRequest>(frame.p),
            v1::UPGRADE_SWITCH => typed::<v1::SwitchRequest>(frame.p),
            v1::UPGRADE_STATUS => typed::<v1::StatusRequest>(frame.p),
            _ => panic!("uncovered request fixture: {name}"),
        },
        v1::Kind::Res => {
            let outcome: v1::Outcome = serde_json::from_value(frame.p).unwrap();
            value(match outcome {
                v1::Outcome::Err(error) => v1::Outcome::Err(error),
                v1::Outcome::Ok(payload) => v1::Outcome::Ok(if name.starts_with("probe_build.") {
                    typed::<v1::ProbeBuildResponse>(payload)
                } else if name.starts_with("blob.fetch.") {
                    typed::<v1::FetchResponse>(payload)
                } else {
                    typed::<v1::UpgradeState>(payload)
                }),
            })
        }
        v1::Kind::Unknown => frame.p,
    };
    value(frame)
}

#[test]
fn every_upgrade_v1_shape_matches_its_frozen_bytes() {
    let directory =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/upgrade_v1");
    let cases = cases();
    let mut files: Vec<_> = std::fs::read_dir(&directory)
        .unwrap()
        .map(|p| p.unwrap().file_name().into_string().unwrap())
        .collect();
    files.sort();
    assert_eq!(
        files,
        cases.keys().cloned().collect::<Vec<_>>(),
        "each fixture is exercised"
    );
    for (name, value) in cases {
        let mut actual = serde_json::to_vec_pretty(&value).unwrap();
        actual.push(b'\n');
        assert_eq!(
            actual,
            std::fs::read(directory.join(&name)).unwrap(),
            "{name}: frozen v1 bytes changed"
        );
        let decoded = roundtrip(&name, serde_json::from_slice(&actual).unwrap());
        let mut reencoded = serde_json::to_vec_pretty(&decoded).unwrap();
        reencoded.push(b'\n');
        assert_eq!(
            actual, reencoded,
            "{name}: typed decode/reencode changed frozen bytes"
        );
    }
}

#[test]
fn the_permanent_module_has_no_dependency_on_mutable_node_contracts() {
    let source = include_str!("../src/upgrade_v1.rs");
    for path in ["crate::", "super::", "steve_types::", "steve_node_api::"] {
        assert!(
            !source.contains(path),
            "v1 must compile independently of {path}"
        );
    }
    assert_eq!(v1::UPGRADE_PROTOCOL, 1);
    assert_eq!(
        v1::METHODS,
        [
            "probe_build",
            "upgrade_stage",
            "blob.fetch",
            "upgrade_switch",
            "upgrade_status"
        ]
    );
    let mode_name = |mode| match mode {
        v1::Mode::Normal => "normal",
        v1::Mode::Upgrade => "upgrade",
    };
    assert_eq!(mode_name(v1::Mode::Normal), "normal");
    let operation = |scope| match scope {
        v1::FileAccess::Operation { operation } => operation,
    };
    assert_eq!(
        operation(v1::FileAccess::Operation {
            operation: "op".into()
        }),
        "op"
    );
    assert_eq!((v1::REPORT_MAX, v1::HANDSHAKE_BUDGET_MS), (1024, 10_000));
    assert_eq!(
        (
            v1::MAX_FRAME,
            v1::CONTROL_CAP,
            v1::BULK_CAP,
            v1::BULK_FRAME,
            v1::CHUNK_BYTES
        ),
        (294912, 65536, 262144, 49152, 23552)
    );
}

#[test]
fn fixed_payloads_reject_unknown_or_mutable_scope_fields() {
    assert!(serde_json::from_value::<v1::ProbeBuildRequest>(json!({"fresh":true})).is_err());
    assert!(
        serde_json::from_value::<v1::FetchRequest>(
            json!({"access":{"for":"execution","attempt":"x"},"id":"x","offset":0,"max":10})
        )
        .is_err()
    );
    assert!(
        serde_json::from_value::<v1::StatusRequest>(json!({"operation":"op_upgrade","extra":1}))
            .is_err()
    );
    assert_eq!(v1::Error::incompatible().retry, v1::Retry::Never);
}
