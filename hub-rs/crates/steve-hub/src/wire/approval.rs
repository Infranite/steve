//! Connect workspace policy, the selected harness declaration and the Node's applied facts.

use std::collections::BTreeMap;
use std::sync::Arc;

use steve_harness_spec::{Declaration, NativeApprovalFallback, PermissionPolicy};
use steve_node_api::PermissionMode;
use steve_store::{Result, Store};
use steve_types::ApprovalMode;

#[derive(Clone, Default)]
pub struct ApprovalModes(pub Arc<BTreeMap<String, Declaration>>);

pub struct Desired {
    pub mode: PermissionMode,
    pub policy: ApprovalMode,
    pub native_id: Option<String>,
    pub native: Option<NativeApprovalFallback>,
}

impl ApprovalModes {
    pub fn native_mode(&self, harness: &str) -> Option<String> {
        steve_harness_spec::spec(harness, self.0.get(harness))
            .properties
            .native_mode_id()
    }

    pub fn desired(&self, db: &Store, session: &str) -> Result<Option<Desired>> {
        let Some(s) = db.sessions().get(session)? else {
            return Ok(None);
        };
        if s.readonly != 0 || s.status == "ended" {
            return Ok(None);
        }
        let spec = steve_harness_spec::spec(&s.harness, self.0.get(&s.harness));
        let native = spec.properties.native_approval();
        let bot = s
            .bot_id
            .as_deref()
            .map(|id| db.bots().get(id))
            .transpose()?
            .flatten();
        let configured = bot
            .as_ref()
            .map(|b| b.perm_mode.as_str())
            .unwrap_or(PermissionMode::Ask.as_str());
        let native_only = spec.properties.native_execution
            || spec.properties.permission_policy == PermissionPolicy::FullAccessOnly;
        let policy = db
            .read(|r| steve_store::approval::policy(r, &s.workspace_id))?
            .mode;
        // Plan is a read-only capability and never widened. Native-only adapters keep their
        // separately explicit admission requirement; the workspace switch cannot admit them.
        let mode = if configured == PermissionMode::Plan.as_str() {
            PermissionMode::Plan
        } else if native_only && configured == PermissionMode::FullAccess.as_str() {
            PermissionMode::FullAccess
        } else if native.is_some() && policy == ApprovalMode::HarnessAuto {
            PermissionMode::Auto
        } else {
            PermissionMode::Ask
        };
        Ok(Some(Desired {
            mode,
            native,
            policy,
            native_id: spec.properties.native_mode_id(),
        }))
    }
}
