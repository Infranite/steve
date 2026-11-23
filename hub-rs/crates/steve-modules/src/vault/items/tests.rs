//! V2 acceptance tests: create, change fields, delete leaves nothing to
//! recover; list and detail without secret values; personal vault 404 for another user; plus the
//! template rules, search, reveal gate, `with_credential`, audit and SSE. The leak scan over logs and
//! the database files is `tests/vault_secrets.rs`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use axum::Router;
use serde_json::{Value, json};
use steve_store::{
    GrantRows, NewVaultGrant,
    testkit::vault as fixtures,
    vault_items::{FieldRow, ItemRow, VaultItems},
};
use tempfile::TempDir;

use super::*;
use crate::testkit::{ME, OTHER, TestEnv, WS, WS2, call, caller};
use crate::vault::keystore::{FileKeyStore, KeyStore, KeyStoreError};
use crate::vault::secret::Kek;
use crate::vault::{Vault, VaultError, VaultResult};

const PW: &str = "PW-correct-horse-9271";
const CARD: &str = "4111 1111 1111 4821";

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

/// A gate that verifies everything (the crate can make a `Verified`; V3's will check WebAuthn).
struct Yes;

#[async_trait]
impl RevealGate for Yes {
    async fn start(&self, _: &RevealTarget) -> VaultResult<Value> {
        Ok(json!({"challenge": "c"}))
    }
    async fn finish(&self, t: &RevealTarget, a: &Value) -> VaultResult<Verified> {
        if a == "ok" {
            Ok(Verified::new(t.clone()))
        } else {
            Err(crate::ApiError::rule("验证没有通过").into())
        }
    }
}

struct Env {
    env: TestEnv,
    _keys: TempDir,
    ks: Arc<Lockable>,
    vault: Vault,
    r: Router,
    open: Router,
}

fn setup() -> Env {
    let env = TestEnv::new(|_| {});
    let keys = tempfile::tempdir().expect("tmp");
    let ks = Arc::new(Lockable {
        inner: FileKeyStore::open(keys.path()).expect("keys"),
        locked: AtomicBool::new(false),
    });
    let vault = Vault::new(env.store().clone(), ks.clone());
    vault.init(WS, ME).expect("init");
    vault.init(WS2, ME).expect("init w2");
    let r = routes(vault.clone(), Arc::new(NoPasskeys));
    let open = routes(vault.clone(), Arc::new(Yes));
    Env {
        env,
        _keys: keys,
        ks,
        vault,
        r,
        open,
    }
}

impl Env {
    async fn as_(&self, who: &str, m: &str, p: &str, b: Option<Value>) -> (u16, Value) {
        call(&self.r, Some(&caller(WS, who)), m, p, b.as_ref()).await
    }
    async fn me(&self, m: &str, p: &str, b: Option<Value>) -> (u16, Value) {
        self.as_(ME, m, p, b).await
    }
    async fn vault_id(&self, scope: &str) -> String {
        let (s, v) = self.me("GET", "/vaults", None).await;
        assert_eq!(s, 200, "{v}");
        v.as_array()
            .expect("vaults")
            .iter()
            .find(|x| x["scope"] == scope)
            .expect("vault")["id"]
            .as_str()
            .expect("id")
            .to_string()
    }
    async fn login(&self, scope: &str) -> Value {
        let vid = self.vault_id(scope).await;
        let (s, v) = self
            .me(
                "POST",
                "/vault/items",
                Some(json!({
                    "vault_id": vid, "template": "login", "title": "GitHub", "site": "github.com",
                    "fields": [
                        {"label": "用户名", "type": "text", "secret": false, "value": "Alice"},
                        // the client says not secret; the template wins
                        {"label": "密码", "type": "password", "secret": false, "value": PW},
                    ]
                })),
            )
            .await;
        assert_eq!(s, 200, "{v}");
        v
    }
    fn item(&self, id: &str) -> ItemRow {
        self.env
            .store()
            .read(|r| VaultItems(r).item(id))
            .unwrap()
            .unwrap()
    }
    fn fields(&self, id: &str) -> Vec<FieldRow> {
        self.env.store().read(|r| VaultItems(r).fields(id)).unwrap()
    }
    fn vaults(&self) -> Vec<steve_store::vault_items::VaultRow> {
        self.env.store().read(|r| VaultItems(r).vaults(WS)).unwrap()
    }
    fn grant(&self, item: &str) {
        let at = steve_store::now();
        self.env
            .store()
            .write(|w| {
                GrantRows(w).insert_vault_grant(&NewVaultGrant {
                    id: &steve_store::uid("gr"),
                    ws: WS,
                    bot: "fe",
                    item,
                    scope: &json!({"scratch":true}),
                    until: at + 3_600_000,
                    actor: ME,
                    decision: None,
                    at,
                })
            })
            .expect("grant");
    }
    fn account(&self) -> String {
        fixtures::seed_vault_search_account(self.env.store()).expect("account");
        "acc_gh".into()
    }
}

fn field<'a>(item: &'a Value, label: &str) -> &'a Value {
    item["fields"]
        .as_array()
        .expect("fields")
        .iter()
        .find(|f| f["label"] == label)
        .expect("field")
}

#[tokio::test]
async fn vaults_and_templates_on_first_use() {
    let e = setup();
    let (_, v) = e.me("GET", "/vaults", None).await;
    let scopes: Vec<_> = v
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["scope"].clone())
        .collect();
    assert_eq!(scopes, vec![json!("personal"), json!("shared")]);
    assert_eq!(v[0]["name"], "个人");
    // the system vault exists but is never listed; a second user gets their own personal vault
    let n: i64 = e
        .vaults()
        .into_iter()
        .filter(|v| v.scope == "system")
        .count() as i64;
    assert_eq!(n, 1);
    let (_, o) = e.as_(OTHER, "GET", "/vaults", None).await;
    assert_ne!(o[0]["id"], v[0]["id"]);
    assert_eq!(o[1]["id"], v[1]["id"]);
    let n: i64 = e.vaults().len() as i64;
    assert_eq!(n, 4);

    let (s, t) = e.me("GET", "/vault/templates", None).await;
    assert_eq!(s, 200);
    let ids: Vec<_> = t
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        ids,
        [
            "login",
            "bank_card",
            "api_credential",
            "id_document",
            "secure_note",
            "ssh_key",
            "recovery_codes"
        ]
    );
    assert_eq!(t[1]["default_sensitivity"], "high");
    assert_eq!(
        t[0]["fields"][1],
        json!({"label": "密码", "type": "password", "secret": true})
    );
}

#[tokio::test]
async fn create_masks_secrets_everywhere() {
    let e = setup();
    e.env.take_events();
    let it = e.login("personal").await;
    let id = it["id"].as_str().unwrap();
    assert_eq!(it["sensitivity"], "sensitive");
    assert_eq!(it["backend"], "builtin");
    assert_eq!(it["vault"]["scope"], "personal");
    assert_eq!(field(&it, "用户名")["value"], "Alice");
    let pw = field(&it, "密码");
    assert_eq!(pw["secret"], true, "template's secret flag wins");
    assert_eq!(pw["value"], Value::Null);
    assert!(!it.to_string().contains(PW));

    // detail and list
    let (_, d) = e.me("GET", &format!("/vault/items/{id}"), None).await;
    assert_eq!(d, it);
    let (_, l) = e.me("GET", "/vault/items", None).await;
    assert_eq!(l.as_array().unwrap().len(), 1);
    assert!(l[0].get("fields").is_none(), "the list leaves fields out");
    assert!(!l.to_string().contains(PW));

    // stored: ciphertext only, plain value only for the non-secret field
    let plain: Option<String> = e
        .fields(&id)
        .into_iter()
        .find(|f| f.label == "密码")
        .unwrap()
        .value_plain;
    assert_eq!(plain, None);
    let ct: Vec<u8> = e
        .fields(&id)
        .into_iter()
        .find(|f| f.label == "密码")
        .unwrap()
        .value_ct
        .unwrap();
    assert!(ct.len() > 24 + 16 && !ct.windows(PW.len()).any(|w| w == PW.as_bytes()));

    // audit without values, one SSE event with ids only
    let detail: String =
        fixtures::audit_detail(e.env.store(), "vault_item.create", Some(&id)).unwrap();
    assert!(detail.contains("密码") && !detail.contains(PW));
    let ev = e.env.take_events();
    assert_eq!(
        serde_json::to_value(&ev).unwrap(),
        json!([{"type": "vault", "id": id, "ws": WS}])
    );
}

#[tokio::test]
async fn validation_and_template_rules() {
    let e = setup();
    let vid = e.vault_id("shared").await;
    let (s, v) = e
        .me(
            "POST",
            "/vault/items",
            Some(json!({"vault_id": vid, "template": "login", "title": " ", "fields": [{"label": "a", "type": "nope", "value": "x"}]})),
        )
        .await;
    assert_eq!(
        (s, v["error"].as_str().unwrap()),
        (400, "参数不对：title、fields.0.type")
    );
    let (s, v) = e
        .me(
            "POST",
            "/vault/items",
            Some(json!({"vault_id": vid, "template": "login", "title": "t",
                "fields": [{"label": "a", "type": "text", "value": "1"}, {"label": "a", "type": "text", "value": "2"}]})),
        )
        .await;
    assert_eq!(
        (s, v["error"].as_str().unwrap()),
        (400, "已经有叫「a」的字段了")
    );
    let (s, v) = e
        .me(
            "POST",
            "/vault/items",
            Some(json!({"vault_id": vid, "template": "connection_credential", "title": "t"})),
        )
        .await;
    assert_eq!((s, v["error"].as_str().unwrap()), (404, "没有这个模板"));
    let (s, v) = e
        .me(
            "POST",
            "/vault/items",
            Some(
                json!({"vault_id": vid, "template": "login", "title": "t", "account_id": "acc_x"}),
            ),
        )
        .await;
    assert_eq!((s, v["error"].as_str().unwrap()), (404, "没有这个账号"));
    let sys: String = e
        .vaults()
        .into_iter()
        .find(|v| v.scope == "system")
        .unwrap()
        .id;
    let (s, _) = e
        .me(
            "POST",
            "/vault/items",
            Some(json!({"vault_id": sys, "template": "login", "title": "t"})),
        )
        .await;
    assert_eq!(s, 404, "the system vault takes no items over the API");
    let (s, v) = e
        .me(
            "POST",
            "/vault/items",
            Some(json!({"vault_id": vid, "template": "login", "title": "x".repeat(81)})),
        )
        .await;
    assert_eq!((s, v["error"].as_str().unwrap()), (400, "参数不对：title"));
    let (s, _) = e
        .me(
            "POST",
            "/vault/items",
            Some(json!({"vault_id": vid, "template": "login", "title": "t", "fields": [{"label": "a", "type": "text", "value": "x".repeat(20_001)}]})),
        )
        .await;
    assert_eq!(s, 400);

    // bank card: high, last four as hint, sensitivity fixed
    let (s, v) = e
        .me(
            "POST",
            "/vault/items",
            Some(json!({"vault_id": vid, "template": "bank_card", "title": "招行", "sensitivity": "sensitive"})),
        )
        .await;
    assert_eq!(
        (s, v["error"].as_str().unwrap()),
        (400, "银行卡条目的敏感度不能修改")
    );
    let (s, card) = e
        .me(
            "POST",
            "/vault/items",
            Some(json!({"vault_id": vid, "template": "bank_card", "title": "招行",
                "fields": [{"label": "卡号", "type": "card_number", "secret": true, "value": CARD}]})),
        )
        .await;
    assert_eq!(s, 200, "{card}");
    assert_eq!(card["sensitivity"], "high");
    assert_eq!(field(&card, "卡号")["hint"], "•••• 4821");
    assert_eq!(field(&card, "卡号")["value"], Value::Null);
    let (s, v) = e
        .me(
            "PATCH",
            &format!("/vault/items/{}", card["id"].as_str().unwrap()),
            Some(json!({"sensitivity": "normal"})),
        )
        .await;
    assert_eq!(
        (s, v["error"].as_str().unwrap()),
        (400, "银行卡条目的敏感度不能修改")
    );
}

#[tokio::test]
async fn patch_raising_to_high_revokes_grants() {
    let e = setup();
    let acc = e.account();
    let it = e.login("shared").await;
    let id = it["id"].as_str().unwrap();
    e.grant(id);
    let (_, d) = e.me("GET", &format!("/vault/items/{id}"), None).await;
    assert_eq!(d["grants"][0]["bot_id"], "fe");
    assert_eq!(d["grants"][0]["scope_label"], "没有项目的事项");

    let (s, p) = e
        .me(
            "PATCH",
            &format!("/vault/items/{id}"),
            Some(json!({"title": "GitHub 个人", "account_id": acc})),
        )
        .await;
    assert_eq!(s, 200, "{p}");
    assert_eq!(
        p["account"],
        json!({"id": "acc_gh", "display_name": "octo-handle"})
    );
    assert_eq!(p["grants"].as_array().unwrap().len(), 1);
    let (_, p) = e
        .me(
            "PATCH",
            &format!("/vault/items/{id}"),
            Some(json!({"sensitivity": "high"})),
        )
        .await;
    assert_eq!(p["grants"], json!([]));
    let revoked: i64 = fixtures::revoked_grants(e.env.store(), &id).unwrap();
    assert_eq!(revoked, 1);
    let (_, p) = e
        .me(
            "PATCH",
            &format!("/vault/items/{id}"),
            Some(json!({"account_id": null})),
        )
        .await;
    assert_eq!(p["account"], Value::Null);
    assert_eq!(p["title"], "GitHub 个人");
    let n: i64 = fixtures::audit_count(e.env.store(), "vault_item.patch", &id).unwrap();
    assert_eq!(n, 3);
}

#[tokio::test]
async fn fields_change_by_whole_value() {
    let e = setup();
    let it = e.login("personal").await;
    let id = it["id"].as_str().unwrap().to_string();
    let fid = field(&it, "密码")["id"].as_str().unwrap().to_string();
    let before: Vec<u8> = e
        .fields(&id)
        .into_iter()
        .find(|f| f.id == fid)
        .unwrap()
        .value_ct
        .unwrap();

    let (s, p) = e
        .me(
            "PUT",
            &format!("/vault/items/{id}/fields/{fid}"),
            Some(json!({"value": "NEW-pw-55"})),
        )
        .await;
    assert_eq!(s, 200, "{p}");
    assert_eq!(field(&p, "密码")["value"], Value::Null);
    let after: Vec<u8> = e
        .fields(&id)
        .into_iter()
        .find(|f| f.id == fid)
        .unwrap()
        .value_ct
        .unwrap();
    assert_ne!(before[..24], after[..24], "new nonce");
    let got = e
        .vault
        .with_credential(WS, &id, "test", "测试", |c| {
            c.text("密码").map(str::to_string)
        })
        .unwrap();
    assert_eq!(got.as_deref(), Some("NEW-pw-55"));

    // add: secret field sealed, non-secret plain; duplicate label refused
    let (s, p) = e
        .me(
            "POST",
            &format!("/vault/items/{id}/fields"),
            Some(json!({"label": "OTP 种子", "type": "otp", "secret": true, "value": "JBSWY3DP"})),
        )
        .await;
    assert_eq!(s, 200, "{p}");
    assert_eq!(field(&p, "OTP 种子")["value"], Value::Null);
    let (s, v) = e
        .me(
            "POST",
            &format!("/vault/items/{id}/fields"),
            Some(json!({"label": "用户名", "type": "text", "value": "x"})),
        )
        .await;
    assert_eq!(
        (s, v["error"].as_str().unwrap()),
        (400, "已经有叫「用户名」的字段了")
    );
    let (s, v) = e
        .me(
            "POST",
            &format!("/vault/items/{id}/fields"),
            Some(json!({"label": "", "type": "bad", "value": "x"})),
        )
        .await;
    assert_eq!(
        (s, v["error"].as_str().unwrap()),
        (400, "参数不对：label、type")
    );
    let uf = field(&p, "用户名")["id"].as_str().unwrap().to_string();
    let (_, p) = e
        .me(
            "PUT",
            &format!("/vault/items/{id}/fields/{uf}"),
            Some(json!({"value": "octo"})),
        )
        .await;
    assert_eq!(field(&p, "用户名")["value"], "octo");

    let (s, p) = e
        .me("DELETE", &format!("/vault/items/{id}/fields/{uf}"), None)
        .await;
    assert_eq!(s, 200);
    assert_eq!(p["fields"].as_array().unwrap().len(), 2);
    let (s, v) = e
        .me("DELETE", &format!("/vault/items/{id}/fields/{uf}"), None)
        .await;
    assert_eq!((s, v["error"].as_str().unwrap()), (404, "没有这个字段"));
    for a in [
        "vault_field.update",
        "vault_field.add",
        "vault_field.delete",
    ] {
        let d: String = fixtures::audit_detail(e.env.store(), a, None).unwrap();
        assert!(
            !d.contains("NEW-pw-55") && !d.contains("JBSWY3DP"),
            "{a}: {d}"
        );
    }
}

#[tokio::test]
async fn delete_destroys_key_and_values() {
    let e = setup();
    let it = e.login("shared").await;
    let id = it["id"].as_str().unwrap().to_string();
    e.grant(&id);
    let wrapped = e.item(&id).dek_wrapped.unwrap();
    let ct: Vec<u8> = e
        .fields(&id)
        .into_iter()
        .find(|f| f.secret)
        .unwrap()
        .value_ct
        .unwrap();

    let (s, v) = e.me("DELETE", &format!("/vault/items/{id}"), None).await;
    assert_eq!((s, v), (200, json!({"ok": true, "revoked_grants": 1})));
    let row = e.item(&id);
    let (st, title) = (row.status, row.title);
    assert_eq!(
        (st.as_str(), title.as_str()),
        ("deleted", "GitHub"),
        "title kept for the use log"
    );
    let item = e.item(&id);
    assert!(item.dek_wrapped.is_none() && item.kek_version.is_none());
    let left: i64 = e
        .fields(&id)
        .into_iter()
        .filter(|f| f.value_ct.is_some() || f.value_plain.is_some())
        .count() as i64;
    assert_eq!(left, 0);
    let (s, _) = e.me("GET", &format!("/vault/items/{id}"), None).await;
    assert_eq!(s, 404);
    let (_, l) = e.me("GET", "/vault/items", None).await;
    assert_eq!(l, json!([]));
    assert!(e.vault.with_credential(WS, &id, "t", "t", |_| ()).is_err());

    // neither the old wrapped key nor the ciphertext survives in the database files
    fixtures::checkpoint(e.env.store()).unwrap();
    let mut db = std::fs::read(e.env.store().path()).unwrap();
    db.extend(std::fs::read(format!("{}-wal", e.env.store().path().display())).unwrap_or_default());
    assert!(
        !db.windows(wrapped.len()).any(|w| w == wrapped.as_slice()),
        "wrapped DEK left in the file"
    );
    assert!(
        !db.windows(ct.len()).any(|w| w == ct.as_slice()),
        "ciphertext left in the file"
    );
}

#[tokio::test]
async fn personal_vault_is_private() {
    let e = setup();
    let it = e.login("personal").await;
    let id = it["id"].as_str().unwrap();
    let fid = field(&it, "密码")["id"].as_str().unwrap();
    let base = format!("/vault/items/{id}");
    for (m, p, b) in [
        ("GET", base.clone(), None),
        ("PATCH", base.clone(), Some(json!({"title": "x"}))),
        (
            "POST",
            format!("{base}/fields"),
            Some(json!({"label": "x", "type": "text", "value": "x"})),
        ),
        (
            "PUT",
            format!("{base}/fields/{fid}"),
            Some(json!({"value": "x"})),
        ),
        ("DELETE", format!("{base}/fields/{fid}"), None),
        ("POST", format!("{base}/fields/{fid}/reveal/start"), None),
        (
            "POST",
            format!("{base}/fields/{fid}/reveal/finish"),
            Some(json!({"assertion": "ok"})),
        ),
        ("DELETE", base.clone(), None),
    ] {
        let (s, v) = e.as_(OTHER, m, &p, b.clone()).await;
        assert_eq!(
            (s, v["error"].as_str()),
            (404, Some("没有这个条目")),
            "{m} {p}"
        );
        let (s, _) = call(&e.open, Some(&caller(WS, OTHER)), m, &p, b.as_ref()).await;
        assert_eq!(s, 404, "{m} {p} with a passing gate");
        let (s, _) = call(&e.r, Some(&caller(WS2, ME)), m, &p, b.as_ref()).await;
        assert_eq!(s, 404, "{m} {p} from another workspace");
    }
    let (_, l) = e.as_(OTHER, "GET", "/vault/items", None).await;
    assert_eq!(l, json!([]));
    let my_vault = it["vault"]["id"].as_str().unwrap();
    let (s, _) = e
        .as_(
            OTHER,
            "GET",
            &format!("/vault/items?vault={my_vault}"),
            None,
        )
        .await;
    assert_eq!(s, 404);
    // a shared item is visible to both
    let sh = e.login("shared").await;
    let (s, _) = e
        .as_(
            OTHER,
            "GET",
            &format!("/vault/items/{}", sh["id"].as_str().unwrap()),
            None,
        )
        .await;
    assert_eq!(s, 200);
    // still there for its owner
    let (s, _) = e.me("GET", &base, None).await;
    assert_eq!(s, 200);
}

#[tokio::test]
async fn search_only_non_secret() {
    let e = setup();
    let acc = e.account();
    let it = e.login("shared").await;
    let id = it["id"].as_str().unwrap();
    e.me(
        "PATCH",
        &format!("/vault/items/{id}"),
        Some(json!({"account_id": acc})),
    )
    .await;
    let vid = e.vault_id("personal").await;
    e.me(
        "POST",
        "/vault/items",
        Some(
            json!({"vault_id": vid, "template": "api_credential", "title": "OpenAI",
        "fields": [{"label": "令牌", "type": "password", "secret": true, "value": "sk-zzz"}]}),
        ),
    )
    .await;
    let hits = |v: &Value| {
        v.as_array()
            .unwrap()
            .iter()
            .map(|x| x["title"].as_str().unwrap().to_string())
            .collect::<Vec<_>>()
    };
    for (q, want) in [
        ("github", vec!["GitHub"]),
        ("ALI", vec!["GitHub"]),         // non-secret value, case-insensitive
        ("octo-handle", vec!["GitHub"]), // the linked account
        ("令牌", vec!["OpenAI"]),        // a field label
        ("correct-horse", vec![]),       // a secret value: never
        ("sk-zzz", vec![]),
    ] {
        let (s, v) = e
            .me("GET", &format!("/vault/items?q={}", urlenc(q)), None)
            .await;
        assert_eq!(s, 200);
        assert_eq!(hits(&v), want, "q={q}");
    }
    let (_, v) = e
        .me("GET", "/vault/items?template=api_credential", None)
        .await;
    assert_eq!(hits(&v), vec!["OpenAI"]);
    let (_, v) = e
        .me("GET", &format!("/vault/items?account={acc}"), None)
        .await;
    assert_eq!(hits(&v), vec!["GitHub"]);
    let (_, v) = e
        .me("GET", &format!("/vault/items?vault={vid}"), None)
        .await;
    assert_eq!(hits(&v), vec!["OpenAI"]);
}

fn urlenc(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b == b'-' {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

#[tokio::test]
async fn reveal_needs_local_verification() {
    let e = setup();
    let it = e.login("personal").await;
    let id = it["id"].as_str().unwrap();
    let fid = field(&it, "密码")["id"].as_str().unwrap();
    let uf = field(&it, "用户名")["id"].as_str().unwrap();
    let p = format!("/vault/items/{id}/fields/{fid}/reveal");

    // until V3: no passkeys, so no reveal
    let (s, v) = e.me("POST", &format!("{p}/start"), None).await;
    assert_eq!((s, v["error"].as_str().unwrap()), (400, NEED_PASSKEY));
    let (s, v) = e
        .me(
            "POST",
            &format!("{p}/finish"),
            Some(json!({"assertion": "ok"})),
        )
        .await;
    assert_eq!((s, v["error"].as_str().unwrap()), (400, NEED_PASSKEY));
    let n: i64 = fixtures::audit_count(e.env.store(), "vault.reveal_refused", &id).unwrap();
    assert_eq!(n, 1);
    let n: i64 = fixtures::use_count(e.env.store(), None, None, None).unwrap();
    assert_eq!(n, 0);

    // with a gate: a bad assertion fails, a good one returns the value, no-store, and is logged
    let me = caller(WS, ME);
    let (s, _) = call(&e.open, Some(&me), "POST", &format!("{p}/start"), None).await;
    assert_eq!(s, 200);
    let (s, _) = call(
        &e.open,
        Some(&me),
        "POST",
        &format!("{p}/finish"),
        Some(&json!({"assertion": "bad"})),
    )
    .await;
    assert_eq!(s, 400);
    let (s, v) = call(
        &e.open,
        Some(&me),
        "POST",
        &format!("{p}/finish"),
        Some(&json!({"assertion": "ok", "copy": true})),
    )
    .await;
    assert_eq!((s, v), (200, json!({"value": PW, "ttl": 30})));
    let res = tower::ServiceExt::oneshot(e.open.clone(), {
        let mut r = axum::http::Request::post(format!("{p}/finish"))
            .header("content-type", "application/json")
            .body(axum::body::Body::from(r#"{"assertion":"ok"}"#))
            .unwrap();
        r.extensions_mut().insert(me.clone());
        r
    })
    .await
    .unwrap();
    assert_eq!(res.headers()["cache-control"], "no-store");
    let uses = fixtures::access_log(e.env.store()).unwrap();
    assert_eq!(
        uses,
        vec![
            ("me".into(), ME.into(), "copy".into(), "[\"密码\"]".into()),
            ("me".into(), ME.into(), "view".into(), "[\"密码\"]".into())
        ]
    );
    let n: i64 = fixtures::audit_count(e.env.store(), "vault.reveal", &id).unwrap();
    assert_eq!(n, 2);
    let a: String = fixtures::all_audit_details(e.env.store()).unwrap();
    assert!(!a.contains(PW));

    // a non-secret field needs no reveal; a locked keychain is a 503
    let (s, v) = call(
        &e.open,
        Some(&me),
        "POST",
        &format!("/vault/items/{id}/fields/{uf}/reveal/start"),
        None,
    )
    .await;
    assert_eq!(
        (s, v["error"].as_str().unwrap()),
        (400, "这个字段不是机密，不需要验证")
    );
    e.ks.locked.store(true, Ordering::SeqCst);
    let cold = routes(
        Vault::new(e.env.store().clone(), e.ks.clone()),
        Arc::new(Yes),
    );
    let (s, v) = call(
        &cold,
        Some(&me),
        "POST",
        &format!("{p}/finish"),
        Some(&json!({"assertion": "ok"})),
    )
    .await;
    assert_eq!(
        (s, v["error"].as_str().unwrap()),
        (503, "保险箱暂时打不开：Mac mini 的钥匙串被锁了")
    );
}

#[tokio::test]
async fn locked_or_uninitialized_vault() {
    let e = setup();
    let vid = e.vault_id("shared").await;
    let body = json!({"vault_id": vid, "template": "login", "title": "t", "fields": [{"label": "密码", "type": "password", "value": PW}]});
    e.ks.locked.store(true, Ordering::SeqCst);
    let cold = routes(
        Vault::new(e.env.store().clone(), e.ks.clone()),
        Arc::new(NoPasskeys),
    );
    let (s, _) = call(
        &cold,
        Some(&caller(WS, ME)),
        "POST",
        "/vault/items",
        Some(&body),
    )
    .await;
    assert_eq!(s, 503);
    let n: i64 = fixtures::vault_item_count(e.env.store()).unwrap();
    assert_eq!(n, 0);
    // a workspace without a key: 400
    e.ks.locked.store(false, Ordering::SeqCst);
    fixtures::seed_vault_workspace(e.env.store()).unwrap();
    let v3 = e.vault.list_vaults("w3", ME).unwrap();
    let body = json!({"vault_id": v3[1].id, "template": "login", "title": "t"});
    let (s, v) = call(
        &e.r,
        Some(&caller("w3", ME)),
        "POST",
        "/vault/items",
        Some(&body),
    )
    .await;
    assert_eq!((s, v["error"].as_str().unwrap()), (400, "保险箱还没有启用"));
}

#[tokio::test]
async fn connection_credentials_in_the_system_vault() {
    let e = setup();
    let z = |s: &str| zeroize::Zeroizing::new(s.to_string());
    let id = e
        .vault
        .create_credential(
            WS,
            ME,
            "飞书应用凭据",
            None,
            vec![
                ("App Secret".into(), z("app-secret-1")),
                ("App ID".into(), z("cli_a1")),
            ],
        )
        .unwrap();
    // never listed, never visible over the API
    let (_, l) = e.me("GET", "/vault/items", None).await;
    assert_eq!(l, json!([]));
    let (s, _) = e.me("GET", &format!("/vault/items/{id}"), None).await;
    assert_eq!(s, 404);
    let all_secret: i64 = e
        .fields(&id)
        .into_iter()
        .map(|f| i64::from(f.secret))
        .min()
        .unwrap();
    assert_eq!(all_secret, 1);

    let got = e.vault.with_credential(WS, &id, "con_1", "刷新令牌", |c| {
        assert_eq!(format!("{c:?}"), "Credential([\"App Secret\", \"App ID\"])");
        (
            c.text("App Secret").map(str::to_string),
            c.text("nope").is_none(),
        )
    });
    assert_eq!(got.unwrap(), (Some("app-secret-1".into()), true));
    e.vault
        .with_credential(WS, &id, "con_1", "刷新令牌", |_| ())
        .unwrap();
    let n: i64 =
        fixtures::item_use_count(e.env.store(), &id, Some("internal"), "ok", None).unwrap();
    assert_eq!(n, 1, "internal uses merge per day");
    assert!(
        e.vault
            .with_credential(WS2, &id, "con_1", "x", |_| ())
            .is_err(),
        "other workspace"
    );

    e.vault
        .replace_credential(WS, ME, &id, vec![("App Secret".into(), z("app-secret-2"))])
        .unwrap();
    let got = e
        .vault
        .with_credential(WS, &id, "con_1", "x", |c| c.labels().count())
        .unwrap();
    assert_eq!(got, 1);

    // a broken ciphertext: a failed use of its own
    fixtures::corrupt_fields(e.env.store(), &id).unwrap();
    assert!(matches!(
        e.vault.with_credential(WS, &id, "con_1", "x", |_| ()),
        Err(VaultError::Api(_))
    ));
    let n: i64 = fixtures::item_use_count(e.env.store(), &id, None, "failed", None).unwrap();
    assert_eq!(n, 1);
    // the same connection failing again (a retry loop against a locked keychain): one row, its
    // finished moves on, no new audit line; another user of the item gets a row of its own
    let audits =
        |e: &Env| -> i64 { fixtures::audit_count(e.env.store(), "vault.use", &id).unwrap() };
    let before = audits(&e);
    let first: i64 = fixtures::first_failed_use_finished(e.env.store(), &id).unwrap();
    for _ in 0..5 {
        assert!(
            e.vault
                .with_credential(WS, &id, "con_1", "x", |_| ())
                .is_err()
        );
    }
    let n: i64 = fixtures::item_use_count(e.env.store(), &id, None, "failed", None).unwrap();
    assert_eq!(n, 1, "consecutive failures merge");
    let last: i64 = fixtures::first_failed_use_finished(e.env.store(), &id).unwrap();
    assert!(last >= first);
    assert_eq!(audits(&e), before);
    assert!(
        e.vault
            .with_credential(WS, &id, "con_2", "x", |_| ())
            .is_err()
    );
    let n: i64 = fixtures::item_use_count(e.env.store(), &id, None, "failed", None).unwrap();
    assert_eq!(n, 2, "per actor");
    // an hour after the last failure, a new row
    fixtures::age_uses(e.env.store(), &id, 7_200_000).unwrap();
    assert!(
        e.vault
            .with_credential(WS, &id, "con_1", "x", |_| ())
            .is_err()
    );
    let n: i64 =
        fixtures::item_use_count(e.env.store(), &id, None, "failed", Some("con_1")).unwrap();
    assert_eq!(n, 2, "a new hour, a new row");

    let n = e
        .env
        .store()
        .transaction(|tx| e.vault.destroy_item(tx, WS, ME, &id))
        .unwrap();
    assert_eq!(n, 0);
    let w: Option<Vec<u8>> = e.item(&id).dek_wrapped;
    assert_eq!(w, None);
}

/// Field values are decrypted in exactly three places: reveal/finish, injection (V4) and
/// `with_credential`.
#[test]
fn decryption_call_sites() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut sites = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(d) = stack.pop() {
        for f in std::fs::read_dir(&d).unwrap().flatten() {
            let p = f.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "rs") && !p.ends_with("tests.rs") {
                let s = std::fs::read_to_string(&p).unwrap();
                // in-file unit tests don't count
                let s = s
                    .split("#[cfg(test)]\nmod tests")
                    .next()
                    .unwrap_or_default();
                let n = s.matches("open_field(").count();
                if n > 0 {
                    sites.push((p.strip_prefix(&root).unwrap().display().to_string(), n));
                }
            }
        }
    }
    sites.sort();
    assert_eq!(
        sites,
        vec![
            ("vault/crypto.rs".to_string(), 1),
            ("vault/inject.rs".to_string(), 1),
            ("vault/items/credential.rs".to_string(), 1),
            ("vault/items/reveal.rs".to_string(), 1),
        ],
        "open_field may only be called from reveal/finish, injection and with_credential"
    );
}
