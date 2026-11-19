use steve_types::{error::check_registry, *};

reasons! {
    /// Reasons used to exercise the public declaration macro.
    #[allow(clippy::enum_variant_names)]
    pub enum TestReason = "test" {
        /// The caller's branch position is stale; `now` is only for display.
        Moved = moved [Stale] (branch, now),
        Wait = wait [Busy] (delay_ms),
        Lost = lost [OutcomeUnknown] (),
    }
    retired { "old" }
}

#[test]
fn registry_is_valid() {
    assert_eq!(check_registry(ALL_REASONS), Ok(()));
    assert_eq!(ALL_REASONS.len(), 11);
    assert_eq!(check_registry(&[&[TestReason::SET]]), Ok(()));
}

#[test]
fn constructors_and_wire_values_round_trip() {
    let error = TestReason::moved("main", String::from("a1b2c3d"), "the branch moved");
    assert_eq!(error.code, Code::Stale);
    assert_eq!(error.retry, Code::Stale.default_retry());
    assert_eq!(error.params.0["branch"], Param::Str("main".into()));
    assert_eq!(error.params.0["now"], Param::Str("a1b2c3d".into()));
    let bytes = serde_json::to_vec(&error).unwrap();
    let decoded: Error = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(decoded, error);
    let reason = decoded.reason.as_ref().unwrap();
    assert_eq!(reason.as_str(), "test.moved");
    assert_eq!(reason.domain(), Some("test"));
    assert!(reason.is(TestReason::Moved));
    assert!(!reason.is(TestReason::Lost));
    assert_eq!(reason.known::<TestReason>(), Some(TestReason::Moved));
    assert_eq!(TestReason::Wait.def().params, &["delay_ms"]);
    let waiting = TestReason::wait(1500_i64, "busy");
    assert_eq!(waiting.params.0["delay_ms"], Param::Int(1500));
    assert_eq!(waiting.retry, Code::Busy.default_retry());
    assert_eq!(TestReason::lost("lost reply").retry, Retry::Never);
    let retry = Retry::Later {
        after_ms: Some(250),
    };
    assert_eq!(error.clone().retry(retry), Error { retry, ..error });
}

#[test]
fn borrowed_strings_and_lossless_integers_are_constructor_arguments() {
    let branch = String::from("main");
    let error = TestReason::moved(&branch, 42, "the branch moved");
    assert_eq!(error.params.0["branch"], Param::Str(branch.clone()));
    assert_eq!(error.params.0["now"], Param::Int(42));
    assert_eq!(
        TestReason::wait(u32::MAX, "busy").params.0["delay_ms"],
        Param::Int(i64::from(u32::MAX))
    );
    macro_rules! check_int {
        ($($value:expr),* $(,)?) => { $(
            assert_eq!(Param::from($value), Param::Int(i64::from($value)));
        )* };
    }
    check_int!(
        i8::MIN,
        i16::MIN,
        i32::MIN,
        i64::MIN,
        u8::MAX,
        u16::MAX,
        u32::MAX
    );
}

#[test]
fn variant_definitions_come_from_the_registry_in_declaration_order() {
    for (reason, def) in [TestReason::Moved, TestReason::Wait, TestReason::Lost]
        .into_iter()
        .zip(TestReason::SET.defs)
    {
        assert_eq!(reason.def().reason, def.reason);
        assert_eq!(reason.def().code, def.code);
        assert_eq!(reason.def().params, def.params);
    }
}

#[test]
fn detail_is_reserved_for_the_constructor_safe_detail() {
    let error = check_registry(&[&[set(
        "test",
        &[ReasonDef {
            reason: "test.invalid",
            code: Code::Invalid,
            params: &["detail"],
        }],
        &[],
    )]])
    .unwrap_err();
    assert!(error.contains("reserved parameter"));
    assert!(error.contains("detail"));
}

#[test]
fn readers_preserve_unknown_retired_and_malformed_reasons() {
    let overlong = format!("test.{}", "a".repeat(60));
    for (text, domain) in [
        ("future.new", Some("future")),
        ("test.old", Some("test")),
        ("Test.moved", None),
        ("test.has space", None),
        ("no_dot", None),
        ("two.dots.here", None),
        ("", None),
        (".empty", None),
        ("empty.", None),
        (overlong.as_str(), None),
    ] {
        let mut error = TestReason::lost("fallback");
        error.reason = Some(serde_json::from_str(&serde_json::to_string(text).unwrap()).unwrap());
        let bytes = serde_json::to_vec(&error).unwrap();
        let decoded: Error = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(serde_json::to_vec(&decoded).unwrap(), bytes);
        let reason = decoded.reason.unwrap();
        assert_eq!(reason.as_str(), text);
        assert_eq!(reason.domain(), domain);
        assert_eq!(reason.known::<TestReason>(), None);
        assert!(!reason.is(TestReason::Moved));
    }
}

fn set(
    domain: &'static str,
    defs: &'static [ReasonDef],
    retired: &'static [&'static str],
) -> ReasonSet {
    ReasonSet {
        domain,
        defs,
        retired,
    }
}

const MOVED: ReasonDef = ReasonDef {
    reason: "test.moved",
    code: Code::Stale,
    params: &["branch", "now"],
};

#[test]
fn registry_rejects_duplicate_prefixes_and_names_including_tombstones() {
    let cases = [
        vec![set("test", &[], &[]), set("test", &[], &[])],
        vec![set("test", &[MOVED, MOVED], &[])],
        vec![set("test", &[MOVED], &["test.moved"])],
        vec![set("test", &[], &["test.old", "test.old"])],
    ];
    for sets in cases {
        assert!(check_registry(&[&sets]).unwrap_err().contains("duplicate"));
    }
}

#[test]
fn registry_checks_formats_prefixes_lengths_and_parameter_names() {
    macro_rules! invalid_reason {
        ($($reason:literal),* $(,)?) => { $(
            assert!(check_registry(&[&[set("test", &[ReasonDef {
                reason: $reason, code: Code::Stale, params: &[],
            }], &[])]]).is_err());
            assert!(check_registry(&[&[set("test", &[], &[$reason])]]).is_err());
        )* };
    }
    invalid_reason!(
        "Test.moved",
        "test.has space",
        "no_dot",
        "two.dots.here",
        "",
        ".empty",
        "test.",
        "other.moved",
        "test.aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    );
    for domain in ["", "Test", "two.dots", "has space"] {
        assert!(check_registry(&[&[set(domain, &[], &[])]]).is_err());
    }
    macro_rules! invalid_params {
        ($($params:expr),* $(,)?) => { $(
            assert!(check_registry(&[&[set("test", &[ReasonDef {
                reason: "test.moved", code: Code::Stale, params: $params,
            }], &[])]]).is_err());
        )* };
    }
    invalid_params!(
        &["branch", "branch"],
        &["9first"],
        &["Upper"],
        &["has space"],
        &[""],
        &["_first"]
    );
    assert!(
        check_registry(&[&[set(
            "test",
            &[ReasonDef {
                reason: "test.aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                code: Code::Invalid,
                params: &["branch_1", "count", "ready_at", "wait_ms"],
            }],
            &[]
        )]])
        .is_ok()
    );
}
