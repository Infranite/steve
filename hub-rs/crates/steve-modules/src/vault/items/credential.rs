//! In-process use of an item's secrets: the vault side
//! of `ConnCx::with_credential`. The values are lent to a closure and zeroed when it returns, so an
//! adapter never holds a copy it could keep. One of the three decryption sites.

use std::fmt;

use serde_json::json;
use steve_store::{now, uid, vault_items::VaultItems};

use crate::ApiError;
use crate::vault::crypto::open_field;
use crate::vault::secret::SecretBytes;
use crate::vault::{Vault, VaultError, VaultResult};
use steve_store::secret_uses::{NewUseLog, SecretUses};

/// The decrypted secret fields of one item, by label. Not `Clone`, `Serialize` or `Display`; `Debug`
/// shows the labels only.
pub struct Credential {
    fields: Vec<(String, SecretBytes)>,
}

impl Credential {
    pub fn get(&self, label: &str) -> Option<&SecretBytes> {
        self.fields.iter().find(|(l, _)| l == label).map(|(_, v)| v)
    }
    /// The value as UTF-8 (tokens and keys are text).
    pub fn text(&self, label: &str) -> Option<&str> {
        self.get(label)
            .and_then(|v| std::str::from_utf8(v.expose()).ok())
    }
    pub fn labels(&self) -> impl Iterator<Item = &str> {
        self.fields.iter().map(|(l, _)| l.as_str())
    }
}

impl fmt::Debug for Credential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Credential")
            .field(&self.labels().collect::<Vec<_>>())
            .finish()
    }
}

const DAY_MS: i64 = 86_400_000;
/// Failures of the same item and user this close after the last one extend its row.
const FAILURE_MERGE_MS: i64 = 3_600_000;

impl Vault {
    /// Lends the secret fields of `item_id` to `f`. `actor` is who uses them (`con_…` for a
    /// connection), `purpose` goes into the use log. Logged once per item, actor and (UTC) day when it
    /// works; a failure gets a `failed` row, and failures that follow it within an hour (nothing
    /// worked in between) only move that row's `finished` on.
    pub fn with_credential<R>(
        &self,
        ws: &str,
        item_id: &str,
        actor: &str,
        purpose: &str,
        f: impl FnOnce(&Credential) -> R,
    ) -> VaultResult<R> {
        let (labels, opened) = self.open_credential(ws, item_id, None)?;
        self.log_use(ws, item_id, actor, purpose, &labels, opened.is_ok())?;
        let cred = opened?;
        Ok(f(&cred))
    }

    /// Like [`Vault::with_credential`] but writes nothing, not even on failure: for checks on
    /// unauthenticated input that must leave no trace until they pass (a webhook signature). Once the
    /// value was actually relied on, the caller records it with [`Vault::log_internal_use`].
    ///
    /// Only for items of the workspace's system vault (connection credentials, webhook secrets): a
    /// user's own items are never opened without a use record (`404 没有这个条目` otherwise).
    pub fn with_credential_unlogged<R>(
        &self,
        ws: &str,
        item_id: &str,
        f: impl FnOnce(&Credential) -> R,
    ) -> VaultResult<R> {
        let (_, opened) = self.open_credential(ws, item_id, Some("system"))?;
        Ok(f(&opened?))
    }

    /// The use log entry [`Vault::with_credential`] writes (once per item, actor and UTC day), for a
    /// value opened with [`Vault::with_credential_unlogged`].
    pub fn log_internal_use(
        &self,
        ws: &str,
        item_id: &str,
        actor: &str,
        purpose: &str,
    ) -> VaultResult<()> {
        let labels: Vec<String> = self
            .store()
            .read(|r| VaultItems(r).fields(item_id))?
            .into_iter()
            .filter(|r| r.secret)
            .map(|r| r.label)
            .collect();
        self.log_use(ws, item_id, actor, purpose, &labels, true)
    }

    /// Decrypts the secret fields: `(labels, credential or why not)`. `Err` only when the item is not
    /// an active item of `ws`.
    fn open_credential(
        &self,
        ws: &str,
        item_id: &str,
        scope: Option<&str>,
    ) -> VaultResult<(Vec<String>, VaultResult<Credential>)> {
        let (item, rows) = self
            .store()
            .read::<_, steve_store::StoreError>(|r| {
                let mut vaults = VaultItems(r);
                let Some(item) = vaults
                    .item(item_id)?
                    .filter(|i| i.workspace_id == ws && i.status == "active")
                else {
                    return Ok(None);
                };
                if let Some(scope) = scope
                    && !vaults
                        .vault(&item.vault_id)?
                        .is_some_and(|v| v.scope == scope)
                {
                    return Ok(None);
                }
                let rows = vaults.fields(item_id)?;
                Ok(Some((item, rows)))
            })?
            .ok_or_else(|| ApiError::not_found("条目"))?;
        let opened = (|| {
            let dek = self
                .dek_of(&item)?
                .ok_or_else(|| VaultError::internal("vault item without a data key"))?;
            let mut fields = Vec::new();
            for r in rows.iter().filter(|r| r.secret) {
                let ct = r.value_ct.as_deref().unwrap_or_default();
                fields.push((r.label.clone(), open_field(&dek, ws, item_id, &r.id, ct)?));
            }
            Ok::<_, VaultError>(Credential { fields })
        })();
        let labels: Vec<String> = rows
            .iter()
            .filter(|r| r.secret)
            .map(|r| r.label.clone())
            .collect();
        Ok((labels, opened))
    }

    fn log_use(
        &self,
        ws: &str,
        item_id: &str,
        actor: &str,
        purpose: &str,
        labels: &[String],
        ok: bool,
    ) -> VaultResult<()> {
        let at = now();
        self.store().write(|tx| {
            if ok && SecretUses(tx).has_internal(ws, item_id, actor, at - at.rem_euclid(DAY_MS))? {
                return Ok::<_, VaultError>(());
            }
            if !ok
                && let Some((id, status, finished)) =
                    SecretUses(tx).last_internal(ws, item_id, actor)?
                && status == "failed"
                && at - finished <= FAILURE_MERGE_MS
            {
                SecretUses(tx).touch_finished(&id, at)?;
                tx.emit(steve_types::DomainEvent::from(
                    steve_types::VaultEvent::Vault {
                        id: {
                            let value: String = item_id.into();
                            steve_types::VaultItemId::from(value)
                        },
                        ws: {
                            let value: String = ws.into();
                            steve_types::WorkspaceId::from(value)
                        },
                    },
                ));
                return Ok(());
            }
            let su = uid("su");
            SecretUses(tx).log(&NewUseLog {
                ws: ws,
                id: &su,
                item: item_id,
                actor_type: "steve",
                actor,
                purpose,
                mode: "internal",
                fields: labels,
                status: if ok { "ok" } else { "failed" },
                at,
            })?;
            tx.audit(
                ws,
                actor,
                "vault.use",
                item_id,
                &json!({"mode": "internal", "use": su, "ok": ok, "fields": labels}),
            )?;
            tx.emit(steve_types::DomainEvent::from(
                steve_types::VaultEvent::Vault {
                    id: {
                        let value: String = item_id.into();
                        steve_types::VaultItemId::from(value)
                    },
                    ws: {
                        let value: String = ws.into();
                        steve_types::WorkspaceId::from(value)
                    },
                },
            ));
            Ok(())
        })
    }
}
