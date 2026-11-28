//! Strongly-typed workflow script host primitives, `CallIdentity`, step inputs, frames and faults.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;

use crate::canonical::{CanonicalError, is_safe_i64, sha256_hex, to_canonical_json};
use crate::limits::{MAX_CALL_IDENTITY_BYTES, StepLimitsOverride};
use crate::schema::{
    JsonSchemaSubset, ResultValidationError, SchemaError, validate_structured_result,
};

/// Current stdin/stdout wire protocol version between Hub parent and `steve-hub --workflow-step`.
///
/// Checked via [`StepWireHeader`] (which ignores unknown fields) before full deserialization so
/// rolling Hub binary upgrades fail cleanly with `WorkflowFaultCode::ProtocolError` instead of a
/// cryptic `deny_unknown_fields` error.
pub const STEP_PROTOCOL_VERSION: u32 = 1;

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
    pub node_tag: Option<String>,
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
    /// Optional explicit call-site phase override (`agent(prompt, { phase: "build" })`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
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
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub options_canonical_json: String,
    pub occurrence: u32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
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

    /// Returns a compact replay reference omitting the redundant canonical JSON strings for a call
    /// already known to the Hub (`identity_hash` + `prompt_sha256` remain authoritative).
    pub fn compact_known(&self) -> Self {
        Self {
            phase: self.phase.clone(),
            prompt_sha256: self.prompt_sha256.clone(),
            options_canonical_json: String::new(),
            occurrence: self.occurrence,
            canonical_json: String::new(),
            identity_hash: self.identity_hash.clone(),
        }
    }
}

/// One `agent()` call emitted during step replay.
///
/// For calls already known to the Hub (`StepInput::known_call_hashes` or `recorded_calls`),
/// `prompt` and `options` are omitted (`None`) so replaying up to 256 calls with large prompts
/// never overflows [`crate::MAX_STEP_FRAME_BYTES`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CallSpec {
    /// 0-based emission sequence number in this replay step (`wf_version_calls.seq_no`).
    pub seq_no: u32,
    pub identity: CallIdentity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<CallOptions>,
}

impl CallSpec {
    /// True when this emitted call is a compact reference to an already-known Hub call.
    pub fn is_compact_known(&self) -> bool {
        self.prompt.is_none() && self.options.is_none()
    }
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

/// Lightweight header parsed first (ignoring unknown fields) to verify [`STEP_PROTOCOL_VERSION`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepWireHeader {
    pub protocol_version: u32,
}

/// Versioned stdin envelope sent from Hub parent to `steve-hub --workflow-step`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepWireRequest {
    pub protocol_version: u32,
    pub input: StepInput,
}

/// Versioned stdout envelope returned from `steve-hub --workflow-step` to Hub parent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepWireResponse {
    pub protocol_version: u32,
    pub frame: StepFrame,
}

/// Single JSON payload sent from Hub parent to `steve-hub --workflow-step` child via stdin.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepInput {
    pub script_source: String,
    pub state: WorkflowState,
    /// Previously settled calls keyed by `CallIdentity.identity_hash`.
    #[serde(default)]
    pub recorded_calls: BTreeMap<String, RecordedCall>,
    /// Identity hashes already persisted in the Hub's `wf_calls` ledger (including active/queued
    /// calls) whose full prompts and options may be omitted on replay.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub known_call_hashes: BTreeSet<String>,
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

/// Separates deterministic script bugs from transient subprocess/protocol infrastructure failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowFaultClass {
    Script,
    Infrastructure,
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
    SandboxDenied,
    ProtocolError,
}

impl WorkflowFaultCode {
    /// Classifies whether this fault is an infrastructure failure or a deterministic
    /// script/contract fault.
    pub const fn class(self) -> WorkflowFaultClass {
        match self {
            Self::Timeout | Self::SubprocessFailed | Self::SandboxDenied | Self::ProtocolError => {
                WorkflowFaultClass::Infrastructure
            }
            Self::SyntaxError
            | Self::MissingEntrypoint
            | Self::RuntimeError
            | Self::UnresolvedPromise
            | Self::OutOfMemory
            | Self::StackOverflow
            | Self::TooManyCalls
            | Self::CallIdentityTooLarge
            | Self::ResultTooLarge
            | Self::PromptTooLarge
            | Self::ScriptTooLarge
            | Self::FrameTooLarge
            | Self::InputTooLarge
            | Self::InvalidArguments
            | Self::InvalidOptions
            | Self::InvalidSchema
            | Self::UnsafeRegex
            | Self::NonFiniteNumber
            | Self::UnsafeInteger
            | Self::UnsupportedValue => WorkflowFaultClass::Script,
        }
    }

    /// Whether retrying the exact same step without modifying the script or Hub could succeed.
    pub const fn is_retryable(self) -> bool {
        matches!(self, Self::Timeout | Self::SubprocessFailed)
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SyntaxError => "syntax_error",
            Self::MissingEntrypoint => "missing_entrypoint",
            Self::RuntimeError => "runtime_error",
            Self::UnresolvedPromise => "unresolved_promise",
            Self::Timeout => "timeout",
            Self::OutOfMemory => "out_of_memory",
            Self::StackOverflow => "stack_overflow",
            Self::TooManyCalls => "too_many_calls",
            Self::CallIdentityTooLarge => "call_identity_too_large",
            Self::ResultTooLarge => "result_too_large",
            Self::PromptTooLarge => "prompt_too_large",
            Self::ScriptTooLarge => "script_too_large",
            Self::FrameTooLarge => "frame_too_large",
            Self::InputTooLarge => "input_too_large",
            Self::InvalidArguments => "invalid_arguments",
            Self::InvalidOptions => "invalid_options",
            Self::InvalidSchema => "invalid_schema",
            Self::UnsafeRegex => "unsafe_regex",
            Self::NonFiniteNumber => "non_finite_number",
            Self::UnsafeInteger => "unsafe_integer",
            Self::UnsupportedValue => "unsupported_value",
            Self::SubprocessFailed => "subprocess_failed",
            Self::SandboxDenied => "sandbox_denied",
            Self::ProtocolError => "protocol_error",
        }
    }
}

/// Structured fault returned when a workflow step cannot proceed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowFault {
    pub code: WorkflowFaultCode,
    pub detail: String,
    /// Whether retrying the exact same step without changing the script or Hub could succeed.
    /// Defaults to [`WorkflowFaultCode::is_retryable`] when omitted on the wire.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retryable: Option<bool>,
}

impl WorkflowFault {
    pub fn new(code: WorkflowFaultCode, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
            retryable: None,
        }
    }

    pub fn non_retryable(code: WorkflowFaultCode, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
            retryable: Some(false),
        }
    }

    pub const fn is_retryable(&self) -> bool {
        match self.retryable {
            Some(v) => v,
            None => self.code.is_retryable(),
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
