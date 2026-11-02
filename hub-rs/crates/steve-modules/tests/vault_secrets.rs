//! accounts-vault.md §10 rule 1 for slices V1 and V2: after init, items with secret fields (create,
//! list, detail, search, field edits, reveal refused, a connection credential and `with_credential`,
//! delete), rotation, recovery and the error paths (through the service and the HTTP routes), no KEK,
//! recovery code or field plaintext appears in the captured logs, in responses, in SSE events, in
//! error/`Debug` text, in the audit table, or in the database files.
//!
//! Its own test binary with a global subscriber, so logs from every thread (spawn_blocking, the
//! background re-wrap) are captured.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::Request;
use data_encoding::{BASE64, HEXLOWER};
use serde_json::json;
use steve_modules::testkit::{ME, TestEnv, WS, caller};
use steve_modules::vault::crypto::recovery_code;
use steve_modules::vault::keystore::{FileKeyStore, KeyStore, KeyStoreError};
use steve_modules::vault::secret::Kek;
use steve_modules::vault::{self, Vault};
use tower::ServiceExt;
use zeroize::Zeroizing;

const PLAIN: &str = "PLAINTEXT-hunter2-SECRET";
const PLAIN2: &str = "PLAINTEXT-replaced-77";
const PLAIN3: &str = "PLAINTEXT-otp-seed-JBSW";
const PLAIN4: &str = "PLAINTEXT-app-secret-cli";
const CARD: &str = "4111111111114821";
const CVV: &str = "PLAINTEXT-cvv-913";

fn id_of(res: &str) -> String {
    res.split("\"id\":\"")
        .nth(1)
        .and_then(|s| s.split('"').next())
        .expect("id")
        .to_string()
}

/// The id of the field with `label` in an item response.
fn field_id(res: &str, label: &str) -> String {
    // `send` gives "<status> <headers> <body>"; the headers' Debug ends in "} "
    let body = res.split_once("} ").expect("json").1;
    let v: serde_json::Value = serde_json::from_str(body).expect("item json");
    v["fields"]
        .as_array()
        .expect("fields")
        .iter()
        .find(|f| f["label"] == label)
        .and_then(|f| f["id"].as_str())
        .expect("field")
        .to_string()
}

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
    type Writer = Capture;
    fn make_writer(&'a self) -> Capture {
        self.clone()
    }
}

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
        self.inner.set(a, k)
    }
    fn delete(&self, a: &str) -> Result<(), KeyStoreError> {
        self.inner.delete(a)
    }
}

async fn send(r: &Router, method: &str, path: &str, body: &str) -> String {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("req");
    req.extensions_mut().insert(caller(WS, ME));
    let res = r.clone().oneshot(req).await.expect("infallible");
    let head = format!("{} {:?}", res.status(), res.headers());
    let b = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .expect("body");
    format!("{head} {}", String::from_utf8_lossy(&b))
}

/// The keys currently in the file store, as hex and base64.
fn key_needles(dir: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    for f in std::fs::read_dir(dir).expect("dir").flatten() {
        let b64 = std::fs::read_to_string(f.path()).expect("read");
        let raw = BASE64.decode(b64.trim().as_bytes()).expect("base64");
        out.push(b64.trim().to_string());
        out.push(HEXLOWER.encode(&raw));
    }
    out
}

#[tokio::test(flavor = "multi_thread")]
async fn no_secret_in_logs_responses_errors_or_database() {
    let cap = Capture::default();
    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_writer(cap.clone())
            .with_ansi(false)
            .finish(),
    )
    .expect("subscriber");

    let env = TestEnv::new(|_| {});
    let keydir = tempfile::tempdir().expect("tmp");
    let ks = Arc::new(Lockable {
        inner: FileKeyStore::open(keydir.path()).expect("keys"),
        locked: AtomicBool::new(false),
    });
    let v = Vault::new(env.store().clone(), ks.clone());
    let r: Router = vault::routes(v.clone());
    let mut texts: Vec<String> = Vec::new();
    let mut needles: Vec<String> = [PLAIN, PLAIN2, PLAIN3, PLAIN4, CARD, CVV, "1111111111114821"]
        .map(String::from)
        .to_vec();

    // init over HTTP; the code is in that response only
    let init = send(&r, "POST", "/vault/init", "").await;
    let code1 = init
        .split("\"recovery_code\":\"")
        .nth(1)
        .and_then(|s| s.split('"').next())
        .expect("code")
        .to_string();
    needles.extend(key_needles(keydir.path()));

    // V2 over HTTP: items with secret fields, every read, field edits, search, reveal, errors
    let key = v.new_item_key(WS, "vi_probe").expect("item key");
    texts.push(format!("{key:?}"));
    let items: Router = vault::items::routes(v.clone(), Arc::new(vault::items::NoPasskeys));
    let vaults = send(&items, "GET", "/vaults", "").await;
    let shared = vaults
        .split("\"id\":\"")
        .skip(1)
        .map(|s| s.split('"').next().unwrap_or_default().to_string())
        .nth(1)
        .expect("shared vault");
    texts.push(vaults);
    let login = send(
        &items,
        "POST",
        "/vault/items",
        &json!({"vault_id": shared, "template": "login", "title": "GitHub", "fields": [
            {"label": "用户名", "type": "text", "value": "Alice"},
            {"label": "密码", "type": "password", "secret": false, "value": PLAIN}]})
        .to_string(),
    )
    .await;
    let item = id_of(&login);
    texts.push(login.clone());
    let card = send(
        &items,
        "POST",
        "/vault/items",
        &json!({"vault_id": shared, "template": "bank_card", "title": "招行", "fields": [
            {"label": "卡号", "type": "card_number", "secret": true, "value": CARD},
            {"label": "CVV", "type": "password", "secret": true, "value": CVV}]})
        .to_string(),
    )
    .await;
    assert!(card.contains("•••• 4821"), "{card}");
    texts.push(card.clone());
    let field = field_id(&login, "密码");
    let card_id = id_of(&card);
    for (m, p, b) in [
        ("GET", "/vault/items".to_string(), String::new()),
        ("GET", format!("/vault/items/{item}"), String::new()),
        ("GET", format!("/vault/items/{card_id}"), String::new()),
        ("GET", format!("/vault/items?q={PLAIN}"), String::new()),
        (
            "PUT",
            format!("/vault/items/{item}/fields/{field}"),
            json!({"value": PLAIN2}).to_string(),
        ),
        (
            "POST",
            format!("/vault/items/{item}/fields"),
            json!({"label": "OTP", "type": "otp", "secret": true, "value": PLAIN3}).to_string(),
        ),
        (
            "POST",
            format!("/vault/items/{item}/fields"),
            json!({"label": "OTP", "type": "otp", "secret": true, "value": PLAIN3}).to_string(),
        ),
        (
            "POST",
            format!("/vault/items/{item}/fields"),
            json!({"label": "x", "type": "bad", "value": PLAIN3}).to_string(),
        ),
        (
            "POST",
            format!("/vault/items/{item}/fields"),
            format!("{{\"label\": \"x\", \"value\": \"{PLAIN3}\""),
        ),
        (
            "PUT",
            format!("/vault/items/{item}/fields/{field}"),
            json!({"value": [PLAIN3]}).to_string(),
        ),
        (
            "POST",
            "/vault/items".to_string(),
            json!({"vault_id": shared, "template": 7, "title": PLAIN3}).to_string(),
        ),
        (
            "PATCH",
            format!("/vault/items/{item}"),
            json!({"title": "GitHub 2", "sensitivity": PLAIN3}).to_string(),
        ),
        (
            "POST",
            format!("/vault/items/{item}/fields/{field}/reveal/start"),
            String::new(),
        ),
        (
            "POST",
            format!("/vault/items/{item}/fields/{field}/reveal/finish"),
            json!({"assertion": PLAIN3}).to_string(),
        ),
        ("GET", format!("/vault/items/{item}"), String::new()),
    ] {
        texts.push(send(&items, m, &p, &b).await);
    }
    let sys = v
        .create_credential(
            WS,
            ME,
            "飞书",
            None,
            vec![("App Secret".into(), Zeroizing::new(PLAIN4.into()))],
        )
        .expect("credential");
    let seen = v
        .with_credential(WS, &sys, "con_x", "测试", |c| {
            texts.push(format!("{c:?}"));
            c.text("App Secret") == Some(PLAIN4)
        })
        .expect("with_credential");
    assert!(seen);
    texts.push(format!(
        "{:?}",
        v.with_credential(WS, "vi_nope", "con_x", "测试", |_| ())
    ));
    let detail = send(&items, "GET", &format!("/vault/items/{item}"), "").await;
    let otp = field_id(&detail, "OTP");
    texts.push(detail);
    texts.push(
        send(
            &items,
            "DELETE",
            &format!("/vault/items/{item}/fields/{otp}"),
            "",
        )
        .await,
    );

    // rotation over HTTP, then wait for the background re-wrap
    let rot = send(&r, "POST", "/vault/rotate", "").await;
    let code2 = rot
        .split("\"recovery_code\":\"")
        .nth(1)
        .and_then(|s| s.split('"').next())
        .expect("code")
        .to_string();
    needles.extend(key_needles(keydir.path()));
    for _ in 0..300 {
        if send(&r, "GET", "/vault/status", "")
            .await
            .contains("\"rotating\":false")
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    // still readable after the re-wrap; then delete one item
    assert!(
        v.with_credential(WS, &item, "con_x", "测试", |c| c.text("密码")
            == Some(PLAIN2))
            .expect("after rotation")
    );
    texts.push(send(&items, "DELETE", &format!("/vault/items/{card_id}"), "").await);
    needles.push(code1.clone());
    needles.push(code1.replace('-', ""));
    needles.push(code2.clone());
    needles.push(code2.replace('-', ""));
    needles.push(
        recovery_code(&Kek::generate().expect("random"))
            .expose()
            .to_string(),
    );
    let wrong = needles.last().expect("wrong code").clone();

    // error paths: old code, wrong code, garbage, init again, locked key store
    for body in [
        format!("{{\"code\":\"{code1}\"}}"),
        format!("{{\"code\":\"{wrong}\"}}"),
        format!("{{\"code\":{}}}", 42),
        format!("{{\"code\":\"{PLAIN}\"}}"),
        "not json".to_string(),
    ] {
        texts.push(send(&r, "POST", "/vault/recover", &body).await);
    }
    texts.push(send(&r, "POST", "/vault/init", "").await);
    texts.push(format!("{:?}", v.recover(WS, &wrong, ME)));
    texts.push(format!("{:?}", v.recover(WS, PLAIN, ME)));
    ks.locked.store(true, Ordering::SeqCst);
    let cold = Vault::new(env.store().clone(), ks.clone());
    texts.push(format!(
        "{:?}",
        cold.item_key(WS, "vi_probe", &key.wrapped, key.kek_version)
    ));
    texts.push(send(&vault::routes(cold.clone()), "POST", "/vault/rotate", "").await);
    texts.push(send(&vault::routes(cold), "GET", "/vault/status", "").await);
    ks.locked.store(false, Ordering::SeqCst);
    texts.push(
        send(
            &r,
            "POST",
            "/vault/recover",
            &format!("{{\"code\":\"{code2}\"}}"),
        )
        .await,
    );
    texts.push(format!("{:?}", v.status(WS)));
    texts.push(format!("{:?}", ks.inner));

    for e in env.take_events() {
        texts.push(serde_json::to_string(&e).expect("event"));
    }
    let logs = String::from_utf8(cap.0.lock().expect("logs").clone()).expect("utf-8");
    assert!(
        logs.contains("vault initialized")
            && logs.contains("vault rewrap finished")
            && logs.contains("vault item created"),
        "logs captured: {logs}"
    );
    let mut db = Vec::new();
    for ext in ["", "-wal", "-shm"] {
        if let Ok(b) = std::fs::read(format!("{}{ext}", env.store().path().display())) {
            db.extend_from_slice(&b);
        }
    }
    let db_text = String::from_utf8_lossy(&db).to_string();
    for n in &needles {
        assert!(!logs.contains(n.as_str()), "a log line leaks a secret");
        assert!(!db_text.contains(n.as_str()), "the database leaks a secret");
        for t in &texts {
            assert!(
                !t.contains(n.as_str()),
                "response/error/debug leaks a secret: {t}"
            );
        }
    }
    assert!(!db.windows(PLAIN.len()).any(|w| w == PLAIN.as_bytes()));
    // the two codes were only ever in their own responses
    assert!(!texts.iter().any(|t| t.contains(&code2)));
    // and the no-store header was on them
    assert!(init.contains("\"cache-control\": \"no-store\""), "{init}");
    assert!(rot.contains("\"cache-control\": \"no-store\""), "{rot}");
}
