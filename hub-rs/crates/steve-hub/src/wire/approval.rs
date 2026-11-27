//! Connect workspace permission policy, the selected harness declaration, and the Node's applied facts.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use steve_core::harnesses::Harness;
use steve_harness_spec::{Declaration, NativeApprovalFallback, PermissionPolicy};
use steve_node_api::PermissionMode;
use steve_store::{Result, Store};
use steve_types::{ApprovalMode, PermissionHarnessSupport};

#[derive(Clone, Default)]
pub struct ApprovalModes(pub Arc<BTreeMap<String, Declaration>>);

pub struct Desired {
    pub mode: PermissionMode,
    pub policy: ApprovalMode,
    pub native_id: Option<String>,
    pub native: Option<NativeApprovalFallback>,
    /// Whether this session's harness supports the workspace's requested `policy` level.
    pub policy_supported: bool,
}

impl ApprovalModes {
    /// Whether `harness` declares support for `policy` and its adapter-specific mode id or flag.
    pub fn policy_support(&self, harness: &str, policy: ApprovalMode) -> (bool, Option<String>) {
        let properties = steve_harness_spec::spec(harness, self.0.get(harness)).properties;
        match policy {
            ApprovalMode::Ask => (true, None),
            ApprovalMode::Auto => {
                let id = properties.auto_mode_id();
                (id.is_some(), id)
            }
            ApprovalMode::FullAccess => {
                let id = properties.full_access_mode();
                (id.is_some(), id)
            }
        }
    }

    /// Spec-derived capabilities for harnesses discovered on Nodes (`ready == Some(true)`),
    /// plus display names of all known harnesses whose spec sets `nested_sandbox = false`.
    pub fn harness_summary(
        &self,
        discovered: &[Harness],
    ) -> (Vec<PermissionHarnessSupport>, Vec<String>) {
        let mut harnesses = Vec::new();
        for h in discovered {
            if h.ready != Some(true) {
                continue;
            }
            let spec = steve_harness_spec::spec(&h.id, self.0.get(&h.id));
            let props = &spec.properties;
            let auto_mode = props.auto_mode_id();
            let full_access_mode = props.full_access_mode();
            let auto = auto_mode.is_some()
                && h.modes
                    .as_ref()
                    .is_none_or(|ms| ms.contains(&PermissionMode::Auto));
            let full_access = full_access_mode.is_some()
                && h.modes
                    .as_ref()
                    .is_none_or(|ms| ms.contains(&PermissionMode::FullAccess));
            harnesses.push(PermissionHarnessSupport {
                id: h.id.clone(),
                name: h.name.clone(),
                ready: h.ready,
                auto,
                auto_mode: auto.then_some(auto_mode).flatten(),
                full_access,
                full_access_mode: full_access.then_some(full_access_mode).flatten(),
                nested_sandbox: props.nested_sandbox,
            });
        }
        let mut seen = BTreeSet::new();
        let mut unconfined = BTreeSet::new();
        for b in steve_harness_spec::builtins() {
            seen.insert(b.id);
            let spec = steve_harness_spec::spec(b.id, self.0.get(b.id));
            if !spec.properties.nested_sandbox {
                unconfined.insert(spec.name);
            }
        }
        for (id, decl) in self.0.iter() {
            if seen.contains(id.as_str()) {
                continue;
            }
            let spec = steve_harness_spec::spec(id, Some(decl));
            if !spec.properties.nested_sandbox {
                unconfined.insert(spec.name);
            }
        }
        for h in discovered {
            if h.ready == Some(true) {
                let spec = steve_harness_spec::spec(&h.id, self.0.get(&h.id));
                if !spec.properties.nested_sandbox {
                    unconfined.insert(h.name.clone());
                }
            }
        }
        (harnesses, unconfined.into_iter().collect())
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
        let auto_id = spec.properties.auto_mode_id();
        let full_access_id = spec.properties.full_access_mode();
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
        // separately explicit Bot admission requirement; workspace policy cannot admit them
        // unless their Bot is already configured for full_access.
        let (mode, native_id, policy_supported) = if configured == PermissionMode::Plan.as_str() {
            (PermissionMode::Plan, None, true)
        } else if native_only && configured == PermissionMode::FullAccess.as_str() {
            (
                PermissionMode::FullAccess,
                full_access_id,
                policy != ApprovalMode::Auto,
            )
        } else {
            match policy {
                ApprovalMode::Ask => (PermissionMode::Ask, None, true),
                ApprovalMode::Auto => match auto_id {
                    Some(id) => (PermissionMode::Auto, Some(id), true),
                    None => (PermissionMode::Ask, None, false),
                },
                ApprovalMode::FullAccess => {
                    match (!native_only).then_some(full_access_id).flatten() {
                        Some(id) => (PermissionMode::FullAccess, Some(id), true),
                        None => (PermissionMode::Ask, None, false),
                    }
                }
            }
        };
        Ok(Some(Desired {
            mode,
            native,
            policy,
            native_id,
            policy_supported,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use steve_store::{ImportedSession, NewBot, NewSession, NoPublisher};

    fn seed_session(store: &Store, label: &str, harness: &str, bot_perm: &str) -> String {
        let bot = store
            .bots()
            .create(
                "default",
                &NewBot {
                    name: label.into(),
                    harness: harness.into(),
                    perm_mode: Some(bot_perm.into()),
                    ..Default::default()
                },
                "owner",
            )
            .unwrap()
            .expect("bot created");
        store
            .sessions()
            .create(&NewSession {
                workspace_id: "default".into(),
                item_id: None,
                bot_id: Some(bot.id),
                harness: harness.into(),
                model: None,
                reasoning_effort: Default::default(),
                cwd: String::new(),
                title: label.into(),
                token: format!("tok_{label}"),
            })
            .unwrap()
            .id
    }

    fn set_policy(store: &Store, owner: &str, mode: ApprovalMode) {
        let rev = store
            .read(|r| steve_store::approval::policy(r, "default"))
            .unwrap()
            .revision;
        store
            .write(|w| steve_store::approval::set(w, "default", owner, rev, mode))
            .unwrap();
    }

    #[test]
    fn three_level_mapping_respects_spec_plan_and_readonly() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("perm.sqlite3"), NoPublisher).unwrap();
        store.ensure_workspace().unwrap();
        let owner = store
            .users()
            .create("owner-sub", "Owner", None, "owner")
            .unwrap();

        let s_claude = seed_session(&store, "claude", "claude-code", "ask");
        let s_agy = seed_session(&store, "agy", "antigravity", "ask");
        let s_opencode = seed_session(&store, "opencode", "opencode", "ask");
        let s_plan = seed_session(&store, "plan", "claude-code", "plan");
        let s_ro = store
            .sessions()
            .insert_imported(&ImportedSession {
                workspace_id: "default".into(),
                harness: "claude-code".into(),
                cwd: String::new(),
                title: "imported".into(),
                source: "import".into(),
                import_key: "imp_ro".into(),
                created: 1,
            })
            .unwrap();
        let modes = ApprovalModes::default();

        // Default Ask
        let d = modes.desired(&store, &s_claude).unwrap().unwrap();
        assert_eq!((d.mode, d.policy_supported), (PermissionMode::Ask, true));

        // Auto: supported harnesses get Auto; opencode stays Ask (unsupported); plan stays Plan
        set_policy(&store, &owner, ApprovalMode::Auto);
        let d = modes.desired(&store, &s_claude).unwrap().unwrap();
        assert_eq!(
            (d.mode, d.policy_supported, d.native_id.as_deref()),
            (PermissionMode::Auto, true, Some("auto"))
        );
        let d = modes.desired(&store, &s_opencode).unwrap().unwrap();
        assert_eq!(
            (d.mode, d.policy_supported, d.native_id.as_deref()),
            (PermissionMode::Ask, false, None)
        );
        let d = modes.desired(&store, &s_plan).unwrap().unwrap();
        assert_eq!(d.mode, PermissionMode::Plan);
        assert!(modes.desired(&store, &s_ro).unwrap().is_none());

        // FullAccess: modes.full_access and mode_args.full_access both work; opencode unsupported; plan stays Plan
        set_policy(&store, &owner, ApprovalMode::FullAccess);
        let d = modes.desired(&store, &s_claude).unwrap().unwrap();
        assert_eq!(
            (d.mode, d.policy_supported, d.native_id.as_deref()),
            (PermissionMode::FullAccess, true, Some("bypassPermissions"))
        );
        let d = modes.desired(&store, &s_agy).unwrap().unwrap();
        assert_eq!(
            (d.mode, d.policy_supported, d.native_id.as_deref()),
            (
                PermissionMode::FullAccess,
                true,
                Some("--dangerously-skip-permissions")
            )
        );
        let d = modes.desired(&store, &s_opencode).unwrap().unwrap();
        assert_eq!(
            (d.mode, d.policy_supported, d.native_id.as_deref()),
            (PermissionMode::Ask, false, None)
        );
        let d = modes.desired(&store, &s_plan).unwrap().unwrap();
        assert_eq!(d.mode, PermissionMode::Plan);
        assert!(modes.desired(&store, &s_ro).unwrap().is_none());
    }

    #[test]
    fn harness_summary_derives_unconfined_from_nested_sandbox_spec() {
        let mut declared = BTreeMap::new();
        declared.insert(
            "custom-self-box".into(),
            serde_json::from_value(serde_json::json!({
                "name": "Custom Self Box",
                "properties": { "nested_sandbox": false }
            }))
            .unwrap(),
        );
        let modes = ApprovalModes(Arc::new(declared));
        let (_, unconfined) = modes.harness_summary(&[]);
        assert!(
            unconfined.iter().any(|name| name == "Codex"),
            "built-in nested_sandbox=false harness included: {unconfined:?}"
        );
        assert!(
            unconfined.iter().any(|name| name == "Custom Self Box"),
            "declared nested_sandbox=false harness included: {unconfined:?}"
        );
    }
}
