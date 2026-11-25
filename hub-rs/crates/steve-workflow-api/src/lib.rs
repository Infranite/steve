//! Pure strongly-typed contract for Steve's sandboxed JavaScript workflow engine.
//!
//! Contains:
//! - Hard resource budget constants (`limits`)
//! - Deterministic UTF-8 canonical JSON & safe-integer rules (`canonical`)
//! - Closed JSON Schema 2020-12 subset validator with static ReDoS interception (`schema`)
//! - Host primitive inventory, `CallIdentity`, `StepInput`, `StepFrame`, and `WorkflowFault` (`types`)

pub mod canonical;
pub mod limits;
pub mod schema;
pub mod types;

pub use canonical::{
    CanonicalError, is_safe_i64, sha256_hex, to_canonical_json, validate_f64, validate_json_value,
    validate_number,
};
pub use limits::{
    EffectiveLimits, MAX_CALL_IDENTITY_BYTES, MAX_CALLS_PER_STEP, MAX_JS_HEAP_BYTES,
    MAX_JS_STACK_BYTES, MAX_JSON_DEPTH, MAX_LOG_ENTRIES, MAX_LOG_ENTRY_BYTES, MAX_PHASE_NAME_BYTES,
    MAX_PROMPT_BYTES, MAX_RESULT_BYTES, MAX_SAFE_INTEGER, MAX_SCHEMA_DEPTH, MAX_SCHEMA_NODES,
    MAX_SCHEMA_REGEX_BYTES, MAX_SCHEMA_REGEX_COMPILED_BYTES, MAX_SCHEMA_REGEX_DFA_BYTES,
    MAX_SCHEMA_REGEX_REPETITION, MAX_SCRIPT_SOURCE_BYTES, MAX_STEP_FRAME_BYTES,
    MAX_STEP_INPUT_BYTES, MAX_STEP_WALL_TIME, MAX_STEP_WALL_TIME_MS, MIN_SAFE_INTEGER,
    StepLimitsOverride,
};
pub use schema::{
    AdditionalProperties, CompiledSchema, JsonSchemaSubset, PrimitiveType, ResultValidationError,
    SchemaError, SchemaType, ValidationViolation, ValidationViolationCode, compile_safe_regex,
    validate_structured_result,
};
pub use types::{
    CallIdentity, CallOptions, CallSpec, HostPrimitive, IdentityError, RecordedCall,
    STEP_PROTOCOL_VERSION, StepFrame, StepInput, StepLogEntry, StepWireHeader, StepWireRequest,
    StepWireResponse, WorkflowFault, WorkflowFaultClass, WorkflowFaultCode, WorkflowState,
};

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn canonical_json_sorts_keys_by_utf8_and_normalizes_numbers() {
        let v1 = json!({ "z": 1, "a": { "b": -0.0, "a": 2.0 }, "ä": 3, "B": 4 });
        let v2 = json!({ "B": 4, "ä": 3, "a": { "a": 2, "b": 0 }, "z": 1 });
        let c1 = to_canonical_json(&v1).unwrap();
        let c2 = to_canonical_json(&v2).unwrap();
        assert_eq!(c1, c2);
        assert_eq!(c1, r#"{"B":4,"a":{"a":2,"b":0},"z":1,"ä":3}"#);
    }

    #[test]
    fn canonical_json_rejects_unsafe_integers_and_non_finite_floats() {
        assert_eq!(validate_f64(f64::NAN), Err(CanonicalError::NonFiniteNumber));
        assert_eq!(
            validate_f64(f64::INFINITY),
            Err(CanonicalError::NonFiniteNumber)
        );
        assert_eq!(
            validate_f64(f64::NEG_INFINITY),
            Err(CanonicalError::NonFiniteNumber)
        );
        assert!(validate_f64(MAX_SAFE_INTEGER as f64).is_ok());
        assert!(validate_f64(MIN_SAFE_INTEGER as f64).is_ok());
        assert!(matches!(
            validate_f64((MAX_SAFE_INTEGER + 1) as f64),
            Err(CanonicalError::UnsafeInteger { .. })
        ));
        assert!(matches!(
            validate_f64((MIN_SAFE_INTEGER - 1) as f64),
            Err(CanonicalError::UnsafeInteger { .. })
        ));
        assert!(matches!(
            to_canonical_json(&json!({ "big": MAX_SAFE_INTEGER + 1 })),
            Err(CanonicalError::UnsafeInteger { .. })
        ));
        assert!(matches!(
            to_canonical_json(&json!({ "big": 1e20 })),
            Err(CanonicalError::UnsafeInteger { .. })
        ));
        // Finite fractional floats stay valid.
        assert_eq!(
            to_canonical_json(&json!({ "pi": 3.25 })).unwrap(),
            r#"{"pi":3.25}"#
        );
    }

    #[test]
    fn call_identity_is_deterministic_and_enforces_8kib_cap() {
        let opts = CallOptions {
            bot: Some("coder".into()),
            tags: Some(vec!["rust".into()]),
            review: Some(false),
            ..CallOptions::default()
        };
        let id1 = CallIdentity::compute(Some("build"), "implement feature", &opts, 0).unwrap();
        let id2 = CallIdentity::compute(Some("build"), "implement feature", &opts, 0).unwrap();
        assert_eq!(id1, id2);
        assert_eq!(id1.identity_hash.len(), 64);
        assert!(id1.verify("implement feature", &opts).is_ok());

        let id_occ1 = CallIdentity::compute(Some("build"), "implement feature", &opts, 1).unwrap();
        assert_ne!(id1.identity_hash, id_occ1.identity_hash);

        let huge_opts = CallOptions {
            title: Some("x".repeat(MAX_CALL_IDENTITY_BYTES + 1)),
            ..CallOptions::default()
        };
        assert!(matches!(
            CallIdentity::compute(None, "p", &huge_opts, 0),
            Err(IdentityError::KeyTooLarge { .. })
        ));
    }

    #[test]
    fn schema_subset_validates_supported_keywords_and_rejects_ref() {
        // $ref must be rejected at parse time.
        let with_ref = json!({ "$ref": "https://example.com/schema.json" });
        assert!(matches!(
            JsonSchemaSubset::from_json_value(with_ref),
            Err(SchemaError::InvalidDefinition { .. })
        ));

        let schema = JsonSchemaSubset::from_json_value(json!({
            "type": "object",
            "required": ["status", "score", "tags"],
            "additionalProperties": false,
            "properties": {
                "status": { "enum": ["ok", "retry"] },
                "score": { "type": "integer", "minimum": 0, "maximum": 100 },
                "version": { "type": "string", "pattern": "^v[0-9]+\\.[0-9]+\\.[0-9]+$", "minLength": 5, "maxLength": 16 },
                "tags": { "type": "array", "minItems": 1, "maxItems": 3, "items": { "type": "string" } },
                "choice": { "oneOf": [{ "const": "a" }, { "type": "integer" }] },
                "either": { "anyOf": [{ "type": "null" }, { "type": "boolean" }] }
            }
        }))
        .unwrap()
        .compile()
        .unwrap();

        assert!(
            schema
                .validate(&json!({
                    "status": "ok",
                    "score": 95,
                    "version": "v1.2.3",
                    "tags": ["fast"],
                    "choice": 7,
                    "either": null
                }))
                .is_ok()
        );

        let err = schema
            .validate(&json!({
                "status": "bad",
                "score": 101,
                "version": "invalid",
                "tags": [],
                "extra": true
            }))
            .unwrap_err();
        let ResultValidationError::Violations(vs) = err else {
            panic!("expected violations, got {err:?}");
        };
        let codes: Vec<_> = vs.iter().map(|v| v.code).collect();
        assert!(codes.contains(&ValidationViolationCode::EnumMismatch));
        assert!(codes.contains(&ValidationViolationCode::Maximum));
        assert!(codes.contains(&ValidationViolationCode::PatternMismatch));
        assert!(codes.contains(&ValidationViolationCode::MinItems));
        assert!(codes.contains(&ValidationViolationCode::AdditionalPropertyForbidden));
    }

    #[test]
    fn schema_intercepts_redos_and_oversized_regexes() {
        for bad in [
            "(a+)+$",
            "(a*)*b",
            "([a-z]+)+",
            "(a|aa)+",
            "(a|a)+",
            ".*.*a",
            "[a-z]+[a-z]+$",
            "(a?)+",
            "a{101}",
            "a{1,200}",
        ] {
            let err = compile_safe_regex(bad, "$.pattern").unwrap_err();
            assert!(
                matches!(err, SchemaError::RegexUnsafe { .. }),
                "pattern `{bad}` should be rejected as unsafe, got {err:?}"
            );
        }

        // Backreferences & lookarounds rejected by linear-time regex parser.
        for unsupported in [r"(a)\1", r"(?=a)a", r"(?!a)b"] {
            let err = compile_safe_regex(unsupported, "$.pattern").unwrap_err();
            assert!(
                matches!(err, SchemaError::RegexInvalid { .. }),
                "pattern `{unsupported}` should fail parsing, got {err:?}"
            );
        }

        // Length cap.
        let too_long = "a".repeat(MAX_SCHEMA_REGEX_BYTES + 1);
        assert!(matches!(
            compile_safe_regex(&too_long, "$.pattern"),
            Err(SchemaError::RegexTooLong { .. })
        ));

        // Safe patterns remain allowed.
        for safe in [
            "^[a-z0-9-]+$",
            "^(foo|bar)+$",
            "^([a-z]|[0-9])+$",
            r"^v\d{1,3}\.\d{1,3}\.\d{1,3}$",
        ] {
            assert!(
                compile_safe_regex(safe, "$.pattern").is_ok(),
                "safe pattern `{safe}` should compile"
            );
        }
    }

    #[test]
    fn review_result_enforces_approved_boolean_and_32kib_cap() {
        let err = validate_structured_result(&json!({"notes": "ok"}), None, true, MAX_RESULT_BYTES)
            .unwrap_err();
        assert!(matches!(err, ResultValidationError::Violations(vs)
            if vs[0].code == ValidationViolationCode::ReviewApprovedRequired));

        assert!(
            validate_structured_result(
                &json!({"approved": true, "summary": "lgtm"}),
                None,
                true,
                MAX_RESULT_BYTES
            )
            .is_ok()
        );

        let huge = json!({"approved": true, "blob": "x".repeat(MAX_RESULT_BYTES + 10)});
        assert!(matches!(
            validate_structured_result(&huge, None, true, MAX_RESULT_BYTES),
            Err(ResultValidationError::ResultTooLarge { .. })
        ));
    }

    #[test]
    fn limits_override_never_exceeds_hard_ceilings() {
        let eff = EffectiveLimits::resolve(Some(&StepLimitsOverride {
            wall_time_ms: Some(999_999),
            js_heap_bytes: Some(usize::MAX),
            js_stack_bytes: Some(usize::MAX),
            max_calls: Some(10_000),
            max_call_identity_bytes: Some(100_000),
            max_result_bytes: Some(100_000),
            max_frame_bytes: Some(usize::MAX),
        }));
        assert_eq!(eff, EffectiveLimits::default());
    }
}
