//! SG28: tenant identity, naming provenance, workspace isolation and atomic effects.
use axum::Router;
use serde_json::json;
use steve_modules::accounts::{self, TenantHint};
use steve_modules::contacts::{self, repo::Party};
use steve_modules::testkit::{ME, TestEnv, WS, WS2, call, caller};
use steve_modules::{ApiError, ApiResult};
struct Fixture {
    env: TestEnv,
    r: Router,
}
fn fixture() -> Fixture {
    let env = TestEnv::new(accounts::register);
    for ws in [WS, WS2] {
        contacts::service::me_contact(&env.modules, &caller(ws, ME)).unwrap();
        accounts::service::systems(&env.modules, &caller(ws, ME)).unwrap();
    }
    let r = contacts::routes(accounts::ports()).with_state(env.modules.clone());
    env.take_events();
    Fixture { env, r }
}
impl Fixture {
    fn tenant(&self, ws: &str, h: &TenantHint) -> String {
        self.env
            .store()
            .transaction(|tx| accounts::tenant_org(tx, ws, ME, h))
            .unwrap()
    }
    fn party(&self, id: &str) -> Party {
        self.env
            .store()
            .reader(|c| contacts::repo::get(c, id))
            .unwrap()
            .unwrap()
    }
    fn counts(&self) -> Vec<i64> {
        self.env
            .store()
            .reader(|c| {
                [
                    "parties",
                    "accounts",
                    "account_handles",
                    "audit",
                    "revisions",
                ]
                .iter()
                .map(|t| Ok(c.query_row(&format!("SELECT count(*) FROM {t}"), [], |r| r.get(0))?))
                .collect::<steve_store::Result<Vec<_>>>()
            })
            .unwrap()
    }
}
fn hint(key: &str) -> TenantHint {
    TenantHint {
        key: key.into(),
        ..Default::default()
    }
}

#[tokio::test]
async fn sg28_tenant_key_and_external_name_boundaries() {
    let f = fixture();
    for key in ["", " \n\t "] {
        let h = hint(key);
        let e = f
            .env
            .store()
            .transaction(|tx| accounts::tenant_org(tx, WS, ME, &h))
            .unwrap_err();
        assert_eq!(e.status(), 400);
    }
    for (key, name, seen, expected) in [
        ("a", None, None, "外部组织".to_string()),
        (
            "b",
            Some("  "),
            Some("  Team  "),
            "外部组织（Team 里的成员）".to_string(),
        ),
        ("c", None, Some("  "), "外部组织".to_string()),
        (
            "d",
            Some("  Explicit  "),
            Some("ignored"),
            "Explicit".to_string(),
        ),
    ] {
        let mut h = hint(&format!("  {key}  "));
        h.name = name.map(str::to_owned);
        h.seen_in = seen.map(str::to_owned);
        let id = f.tenant(WS, &h);
        let p = f.party(&id);
        assert_eq!(p.name, expected);
        assert_eq!(p.kind, "org");
        assert_eq!(p.source, "sync");
        assert_eq!(p.source_ref.as_deref(), Some(key));
        assert_eq!(f.tenant(WS, &hint(key)), id);
    }
    let mut h = hint("long-name");
    h.name = Some("界".repeat(81));
    assert_eq!(f.party(&f.tenant(WS, &h)).name, "界".repeat(80));
    let mut h = hint("long-seen");
    h.seen_in = Some("界".repeat(41));
    assert_eq!(
        f.party(&f.tenant(WS, &h)).name,
        format!("外部组织（{} 里的成员）", "界".repeat(40))
    );
}

#[tokio::test]
async fn sg28_existing_placeholder_rename_manual_name_and_idempotence() {
    let f = fixture();
    let id = f.tenant(WS, &hint("partner"));
    f.env.take_events();
    let mut h = hint("partner");
    h.name = Some("  Partner  ".into());
    assert_eq!(f.tenant(WS, &h), id);
    assert_eq!(f.party(&id).name, "Partner");
    let revisions = f
        .env
        .store()
        .reader(|c| steve_store::base::revisions(c, WS, "party", &id))
        .unwrap();
    assert_eq!(revisions[0].1["after"]["source"], "sync");
    let before = f.counts();
    f.env.take_events();
    assert_eq!(f.tenant(WS, &h), id);
    assert_eq!(f.counts(), before);
    assert!(f.env.take_events().is_empty());
    let (status, body) = call(
        &f.r,
        Some(&caller(WS, ME)),
        "PATCH",
        &format!("/orgs/{id}"),
        Some(&json!({"name":"外部组织手工命名"})),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    h.name = Some("Provider rename".into());
    f.tenant(WS, &h);
    assert_eq!(f.party(&id).name, "外部组织手工命名");
}

#[tokio::test]
async fn sg28_own_org_and_workspace_binding() {
    let f = fixture();
    let mut h = hint("own");
    h.own = true;
    let id = f.tenant(WS, &h);
    assert_eq!(f.party(&id).org_kind.as_deref(), Some("self"));
    h.name = Some("  Own company  ".into());
    assert_eq!(f.tenant(WS, &h), id);
    assert_eq!(f.party(&id).name, "Own company");
    let (status, body) = call(
        &f.r,
        Some(&caller(WS, ME)),
        "PATCH",
        &format!("/orgs/{id}"),
        Some(&json!({"name":"Manual company"})),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    h.name = Some("Ignored".into());
    f.tenant(WS, &h);
    assert_eq!(f.party(&id).name, "Manual company");
    let other = f.tenant(WS2, &h);
    assert_ne!(other, id);
    assert_eq!(f.party(&other).workspace_id, WS2);
    let labels:Vec<String>=f.env.store().reader(|c|{let mut s=c.prepare("SELECT source_label FROM accounts WHERE system_id='feishu_tenant' AND workspace_id=?")?;Ok::<_,steve_store::StoreError>(s.query_map([WS],|r|r.get(0))?.collect::<rusqlite::Result<_>>()?)}).unwrap();
    assert!(labels.iter().any(|x| x == "本公司的飞书租户"));
}

#[tokio::test]
async fn sg28_tenant_creation_and_events_roll_back_with_the_caller() {
    let f = fixture();
    let before = f.counts();
    f.env.take_events();
    let r: ApiResult<()> = f.env.store().transaction(|tx| {
        accounts::tenant_org(tx, WS, ME, &hint("rollback"))?;
        Err(ApiError::rule("fixture rollback"))
    });
    assert!(r.is_err());
    assert_eq!(f.counts(), before);
    assert!(f.env.take_events().is_empty());
    let id = f.tenant(WS, &hint("rollback"));
    assert_eq!(f.party(&id).source_ref.as_deref(), Some("rollback"));
}
