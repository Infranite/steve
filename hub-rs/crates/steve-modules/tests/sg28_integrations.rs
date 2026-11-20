//! SG28: render stored connections without invoking a provider or changing data.
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::Arc;
use steve_modules::accounts::connections::integration::{AREAS, CapSpec, Integration, ScopeSpec};
use steve_modules::accounts::connections::{
    AccountInfo, AuthKind, ConnCx, ConnectionProvider, Connections, ConnectionsConfig,
    ProviderError,
};
use steve_modules::accounts::{self, connections};
use steve_modules::contacts;
use steve_modules::testkit::{ME, TestEnv, WS, WS2, caller};
use steve_modules::vault::{Vault, keystore::FileKeyStore};

struct Provider {
    id: &'static str,
    provider: &'static str,
    identity: &'static str,
    specs: Vec<CapSpec>,
}
#[async_trait]
impl ConnectionProvider for Provider {
    fn id(&self) -> &str {
        self.id
    }
    fn name(&self) -> &str {
        self.id
    }
    fn system(&self) -> &str {
        "gmail"
    }
    fn caps(&self) -> &[&'static str] {
        &["cal.read", "msg.receive", "mail.read"]
    }
    fn auth(&self) -> AuthKind {
        AuthKind::None
    }
    fn integration(&self) -> Integration {
        Integration {
            provider: self.provider.into(),
            provider_name: format!("{} fixture", self.provider),
            identity: self.identity.into(),
            capabilities: self.specs.clone(),
        }
    }
    fn available_caps(&self, granted: &[String]) -> Vec<String> {
        if granted.iter().any(|s| s == "deny") {
            Vec::new()
        } else {
            self.caps().iter().map(|s| s.to_string()).collect()
        }
    }
    fn integration_detail(
        &self,
        config: &Value,
        _: &[connections::integration::CapState],
    ) -> Option<Value> {
        config.get("detail").cloned()
    }
    async fn verify(&self, _: &ConnCx) -> Result<AccountInfo, ProviderError> {
        panic!("rendering must not verify or make a provider request")
    }
}
fn provider(
    id: &'static str,
    group: &'static str,
    identity: &'static str,
    specs: Vec<CapSpec>,
) -> Arc<dyn ConnectionProvider> {
    Arc::new(Provider {
        id,
        provider: group,
        identity,
        specs,
    })
}
fn calendar() -> CapSpec {
    CapSpec::supported("calendar", &["cal.read"]).with_scopes(vec![
        ScopeSpec::new("calendar.scope", "calendar", true),
        ScopeSpec::new("calendar.optional", "optional", false),
    ])
}
struct Fixture {
    env: TestEnv,
    _keys: tempfile::TempDir,
    conns: Connections,
}
fn fixture(ps: Vec<Arc<dyn ConnectionProvider>>) -> Fixture {
    let env = TestEnv::new(connections::register);
    for ws in [WS, WS2] {
        let who = caller(ws, ME);
        contacts::service::me_contact(&env.modules, &who).unwrap();
        accounts::service::systems(&env.modules, &who).unwrap();
    }
    let keys = tempfile::tempdir().unwrap();
    let vault = Vault::new(
        env.store().clone(),
        Arc::new(FileKeyStore::open(keys.path()).unwrap()),
    );
    vault.init(WS, ME).unwrap();
    let mut b = Connections::builder(
        env.modules.clone(),
        vault,
        ConnectionsConfig::new(
            vec![
                "https://first.example".into(),
                "http://127.0.0.1:18990".into(),
            ],
            true,
            |_| None,
        ),
    );
    for p in ps {
        b = b.provider(p);
    }
    Fixture {
        env,
        _keys: keys,
        conns: b.build(),
    }
}
impl Fixture {
    fn seed(&self, ws: &str, id: &str, adapter: &str, config: Value) {
        let party = contacts::service::me_contact(&self.env.modules, &caller(ws, ME))
            .unwrap()
            .head
            .id;
        self.env.store().with_conn(|c|{
            c.execute("INSERT INTO accounts (id,workspace_id,system_id,family,party_id,kind,handle,handle_display,display_name,source,created,updated) VALUES (?1,?2,'gmail','email',?3,'other',?4,?4,?5,'connect',1,1)",rusqlite::params![format!("acc_{id}"),ws,party,format!("{id}@example.test"),format!("Name {id}")])?;
            c.execute("INSERT INTO connections (id,workspace_id,account_id,adapter,auth,caps,config,created_by,created,updated) VALUES (?1,?2,?3,?4,'none','[\"cal.read\"]',?5,?6,1,1)",rusqlite::params![id,ws,format!("acc_{id}"),adapter,config.to_string(),ME])?;Ok(())
        }).unwrap();
    }
    fn out(&self) -> connections::integration::IntegrationsOut {
        self.conns.integrations(&caller(WS, ME)).unwrap()
    }
}

#[tokio::test]
async fn sg28_provider_area_union_and_display_order() {
    let f = fixture(vec![
        provider(
            "custom",
            "custom",
            "account",
            vec![CapSpec::planned("mail", "future")],
        ),
        provider(
            "mail",
            "mail",
            "account",
            vec![CapSpec::supported("mail", &["mail.read"])],
        ),
        provider("github", "github", "app", vec![]),
        provider(
            "a-plan",
            "feishu",
            "self",
            vec![CapSpec::planned("calendar", "future")],
        ),
        provider(
            "fs-plan",
            "feishu",
            "self",
            vec![CapSpec::planned("calendar", "future")],
        ),
        provider(
            "fs-live",
            "feishu",
            "bot",
            vec![
                calendar(),
                CapSpec::supported("channel", &["msg.receive"]),
                CapSpec::planned("unknown-area", "future"),
            ],
        ),
    ]);
    let out = f.out();
    assert_eq!(
        out.providers
            .iter()
            .map(|p| p.id.as_str())
            .collect::<Vec<_>>(),
        ["feishu", "github", "mail", "custom"]
    );
    let fs = &out.providers[0];
    assert_eq!(
        fs.adapters
            .iter()
            .map(|p| p.id.as_str())
            .collect::<Vec<_>>(),
        ["a-plan", "fs-live", "fs-plan"]
    );
    assert_eq!(
        fs.areas
            .iter()
            .map(|a| (a.area.as_str(), a.state))
            .collect::<Vec<_>>(),
        [
            ("channel", "supported"),
            ("calendar", "supported"),
            ("unknown-area", "planned")
        ]
    );
    assert_eq!(
        out.areas.iter().map(|a| a.id).collect::<Vec<_>>(),
        AREAS.iter().map(|(id, _)| *id).collect::<Vec<_>>()
    );
    assert!(out.installations.is_empty());
    assert_eq!(
        out.callback_urls,
        [
            "https://first.example/auth/connect/callback",
            "http://127.0.0.1:18990/auth/connect/callback"
        ]
    );
}

#[tokio::test]
async fn sg28_installation_holder_scope_platform_and_uninstalled_fallback() {
    let f = fixture(vec![
        provider("holder", "feishu", "bot", vec![calendar()]),
        provider("user", "feishu", "self", vec![calendar()]),
        provider("bridge", "bridge", "account", vec![calendar()]),
    ]);
    let missing = json!({"permissions":[{"scope":"calendar.scope","status":"missing"}],"detail":{"missing":[{"scope":"calendar.scope"},{"scope":"calendar.scope"}]}});
    f.seed(WS, "holder", "holder", missing.clone());
    let mut user = missing;
    user["app_connection"] = json!("holder");
    f.seed(WS, "user", "user", user);
    f.seed(WS, "orphan", "gone", json!({"app_connection":"holder"}));
    f.seed(WS2, "hidden-holder", "holder", json!({}));
    f.seed(
        WS,
        "bridge",
        "bridge",
        json!({"platform":"feishu","app_connection":"hidden-holder"}),
    );
    f.env
        .store()
        .with_conn(|c| {
            c.execute(
                "UPDATE connections SET status='error' WHERE id='holder'",
                [],
            )?;
            c.execute(
                "UPDATE connections SET status='revoked' WHERE id='user'",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    let out = f.out();
    let installed = out.installations.iter().find(|i| i.id == "holder").unwrap();
    assert_eq!(installed.provider, "feishu");
    assert_eq!(installed.name, "Name holder");
    assert_eq!(installed.status, "revoked");
    assert_eq!(installed.missing, 2);
    assert_eq!(
        installed
            .identities
            .iter()
            .map(|i| (i.kind, i.connection.id.as_str()))
            .collect::<Vec<_>>(),
        [("steve", "holder"), ("you", "user")]
    );
    let bridge = out.installations.iter().find(|i| i.id == "bridge").unwrap();
    assert_eq!(bridge.provider, "feishu");
    assert_eq!(bridge.identities.len(), 1);
    let orphan = out.installations.iter().find(|i| i.id == "orphan").unwrap();
    assert_eq!(orphan.provider, "gmail");
    assert!(!orphan.identities[0].connection.installed);
    assert!(orphan.identities[0].connection.capabilities.is_empty());
    assert_eq!(out.installations.len(), 3);
    assert!(out.installations.iter().all(|i| i.id != "hidden-holder"));
}

#[tokio::test]
async fn sg28_permission_projection_table_and_render_is_read_only() {
    let f = fixture(vec![
        provider("live", "mail", "account", vec![calendar()]),
        provider(
            "planned",
            "custom",
            "account",
            vec![CapSpec::planned("calendar", "later")],
        ),
    ]);
    for (id, adapter, enabled, granted, config, state) in [
        ("on", "live", true, false, json!({}), "on"),
        ("off", "live", false, false, json!({}), "off"),
        ("denied", "live", true, true, json!({}), "missing"),
        (
            "missing",
            "live",
            true,
            false,
            json!({"permissions":[{"scope":"calendar.scope","status":"missing"},{"scope":"calendar.optional","status":"missing"}]}),
            "missing",
        ),
        ("planned", "planned", true, false, json!({}), "planned"),
    ] {
        f.seed(WS, id, adapter, config);
        f.env
            .store()
            .with_conn(|c| {
                c.execute(
                    "UPDATE connections SET caps=?1,granted_scopes=?2 WHERE id=?3",
                    rusqlite::params![
                        if enabled { "[\"cal.read\"]" } else { "[]" },
                        if granted { "[\"deny\"]" } else { "[]" },
                        id
                    ],
                )?;
                Ok(())
            })
            .unwrap();
        let out = f.out();
        let c = out
            .providers
            .iter()
            .flat_map(|p| &p.connections)
            .find(|c| c.id == id)
            .unwrap();
        assert_eq!(c.capabilities[0].state, state, "{id}");
        if id == "missing" {
            assert_eq!(c.capabilities[0].missing_scopes, ["calendar.scope"]);
            assert_eq!(c.capabilities[0].suggested_scopes, ["calendar.optional"]);
        }
    }
    let before = f
        .env
        .store()
        .with_conn(|c| Ok::<_, steve_store::StoreError>(c.total_changes()))
        .unwrap();
    let a = serde_json::to_value(f.out()).unwrap();
    let b = serde_json::to_value(f.out()).unwrap();
    assert_eq!(a, b);
    assert_eq!(
        f.env
            .store()
            .with_conn(|c| Ok::<_, steve_store::StoreError>(c.total_changes()))
            .unwrap(),
        before
    );
}
