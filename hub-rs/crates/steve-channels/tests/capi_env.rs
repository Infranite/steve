//! `STEVE_FEISHU_API_BASE` (FeishuEnv) in a process of its own: the base is process-wide. A bot
//! connected without `config.api_base` and the user provider both talk to the fake.

use std::sync::Arc;

use serde_json::json;
use steve_channels::adapters::feishu::FeishuApp;
use steve_channels::adapters::feishu::client::{self, Domain, FeishuEnv};
use steve_channels::fake::feishu::{FakeFeishu, ids};
use steve_modules::accounts::connections::{
    self, ConnectionProvider, Connections, ConnectionsConfig,
};
use steve_modules::testkit::{ME, TestEnv, WS, caller};
use steve_modules::vault::Vault;
use steve_modules::vault::keystore::FileKeyStore;
use zeroize::Zeroizing;

#[tokio::test(flavor = "multi_thread")]
async fn the_env_base_reaches_the_bot_and_the_user_provider() {
    let fake = FakeFeishu::start().await.expect("fake feishu");
    let base = fake.base_url().to_string();
    let env = FeishuEnv::from_env(|k| match k {
        "HOME" => Some("/Users/user".into()),
        "STEVE_HOME" => Some("/tmp/steve-test".into()),
        "STEVE_FEISHU_API_BASE" => Some(base.clone()),
        "STEVE_FORBID_REAL_HOSTS" => Some("1".into()),
        _ => None,
    });
    env.install();
    assert_eq!(client::api_base(&json!({})), base);
    assert_eq!(client::accounts_base(&json!({"domain": "lark"})), base);
    let user = env.user_provider(Domain::Feishu);
    assert!(
        user.oauth()
            .is_some_and(|s| s.authorize_url.starts_with(&base) && s.token_url.starts_with(&base))
    );

    let tenv = TestEnv::new(connections::register);
    let keys = tempfile::tempdir().expect("tmp");
    let vault = Vault::new(
        tenv.store().clone(),
        Arc::new(FileKeyStore::open(keys.path()).expect("keys")),
    );
    vault.init(WS, ME).expect("vault init");
    // the framework drops `api_base` from input (as on the real ~/.steve): only the env base is left
    let conns = Connections::builder(
        tenv.modules.clone(),
        vault,
        ConnectionsConfig::new(vec!["http://hub.test:8440".into()], false, |_| None),
    )
    .provider(Arc::new(FeishuApp::new()))
    .provider(Arc::new(user))
    .build();
    let fields = [("App ID", ids::APP_ID), ("App Secret", ids::APP_SECRET)]
        .into_iter()
        .map(|(k, v)| (k.to_string(), Zeroizing::new(v.to_string())))
        .collect();
    let c = conns
        .create(
            &caller(WS, ME),
            "feishu_app",
            &fields,
            json!({"api_base": "http://dropped.invalid"}),
        )
        .await
        .expect("verified against the fake");
    assert_eq!(c.status, "ok");
    assert!(!fake.requests().is_empty());
}
