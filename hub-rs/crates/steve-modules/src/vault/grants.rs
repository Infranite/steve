//! Grants: a bot may use one `normal` or `sensitive` item without asking,
//! within one scope, for 1, 8 or 24 hours. Scope is a project (`{"project_id"}`), the items without a
//! project (`{"scratch": true}`), or a site (`{"site"}`, kept for browser filling later; it never
//! covers a command). `high` items and bank cards are never grantable.

use serde::Deserialize;
use serde_json::{Value, json};
use steve_store::{ChangeEvent, Tx, base, now, uid};

use super::items::GrantOut;
use super::items::repo::{self as irepo, ItemRow};
use super::items::templates::BANK_CARD;
use super::use_repo as urepo;
use super::{Vault, VaultError, VaultResult};
use crate::ApiError;

/// `400` for a grant on a `high` item or a bank card, verbatim.
pub const GRANT_HIGH: &str = "高密条目不允许授权，每次都要你批准";
/// The allowed durations; 24 hours is the cap.
pub const HOURS: [i64; 3] = [1, 8, 24];
const HOUR_MS: i64 = 3_600_000;
const SITE_MAX: usize = 200;

/// `POST /vault/items/:id/grants`.
#[derive(Debug, Clone, Deserialize)]
pub struct NewGrant {
    pub bot_id: String,
    pub scope: Value,
    pub hours: i64,
}

/// Whether grants are possible on this item at all. Checked on every grant, and again before a grant
/// is honoured, so an item changed after the grant can't ride on it.
pub fn grantable(item: &ItemRow) -> bool {
    item.sensitivity != "high" && item.template != BANK_CARD
}

/// The scope in its stored form, or `400 参数不对：scope`. Projects must be in `ws`.
fn normalize_scope(c: &rusqlite::Connection, ws: &str, scope: &Value) -> VaultResult<Value> {
    let bad = || VaultError::from(ApiError::invalid("scope"));
    let m = scope.as_object().filter(|m| m.len() == 1).ok_or_else(bad)?;
    if let Some(p) = m.get("project_id") {
        let p = p.as_str().filter(|p| !p.is_empty()).ok_or_else(bad)?;
        if irepo::project_name(c, ws, p)?.is_none() {
            return Err(ApiError::not_found("项目").into());
        }
        return Ok(json!({ "project_id": p }));
    }
    if let Some(s) = m.get("site") {
        let s = s
            .as_str()
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty() && s.chars().count() <= SITE_MAX)
            .ok_or_else(bad)?;
        return Ok(json!({ "site": s }));
    }
    if m.get("scratch") == Some(&Value::Bool(true)) {
        return Ok(json!({ "scratch": true }));
    }
    Err(bad())
}

/// Writes a grant inside `tx` after the item and duration checks. Returns its id.
#[allow(clippy::too_many_arguments)]
pub(crate) fn add_grant(
    tx: &Tx,
    ws: &str,
    item: &ItemRow,
    bot: &str,
    scope: &Value,
    hours: i64,
    actor: &str,
    decision_id: Option<&str>,
) -> VaultResult<String> {
    if !grantable(item) {
        return Err(ApiError::rule(steve_i18n::t!("modules.vault.grant_high")).into());
    }
    if !HOURS.contains(&hours) {
        return Err(ApiError::invalid("hours").into());
    }
    let at = now();
    let id = uid("gr");
    urepo::insert_grant(
        tx,
        &id,
        ws,
        bot,
        &item.id,
        scope,
        at + hours * HOUR_MS,
        actor,
        decision_id,
        at,
    )?;
    base::audit(
        tx,
        ws,
        actor,
        "grant.add",
        &item.id,
        &json!({"grant": id, "bot": bot, "scope": scope, "hours": hours, "decision": decision_id}),
    )?;
    tx.publish(ChangeEvent::Vault {
        id: item.id.clone(),
        ws: ws.into(),
    });
    Ok(id)
}

/// The open grant that lets `bot` use `item` in `scope` now, if any (never for site scopes, never on
/// an item that is no longer grantable).
pub(crate) fn covering(
    c: &rusqlite::Connection,
    ws: &str,
    item: &ItemRow,
    bot: &str,
    scope: &Value,
) -> VaultResult<Option<String>> {
    if !grantable(item) || scope.get("site").is_some() {
        return Ok(None);
    }
    Ok(urepo::covering_grants(c, ws, &item.id, bot, scope, now())?
        .into_iter()
        .next())
}

impl Vault {
    /// `POST /vault/items/:id/grants`.
    pub fn create_grant(
        &self,
        ws: &str,
        user: &str,
        item_id: &str,
        g: NewGrant,
    ) -> VaultResult<GrantOut> {
        let id = self.store().transaction(|tx| {
            let (item, _) = super::items::own_item(tx, ws, user, item_id)?;
            if !grantable(&item) {
                return Err(ApiError::rule(steve_i18n::t!("modules.vault.grant_high")).into());
            }
            if !urepo::bot_in(tx, ws, &g.bot_id)? {
                return Err(ApiError::not_found("Bot").into());
            }
            if !HOURS.contains(&g.hours) {
                return Err(ApiError::invalid("hours").into());
            }
            let scope = normalize_scope(tx, ws, &g.scope)?;
            add_grant(tx, ws, &item, &g.bot_id, &scope, g.hours, user, None)
        })?;
        self.get_item(ws, user, item_id)?
            .grants
            .into_iter()
            .find(|x| x.id == id)
            .ok_or_else(|| VaultError::internal("new grant not listed"))
    }

    /// `DELETE /grants/:id`: `revoked` set; already revoked or expired is fine.
    pub fn revoke_grant(&self, ws: &str, user: &str, grant_id: &str) -> VaultResult<()> {
        self.store().transaction(|tx| {
            let g = urepo::grant(tx, grant_id)?
                .filter(|g| g.workspace_id == ws)
                .ok_or_else(|| ApiError::not_found("授权"))?;
            // the item must be visible to the caller (a deleted item has no open grants anyway)
            let visible = irepo::item(tx, &g.resource_id)?
                .and_then(|i| irepo::vault(tx, &i.vault_id).ok().flatten())
                .is_some_and(|v| v.visible_to(ws, user));
            if !visible {
                return Err(ApiError::not_found("授权").into());
            }
            if urepo::revoke_grant(tx, grant_id, now())? {
                base::audit(
                    tx,
                    ws,
                    user,
                    "grant.revoke",
                    &g.resource_id,
                    &json!({"grant": grant_id, "bot": g.subject_id}),
                )?;
                tx.publish(ChangeEvent::Vault {
                    id: g.resource_id.clone(),
                    ws: ws.into(),
                });
            }
            Ok(())
        })
    }
}
