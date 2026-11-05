//! Steve's tools as the Node asks the Hub for them. A Node exposes them to its harnesses in whatever
//! tool protocol the harness speaks (MCP today); the Hub defines, authorizes, runs and audits them once.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::common::{ApiError, ExecutionRef};
use crate::ids::*;
use crate::run::Part;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDescriptor {
    pub name: String,
    pub description: String,
    /// JSON Schema of the arguments.
    pub input_schema: Value,
    pub read_only: bool,
}

/// The tools one execution may see. Hiding a tool is only for the harness's sake: every call is
/// checked again.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Catalog {
    pub revision: Revision,
    pub tools: Vec<ToolDescriptor>,
}

/// `(execution, id)` is the key: a resend with the same key returns the first call's result; the same
/// key with other arguments is a conflict.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: CallId,
    pub execution: ExecutionRef,
    pub catalog: Revision,
    pub name: String,
    pub args: Value,
    pub deadline: Timestamp,
}

impl ToolCall {
    pub fn key(&self) -> ToolKey {
        ToolKey {
            execution: self.execution.clone(),
            id: self.id.clone(),
        }
    }
    /// Canonical over the tool name and its arguments (key order never matters).
    pub fn args_digest(&self) -> crate::common::ApiResult<Digest> {
        crate::common::digest_of(&serde_json::json!({"name": self.name, "args": self.args}))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ToolKey {
    pub execution: ExecutionRef,
    pub id: CallId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ToolResult {
    Completed {
        parts: Vec<Part>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        structured: Option<Value>,
        /// Tell the agent to end its turn now (after asking for approval, after delivering).
        #[serde(default)]
        end_turn: bool,
    },
    /// Submitted for approval; the call itself is done (nobody waits hours on a request).
    PendingApproval {
        operation: OperationId,
        decision: String,
    },
    Running {
        operation: OperationId,
    },
    /// Refused or failed; `is_error` for the harness.
    Failed {
        error: ApiError,
    },
    /// An outside effect may have happened: look it up, do not retry under a new id.
    Unknown {
        operation: OperationId,
    },
}
