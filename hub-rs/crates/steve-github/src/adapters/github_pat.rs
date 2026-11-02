//! `github_pat`: a fine-grained personal access token (§1.2), entered in the generic
//! `POST /connections {adapter:"github_pat", fields:{令牌}}` form. The account is the token owner's
//! GitHub login, owned by “我”.

use async_trait::async_trait;
use serde_json::json;
use steve_modules::accounts::connections::{
    AccountInfo, AuthKind, ConnCx, ConnectionProvider, FieldDef, Owner, ProviderError, TokenKind,
};

use crate::api::{DEFAULT_API_BASE, EtagCache, GhConn, GhError, api_base_or};

pub const ID: &str = "github_pat";
/// The credential field key (vault label); the form may also send it under its label “令牌”.
pub const TOKEN: &str = "token";

const FIELDS: [FieldDef; 1] = [FieldDef::secret(TOKEN, "令牌").with_hint(
    "GitHub 设置 → Developer settings → Fine-grained tokens：选好仓库，给 Contents、Pull requests、Issues 读写，Checks、Actions 读；要接收事件再给 Webhooks 读写",
)];

/// `api_base`: where a connection without `config.api_base` talks to (`STEVE_GITHUB_API_BASE`
/// under the test guard, else github.com), see [`crate::providers_for`].
pub struct GitHubPat {
    api_base: String,
}

impl Default for GitHubPat {
    fn default() -> Self {
        Self::new(DEFAULT_API_BASE)
    }
}

impl GitHubPat {
    pub fn new(api_base: &str) -> Self {
        let b = api_base.trim().trim_end_matches('/');
        Self {
            api_base: if b.is_empty() { DEFAULT_API_BASE } else { b }.to_string(),
        }
    }

    /// The base of `cx`: its `config.api_base` (a test's form), else the adapter's.
    pub fn base_of<'a>(&'a self, cx: &'a ConnCx) -> &'a str {
        api_base_or(cx, &self.api_base)
    }
}

#[async_trait]
impl ConnectionProvider for GitHubPat {
    fn id(&self) -> &str {
        ID
    }
    fn system(&self) -> &str {
        "github"
    }
    fn name(&self) -> &str {
        "GitHub 个人令牌"
    }
    fn caps(&self) -> &[&'static str] {
        &super::CAPS
    }
    fn integration(&self) -> steve_modules::accounts::connections::Integration {
        super::integration("self")
    }
    fn auth(&self) -> AuthKind {
        AuthKind::Credential
    }
    fn fields(&self) -> &[FieldDef] {
        &FIELDS
    }
    fn token_kind(&self) -> TokenKind {
        TokenKind::Field(TOKEN)
    }

    /// `GET /user`: whose token it is.
    async fn verify(&self, cx: &ConnCx) -> Result<AccountInfo, ProviderError> {
        let base = self.base_of(cx).to_string();
        let v = GhConn::on(cx.clone(), EtagCache::default(), &base)
            .get("/user")
            .await
            .map_err(|e| match e {
                GhError::Unauthorized => ProviderError::Auth("GitHub 不认这个令牌".into()),
                e => e.into(),
            })?;
        let login = v["login"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| ProviderError::Transient("GitHub 的回答看不懂".into()))?;
        let name = v["name"]
            .as_str()
            .filter(|s| !s.is_empty())
            .unwrap_or(login);
        let mut a = AccountInfo::new(login, name);
        a.kind = "personal".into();
        a.owner = Owner::Me;
        a.config = json!({"account_login": login});
        // recorded like an App connection's, so every later client of it uses the same base
        if base != DEFAULT_API_BASE {
            a.config["api_base"] = json!(base);
        }
        Ok(a)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_base_from_the_env() {
        assert_eq!(GitHubPat::default().api_base, DEFAULT_API_BASE);
        assert_eq!(GitHubPat::new("").api_base, DEFAULT_API_BASE);
        assert_eq!(
            GitHubPat::new("http://127.0.0.1:7/").api_base,
            "http://127.0.0.1:7"
        );
        let home = |k: &str| match k {
            "HOME" => Some("/Users/user".to_string()),
            "STEVE_HOME" => Some("/tmp/steve-test".to_string()),
            "STEVE_GITHUB_API_BASE" => Some("http://127.0.0.1:7".to_string()),
            _ => None,
        };
        let env = crate::GitHubEnv::from_env(home);
        let ps = crate::providers_for(&env);
        assert_eq!(ps.len(), 2);
        assert_eq!(GitHubPat::new(&env.api_base).api_base, "http://127.0.0.1:7");
        // on the real ~/.steve the variable is ignored
        let real = |k: &str| match k {
            "STEVE_HOME" => Some("/Users/user/.steve".to_string()),
            k => home(k),
        };
        let env = crate::GitHubEnv::from_env(real);
        assert_eq!(GitHubPat::new(&env.api_base).api_base, DEFAULT_API_BASE);
    }
}
