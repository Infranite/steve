//! Viewing a secret value (accounts-vault.md §4.2). Every reveal needs local verification by the user
//! (WebAuthn, `userVerification=required`), which slice V3 implements as a [`RevealGate`]. Until then
//! the gate is [`NoPasskeys`]: there is no passkey to verify with, so both steps answer
//! `400 先在这台设备上登记 Touch ID 或通行密钥`.
//!
//! This file is one of the three places that decrypt a field (§10 rule 2): [`Vault::reveal`], which
//! can only be called with a [`Verified`] that a gate hands out.

use async_trait::async_trait;
use serde_json::{Value, json};
use steve_store::{ChangeEvent, base, now, uid};

use super::repo::{self, UseRow};
use super::service::own_item;
use crate::ApiError;
use crate::vault::crypto::open_field;
use crate::vault::secret::SecretBytes;
use crate::vault::{Vault, VaultError, VaultResult};

/// `400` when the user has no passkey on this Hub (§4.2), verbatim.
pub const NEED_PASSKEY: &str = "先在这台设备上登记 Touch ID 或通行密钥";
/// How long the web shows a revealed value (§4.2).
pub const REVEAL_TTL_S: u64 = 30;

/// What is being revealed, by whom.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevealTarget {
    pub ws: String,
    pub user: String,
    pub item_id: String,
    pub field_id: String,
    /// The host the browser used (`x-forwarded-host`, else `Host`; lowercase), which picks the
    /// WebAuthn relying party like steve-auth's `origin_of`.
    pub host: String,
}

/// Proof that the user just passed local verification for one [`RevealTarget`]. Only gates inside
/// this crate can make one.
#[derive(Debug)]
pub struct Verified {
    target: RevealTarget,
}

impl Verified {
    /// For [`RevealGate::finish`] implementations, after the assertion checked out for `target`.
    pub(crate) fn new(target: RevealTarget) -> Self {
        Self { target }
    }
}

/// Local verification before a reveal. V3 implements it with webauthn-rs: `start` issues a single-use
/// 60 s challenge bound to the target, `finish` verifies the assertion against the user's passkeys.
#[async_trait]
pub trait RevealGate: Send + Sync {
    /// `POST …/reveal/start`: the public challenge options for `navigator.credentials.get`.
    async fn start(&self, target: &RevealTarget) -> VaultResult<Value>;
    /// `POST …/reveal/finish`: checks `assertion`; `Ok` only for a valid one for this target.
    async fn finish(&self, target: &RevealTarget, assertion: &Value) -> VaultResult<Verified>;
}

/// The gate until V3: nobody can have registered a passkey yet.
pub struct NoPasskeys;

#[async_trait]
impl RevealGate for NoPasskeys {
    async fn start(&self, _: &RevealTarget) -> VaultResult<Value> {
        Err(ApiError::rule(NEED_PASSKEY).into())
    }
    async fn finish(&self, _: &RevealTarget, _: &Value) -> VaultResult<Verified> {
        Err(ApiError::rule(NEED_PASSKEY).into())
    }
}

impl Vault {
    /// Checks that `t` names a visible secret field, before a gate step runs.
    pub fn reveal_check(&self, t: &RevealTarget) -> VaultResult<()> {
        self.store().reader(|c| {
            own_item(c, &t.ws, &t.user, &t.item_id)?;
            let f = repo::fields(c, &t.item_id)?
                .into_iter()
                .find(|f| f.id == t.field_id)
                .ok_or_else(|| ApiError::not_found("字段"))?;
            if !f.secret {
                return Err(ApiError::rule("这个字段不是机密，不需要验证").into());
            }
            Ok::<_, VaultError>(())
        })
    }

    /// An audit row for a reveal the gate refused (no value was read).
    pub fn reveal_refused(&self, t: &RevealTarget, why: &str) -> VaultResult<()> {
        self.store().transaction(|tx| {
            base::audit(
                tx,
                &t.ws,
                &t.user,
                "vault.reveal_refused",
                &t.item_id,
                &json!({"field": t.field_id, "reason": why}),
            )?;
            Ok::<_, VaultError>(())
        })
    }

    /// `reveal/finish` after the gate: decrypts one field, logs a use (`mode=view|copy`, actor me)
    /// and an audit row. The value goes only into the `no-store` response.
    pub fn reveal(&self, v: Verified, copy: bool) -> VaultResult<SecretBytes> {
        let t = v.target;
        let (item, field) = self.store().reader(|c| {
            let (i, _) = own_item(c, &t.ws, &t.user, &t.item_id)?;
            let f = repo::fields(c, &t.item_id)?
                .into_iter()
                .find(|f| f.id == t.field_id && f.secret)
                .ok_or_else(|| ApiError::not_found("字段"))?;
            Ok::<_, VaultError>((i, f))
        })?;
        let dek = self
            .dek_of(&item)?
            .ok_or_else(|| VaultError::internal("vault item without a data key"))?;
        let ct = field
            .value_ct
            .as_deref()
            .ok_or_else(|| VaultError::internal("secret field without a value"))?;
        let value = open_field(&dek, &t.ws, &t.item_id, &t.field_id, ct)?;
        let mode = if copy { "copy" } else { "view" };
        let at = now();
        self.store().transaction(|tx| {
            let su = uid("su");
            repo::insert_use(
                tx,
                &t.ws,
                &UseRow {
                    id: su.clone(),
                    item_id: t.item_id.clone(),
                    actor_type: "me".into(),
                    actor_id: t.user.clone(),
                    purpose: if copy { "复制原值" } else { "查看原值" }.into(),
                    mode: mode.into(),
                    fields: vec![field.label.clone()],
                    status: "ok".into(),
                    created: at,
                },
            )?;
            base::audit(
                tx,
                &t.ws,
                &t.user,
                "vault.reveal",
                &t.item_id,
                &json!({"field": t.field_id, "label": field.label, "mode": mode, "use": su}),
            )?;
            tx.publish(ChangeEvent::Vault {
                id: t.item_id.clone(),
                ws: t.ws.clone(),
            });
            Ok::<_, VaultError>(())
        })?;
        tracing::info!(ws = %t.ws, item = %t.item_id, mode, "vault field revealed");
        Ok(value)
    }
}
