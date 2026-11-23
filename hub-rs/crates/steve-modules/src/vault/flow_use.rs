//! Flow use of secrets: an `http` node of a flow expands
//! `{{secrets.<item>.<field>}}` at send time. Same rules as a bot's use with
//! the flow as the subject: a `secret.use` grant with `subject_type='flow'` lets it go ahead,
//! otherwise you approve the step. `high` items and bank cards are never covered by a grant. Each
//! release writes one `secret_uses` row (`actor_type=flow`, `run_id`, `mode=internal`).

use steve_store::{now, uid};

use super::grants::grantable;
use super::secret::SecretBytes;
use super::{Vault, VaultResult};
use crate::ApiError;
use steve_store::{
    GrantRows,
    secret_uses::{SecretUses as UseRows, UseRecord},
    vault_items::VaultItems,
};

/// One flow step's use of one item.
#[derive(Debug, Clone)]
pub struct FlowSecretRequest<'a> {
    pub ws: &'a str,
    pub flow_id: &'a str,
    /// The run's flow version: only a grant written for this version counts.
    pub flow_version: i64,
    /// `flows.created_by`: personal items are usable only by their owner's flows.
    pub flow_owner: &'a str,
    pub run_id: &'a str,
    pub item_id: &'a str,
    /// Field labels.
    pub labels: &'a [String],
    /// `secret_uses.purpose` (the step's name), `command` (the request line, no values).
    pub purpose: &'a str,
    pub command: &'a str,
    /// You approved this very step (a `flow_step` decision).
    pub approved: bool,
}

/// What [`Vault::use_for_flow`] decided.
pub enum FlowSecretOutcome {
    /// `labels` → values, and the `secret_uses` id to [`Vault::finish_flow_use`].
    Released {
        use_id: String,
        values: Vec<(String, SecretBytes)>,
    },
    /// No grant covers it: ask. `title` is the item's.
    NeedsApproval { title: String },
}

/// Whether a flow step may use an item now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowSecretCheck {
    /// The item's title (for the approval question).
    pub title: String,
    /// The `secret.use` grant of the flow that covers it.
    pub grant_id: Option<String>,
    /// A grant covers it or you approved the step.
    pub allowed: bool,
    /// A flow grant may cover it at all (never for `high` items and bank cards).
    pub grantable: bool,
}

impl Vault {
    /// Checks without decrypting. `404 条目` when the item is not an active item of `ws` the
    /// flow's owner can see.
    pub fn check_flow_use(&self, r: &FlowSecretRequest<'_>) -> VaultResult<FlowSecretCheck> {
        let at = now();
        self.read(|c| {
            let item = VaultItems(c)
                .item(r.item_id)?
                .filter(|i| i.workspace_id == r.ws && i.status == "active")
                .ok_or_else(|| ApiError::not_found("条目"))?;
            let visible = VaultItems(c)
                .vault(&item.vault_id)?
                .is_some_and(|v| v.visible_to(r.ws, r.flow_owner));
            if !visible {
                return Err(ApiError::not_found("条目").into());
            }
            let can_grant = grantable(&item);
            let grant_id: Option<String> = if can_grant {
                GrantRows(c).vault_flow_grant(r.ws, r.flow_id, r.flow_version, &item.id, at)?
            } else {
                None
            };
            Ok::<_, super::VaultError>(FlowSecretCheck {
                title: item.title,
                allowed: grant_id.is_some() || r.approved,
                grant_id,
                grantable: can_grant,
            })
        })
    }

    /// Checks ([`Vault::check_flow_use`]) and, when allowed, decrypts the fields (`404 字段` for
    /// an unknown label) and writes the use row (`running`).
    pub fn use_for_flow(&self, r: &FlowSecretRequest<'_>) -> VaultResult<FlowSecretOutcome> {
        let check = self.check_flow_use(r)?;
        if !check.allowed {
            return Ok(FlowSecretOutcome::NeedsApproval { title: check.title });
        }
        let at = now();
        let values = self.injection_values(r.ws, r.item_id, r.labels)?;
        let use_id = uid("su");
        let grant = check.grant_id;
        self.store().write(|tx| {
            UseRows(tx).insert(&UseRecord {
                id: use_id.clone(),
                workspace_id: r.ws.into(),
                item_id: r.item_id.into(),
                actor_type: "flow".into(),
                actor_id: r.flow_id.into(),
                session_id: None,
                work_item_id: None,
                purpose: r.purpose.into(),
                mode: "internal".into(),
                command: Some(r.command.into()),
                fields: r.labels.to_vec(),
                decision_id: None,
                grant_id: grant.clone(),
                device_id: "hub".into(),
                status: "running".into(),
                exit_code: None,
                created: at,
                finished: None,
                run_id: Some(r.run_id.into()),
            })?;
            Ok::<_, super::VaultError>(())
        })?;
        Ok(FlowSecretOutcome::Released { use_id, values })
    }

    /// Ends a use of [`Vault::use_for_flow`]: `ok` or `failed`.
    pub fn finish_flow_use(&self, use_id: &str, ok: bool) -> VaultResult<()> {
        let at = now();
        self.store().write(|tx| {
            UseRows(tx).transition(
                use_id,
                "running",
                if ok { "ok" } else { "failed" },
                None,
                None,
                Some(at),
            )?;
            Ok::<_, super::VaultError>(())
        })
    }
}
