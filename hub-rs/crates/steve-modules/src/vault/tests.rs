//! V1 acceptance tests: lifecycle, rotation, the key store guard, API. The no-leak rule
//! (no secret in logs, errors, the database file) is `tests/vault_secrets.rs`, a binary of its own so its
//! log capture sees every callsite. Key stores are files in temp dirs; the
//! real keychain is only used by the ignored test in `keystore/keychain.rs`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::Router;
use axum::body::Body as HttpBody;
use axum::http::Request;
use rusqlite::params;
use serde_json::Value;
use steve_store::now;
use tempfile::TempDir;
use tower::ServiceExt;

use super::crypto::{open_field, seal_field};
use super::keystore::{FileKeyStore, KeyStore, KeyStoreError, account_name};
use super::secret::Kek;
use super::*;
use crate::testkit::{ME, TestEnv, WS, WS2, caller};

/// Until M0 lands migration 005: the draft tables, after a stub `accounts` (vault_items references it).
const DRAFT: &str = include_str!("../../docs/v1-schema-draft.sql");

/// A file key store whose reads can be made to fail like a locked keychain.
struct Lockable {
    inner: FileKeyStore,
    locked: AtomicBool,
}

impl KeyStore for Lockable {
    fn kind(&self) -> &'static str {
        "file"
    }
    fn get(&self, a: &str) -> Result<Option<Kek>, KeyStoreError> {
        if self.locked.load(Ordering::SeqCst) {
            return Err(KeyStoreError::Locked);
        }
        self.inner.get(a)
    }
    fn set(&self, a: &str, k: &Kek) -> Result<(), KeyStoreError> {
        if self.locked.load(Ordering::SeqCst) {
            return Err(KeyStoreError::Locked);
        }
        self.inner.set(a, k)
    }
    fn delete(&self, a: &str) -> Result<(), KeyStoreError> {
        self.inner.delete(a)
    }
}

struct Env {
    env: TestEnv,
    _keydir: TempDir,
    ks: Arc<Lockable>,
    vault: Vault,
}

fn setup() -> Env {
    let env = TestEnv::new(|_| {});
    env.store()
        .with_conn(|c| {
            c.execute_batch("CREATE TABLE IF NOT EXISTS accounts (id TEXT PRIMARY KEY);")?;
            c.execute_batch(DRAFT)?;
            c.execute(
                "INSERT INTO vaults (id, workspace_id, name, scope, created) VALUES ('vlt_s', ?, '共享', 'shared', 1), ('vlt_sys', ?, '系统', 'system', 1)",
                [WS, WS],
            )?;
            Ok(())
        })
        .expect("schema");
    let keydir = tempfile::tempdir().expect("tmp");
    let ks = Arc::new(Lockable {
        inner: FileKeyStore::open(keydir.path()).expect("keys"),
        locked: AtomicBool::new(false),
    });
    let vault = Vault::new(env.store().clone(), ks.clone());
    Env {
        env,
        _keydir: keydir,
        ks,
        vault,
    }
}

impl Env {
    /// A fresh `Vault` on the same store and key store (a Hub restart: empty KEK cache).
    fn restart(&self) -> Vault {
        Vault::new(self.env.store().clone(), self.ks.clone())
    }

    /// Inserts an item with one secret field the way slice V2 will; returns (item, field).
    fn item(&self, v: &Vault, secret: &str) -> (String, String) {
        let item = steve_store::uid("vi");
        let field = steve_store::uid("vf");
        let key = v.new_item_key(WS, &item).expect("item key");
        let ct = seal_field(&key.dek, WS, &item, &field, secret.as_bytes()).expect("seal");
        self.env
            .store()
            .transaction(|tx| {
                ensure_active(tx, WS, key.kek_version)?;
                tx.execute(
                    "INSERT INTO vault_items (id, workspace_id, vault_id, template, title, sensitivity, dek_wrapped, kek_version, created_by, created, updated)
                     VALUES (?, ?, 'vlt_s', 'login', 'GitHub', 'sensitive', ?, ?, ?, ?, ?)",
                    params![item, WS, key.wrapped, key.kek_version, ME, now(), now()],
                )?;
                tx.execute(
                    "INSERT INTO vault_fields (id, workspace_id, item_id, label, type, secret, value_ct, updated)
                     VALUES (?, ?, ?, '密码', 'password', 1, ?, ?)",
                    params![field, WS, item, ct, now()],
                )?;
                Ok::<_, VaultError>(())
            })
            .expect("insert item");
        (item, field)
    }

    /// Decrypts a field like reveal/finish will.
    fn read(&self, v: &Vault, item: &str, field: &str) -> VaultResult<String> {
        let (wrapped, ver, ct): (Vec<u8>, i64, Vec<u8>) = self
            .env
            .store()
            .with_conn(|c| {
                Ok(c.query_row(
                    "SELECT i.dek_wrapped, i.kek_version, f.value_ct FROM vault_items i JOIN vault_fields f ON f.item_id = i.id WHERE i.id = ? AND f.id = ?",
                    [item, field],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )?)
            })
            .expect("row");
        let dek = v.item_key(WS, item, &wrapped, ver)?;
        let p = open_field(&dek, WS, item, field, &ct)?;
        Ok(String::from_utf8(p.expose().to_vec()).expect("utf-8"))
    }

    fn key_rows(&self) -> Vec<repo::KeyRow> {
        self.env
            .store()
            .reader(|c| repo::keys(c, WS))
            .expect("keys")
    }
}

#[test]
fn init_then_status_then_409() {
    let e = setup();
    let s = e.vault.status(WS).expect("status");
    assert!(!s.initialized);
    assert_eq!(s.keychain, Keychain::Missing);
    let code = e.vault.init(WS, ME).expect("init");
    assert_eq!(code.expose().split('-').count(), 8);
    let s = e.vault.status(WS).expect("status");
    assert!(s.initialized && !s.rotating);
    assert_eq!(s.keychain, Keychain::Ok);
    assert_eq!(s.kek_version, Some(1));
    assert_eq!(
        e.vault.init(WS, ME).err(),
        Some(VaultError::AlreadyInitialized)
    );
    assert_eq!(VaultError::AlreadyInitialized.status(), 409);
    let rows = e.key_rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].keychain_account, "default/kek/v1");
    assert_eq!(rows[0].fingerprint.len(), 16);
    // the other workspace is separate
    assert!(!e.vault.status(WS2).expect("status").initialized);
    let audit: i64 = e
        .env
        .store()
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT COUNT(*) FROM audit WHERE action = 'vault.init' AND workspace_id = ?",
                [WS],
                |r| r.get(0),
            )?)
        })
        .expect("audit");
    assert_eq!(audit, 1);
}

#[test]
fn init_never_overwrites_an_existing_key_store_entry() {
    let e = setup();
    let foreign = Kek::generate().expect("random");
    e.ks.set(&account_name(WS, 1), &foreign).expect("set");
    assert_eq!(e.vault.init(WS, ME).err(), Some(VaultError::KeyExists));
    let still = e.ks.get(&account_name(WS, 1)).expect("get").expect("some");
    assert_eq!(still.bytes(), foreign.bytes());
    assert!(e.key_rows().is_empty());
}

#[test]
fn unlogged_credential_reads_write_nothing_until_logged() {
    let e = setup();
    e.vault.init(WS, ME).expect("init");
    let (shared, _) = e.item(&e.vault, "hunter2");
    // only system-vault items (connection credentials) can be opened without a use record
    let refused = e.vault.with_credential_unlogged(WS, &shared, |_| ());
    assert_eq!(
        refused.err().map(|e| e.to_string()).as_deref(),
        Some("没有这个条目")
    );
    let item = shared.clone();
    e.env
        .store()
        .with_conn(|c| {
            Ok(c.execute(
                "UPDATE vault_items SET vault_id = 'vlt_sys' WHERE id = ?",
                [&item],
            )?)
        })
        .expect("move to system");
    let uses = || -> i64 {
        e.env
            .store()
            .with_conn(|c| Ok(c.query_row("SELECT count(*) FROM secret_uses", [], |r| r.get(0))?))
            .expect("count")
    };
    let seen = e
        .vault
        .with_credential_unlogged(WS, &item, |c| c.text("密码") == Some("hunter2"))
        .expect("open");
    assert!(seen);
    assert_eq!(uses(), 0, "nothing logged before the caller says so");
    e.vault
        .log_internal_use(WS, &item, "con_x", "测试")
        .expect("log");
    e.vault
        .log_internal_use(WS, &item, "con_x", "测试")
        .expect("log again");
    assert_eq!(uses(), 1, "once per item, actor and day");
    assert!(
        e.vault
            .with_credential_unlogged(WS2, &item, |_| ())
            .is_err()
    );
    assert_eq!(uses(), 1);
}

#[test]
fn items_survive_restart() {
    let e = setup();
    e.vault.init(WS, ME).expect("init");
    let (item, field) = e.item(&e.vault, "hunter2");
    let v2 = e.restart();
    assert_eq!(e.read(&v2, &item, &field).expect("read"), "hunter2");
}

#[test]
fn recover_after_key_store_loss() {
    let e = setup();
    let code = e.vault.init(WS, ME).expect("init");
    let (item, field) = e.item(&e.vault, "hunter2");
    e.ks.delete(&account_name(WS, 1)).expect("lose key");
    let v = e.restart();
    assert_eq!(v.status(WS).expect("status").keychain, Keychain::Missing);
    let err = e.read(&v, &item, &field).expect_err("no key");
    assert_eq!(err, VaultError::KeyMissing);
    assert_eq!(err.status(), 503);

    // wrong codes: random key, typo, garbage, another workspace's code
    let other = super::crypto::recovery_code(&Kek::generate().expect("random"));
    assert_eq!(
        v.recover(WS, other.expose(), ME).err(),
        Some(VaultError::BadRecoveryCode)
    );
    assert_eq!(
        v.recover(WS, "abc", ME).err(),
        Some(VaultError::BadRecoveryCode)
    );
    let ws2 = v.init(WS2, ME).expect("init w2");
    assert_eq!(
        v.recover(WS, ws2.expose(), ME).err(),
        Some(VaultError::BadRecoveryCode)
    );
    assert_eq!(VaultError::BadRecoveryCode.to_string(), "恢复码不对");
    assert_eq!(VaultError::BadRecoveryCode.status(), 400);

    // typed sloppily still works
    let typed = code.expose().to_lowercase().replace('-', " ");
    assert_eq!(v.recover(WS, &typed, ME).expect("recover"), 1);
    assert_eq!(v.status(WS).expect("status").keychain, Keychain::Ok);
    assert_eq!(
        e.read(&e.restart(), &item, &field).expect("read"),
        "hunter2"
    );
    // recovering again is a no-op
    assert_eq!(v.recover(WS, code.expose(), ME).expect("again"), 1);
}

#[test]
fn locked_key_store_is_503() {
    let e = setup();
    e.vault.init(WS, ME).expect("init");
    let (item, field) = e.item(&e.vault, "hunter2");
    e.ks.locked.store(true, Ordering::SeqCst);
    let v = e.restart();
    assert_eq!(v.status(WS).expect("status").keychain, Keychain::Locked);
    let err = e.read(&v, &item, &field).expect_err("locked");
    assert_eq!(err.status(), 503);
    assert_eq!(err.to_string(), "保险箱暂时打不开：Mac mini 的钥匙串被锁了");
    // the running Hub keeps its cached key
    assert_eq!(e.read(&e.vault, &item, &field).expect("cached"), "hunter2");
    e.ks.locked.store(false, Ordering::SeqCst);
    assert_eq!(e.read(&v, &item, &field).expect("unlocked"), "hunter2");
}

#[test]
fn moved_ciphertext_does_not_open() {
    let e = setup();
    e.vault.init(WS, ME).expect("init");
    let (a, fa) = e.item(&e.vault, "alpha");
    let (b, fb) = e.item(&e.vault, "bravo");
    // copy b's field ciphertext into a's field row, and b's wrapped DEK onto a
    e.env
        .store()
        .with_conn(|c| {
            c.execute(
                "UPDATE vault_fields SET value_ct = (SELECT value_ct FROM vault_fields WHERE id = ?) WHERE id = ?",
                [&fb, &fa],
            )?;
            Ok(())
        })
        .expect("swap");
    assert!(e.read(&e.vault, &a, &fa).is_err());
    e.env
        .store()
        .with_conn(|c| {
            c.execute(
                "UPDATE vault_items SET dek_wrapped = (SELECT dek_wrapped FROM vault_items WHERE id = ?) WHERE id = ?",
                [&b, &a],
            )?;
            Ok(())
        })
        .expect("swap dek");
    assert!(e.read(&e.vault, &a, &fa).is_err());
    assert_eq!(e.read(&e.vault, &b, &fb).expect("b intact"), "bravo");
}

#[test]
fn rotation_rewraps_everything_and_retires_the_old_key() {
    let e = setup();
    let code1 = e.vault.init(WS, ME).expect("init");
    let items: Vec<_> = (0..230)
        .map(|i| (e.item(&e.vault, &format!("secret-{i}")), i))
        .collect();
    // a data key prepared before the rotation, inserted after it
    let stale = e.vault.new_item_key(WS, "vi_stale").expect("key");

    let r = e.vault.rotate_now(WS, ME).expect("rotate");
    assert_eq!(r.version, 2);
    // done when it returns: nothing left to re-wrap, nothing rotating, the old key gone
    let rows = e.key_rows();
    assert_eq!(rows[0].status, "retired");
    assert!(rows[0].retired.is_some());
    assert_eq!(rows[1].status, "active");
    assert!(
        e.ks.get(&account_name(WS, 1)).expect("get").is_none(),
        "old KEK deleted"
    );
    let s = e.vault.status(WS).expect("status");
    assert!(!s.rotating);
    assert_eq!(s.kek_version, Some(2));
    let on_v1: i64 = e
        .env
        .store()
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT count(*) FROM vault_items WHERE kek_version = 1",
                [],
                |r| r.get(0),
            )?)
        })
        .expect("count");
    assert_eq!(on_v1, 0);
    let r2 = e
        .env
        .store()
        .transaction(|tx| ensure_active(tx, WS, stale.kek_version));
    assert_eq!(r2.err(), Some(VaultError::Rotating));
    let (late, late_f) = e.item(&e.vault, "late");

    // everything readable, including after a restart (only v2 left in the key store)
    let v = e.restart();
    for ((item, field), i) in &items {
        assert_eq!(
            e.read(&v, item, field).expect("read"),
            format!("secret-{i}")
        );
    }
    assert_eq!(e.read(&v, &late, &late_f).expect("late"), "late");

    // the old recovery code is dead, the new one works
    assert_eq!(
        v.recover(WS, code1.expose(), ME).err(),
        Some(VaultError::BadRecoveryCode)
    );
    e.ks.delete(&account_name(WS, 2)).expect("lose v2");
    assert_eq!(
        e.restart()
            .recover(WS, r.recovery_code.expose(), ME)
            .expect("recover"),
        2
    );

    // and again: v3
    let v = e.restart();
    assert_eq!(v.rotate_now(WS, ME).expect("rotate again").version, 3);
    assert_eq!(e.key_rows()[1].status, "retired");
    assert!(e.ks.get(&account_name(WS, 2)).expect("get").is_none());
    assert_eq!(e.read(&e.restart(), &late, &late_f).expect("late"), "late");
}

#[test]
fn rotation_changes_nothing_when_an_item_does_not_open() {
    let e = setup();
    e.vault.init(WS, ME).expect("init");
    let (good, gf) = e.item(&e.vault, "good");
    let (bad, _) = e.item(&e.vault, "bad");
    e.env
        .store()
        .with_conn(|c| {
            c.execute(
                "UPDATE vault_items SET dek_wrapped = zeroblob(72) WHERE id = ?",
                [&bad],
            )?;
            Ok(())
        })
        .expect("corrupt");
    let err = e.vault.rotate_now(WS, ME).expect_err("refused");
    assert!(err.to_string().contains("1 个条目"), "{err}");
    // the whole transaction rolled back, and the new key left the key store again
    let rows = e.key_rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].status, "active");
    assert!(e.ks.get(&account_name(WS, 2)).expect("get").is_none());
    assert!(e.ks.get(&account_name(WS, 1)).expect("get").is_some());
    assert_eq!(e.read(&e.restart(), &good, &gf).expect("good"), "good");
    // destroying the bad item (V2's delete clears dek_wrapped) lets it rotate
    e.env
        .store()
        .with_conn(|c| {
            c.execute(
                "UPDATE vault_items SET dek_wrapped = NULL WHERE id = ?",
                [&bad],
            )?;
            Ok(())
        })
        .expect("destroy");
    assert_eq!(e.vault.rotate_now(WS, ME).expect("rotate").version, 2);
    assert_eq!(e.read(&e.restart(), &good, &gf).expect("good"), "good");
}

/// What a rotation of an earlier Hub (a background re-wrap that was never resumed) left: v2 active, v1
/// `retiring`, every item still on v1. `tidy_keys` at the next start finishes it.
#[test]
fn tidy_keys_finishes_a_rotation_an_earlier_hub_left_retiring() {
    let e = setup();
    e.vault.init(WS, ME).expect("init");
    let items: Vec<_> = (0..3)
        .map(|i| (e.item(&e.vault, &format!("secret-{i}")), i))
        .collect();
    let k2 = Kek::generate().expect("kek");
    e.ks.set(&account_name(WS, 2), &k2).expect("store v2");
    let fp = super::crypto::fingerprint(&k2);
    e.env
        .store()
        .transaction(|tx| {
            repo::set_key_status(tx, WS, 1, repo::RETIRING, None)?;
            repo::insert_key(tx, WS, 2, &account_name(WS, 2), &fp, repo::ACTIVE, now())?;
            Ok::<_, VaultError>(())
        })
        .expect("leave it retiring");
    assert!(e.vault.status(WS).expect("status").rotating);
    let v = e.restart();
    // with the keychain locked at start, nothing changes and nothing fails
    e.ks.locked.store(true, Ordering::SeqCst);
    v.tidy_keys().expect("tidy");
    e.ks.locked.store(false, Ordering::SeqCst);
    assert_eq!(e.key_rows()[0].status, "retiring");

    e.restart().tidy_keys().expect("tidy");
    let rows = e.key_rows();
    assert_eq!(
        (rows[0].status.as_str(), rows[1].status.as_str()),
        ("retired", "active")
    );
    assert!(e.ks.get(&account_name(WS, 1)).expect("get").is_none());
    let v = e.restart();
    assert!(!v.status(WS).expect("status").rotating);
    for ((item, field), i) in &items {
        assert_eq!(
            e.read(&v, item, field).expect("read"),
            format!("secret-{i}")
        );
    }
    // a second start has nothing to do
    v.tidy_keys().expect("tidy again");
}

async fn send(
    r: &Router,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> (u16, Option<String>, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .body(HttpBody::from(body.unwrap_or("").to_string()))
        .expect("req");
    req.extensions_mut().insert(caller(WS, ME));
    let res = r.clone().oneshot(req).await.expect("infallible");
    let status = res.status().as_u16();
    let cc = res
        .headers()
        .get("cache-control")
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    let b = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .expect("body");
    (
        status,
        cc,
        serde_json::from_slice(&b).unwrap_or(Value::Null),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn api_status_init_recover_rotate() {
    let e = setup();
    let r: Router = routes(e.vault.clone());

    let (s, _, b) = send(&r, "GET", "/vault/status", None).await;
    assert_eq!(s, 200);
    assert_eq!(
        b,
        serde_json::json!({"initialized": false, "keychain": "missing", "keystore": {"kind": "file", "label": "测试用密钥文件（不受保护）"}, "kek_version": null, "passkeys": 0, "rotating": false})
    );
    let (s, _, b) = send(&r, "POST", "/vault/rotate", None).await;
    assert_eq!((s, b["error"].as_str()), (400, Some("保险箱还没有启用")));

    let (s, cc, b) = send(&r, "POST", "/vault/init", None).await;
    assert_eq!(s, 200);
    assert_eq!(cc.as_deref(), Some("no-store"));
    let code = b["recovery_code"].as_str().expect("code").to_string();
    assert_eq!(code.len(), 56 + 7);
    let (s, _, b) = send(&r, "POST", "/vault/init", None).await;
    assert_eq!((s, b["error"].as_str()), (409, Some("保险箱已经启用过了")));
    let (_, _, b) = send(&r, "GET", "/vault/status", None).await;
    assert_eq!(
        (b["initialized"].as_bool(), b["keychain"].as_str()),
        (Some(true), Some("ok"))
    );
    let (item, field) = e.item(&e.vault, "hunter2");

    // recover: bad body, wrong code, right code
    let (s, _, b) = send(&r, "POST", "/vault/recover", Some("{}")).await;
    assert_eq!((s, b["error"].as_str()), (400, Some("参数不对：code")));
    let (s, _, b) = send(
        &r,
        "POST",
        "/vault/recover",
        Some(r#"{"code":"AAAA-BBBB"}"#),
    )
    .await;
    assert_eq!((s, b["error"].as_str()), (400, Some("恢复码不对")));
    assert!(!b.to_string().contains("AAAA"));
    let (s, cc, b) = send(
        &r,
        "POST",
        "/vault/recover",
        Some(&serde_json::json!({ "code": code }).to_string()),
    )
    .await;
    assert_eq!((s, cc.as_deref()), (200, Some("no-store")));
    assert_eq!(b, serde_json::json!({"ok": true, "kek_version": 1}));

    // rotate: done when it answers, with the new code
    let (s, cc, b) = send(&r, "POST", "/vault/rotate", None).await;
    assert_eq!((s, cc.as_deref()), (200, Some("no-store")));
    assert!(b.get("job_id").is_none());
    assert_eq!(b["kek_version"], 2);
    assert_ne!(b["recovery_code"].as_str(), Some(code.as_str()));
    assert_eq!(e.key_rows()[0].status, "retired");
    assert_eq!(
        e.read(&e.restart(), &item, &field).expect("read"),
        "hunter2"
    );

    // locked keychain: 503 with the message
    e.ks.locked.store(true, Ordering::SeqCst);
    let r2: Router = routes(e.restart());
    let (s, _, b) = send(&r2, "GET", "/vault/status", None).await;
    assert_eq!((s, b["keychain"].as_str()), (200, Some("locked")));
    let (s, _, b) = send(&r2, "POST", "/vault/rotate", None).await;
    assert_eq!(
        (s, b["error"].as_str()),
        (503, Some("保险箱暂时打不开：Mac mini 的钥匙串被锁了"))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn api_requires_a_caller() {
    let e = setup();
    let r: Router = routes(e.vault.clone());
    let req = Request::builder()
        .uri("/vault/status")
        .body(HttpBody::empty())
        .expect("req");
    let res = r.oneshot(req).await.expect("infallible");
    assert_eq!(res.status().as_u16(), 401);
}
