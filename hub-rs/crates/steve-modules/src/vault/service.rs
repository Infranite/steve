//! Key lifecycle: init, recover, rotate (one transaction re-wraps every data key), and the
//! data-key calls slice V2 builds on. Synchronous (store and key store both block); the API runs it
//! with [`Vault::blocking`].

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use serde_json::json;
use steve_store::{Store, Tx, base, now};

use super::crypto::{self, fingerprint, recovery_code, unwrap_dek, wrap_dek};
use super::error::{VaultError, VaultResult};
use super::keystore::{KeyStore, KeyStoreError, account_name};
use super::repo::{self, ACTIVE, RETIRED, RETIRING};
use super::secret::{Dek, Kek, RecoveryCode};

/// The vault service. Cheap to clone; one per Hub process (the KEK cache lives here).
#[derive(Clone)]
pub struct Vault(Arc<Inner>);

struct Inner {
    store: Store,
    keys: Arc<dyn KeyStore>,
    /// `(workspace, version) → KEK`, filled on first use, gone when the process exits.
    cache: Mutex<HashMap<(String, i64), Arc<Kek>>>,
    /// Serializes init / recover / rotate / tidy, so two requests can't write the same entry.
    lifecycle: Mutex<()>,
}

/// `GET /vault/status` keychain state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Keychain {
    Ok,
    Locked,
    /// Not initialized, or the active key is not in the key store (recover with the code).
    Missing,
    Denied,
}

/// `GET /vault/status`: which store holds the master keys.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KeyStoreInfo {
    /// `keychain`, `secret-service`, `dir` or `file` ([`KeyStore::kind`]).
    pub kind: &'static str,
    /// For people: `macOS 钥匙串`, `Linux 密钥环（Secret Service）`, `密钥目录 /srv/steve-keys`, …
    pub label: String,
}

/// `GET /vault/status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Status {
    pub initialized: bool,
    /// The state of the active key in the store (named after the macOS keychain, for every store).
    pub keychain: Keychain,
    pub keystore: KeyStoreInfo,
    pub kek_version: Option<i64>,
    /// Passkeys registered in this workspace (any user, any RP ID).
    pub passkeys: i64,
    /// A rotation is re-wrapping data keys.
    pub rotating: bool,
}

/// What `POST /vault/rotate` returns. The new recovery code replaces the old one, which stopped
/// working with the old version.
#[derive(Debug)]
pub struct Rotation {
    pub version: i64,
    pub recovery_code: RecoveryCode,
}

/// A new item's data key, wrapped under the active KEK.
#[derive(Debug)]
pub struct NewItemKey {
    pub dek: Dek,
    /// For `vault_items.dek_wrapped`.
    pub wrapped: Vec<u8>,
    /// For `vault_items.kek_version`; check it with [`ensure_active`] in the inserting transaction.
    pub kek_version: i64,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Fails with `Rotating` if `version` is no longer the active one. Slice V2 calls it inside the
/// transaction that inserts an item wrapped with [`Vault::new_item_key`], so an item can't land on a
/// version a concurrent rotation is about to retire.
pub fn ensure_active(tx: &Tx, ws: &str, version: i64) -> VaultResult<()> {
    match repo::active_version(tx, ws)? {
        Some(v) if v == version => Ok(()),
        Some(_) => Err(VaultError::Rotating),
        None => Err(VaultError::NotInitialized),
    }
}

/// Re-wraps, under `new` (version `target`), every data key of `ws` that another version wraps,
/// opening each with its version's KEK from `old`. Returns how many moved and the items whose key did
/// not open (they keep their old version).
fn rewrap_all(
    tx: &Tx,
    ws: &str,
    old: &HashMap<i64, Arc<Kek>>,
    new: &Kek,
    target: i64,
) -> VaultResult<(usize, Vec<String>)> {
    let mut failed = Vec::new();
    let mut moved = 0;
    for it in repo::items_to_rewrap(tx, ws, target, &[], i64::MAX as usize)? {
        let dek = old
            .get(&it.kek_version)
            .ok_or(crypto::CryptoError::Open)
            .and_then(|k| unwrap_dek(k, ws, &it.item_id, it.kek_version, &it.dek_wrapped));
        match dek {
            Ok(dek) => {
                let w = wrap_dek(new, ws, &it.item_id, target, &dek)?;
                repo::set_item_dek(tx, &it.item_id, &w, target)?;
                moved += 1;
            }
            Err(_) => {
                tracing::warn!(ws, item = %it.item_id, version = it.kek_version, "vault: data key did not open");
                failed.push(it.item_id);
            }
        }
    }
    Ok((moved, failed))
}

impl Vault {
    pub fn new(store: Store, keys: Arc<dyn KeyStore>) -> Self {
        Self(Arc::new(Inner {
            store,
            keys,
            cache: Mutex::default(),
            lifecycle: Mutex::new(()),
        }))
    }

    pub fn store(&self) -> &Store {
        &self.0.store
    }

    /// Runs synchronous vault work off the async runtime.
    pub async fn blocking<R: Send + 'static>(
        &self,
        f: impl FnOnce(&Vault) -> VaultResult<R> + Send + 'static,
    ) -> VaultResult<R> {
        let v = self.clone();
        tokio::task::spawn_blocking(steve_i18n::carry(move || f(&v)))
            .await
            .map_err(|e| VaultError::internal(format!("task failed: {e}")))?
    }

    /// Which store holds the master keys.
    pub fn keystore(&self) -> KeyStoreInfo {
        KeyStoreInfo {
            kind: self.0.keys.kind(),
            label: self.0.keys.label(),
        }
    }

    pub fn status(&self, ws: &str) -> VaultResult<Status> {
        let rows = self.0.store.reader(|c| repo::keys(c, ws))?;
        let rotating = rows.iter().any(|r| r.status == RETIRING);
        let Some(active) = rows.iter().rev().find(|r| r.status == ACTIVE) else {
            return Ok(Status {
                initialized: false,
                keychain: Keychain::Missing,
                keystore: self.keystore(),
                kek_version: None,
                passkeys: 0,
                rotating,
            });
        };
        let keychain = match self.kek(ws, active.version) {
            Ok(_) => Keychain::Ok,
            Err(VaultError::Locked) => Keychain::Locked,
            Err(VaultError::KeyMissing) => Keychain::Missing,
            Err(VaultError::Denied) => Keychain::Denied,
            Err(e) => return Err(e),
        };
        let passkeys = self.0.store.reader(|c| {
            c.query_row(
                "SELECT count(*) FROM passkeys WHERE workspace_id = ?",
                [ws],
                |r| r.get::<_, i64>(0),
            )
            .map_err(VaultError::from)
        })?;
        Ok(Status {
            initialized: true,
            keychain,
            keystore: self.keystore(),
            kek_version: Some(active.version),
            passkeys,
            rotating,
        })
    }

    /// `POST /vault/init`: KEK v1 into the key store, its fingerprint into `vault_keys`; returns the
    /// recovery code (show once, never store).
    pub fn init(&self, ws: &str, actor: &str) -> VaultResult<RecoveryCode> {
        let _g = lock(&self.0.lifecycle);
        let rows = self.0.store.reader(|c| repo::keys(c, ws))?;
        if !rows.is_empty() {
            return Err(VaultError::AlreadyInitialized);
        }
        let kek = self.create_key(ws, 1, |tx, fp, _| {
            repo::insert_key(tx, ws, 1, &account_name(ws, 1), fp, ACTIVE, now())?;
            base::audit(
                tx,
                ws,
                actor,
                "vault.init",
                ws,
                &json!({ "version": 1, "fingerprint": fp }),
            )?;
            Ok(())
        })?;
        tracing::info!(ws, version = 1, "vault initialized");
        Ok(recovery_code(&kek))
    }

    /// `POST /vault/recover {code}`: writes the key back to the key store if its fingerprint matches a
    /// version in `vault_keys`. Returns that version.
    pub fn recover(&self, ws: &str, code: &str, actor: &str) -> VaultResult<i64> {
        let _g = lock(&self.0.lifecycle);
        let rows = self.0.store.reader(|c| repo::keys(c, ws))?;
        if rows.is_empty() {
            return Err(VaultError::NotInitialized);
        }
        let kek = crypto::parse_recovery_code(code).ok_or(VaultError::BadRecoveryCode)?;
        let fp = fingerprint(&kek);
        let row = rows
            .iter()
            .find(|r| r.fingerprint == fp && r.status != RETIRED)
            .ok_or(VaultError::BadRecoveryCode)?;
        match self.0.keys.get(&row.keychain_account) {
            Ok(Some(k)) if fingerprint(&k) == fp => {}
            Ok(Some(_)) => return Err(VaultError::KeyExists),
            Ok(None) | Err(KeyStoreError::Malformed) => {
                self.0.keys.set(&row.keychain_account, &kek)?;
            }
            Err(e) => return Err(e.into()),
        }
        self.0.store.transaction(|tx| {
            base::audit(
                tx,
                ws,
                actor,
                "vault.recover",
                ws,
                &json!({ "version": row.version }),
            )?;
            Ok::<_, VaultError>(())
        })?;
        lock(&self.0.cache).insert((ws.to_string(), row.version), Arc::new(kek));
        tracing::info!(ws, version = row.version, "vault key recovered");
        Ok(row.version)
    }

    /// `POST /vault/rotate`: a new KEK version. In one transaction every data key is re-wrapped
    /// under it and the old versions are retired; once that committed, their key store entries go. A
    /// data key that does not open refuses the whole rotation, and nothing changes.
    pub fn rotate_now(&self, ws: &str, actor: &str) -> VaultResult<Rotation> {
        let _g = lock(&self.0.lifecycle);
        let rows = self.0.store.reader(|c| repo::keys(c, ws))?;
        let active = rows
            .iter()
            .rev()
            .find(|r| r.status == ACTIVE)
            .ok_or(VaultError::NotInitialized)?
            .version;
        // every version that may still wrap a data key must be readable (an old background rotation
        // may have left one `retiring`)
        let live: Vec<&repo::KeyRow> = rows.iter().filter(|r| r.status != RETIRED).collect();
        let mut old: HashMap<i64, Arc<Kek>> = HashMap::new();
        for r in &live {
            old.insert(r.version, self.kek(ws, r.version)?);
        }
        let next = rows.iter().map(|r| r.version).max().unwrap_or(0) + 1;
        let kek = self.create_key(ws, next, |tx, fp, new| {
            for r in &live {
                repo::set_key_status(tx, ws, r.version, RETIRED, Some(now()))?;
            }
            repo::insert_key(tx, ws, next, &account_name(ws, next), fp, ACTIVE, now())?;
            let (rewrapped, failed) = rewrap_all(tx, ws, &old, new, next)?;
            if !failed.is_empty() {
                return Err(VaultError::Api(crate::ApiError::rule(steve_i18n::t!(
                    "modules.vault.rotate_unreadable",
                    n = failed.len()
                ))));
            }
            base::audit(
                tx,
                ws,
                actor,
                "vault.rotate",
                ws,
                &json!({ "from": active, "to": next, "rewrapped": rewrapped, "fingerprint": fp }),
            )?;
            Ok(())
        })?;
        tracing::info!(ws, from = active, to = next, "vault key rotated");
        // committed: the old entries (and any a crash left of earlier ones) leave the key store
        let retired: Vec<repo::KeyRow> = self
            .0
            .store
            .reader(|c| repo::keys(c, ws))?
            .into_iter()
            .filter(|r| r.status == RETIRED)
            .collect();
        self.forget(ws, &retired);
        Ok(Rotation {
            version: next,
            recovery_code: recovery_code(&kek),
        })
    }

    /// `POST /vault/rotate` off the async runtime.
    pub async fn rotate(&self, ws: &str, actor: &str) -> VaultResult<Rotation> {
        let (w, a) = (ws.to_string(), actor.to_string());
        self.blocking(move |v| v.rotate_now(&w, &a)).await
    }

    /// At startup: finishes what a rotation of an earlier Hub (a background re-wrap) left `retiring`,
    /// in one transaction per workspace, and removes retired versions from the key store. A workspace
    /// whose keys can't be read now (a locked keychain) is left for the next start.
    pub fn tidy_keys(&self) -> VaultResult<()> {
        let _g = lock(&self.0.lifecycle);
        for ws in self.0.store.reader(repo::rotating_workspaces)? {
            if let Err(e) = self.finish_retiring(&ws) {
                tracing::warn!(ws, error = %e, "vault: an interrupted rotation stays for the next start");
            }
        }
        Ok(())
    }

    fn finish_retiring(&self, ws: &str) -> VaultResult<()> {
        let rows = self.0.store.reader(|c| repo::keys(c, ws))?;
        let active = rows
            .iter()
            .rev()
            .find(|r| r.status == ACTIVE)
            .ok_or(VaultError::NotInitialized)?
            .version;
        let retiring: Vec<&repo::KeyRow> = rows.iter().filter(|r| r.status == RETIRING).collect();
        let new = self.kek(ws, active)?;
        let mut old: HashMap<i64, Arc<Kek>> = HashMap::new();
        for r in &retiring {
            old.insert(r.version, self.kek(ws, r.version)?);
        }
        let failed = self.0.store.transaction(|tx| {
            let (rewrapped, failed) = rewrap_all(tx, ws, &old, &new, active)?;
            if failed.is_empty() {
                for r in &retiring {
                    repo::set_key_status(tx, ws, r.version, RETIRED, Some(now()))?;
                    base::audit(
                        tx,
                        ws,
                        "steve",
                        "vault.retire",
                        ws,
                        &json!({ "version": r.version }),
                    )?;
                }
            } else {
                // what opened is re-wrapped; the old versions stay until the rest is destroyed
                tracing::warn!(
                    ws,
                    rewrapped,
                    failed = failed.len(),
                    "vault: data keys that did not open keep their old version"
                );
            }
            Ok::<_, VaultError>(failed)
        })?;
        if failed.is_empty() {
            let retired: Vec<repo::KeyRow> = retiring.into_iter().cloned().collect();
            self.forget(ws, &retired);
        }
        Ok(())
    }

    /// Removes retired versions from the key store and the cache (a missing entry is fine).
    fn forget(&self, ws: &str, retired: &[repo::KeyRow]) {
        for r in retired {
            if let Err(e) = self.0.keys.delete(&r.keychain_account) {
                tracing::warn!(ws, version = r.version, error = %e, "vault: retired key not deleted from key store");
            }
            lock(&self.0.cache).remove(&(ws.to_string(), r.version));
        }
    }

    /// A data key for a new item, wrapped under the active KEK (slice V2).
    pub fn new_item_key(&self, ws: &str, item_id: &str) -> VaultResult<NewItemKey> {
        let version = self
            .0
            .store
            .reader(|c| repo::active_version(c, ws))?
            .ok_or(VaultError::NotInitialized)?;
        let kek = self.kek(ws, version)?;
        let dek = Dek::generate()?;
        let wrapped = wrap_dek(&kek, ws, item_id, version, &dek)?;
        Ok(NewItemKey {
            dek,
            wrapped,
            kek_version: version,
        })
    }

    /// Opens an item's data key (slice V2: before reading or writing its secret fields).
    pub fn item_key(
        &self,
        ws: &str,
        item_id: &str,
        wrapped: &[u8],
        kek_version: i64,
    ) -> VaultResult<Dek> {
        let kek = self.kek(ws, kek_version)?;
        Ok(unwrap_dek(&kek, ws, item_id, kek_version, wrapped)?)
    }

    /// The KEK of a version: cache, else the key store (checked against the recorded fingerprint).
    fn kek(&self, ws: &str, version: i64) -> VaultResult<Arc<Kek>> {
        if let Some(k) = lock(&self.0.cache).get(&(ws.to_string(), version)) {
            return Ok(k.clone());
        }
        let rows = self.0.store.reader(|c| repo::keys(c, ws))?;
        let row = rows
            .iter()
            .find(|r| r.version == version && r.status != RETIRED)
            .ok_or(VaultError::NotInitialized)?;
        let kek = match self.0.keys.get(&row.keychain_account) {
            Ok(Some(k)) => k,
            Ok(None) => return Err(VaultError::KeyMissing),
            Err(KeyStoreError::Malformed) => return Err(VaultError::KeyMissing),
            Err(e) => return Err(e.into()),
        };
        if fingerprint(&kek) != row.fingerprint {
            tracing::warn!(
                ws,
                version,
                "vault: key store entry does not match the recorded fingerprint"
            );
            return Err(VaultError::KeyMissing);
        }
        let kek = Arc::new(kek);
        lock(&self.0.cache).insert((ws.to_string(), version), kek.clone());
        Ok(kek)
    }

    /// Generates a KEK for `version`, stores it (refusing to overwrite an existing entry), then runs
    /// `record` in a transaction with the fingerprint; on failure the new entry is removed again.
    fn create_key(
        &self,
        ws: &str,
        version: i64,
        record: impl FnOnce(&Tx, &str, &Kek) -> VaultResult<()>,
    ) -> VaultResult<Kek> {
        let account = account_name(ws, version);
        match self.0.keys.get(&account) {
            Ok(None) => {}
            Ok(Some(_)) | Err(KeyStoreError::Malformed) => return Err(VaultError::KeyExists),
            Err(e) => return Err(e.into()),
        }
        let kek = Kek::generate()?;
        self.0.keys.set(&account, &kek)?;
        let fp = fingerprint(&kek);
        let res = self.0.store.transaction(|tx| record(tx, &fp, &kek));
        if let Err(e) = res {
            if let Err(d) = self.0.keys.delete(&account) {
                tracing::warn!(ws, version, error = %d, "vault: could not remove an unrecorded key");
            }
            return Err(e);
        }
        let dup = Kek::from_slice(kek.bytes()).ok_or(crypto::CryptoError::Open)?;
        lock(&self.0.cache).insert((ws.to_string(), version), Arc::new(dup));
        Ok(kek)
    }
}
