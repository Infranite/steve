//! Key lifecycle: init, recover, rotate with background re-wrapping, and the
//! data-key calls slice V2 builds on. Synchronous (store and key store both block); the API runs it
//! with [`Vault::blocking`].

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use serde_json::json;
use steve_store::{Store, Tx, base, now, uid};

use super::crypto::{self, fingerprint, recovery_code, unwrap_dek, wrap_dek};
use super::error::{VaultError, VaultResult};
use super::keystore::{KeyStore, KeyStoreError, account_name};
use super::repo::{self, ACTIVE, RETIRED, RETIRING};
use super::secret::{Dek, Kek, RecoveryCode};

/// Items re-wrapped per transaction.
const BATCH: usize = 100;

/// The vault service. Cheap to clone; one per Hub process (the KEK cache lives here).
#[derive(Clone)]
pub struct Vault(Arc<Inner>);

struct Inner {
    store: Store,
    keys: Arc<dyn KeyStore>,
    /// `(workspace, version) → KEK`, filled on first use, gone when the process exits.
    cache: Mutex<HashMap<(String, i64), Arc<Kek>>>,
    /// Serializes init / recover / starting a rotation, so two requests can't write the same entry.
    lifecycle: Mutex<()>,
    /// Workspaces whose data keys are being re-wrapped right now.
    rewrapping: Mutex<HashSet<String>>,
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

/// What `POST /vault/rotate` returns. The new recovery code replaces the old one, which stops working
/// once the old version is retired.
#[derive(Debug)]
pub struct Rotation {
    pub job_id: String,
    pub version: i64,
    pub recovery_code: RecoveryCode,
}

/// Outcome of one re-wrap pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RewrapReport {
    pub rewrapped: usize,
    /// Items whose DEK did not open; their old version stays `retiring` (and in the key store).
    pub failed: Vec<String>,
    /// Versions moved to `retired` and deleted from the key store.
    pub retired: Vec<i64>,
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

/// Removes the workspace from `rewrapping` when a pass ends, even on error.
struct RewrapGuard<'a>(&'a Vault, String);

impl Drop for RewrapGuard<'_> {
    fn drop(&mut self) {
        lock(&self.0.0.rewrapping).remove(&self.1);
    }
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

impl Vault {
    pub fn new(store: Store, keys: Arc<dyn KeyStore>) -> Self {
        Self(Arc::new(Inner {
            store,
            keys,
            cache: Mutex::default(),
            lifecycle: Mutex::new(()),
            rewrapping: Mutex::default(),
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
        let kek = self.create_key(ws, 1, |tx, fp| {
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

    /// First half of `POST /vault/rotate`: a new active version, the old one `retiring`. The caller
    /// then runs [`Vault::rewrap`] (in the background, see [`Vault::rotate`]).
    pub fn begin_rotation(&self, ws: &str, actor: &str) -> VaultResult<Rotation> {
        let _g = lock(&self.0.lifecycle);
        if lock(&self.0.rewrapping).contains(ws) {
            return Err(VaultError::Rotating);
        }
        let rows = self.0.store.reader(|c| repo::keys(c, ws))?;
        if rows.iter().any(|r| r.status == RETIRING) {
            return Err(VaultError::Rotating);
        }
        let active = rows
            .iter()
            .rev()
            .find(|r| r.status == ACTIVE)
            .ok_or(VaultError::NotInitialized)?;
        // the old key must be readable, or the items it wraps could never be re-wrapped
        self.kek(ws, active.version)?;
        let next = rows.iter().map(|r| r.version).max().unwrap_or(0) + 1;
        let job_id = uid("job");
        let kek = self.create_key(ws, next, |tx, fp| {
            repo::set_key_status(tx, ws, active.version, RETIRING, None)?;
            repo::insert_key(tx, ws, next, &account_name(ws, next), fp, ACTIVE, now())?;
            base::audit(
                tx,
                ws,
                actor,
                "vault.rotate",
                ws,
                &json!({ "from": active.version, "to": next, "job_id": job_id, "fingerprint": fp }),
            )?;
            Ok(())
        })?;
        tracing::info!(
            ws,
            from = active.version,
            to = next,
            job_id,
            "vault rotation started"
        );
        Ok(Rotation {
            job_id,
            version: next,
            recovery_code: recovery_code(&kek),
        })
    }

    /// `POST /vault/rotate`: [`Vault::begin_rotation`], then re-wraps in a background task.
    pub async fn rotate(&self, ws: &str, actor: &str) -> VaultResult<Rotation> {
        let (w, a) = (ws.to_string(), actor.to_string());
        let r = self.blocking(move |v| v.begin_rotation(&w, &a)).await?;
        self.spawn_rewrap(ws.to_string(), r.job_id.clone());
        Ok(r)
    }

    /// Resumes interrupted rotations; the integrator calls it once at startup.
    pub fn resume_rotations(&self) -> VaultResult<()> {
        for ws in self.0.store.reader(repo::rotating_workspaces)? {
            self.spawn_rewrap(ws, "resume".into());
        }
        Ok(())
    }

    fn spawn_rewrap(&self, ws: String, job_id: String) {
        let v = self.clone();
        tokio::task::spawn_blocking(move || match v.rewrap(&ws) {
            Ok(r) => tracing::info!(
                ws,
                job_id,
                rewrapped = r.rewrapped,
                failed = r.failed.len(),
                retired = ?r.retired,
                "vault rewrap finished"
            ),
            Err(e) => tracing::warn!(ws, job_id, error = %e, "vault rewrap stopped"),
        });
    }

    /// `vault.rewrap`: re-wraps every DEK still under a `retiring` version with the active KEK,
    /// [`BATCH`] items per transaction; when none is left, the old versions become `retired` and are
    /// deleted from the key store. Safe to rerun.
    pub fn rewrap(&self, ws: &str) -> VaultResult<RewrapReport> {
        if !lock(&self.0.rewrapping).insert(ws.to_string()) {
            return Err(VaultError::Rotating);
        }
        let _g = RewrapGuard(self, ws.to_string());
        let rows = self.0.store.reader(|c| repo::keys(c, ws))?;
        let mut report = RewrapReport::default();
        let retiring: Vec<_> = rows.iter().filter(|r| r.status == RETIRING).collect();
        if retiring.is_empty() {
            return Ok(report);
        }
        let active = rows
            .iter()
            .rev()
            .find(|r| r.status == ACTIVE)
            .ok_or(VaultError::NotInitialized)?;
        let new = self.kek(ws, active.version)?;
        let mut old: HashMap<i64, Arc<Kek>> = HashMap::new();
        for r in &retiring {
            old.insert(r.version, self.kek(ws, r.version)?);
        }
        loop {
            let n = self.0.store.transaction(|tx| {
                let batch = repo::items_to_rewrap(tx, ws, active.version, &report.failed, BATCH)?;
                for it in &batch {
                    let dek = old
                        .get(&it.kek_version)
                        .ok_or(crypto::CryptoError::Open)
                        .and_then(|k| {
                            unwrap_dek(k, ws, &it.item_id, it.kek_version, &it.dek_wrapped)
                        });
                    match dek {
                        Ok(dek) => {
                            let w = wrap_dek(&new, ws, &it.item_id, active.version, &dek)?;
                            repo::set_item_dek(tx, &it.item_id, &w, active.version)?;
                            report.rewrapped += 1;
                        }
                        Err(_) => {
                            tracing::warn!(ws, item = %it.item_id, version = it.kek_version, "vault rewrap: data key did not open");
                            report.failed.push(it.item_id.clone());
                        }
                    }
                }
                Ok::<_, VaultError>(batch.len())
            })?;
            if n == 0 {
                break;
            }
        }
        if !report.failed.is_empty() {
            return Ok(report);
        }
        let retired = self.0.store.transaction(|tx| {
            let mut done = Vec::new();
            for r in &retiring {
                // an item wrapped under the old version after the loop (a racing insert) keeps it alive
                if repo::count_on_version(tx, ws, r.version)? == 0 {
                    repo::set_key_status(tx, ws, r.version, RETIRED, Some(now()))?;
                    base::audit(
                        tx,
                        ws,
                        "steve",
                        "vault.retire",
                        ws,
                        &json!({ "version": r.version }),
                    )?;
                    done.push(*r);
                }
            }
            Ok::<_, VaultError>(done)
        })?;
        for r in &retired {
            if let Err(e) = self.0.keys.delete(&r.keychain_account) {
                tracing::warn!(ws, version = r.version, error = %e, "vault: retired key not deleted from key store");
            }
            lock(&self.0.cache).remove(&(ws.to_string(), r.version));
            report.retired.push(r.version);
        }
        Ok(report)
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
        record: impl FnOnce(&Tx, &str) -> steve_store::Result<()>,
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
        let res = self
            .0
            .store
            .transaction(|tx| record(tx, &fp).map_err(VaultError::from));
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
