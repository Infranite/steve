use steve_node_api::error as rpc_error;
use steve_node_api::{upgrade_bridge::refuse_downgrade, upgrade_v1 as v1};

#[test]
fn release_order_is_numeric_and_an_unrecognized_version_cannot_switch() {
    for (current, target) in [
        ("1.9.0", "1.10.0"),
        ("1.10.0", "1.10.0"),
        ("0.9.99", "1.0.0"),
    ] {
        refuse_downgrade(current, target).unwrap();
    }
    for (current, target) in [("1.10.0", "1.9.0"), ("2.0.0", "1.99.99")] {
        let error = refuse_downgrade(current, target).unwrap_err();
        assert_eq!(
            (error.code, error.code.retryable()),
            (v1::Code::Downgrade, false)
        );
    }
    for version in [
        "",
        "dev",
        "1.0",
        "1.0.0-rc.1",
        "1.0.-1",
        "18446744073709551616.0.0",
    ] {
        for (current, target) in [(version, "1.0.0"), ("1.0.0", version)] {
            assert_eq!(
                refuse_downgrade(current, target).unwrap_err().code,
                v1::Code::Invalid
            );
        }
    }
}

#[test]
fn wire_incompatible_defers_local_work_and_downgrade_keeps_its_code() {
    use steve_node_api::{Code, Error, Retry};
    let wire = v1::Error::incompatible();
    assert!(!wire.code.retryable());
    let local = Error::from(wire);
    assert_eq!(
        (local.code, local.retry),
        (Code::Unavailable, Retry::Later { after_ms: None })
    );
    let wire = v1::Error::new(v1::Code::Downgrade, "older journal reader");
    let ordinary = Error::from(wire.clone());
    assert!(
        ordinary
            .reason
            .as_ref()
            .unwrap()
            .is(steve_node_api::NodeReason::DowngradeRefused)
    );
    assert_eq!(v1::Error::from(ordinary), wire);
    let plain_invalid = rpc_error(Code::Invalid, "older journal reader");
    assert_eq!(v1::Error::from(plain_invalid).code, v1::Code::Invalid);
}

#[test]
fn uncertain_ordinary_outcomes_cannot_become_retryable_v1_answers() {
    use steve_node_api::{Code, Error, OperationId, Retry};
    for ordinary in [
        steve_node_api::uncertain(&OperationId::new("op_switch").unwrap(), "response lost"),
        rpc_error(Code::Gap, "missing execution events"),
        rpc_error(Code::Internal, "execution outcome unavailable"),
    ] {
        let wire = v1::Error::from(ordinary);
        assert_eq!(wire.code, v1::Code::Internal);
        assert!(!wire.code.retryable());
        assert_eq!(Error::from(wire).retry, Retry::Never);
    }
}

#[test]
fn switch_actions_are_exclusive_and_legacy_flags_are_not_wire_fields() {
    use steve_node_api::{OperationId, UpgradeSwitch};
    for (action, abandon, force) in [
        (v1::SwitchAction::Switch, false, false),
        (v1::SwitchAction::Force, false, true),
        (v1::SwitchAction::Abandon, true, false),
    ] {
        let wire = v1::SwitchRequest {
            operation: "op_upgrade".into(),
            action,
        };
        let ordinary = UpgradeSwitch::try_from(wire.clone()).unwrap();
        assert_eq!((ordinary.abandon, ordinary.force), (abandon, force));
        assert_eq!(ordinary.operation, OperationId::new("op_upgrade").unwrap());
        assert_eq!(v1::SwitchRequest::from(ordinary), wire);
    }
    assert!(
        serde_json::from_value::<v1::SwitchRequest>(serde_json::json!({
            "operation":"op_upgrade", "abandon":true, "force":true
        }))
        .is_err()
    );
}
