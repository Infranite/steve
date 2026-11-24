//! Strongly-typed workflow script host primitives, `CallIdentity`, step inputs, frames and faults.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;

use crate::canonical::{CanonicalError, is_safe_i64, sha256_hex, to_canonical_json};
use crate::limits::{MAX_CALL_IDENTITY_BYTES, StepLimitsOverride};
use crate::schema::{
    JsonSchemaSubset, ResultValidationError, SchemaError, validate_structured_result,
};

/// The exact six host primitives exposed to sandboxed workflow JS scripts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HostPrimitive {
    Agent,
    All,
    Race,
    Phase,
    State,
    Log,
}

impl HostPrimitive {
    /// Complete ordered inventory of the six host primitives.
    pub const ALL: [Self; 6] = [
        Self::Agent,
        Self::All,
        Self::Race,
        Self::Phase,
        Self::State,
        Self::Log,
    ];

    /// Global JavaScript identifier exposed in the sandbox.
    pub const fn js_name(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::All => "all",
            Self::Race => "race",
            Self::Phase => "phase",
            Self::State => "state",
            Self::Log => "log",
        }
    }
}

/// Deeply-frozen read-only `state` object exposed to the workflow JS script.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowState {
    pub parent_item_id: String,
    pub cycle: i64,
    pub goal_version: i64,
    pub run_revision: i64,
    pub script_version: i64,
}

impl WorkflowState {
    /// Ensures every integer field fits within ECMAScript's safe integer range.
    pub fn validate(&self) -> Result<(), CanonicalError> {
        for value in [
            self.cycle,
            self.goal_version,
            self.run_revision,
            self.script_version,
        ] {
            if !is_safe_i64(value) {
                return Err(CanonicalError::UnsafeInteger {
                    value: value.to_string(),
                });
            }
        }
        Ok(())
    }
}

/// Strongly-typed options accepted by `agent(prompt, options?)`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CallOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bot: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub specialties: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<JsonSchemaSubset>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mutex_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

impl CallOptions {
    pub fn is_review(&self) -> bool {
        self.review.unwrap_or(false)
    }

    /// Validates any declared JSON Schema (including closed-subset & ReDoS checks).
    pub fn validate(&self) -> Result<(), SchemaError> {
        if let Some(schema) = &self.schema {
            schema.compile()?;
        }
        Ok(())
    }

    /// Validates a delivered child `result` against this call's declared schema, size ceiling, and
    /// mandatory `approved: boolean` rule when `review: true`.
    pub fn validate_result(
        &self,
        result: &Value,
        max_result_bytes: usize,
    ) -> Result<(), ResultValidationError> {
        validate_structured_result(
            result,
            self.schema.as_ref(),
            self.is_review(),
            max_result_bytes,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum IdentityError {
    #[error("canonical JSON error: {0}")]
    Canonical(#[from] CanonicalError),
    #[error("invalid call options schema: {0}")]
    Schema(#[from] SchemaError),
    #[error("CallIdentity canonical JSON is {actual} bytes, exceeding limit of {limit} bytes")]
    KeyTooLarge { actual: usize, limit: usize },
    #[error("CallIdentity mismatch: stored canonical JSON or hash does not match fields")]
    IntegrityMismatch,
}

/// Authoritative deterministic identity of one logical `agent()` call.
///
/// Computed as `CanonicalJson({ occurrence, options, phase, prompt_sha256 })` with UTF-8 sorted
/// keys, ECMAScript safe integers, finite floats, and capped at [`MAX_CALL_IDENTITY_BYTES`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CallIdentity {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    pub prompt_sha256: String,
    pub options_canonical_json: String,
    pub occurrence: u32,
    pub canonical_json: String,
    pub identity_hash: String,
}

impl CallIdentity {
    /// Computes the canonical `CallIdentity` and SHA-256 hash using [`MAX_CALL_IDENTITY_BYTES`].
    pub fn compute(
        phase: Option<&str>,
        prompt: &str,
        options: &CallOptions,
        occurrence: u32,
    ) -> Result<Self, IdentityError> {
        Self::compute_with_limit(phase, prompt, options, occurrence, MAX_CALL_IDENTITY_BYTES)
    }

    /// Computes the canonical `CallIdentity` with an explicit byte ceiling (clamped to
    /// [`MAX_CALL_IDENTITY_BYTES`]).
    pub fn compute_with_limit(
        phase: Option<&str>,
        prompt: &str,
        options: &CallOptions,
        occurrence: u32,
        max_bytes: usize,
    ) -> Result<Self, IdentityError> {
        let limit = max_bytes.min(MAX_CALL_IDENTITY_BYTES);
        options.validate()?;
        let prompt_sha256 = sha256_hex(prompt.as_bytes());
        let options_value = serde_json::to_value(options).expect("CallOptions to Value");
        let options_canonical_json = to_canonical_json(&options_value)?;

        let mut root = Map::new();
        root.insert("occurrence".into(), Value::from(occurrence));
        root.insert("options".into(), options_value);
        root.insert(
            "phase".into(),
            match phase {
                Some(p) => Value::String(p.to_owned()),
                None => Value::Null,
            },
        );
        root.insert("prompt_sha256".into(), Value::String(prompt_sha256.clone()));

        let canonical_json = to_canonical_json(&Value::Object(root))?;
        if canonical_json.len() > limit {
            return Err(IdentityError::KeyTooLarge {
                actual: canonical_json.len(),
                limit,
            });
        }
        let identity_hash = sha256_hex(canonical_json.as_bytes());

        Ok(Self {
            phase: phase.map(str::to_owned),
            prompt_sha256,
            options_canonical_json,
            occurrence,
            canonical_json,
            identity_hash,
        })
    }

    /// Recomputes and verifies that `canonical_json` and `identity_hash` match `prompt` and
    /// `options` and respect the size ceiling.
    pub fn verify(&self, prompt: &str, options: &CallOptions) -> Result<(), IdentityError> {
        let expected = Self::compute(self.phase.as_deref(), prompt, options, self.occurrence)?;
        if expected == *self {
            Ok(())
        } else {
            Err(IdentityError::IntegrityMismatch)
        }
    }
}

/// One `agent()` call emitted during step replay.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CallSpec {
    /// 0-based emission sequence number in this replay step (`wf_version_calls.seq_no`).
    pub seq_no: u32,
    pub identity: CallIdentity,
    pub prompt: String,
    pub options: CallOptions,
}

/// Previously settled call outcome supplied by the Hub to the replay step.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum RecordedCall {
    Succeeded {
        result: Value,
        /// Monotonic `wf_runs.run_revision` when this call settled; drives deterministic `race()`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        settled_at_revision: Option<i64>,
    },
    Failed {
        error_code: String,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        settled_at_revision: Option<i64>,
    },
    Cancelled {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        settled_at_revision: Option<i64>,
    },
}

/// Single JSON frame sent from Hub parent to `steve-hub --workflow-step` child via stdin.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepInput {
    pub script_source: String,
    pub state: WorkflowState,
    /// Previously settled calls keyed by `CallIdentity.identity_hash`.
    #[serde(default)]
    pub recorded_calls: BTreeMap<String, RecordedCall>,
    /// Optional stricter execution limits (clamped to hard ceilings).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<StepLimitsOverride>,
}

/// One bounded diagnostic log entry emitted by `log(message)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepLogEntry {
    pub seq: u32,
    pub message: String,
}

/// Machine-readable workflow step fault code (no localized prose in core contracts).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowFaultCode {
    SyntaxError,
    MissingEntrypoint,
    RuntimeError,
    UnresolvedPromise,
    Timeout,
    OutOfMemory,
    StackOverflow,
    TooManyCalls,
    CallIdentityTooLarge,
    ResultTooLarge,
    PromptTooLarge,
    ScriptTooLarge,
    FrameTooLarge,
    InputTooLarge,
    InvalidArguments,
    InvalidOptions,
    InvalidSchema,
    UnsafeRegex,
    NonFiniteNumber,
    UnsafeInteger,
    UnsupportedValue,
    SubprocessFailed,
    ProtocolError,
}

/// Structured fault returned when a workflow step cannot proceed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowFault {
    pub code: WorkflowFaultCode,
    pub detail: String,
}

impl WorkflowFault {
    pub fn new(code: WorkflowFaultCode, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
        }
    }
}

/// Single JSON frame returned from `steve-hub --workflow-step` child to Hub parent via stdout.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum StepFrame {
    /// Script suspended waiting for one or more unsettled `agent()` calls.
    PendingCalls {
        /// Ordered list of ALL `agent()` calls encountered during replay (`seq_no = 0..n`).
        emitted_calls: Vec<CallSpec>,
        /// Identity hashes of calls that are unsettled and not cancelled by a winning `race()`.
        pending_identities: Vec<String>,
        /// Identity hashes of unsettled calls in losing branches of a settled `race()`.
        #[serde(default)]
        cancel_identities: Vec<String>,
        #[serde(default)]
        logs: Vec<StepLogEntry>,
    },
    /// Script completed cleanly with a validated canonical JSON return value.
    Completed {
        result: Value,
        #[serde(default)]
        emitted_calls: Vec<CallSpec>,
        #[serde(default)]
        cancel_identities: Vec<String>,
        #[serde(default)]
        logs: Vec<StepLogEntry>,
    },
    /// Script faulted or violated a sandbox budget/invariant.
    Failed {
        fault: WorkflowFault,
        #[serde(default)]
        emitted_calls: Vec<CallSpec>,
        #[serde(default)]
        logs: Vec<StepLogEntry>,
    },
}
