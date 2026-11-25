//! Named hard resource budgets for a single sandboxed workflow step.
//!
//! Every constant is a hard upper bound enforced by both `steve-workflow-js` (in-process QuickJS
//! limits) and the Hub subprocess executor (`steve-hub --workflow-step`). Callers may tighten a
//! limit for tests via [`StepLimitsOverride`], but [`EffectiveLimits::resolve`] clamps every field
//! down to these ceilings so no caller can weaken isolation.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Wall-clock budget for one single-step JS replay (`5 s`).
///
/// Workflow scripts are deterministic orchestration glue (branching, fanning out, checking small
/// structured results), not heavy compute. Five seconds leaves ample headroom for a 256-call
/// replay even under host load while capping worst-case Hub worker latency before `SIGKILL`
/// reclaims an infinite loop or pathological regex.
pub const MAX_STEP_WALL_TIME: Duration = Duration::from_secs(5);

/// Same ceiling as [`MAX_STEP_WALL_TIME`] in milliseconds for wire serialization.
pub const MAX_STEP_WALL_TIME_MS: u64 = 5_000;

/// QuickJS heap limit (`32 MiB`).
///
/// Large enough to hold a full 256-call replay (up to 8 MiB of structured JSON results plus JS
/// AST, closures, and runtime structures), small enough that memory bombs hit QuickJS OOM quickly
/// inside the isolated child process without threatening Hub host memory.
pub const MAX_JS_HEAP_BYTES: usize = 32 * 1024 * 1024;

/// QuickJS interpreter stack limit (`512 KiB`).
///
/// Fits safely inside a standard 2 MiB OS thread stack while allowing legitimate helper recursion
/// and preventing malicious deep recursion from crashing the subprocess with a native stack guard
/// fault.
pub const MAX_JS_STACK_BYTES: usize = 512 * 1024;

/// Maximum logical `agent()` calls emitted in a single step replay (`256`).
///
/// Keeps single-step replay fast (well under 50 ms for typical runs) and prevents accidental
/// unbounded loops from fanning out excessive child items or exceeding DB/frame budgets.
pub const MAX_CALLS_PER_STEP: usize = 256;

/// Maximum byte length of a single `CallIdentity` canonical JSON key (`8 KiB`).
///
/// `CallIdentity` is hashed and indexed per logical call (`phase`, `prompt_sha256`, `options`,
/// `occurrence`). Capping the serialized key prevents oversized option schemas or tags from
/// bloating storage rows or canonicalization CPU; large prompts do not inflate the key because
/// only their SHA-256 digest enters `CallIdentity`.
pub const MAX_CALL_IDENTITY_BYTES: usize = 8 * 1024;

/// Maximum serialized UTF-8 byte length of a single structured call or workflow `result` (`32 KiB`).
///
/// Structured results carry concise orchestration decisions and summaries; bulk artifacts travel
/// through Hub delivery files instead. Keeping every result within 32 KiB bounds storage rows,
/// replay stdin frames, and QuickJS heap usage.
pub const MAX_RESULT_BYTES: usize = 32 * 1024;

/// Maximum serialized UTF-8 byte length of a `StepFrame` written on child stdout (`4 MiB`).
///
/// Prevents a script returning huge prompts or output frames from forcing the Hub parent process
/// to buffer unbounded stdout; the parent streams up to `limit + 1` bytes and kills the child on
/// overflow.
pub const MAX_STEP_FRAME_BYTES: usize = 4 * 1024 * 1024;

/// Maximum serialized UTF-8 byte length of a `StepInput` sent on child stdin (`12 MiB`).
///
/// Accommodates a worst-case 256-call replay where every recorded result approaches
/// [`MAX_RESULT_BYTES`] (`256 * 32 KiB = 8 MiB`) plus script source and JSON framing overhead,
/// while still bounding child stdin allocation strictly.
pub const MAX_STEP_INPUT_BYTES: usize = 12 * 1024 * 1024;

/// Maximum byte length of a workflow JS script source (`256 KiB`).
///
/// Orchestration scripts should stay concise and auditable; capping source size bounds QuickJS
/// parser memory and compilation latency.
pub const MAX_SCRIPT_SOURCE_BYTES: usize = 256 * 1024;

/// Maximum byte length of a single `agent(prompt, ...)` prompt string (`256 KiB`).
///
/// Prevents a script from constructing a multi-megabyte prompt that would blow the stdout frame
/// budget or DB row limits on a single call.
pub const MAX_PROMPT_BYTES: usize = 256 * 1024;

/// Maximum byte length of a `phase(name, ...)` name (`128 B`).
///
/// Phase names appear in `CallIdentity` and UI group headers and are short semantic labels.
pub const MAX_PHASE_NAME_BYTES: usize = 128;

/// Maximum diagnostic `log()` entries recorded in a single step (`64`).
///
/// Logs are diagnostic only and never participate in workflow state transitions; entries beyond
/// this cap are silently dropped so logging can never fault a run.
pub const MAX_LOG_ENTRIES: usize = 64;

/// Maximum UTF-8 byte length of one `log()` entry (`512 B`).
///
/// Longer messages are truncated on a valid UTF-8 character boundary.
pub const MAX_LOG_ENTRY_BYTES: usize = 512;

/// Minimum ECMAScript safe integer (`-(2^53 - 1)`).
///
/// Integers outside `[MIN_SAFE_INTEGER, MAX_SAFE_INTEGER]` lose exact precision in IEEE-754
/// `f64` (the only JS `Number` representation), which would break deterministic replay and
/// `CallIdentity` hashing.
pub const MIN_SAFE_INTEGER: i64 = -(1_i64 << 53) + 1;

/// Maximum ECMAScript safe integer (`2^53 - 1`).
///
/// See [`MIN_SAFE_INTEGER`].
pub const MAX_SAFE_INTEGER: i64 = (1_i64 << 53) - 1;

/// Maximum nesting depth of JSON values converted across the JS/Rust boundary (`32`).
///
/// Prevents stack exhaustion during recursive canonicalization or JS value walking.
pub const MAX_JSON_DEPTH: usize = 32;

/// Maximum nesting depth of a [`crate::JsonSchemaSubset`] definition (`16`).
///
/// Prevents pathological deeply nested schema trees from blowing stack or validation time.
pub const MAX_SCHEMA_DEPTH: usize = 16;

/// Maximum total schema nodes across a [`crate::JsonSchemaSubset`] tree (`256`).
///
/// Prevents combinatorial schema size bombs via wide or nested `anyOf`/`oneOf`/`properties`.
pub const MAX_SCHEMA_NODES: usize = 256;

/// Maximum UTF-8 byte length of a JSON Schema `pattern` regular expression (`256 B`).
///
/// Limits schema regex complexity alongside static ReDoS AST/HIR rejection.
pub const MAX_SCHEMA_REGEX_BYTES: usize = 256;

/// Maximum repetition bound `{n}` / `{n,m}` permitted in a JSON Schema `pattern` (`100`).
///
/// Prevents NFA state explosion from patterns such as `a{10000}`.
pub const MAX_SCHEMA_REGEX_REPETITION: u32 = 100;

/// Maximum compiled NFA size in bytes for a JSON Schema `pattern` (`256 KiB`).
///
/// Passed directly to `regex::RegexBuilder::size_limit`.
pub const MAX_SCHEMA_REGEX_COMPILED_BYTES: usize = 256 * 1024;

/// Maximum compiled DFA cache size in bytes for a JSON Schema `pattern` (`256 KiB`).
///
/// Passed directly to `regex::RegexBuilder::dfa_size_limit`.
pub const MAX_SCHEMA_REGEX_DFA_BYTES: usize = 256 * 1024;

/// Optional per-step budget overrides. Every value is clamped to `[1, HARD_MAX]`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepLimitsOverride {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wall_time_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub js_heap_bytes: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub js_stack_bytes: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_calls: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_call_identity_bytes: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_result_bytes: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_frame_bytes: Option<usize>,
}

/// Resolved execution limits after clamping any caller overrides against the hard ceilings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectiveLimits {
    pub wall_time: Duration,
    pub js_heap_bytes: usize,
    pub js_stack_bytes: usize,
    pub max_calls: usize,
    pub max_call_identity_bytes: usize,
    pub max_result_bytes: usize,
    pub max_frame_bytes: usize,
}

impl Default for EffectiveLimits {
    fn default() -> Self {
        Self {
            wall_time: MAX_STEP_WALL_TIME,
            js_heap_bytes: MAX_JS_HEAP_BYTES,
            js_stack_bytes: MAX_JS_STACK_BYTES,
            max_calls: MAX_CALLS_PER_STEP,
            max_call_identity_bytes: MAX_CALL_IDENTITY_BYTES,
            max_result_bytes: MAX_RESULT_BYTES,
            max_frame_bytes: MAX_STEP_FRAME_BYTES,
        }
    }
}

impl EffectiveLimits {
    /// Clamps every override strictly within `[1, HARD_MAX]`.
    pub fn resolve(overrides: Option<&StepLimitsOverride>) -> Self {
        let base = Self::default();
        let Some(o) = overrides else {
            return base;
        };
        Self {
            wall_time: Duration::from_millis(
                o.wall_time_ms
                    .unwrap_or(MAX_STEP_WALL_TIME_MS)
                    .clamp(1, MAX_STEP_WALL_TIME_MS),
            ),
            js_heap_bytes: o
                .js_heap_bytes
                .unwrap_or(base.js_heap_bytes)
                .clamp(1, MAX_JS_HEAP_BYTES),
            js_stack_bytes: o
                .js_stack_bytes
                .unwrap_or(base.js_stack_bytes)
                .clamp(1, MAX_JS_STACK_BYTES),
            max_calls: o
                .max_calls
                .unwrap_or(base.max_calls)
                .clamp(1, MAX_CALLS_PER_STEP),
            max_call_identity_bytes: o
                .max_call_identity_bytes
                .unwrap_or(base.max_call_identity_bytes)
                .clamp(1, MAX_CALL_IDENTITY_BYTES),
            max_result_bytes: o
                .max_result_bytes
                .unwrap_or(base.max_result_bytes)
                .clamp(1, MAX_RESULT_BYTES),
            max_frame_bytes: o
                .max_frame_bytes
                .unwrap_or(base.max_frame_bytes)
                .clamp(1, MAX_STEP_FRAME_BYTES),
        }
    }
}
