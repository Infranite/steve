//! Vault slice V3 (accounts-vault.md §4.2): WebAuthn passkeys as the local verification before a
//! secret value is shown or copied (Touch ID / Face ID on the Mac and iPhone).
//!
//! - one relying party per configured public origin (`STEVE_PUBLIC_URLS`); a request uses the one
//!   whose host matches the `x-forwarded-host` / `Host` it came in on, else the first, the same
//!   way steve-auth's `origin_of` picks the login origin. Origins that can't be an RP (IP hosts)
//!   are skipped. A passkey belongs to one RP ID, so each domain is registered once;
//! - registration (`/passkeys/register/*`) needs a login from the last 10 minutes
//!   (`auth_sessions.created` of the request's `steve_session`), else `401 请重新登录后再登记`;
//! - [`Passkeys`] is the [`RevealGate`]: `start` issues a challenge (60 s, single use, bound to the
//!   item, field, user and RP), `finish` checks the assertion with `userVerification=required`;
//! - ceremony state lives only in memory; `passkeys.credential` is webauthn-rs's serialized
//!   `Passkey` (a public key and a counter, not a secret).
//!
//! Integration: `let pk = Passkeys::new(store.clone(), &auth.config().public_urls);` then
//! `.merge(vault::items::routes(vault.clone(), Arc::new(pk.clone())))` and
//! `.merge(vault::webauthn::routes(pk))` under `/api` after `require_user`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use async_trait::async_trait;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use steve_store::{Store, base, uid};
use webauthn_rs::prelude::{
    Passkey, PasskeyAuthentication, PasskeyRegistration, PublicKeyCredential,
    RegisterPublicKeyCredential, Url, Uuid, Webauthn, WebauthnBuilder,
};

use super::items::{NEED_PASSKEY, RevealGate, RevealTarget, Verified};
use super::{VaultError, VaultResult};
use crate::clock::{Clock, SystemClock};
use crate::{ApiError, Body, Caller};

/// How long a challenge (reveal or registration) can be answered (§4.2).
pub const CHALLENGE_TTL_MS: i64 = 60_000;
/// Registration needs a login at most this old (§4.2).
pub const FRESH_LOGIN_MS: i64 = 10 * 60_000;
/// The browser session cookie (steve-auth `SESSION`).
const SESSION_COOKIE: &str = "steve_session";

/// No challenge for this target: never started, already used, or answered too late.
pub const CHALLENGE_GONE: &str = "本机验证已过期，请重新验证";
/// The assertion didn't verify (wrong key, no user verification, wrong origin, wrong challenge).
pub const NOT_VERIFIED: &str = "本机验证没有通过";
/// `finish` came in on another host than `start`.
pub const OTHER_ORIGIN: &str = "本机验证要在同一个访问地址上完成，请重新验证";
/// No configured origin can be a WebAuthn relying party.
pub const NO_RP: &str = "这个访问地址不能使用通行密钥（需要 https 域名或 localhost）";
const REG_GONE: &str = "登记已过期，请重新开始";
const REG_FAILED: &str = "登记没有成功：这个通行密钥无效";
const REG_DUP: &str = "这个通行密钥已经登记过了";

/// The request's host: first `x-forwarded-host` value, else `Host`; trimmed, lowercase. Matches
/// steve-auth's `origin_of`.
pub fn request_host(headers: &HeaderMap) -> String {
    headers
        .get("x-forwarded-host")
        .or_else(|| headers.get(header::HOST))
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .split(',')
        .next()
        .unwrap_or("")
        .trim()
        .to_lowercase()
}

/// `new URL(u).host`: lowercase host plus a non-default port (steve-auth `url_host`).
fn url_host(u: &Url) -> Option<String> {
    let host = u.host_str()?.to_lowercase();
    Some(match u.port() {
        Some(p) => format!("{host}:{p}"),
        None => host,
    })
}

fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| k.trim() == name)
        .map(|(_, v)| v.trim().trim_matches('"').to_owned())
        .filter(|v| !v.is_empty())
}

/// One relying party: a configured public origin.
struct Rp {
    host: String,
    rp_id: String,
    webauthn: Webauthn,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct RevealKey {
    ws: String,
    user: String,
    item: String,
    field: String,
}

impl From<&RevealTarget> for RevealKey {
    fn from(t: &RevealTarget) -> Self {
        Self {
            ws: t.ws.clone(),
            user: t.user.clone(),
            item: t.item_id.clone(),
            field: t.field_id.clone(),
        }
    }
}

struct PendingReveal {
    rp: usize,
    state: PasskeyAuthentication,
    at: i64,
}

struct PendingReg {
    rp: usize,
    name: String,
    state: PasskeyRegistration,
    at: i64,
}

struct Inner {
    store: Store,
    rps: Vec<Rp>,
    clock: Arc<dyn Clock>,
    /// One open reveal challenge per (workspace, user, item, field); a new start replaces it.
    reveals: Mutex<HashMap<RevealKey, PendingReveal>>,
    /// One open registration per (workspace, user).
    regs: Mutex<HashMap<(String, String), PendingReg>>,
}

/// Passkey registration and the WebAuthn [`RevealGate`]. Cheap to clone.
#[derive(Clone)]
pub struct Passkeys(Arc<Inner>);

/// A registered passkey as the API shows it (no key material).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PasskeyOut {
    pub id: String,
    pub name: String,
    pub rp_id: String,
    pub created: i64,
    pub last_used: Option<i64>,
}

struct Row {
    id: String,
    passkey: Passkey,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// webauthn-rs wants a UUID per user; derived from the user id so it is stable across restarts.
fn user_uuid(user: &str) -> Uuid {
    let d = Sha256::digest(user.as_bytes());
    let mut b = [0u8; 16];
    b.copy_from_slice(&d[..16]);
    Uuid::from_bytes(b)
}

impl Passkeys {
    /// `public_urls` is steve-auth's `AuthConfig::public_urls` (`STEVE_PUBLIC_URLS`).
    pub fn new(store: Store, public_urls: &[String]) -> Self {
        Self::with_clock(store, public_urls, Arc::new(SystemClock))
    }

    pub fn with_clock(store: Store, public_urls: &[String], clock: Arc<dyn Clock>) -> Self {
        let rps = public_urls
            .iter()
            .filter_map(|u| match Self::rp(u) {
                Some(rp) => Some(rp),
                None => {
                    tracing::warn!(origin = %u, "public origin can't be a WebAuthn relying party; skipped");
                    None
                }
            })
            .collect();
        Self(Arc::new(Inner {
            store,
            rps,
            clock,
            reveals: Mutex::default(),
            regs: Mutex::default(),
        }))
    }

    fn rp(u: &str) -> Option<Rp> {
        let origin = Url::parse(u).ok()?;
        let rp_id = origin.domain()?.to_lowercase();
        let host = url_host(&origin)?;
        let webauthn = WebauthnBuilder::new(&rp_id, &origin)
            .ok()?
            .rp_name("Steve")
            .timeout(Duration::from_millis(CHALLENGE_TTL_MS as u64))
            .build()
            .ok()?;
        Some(Rp {
            host,
            rp_id,
            webauthn,
        })
    }

    /// The RP IDs in use, in configuration order.
    pub fn rp_ids(&self) -> Vec<String> {
        self.0.rps.iter().map(|r| r.rp_id.clone()).collect()
    }

    /// The RP for a request host: exact host match, else the first configured one.
    fn rp_for(&self, host: &str) -> VaultResult<usize> {
        if self.0.rps.is_empty() {
            return Err(ApiError::rule(NO_RP).into());
        }
        Ok(self.0.rps.iter().position(|r| r.host == host).unwrap_or(0))
    }

    fn now(&self) -> i64 {
        self.0.clock.now()
    }

    fn live(&self, at: i64) -> bool {
        self.now() - at <= CHALLENGE_TTL_MS
    }

    async fn blocking<R: Send + 'static>(
        &self,
        f: impl FnOnce(&Passkeys) -> VaultResult<R> + Send + 'static,
    ) -> VaultResult<R> {
        let p = self.clone();
        tokio::task::spawn_blocking(move || f(&p))
            .await
            .map_err(|e| VaultError::internal(format!("task failed: {e}")))?
    }

    fn rows(c: &Connection, ws: &str, user: &str, rp_id: &str) -> VaultResult<Vec<Row>> {
        let mut st = c.prepare_cached(
            "SELECT id, credential FROM passkeys WHERE workspace_id = ? AND user_id = ? AND rp_id = ? ORDER BY created",
        )?;
        let raw = st
            .query_map(params![ws, user, rp_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(raw
            .into_iter()
            .filter_map(|(id, cred)| match serde_json::from_str::<Passkey>(&cred) {
                Ok(passkey) => Some(Row { id, passkey }),
                Err(e) => {
                    tracing::warn!(passkey = %id, "unreadable passkey row skipped: {e}");
                    None
                }
            })
            .collect())
    }

    /// Fails with `401 请重新登录后再登记` unless the request's session was created in the last
    /// 10 minutes and belongs to `user`.
    fn check_fresh_login(&self, user: &str, token: Option<&str>) -> VaultResult<()> {
        let Some(token) = token else {
            return Err(VaultError::StaleLogin);
        };
        let now = self.now();
        let created: Option<i64> = self.0.store.reader(|c| {
            c.query_row(
                "SELECT created FROM auth_sessions WHERE token_hash = ? AND user_id = ? AND expires > ?",
                params![steve_store::sha(token), user, now],
                |r| r.get(0),
            )
            .optional()
            .map_err(VaultError::from)
        })?;
        match created {
            Some(t) if now - t <= FRESH_LOGIN_MS => Ok(()),
            _ => Err(VaultError::StaleLogin),
        }
    }

    /// `POST /passkeys/register/start {name}`: `{publicKey: …}` for `navigator.credentials.create`.
    pub fn register_start(
        &self,
        ws: &str,
        user: &str,
        host: &str,
        token: Option<&str>,
        name: &str,
    ) -> VaultResult<Value> {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 60 {
            return Err(ApiError::invalid("name").into());
        }
        self.check_fresh_login(user, token)?;
        let i = self.rp_for(host)?;
        let rp = &self.0.rps[i];
        let (existing, display) = self.0.store.reader(|c| {
            let rows = Self::rows(c, ws, user, &rp.rp_id)?;
            let display: Option<String> = c
                .query_row("SELECT name FROM users WHERE id = ?", [user], |r| r.get(0))
                .optional()?;
            Ok::<_, VaultError>((rows, display.unwrap_or_else(|| user.to_owned())))
        })?;
        let exclude = existing
            .iter()
            .map(|r| r.passkey.cred_id().clone())
            .collect();
        let (ccr, state) = rp
            .webauthn
            .start_passkey_registration(user_uuid(user), user, &display, Some(exclude))
            .map_err(|e| VaultError::internal(format!("webauthn registration start: {e}")))?;
        let at = self.now();
        let mut regs = lock(&self.0.regs);
        regs.retain(|_, p| self.live(p.at));
        regs.insert(
            (ws.to_owned(), user.to_owned()),
            PendingReg {
                rp: i,
                name: name.to_owned(),
                state,
                at,
            },
        );
        serde_json::to_value(ccr).map_err(|e| VaultError::internal(format!("encode: {e}")))
    }

    /// `POST /passkeys/register/finish {credential}`: stores the new passkey for this RP ID.
    pub fn register_finish(
        &self,
        ws: &str,
        user: &str,
        host: &str,
        token: Option<&str>,
        credential: &Value,
    ) -> VaultResult<PasskeyOut> {
        self.check_fresh_login(user, token)?;
        let pending = lock(&self.0.regs).remove(&(ws.to_owned(), user.to_owned()));
        let Some(p) = pending.filter(|p| self.live(p.at)) else {
            return Err(ApiError::rule(REG_GONE).into());
        };
        if self.rp_for(host)? != p.rp {
            return Err(ApiError::rule(OTHER_ORIGIN).into());
        }
        let reg: RegisterPublicKeyCredential = serde_json::from_value(credential.clone())
            .map_err(|_| ApiError::invalid("credential"))?;
        let rp = &self.0.rps[p.rp];
        let passkey = rp
            .webauthn
            .finish_passkey_registration(&reg, &p.state)
            .map_err(|e| {
                tracing::info!(user, rp = %rp.rp_id, "passkey registration refused: {e}");
                VaultError::from(ApiError::rule(REG_FAILED))
            })?;
        let cred = serde_json::to_string(&passkey)
            .map_err(|e| VaultError::internal(format!("encode passkey: {e}")))?;
        let out = PasskeyOut {
            id: uid("pk"),
            name: p.name,
            rp_id: rp.rp_id.clone(),
            created: self.now(),
            last_used: None,
        };
        self.0.store.transaction(|tx| {
            // webauthn-rs: the credential id must not be registered to anyone else already.
            let mut st = tx.prepare_cached("SELECT credential FROM passkeys WHERE rp_id = ?")?;
            let taken = st
                .query_map([&out.rp_id], |r| r.get::<_, String>(0))?
                .filter_map(|r| r.ok())
                .filter_map(|c| serde_json::from_str::<Passkey>(&c).ok())
                .any(|k| k.cred_id() == passkey.cred_id());
            if taken {
                return Err(ApiError::conflict(REG_DUP).into());
            }
            tx.execute(
                "INSERT INTO passkeys (id, workspace_id, user_id, name, rp_id, credential, created) VALUES (?, ?, ?, ?, ?, ?, ?)",
                params![out.id, ws, user, out.name, out.rp_id, cred, out.created],
            )?;
            base::audit(
                tx,
                ws,
                user,
                "passkey.add",
                &out.id,
                &json!({"name": out.name, "rp_id": out.rp_id}),
            )?;
            Ok::<_, VaultError>(())
        })?;
        Ok(out)
    }

    /// `GET /passkeys`: the caller's passkeys in this workspace, every RP ID.
    pub fn list(&self, ws: &str, user: &str) -> VaultResult<Vec<PasskeyOut>> {
        self.0.store.reader(|c| {
            let mut st = c.prepare_cached(
                "SELECT id, name, rp_id, created, last_used FROM passkeys WHERE workspace_id = ? AND user_id = ? ORDER BY created",
            )?;
            let out = st
                .query_map(params![ws, user], |r| {
                    Ok(PasskeyOut {
                        id: r.get(0)?,
                        name: r.get(1)?,
                        rp_id: r.get(2)?,
                        created: r.get(3)?,
                        last_used: r.get(4)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(out)
        })
    }

    /// `DELETE /passkeys/:id`: only the caller's own; anything else is `404 没有这个通行密钥`.
    pub fn delete(&self, ws: &str, user: &str, id: &str) -> VaultResult<()> {
        self.0.store.transaction(|tx| {
            let n = tx.execute(
                "DELETE FROM passkeys WHERE id = ? AND workspace_id = ? AND user_id = ?",
                params![id, ws, user],
            )?;
            if n == 0 {
                return Err(ApiError::not_found("通行密钥").into());
            }
            base::audit(tx, ws, user, "passkey.delete", id, &json!({}))?;
            Ok(())
        })
    }

    fn reveal_start(&self, t: &RevealTarget) -> VaultResult<Value> {
        let i = self.rp_for(&t.host)?;
        let rp = &self.0.rps[i];
        let rows = self
            .0
            .store
            .reader(|c| Self::rows(c, &t.ws, &t.user, &rp.rp_id))?;
        if rows.is_empty() {
            return Err(ApiError::rule(NEED_PASSKEY).into());
        }
        let keys: Vec<Passkey> = rows.into_iter().map(|r| r.passkey).collect();
        let (rcr, state) = rp
            .webauthn
            .start_passkey_authentication(&keys)
            .map_err(|e| VaultError::internal(format!("webauthn reveal start: {e}")))?;
        let at = self.now();
        let mut pending = lock(&self.0.reveals);
        pending.retain(|_, p| self.live(p.at));
        pending.insert(RevealKey::from(t), PendingReveal { rp: i, state, at });
        serde_json::to_value(rcr).map_err(|e| VaultError::internal(format!("encode: {e}")))
    }

    fn reveal_finish(&self, t: &RevealTarget, assertion: &Value) -> VaultResult<Verified> {
        // Taken out first: a challenge is answered once, whether or not the answer is good.
        let pending = lock(&self.0.reveals).remove(&RevealKey::from(t));
        let Some(p) = pending.filter(|p| self.live(p.at)) else {
            return Err(ApiError::rule(CHALLENGE_GONE).into());
        };
        if self.rp_for(&t.host)? != p.rp {
            return Err(ApiError::rule(OTHER_ORIGIN).into());
        }
        let cred: PublicKeyCredential = serde_json::from_value(assertion.clone())
            .map_err(|_| ApiError::invalid("assertion"))?;
        let rp = &self.0.rps[p.rp];
        let res = rp
            .webauthn
            .finish_passkey_authentication(&cred, &p.state)
            .map_err(|e| {
                tracing::info!(user = %t.user, rp = %rp.rp_id, "passkey assertion refused: {e}");
                VaultError::from(ApiError::rule(NOT_VERIFIED))
            })?;
        if !res.user_verified() {
            return Err(ApiError::rule(NOT_VERIFIED).into());
        }
        let now = self.now();
        self.0.store.transaction(|tx| {
            let mut rows = Self::rows(tx, &t.ws, &t.user, &rp.rp_id)?;
            let Some(row) = rows.iter_mut().find(|r| r.passkey.cred_id() == res.cred_id()) else {
                // deleted between start and finish
                return Err(ApiError::rule(NOT_VERIFIED).into());
            };
            let cred = match row.passkey.update_credential(&res) {
                Some(true) => Some(
                    serde_json::to_string(&row.passkey)
                        .map_err(|e| VaultError::internal(format!("encode passkey: {e}")))?,
                ),
                _ => None,
            };
            tx.execute(
                "UPDATE passkeys SET last_used = ?, credential = COALESCE(?, credential) WHERE id = ?",
                params![now, cred, row.id],
            )?;
            Ok::<_, VaultError>(())
        })?;
        Ok(Verified::new(t.clone()))
    }
}

#[async_trait]
impl RevealGate for Passkeys {
    async fn start(&self, target: &RevealTarget) -> VaultResult<Value> {
        let t = target.clone();
        self.blocking(move |p| p.reveal_start(&t)).await
    }

    async fn finish(&self, target: &RevealTarget, assertion: &Value) -> VaultResult<Verified> {
        let (t, a) = (target.clone(), assertion.clone());
        self.blocking(move |p| p.reveal_finish(&t, &a)).await
    }
}

/// The passkey routes (§5.2). Merge under `/api` after `require_user`.
pub fn routes<S: Clone + Send + Sync + 'static>(pk: Passkeys) -> Router<S> {
    Router::new()
        .route("/passkeys", get(list))
        .route("/passkeys/register/start", post(register_start))
        .route("/passkeys/register/finish", post(register_finish))
        .route("/passkeys/{id}", delete(remove))
        .with_state(pk)
}

fn no_store(v: Value) -> Response {
    ([(header::CACHE_CONTROL, "no-store")], Json(v)).into_response()
}

async fn list(State(pk): State<Passkeys>, who: Caller) -> VaultResult<Response> {
    let out = pk.blocking(move |p| p.list(&who.ws, &who.me)).await?;
    Ok(Json(out).into_response())
}

#[derive(Deserialize)]
struct StartIn {
    name: String,
}

async fn register_start(
    State(pk): State<Passkeys>,
    who: Caller,
    headers: HeaderMap,
    Body(b): Body<StartIn>,
) -> VaultResult<Response> {
    let host = request_host(&headers);
    let token = cookie(&headers, SESSION_COOKIE);
    let out = pk
        .blocking(move |p| p.register_start(&who.ws, &who.me, &host, token.as_deref(), &b.name))
        .await?;
    Ok(no_store(out))
}

#[derive(Deserialize)]
struct FinishIn {
    #[serde(default)]
    credential: Value,
}

async fn register_finish(
    State(pk): State<Passkeys>,
    who: Caller,
    headers: HeaderMap,
    Body(b): Body<FinishIn>,
) -> VaultResult<Response> {
    let host = request_host(&headers);
    let token = cookie(&headers, SESSION_COOKIE);
    let out = pk
        .blocking(move |p| {
            p.register_finish(&who.ws, &who.me, &host, token.as_deref(), &b.credential)
        })
        .await?;
    Ok(Json(out).into_response())
}

async fn remove(
    State(pk): State<Passkeys>,
    who: Caller,
    Path(id): Path<String>,
) -> VaultResult<Response> {
    pk.blocking(move |p| p.delete(&who.ws, &who.me, &id))
        .await?;
    Ok(Json(json!({"ok": true})).into_response())
}

#[cfg(test)]
mod tests;
