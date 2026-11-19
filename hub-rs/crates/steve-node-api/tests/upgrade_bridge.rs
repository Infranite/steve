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
            (error.code, error.retry),
            (v1::Code::Downgrade, v1::Retry::Never)
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
    use steve_node_api::{ApiError, ErrorCode, RetryClass};
    let wire = v1::Error::incompatible();
    assert_eq!(wire.retry, v1::Retry::Never);
    let local = ApiError::from(wire);
    assert_eq!(
        (local.code, local.retry),
        (ErrorCode::Offline, RetryClass::SameOperation)
    );
    let wire = v1::Error::new(v1::Code::Downgrade, "older journal reader");
    assert_eq!(v1::Error::from(ApiError::from(wire.clone())), wire);
}
