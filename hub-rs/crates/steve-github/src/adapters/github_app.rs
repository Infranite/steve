//! `github_app`: one installation of a Steve GitHub App (§1.1). Created by the manifest flow
//! ([`crate::manifest`]), never from the generic dialog (`AuthKind::Managed`). The connection's
//! credential holds the App's private key and webhook secret (a copy of the App's own item, so the
//! connection can be re-verified, minted and disconnected on its own); `config` holds
//! `{app_id, installation_id, account_login, account_type, api_base}`.

use async_trait::async_trait;
use serde_json::json;
use steve_modules::accounts::connections::{
    AccountInfo, AuthKind, ConnCx, ConnectionProvider, FieldDef, Minted, Owner, ProviderError,
    TokenKind,
};

use crate::api::{GhClient, GhError, api_base_of};
use crate::auth;

pub const ID: &str = "github_app";
/// Credential field keys (vault labels).
pub const PRIVATE_KEY: &str = "private_key";
pub const WEBHOOK_SECRET: &str = "webhook_secret";
pub const CLIENT_SECRET: &str = "client_secret";

const FIELDS: [FieldDef; 2] = [
    FieldDef::secret(PRIVATE_KEY, "私钥"),
    FieldDef::secret(WEBHOOK_SECRET, "Webhook 密钥"),
];

pub struct GitHubApp;

fn unix_secs() -> i64 {
    chrono::Utc::now().timestamp()
}

fn num(cx: &ConnCx, key: &str) -> Result<u64, ProviderError> {
    let v = &cx.config()[key];
    v.as_u64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        .ok_or_else(|| ProviderError::Config(format!("连接配置里没有 {key}")))
}

impl GitHubApp {
    async fn bearer(cx: &ConnCx) -> Result<zeroize::Zeroizing<String>, ProviderError> {
        let pem = cx.secret(PRIVATE_KEY).await?;
        auth::app_bearer(&pem, num(cx, "app_id")?, unix_secs())
    }

    fn client(cx: &ConnCx) -> GhClient {
        GhClient::new(cx.http().clone(), api_base_of(cx))
    }
}

/// A 401/404 from the App endpoints: the key or the installation is gone, which needs the user.
fn app_err(e: GhError) -> ProviderError {
    match e {
        GhError::Unauthorized => ProviderError::Auth("GitHub 不认这个 App 的私钥".into()),
        GhError::Status { status: 404, .. } => {
            ProviderError::Auth("GitHub 上找不到这个安装，可能已经卸载了".into())
        }
        e => e.into(),
    }
}

#[async_trait]
impl ConnectionProvider for GitHubApp {
    fn id(&self) -> &str {
        ID
    }
    fn system(&self) -> &str {
        "github"
    }
    fn name(&self) -> &str {
        "GitHub App"
    }
    fn caps(&self) -> &[&'static str] {
        &super::CAPS
    }
    fn integration(&self) -> steve_modules::accounts::connections::Integration {
        super::integration("app")
    }
    fn auth(&self) -> AuthKind {
        AuthKind::Managed
    }
    fn fields(&self) -> &[FieldDef] {
        &FIELDS
    }
    fn token_kind(&self) -> TokenKind {
        TokenKind::Minted
    }

    /// The installation exists and belongs to the configured App.
    async fn verify(&self, cx: &ConnCx) -> Result<AccountInfo, ProviderError> {
        let app_id = num(cx, "app_id")?;
        let inst = num(cx, "installation_id")?;
        let bearer = Self::bearer(cx).await?;
        let v = auth::get_installation(&Self::client(cx), &bearer, inst)
            .await
            .map_err(app_err)?;
        if v["app_id"].as_u64() != Some(app_id) {
            return Err(ProviderError::Rejected(
                "这个安装不属于这个 GitHub App".into(),
            ));
        }
        let slug = v["app_slug"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| ProviderError::Transient("GitHub 的回答看不懂".into()))?;
        let bot = format!("{slug}[bot]");
        let mut a = AccountInfo::new(bot.clone(), bot);
        a.kind = "app_bot".into();
        a.owner = Owner::SelfOrg;
        a.config = json!({
            "account_login": v["account"]["login"],
            "account_type": v["target_type"].as_str().or(v["account"]["type"].as_str()),
        });
        Ok(a)
    }

    async fn mint(&self, cx: &ConnCx) -> Result<Minted, ProviderError> {
        let inst = num(cx, "installation_id")?;
        let bearer = Self::bearer(cx).await?;
        auth::installation_token(
            &Self::client(cx),
            &bearer,
            inst,
            cx.connections().modules().clock.now(),
            steve_store::now(),
        )
        .await
        .map_err(app_err)
    }
}
