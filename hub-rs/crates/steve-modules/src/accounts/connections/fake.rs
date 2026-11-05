//! `FakeAdapter`: a scripted [`ConnectionProvider`] for tests of this and later
//! slices, in both shapes (OAuth against any fake token endpoint, credential form). Never
//! registered by the Hub.

use std::collections::VecDeque;
use std::sync::Mutex;

use async_trait::async_trait;

use super::cx::ConnCx;
use super::oauth::OAuthSpec;
use super::provider::{
    AccountInfo, AuthKind, ConnectionProvider, FieldDef, Health, Minted, ProviderError, TokenKind,
};

/// A scripted adapter. `verify` answers [`FakeProvider::account`] (or the queued error); `health`
/// pops queued results, else `Ok`. Every call is recorded.
pub struct FakeProvider {
    pub id: &'static str,
    pub system: &'static str,
    pub auth: AuthKind,
    pub spec: Option<OAuthSpec>,
    pub fields: Vec<FieldDef>,
    pub caps: Vec<&'static str>,
    pub token: TokenKind,
    /// What `verify` returns.
    pub account: Mutex<AccountInfo>,
    verify_errors: Mutex<VecDeque<ProviderError>>,
    healths: Mutex<VecDeque<Health>>,
    /// `verify`/`health`/`mint`/`revoke`, in order, with the credential labels seen.
    pub calls: Mutex<Vec<String>>,
    mints: Mutex<u32>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl FakeProvider {
    /// An OAuth adapter (`fake_oauth`, system `gmail`) on `spec`.
    pub fn oauth(spec: OAuthSpec, account: AccountInfo) -> Self {
        Self::new(
            "fake_oauth",
            "gmail",
            AuthKind::OAuth,
            Some(spec),
            Vec::new(),
            account,
        )
    }

    /// A credential adapter (`fake_cred`, system `feishu_app`) with one plain and one secret field.
    pub fn credential(account: AccountInfo) -> Self {
        Self::new(
            "fake_cred",
            "feishu_app",
            AuthKind::Credential,
            None,
            vec![
                FieldDef::plain("app_id", "App ID"),
                FieldDef::secret("app_secret", "App Secret"),
            ],
            account,
        )
    }

    pub fn new(
        id: &'static str,
        system: &'static str,
        auth: AuthKind,
        spec: Option<OAuthSpec>,
        fields: Vec<FieldDef>,
        account: AccountInfo,
    ) -> Self {
        Self {
            id,
            system,
            auth,
            spec,
            fields,
            caps: vec!["mail.read", "mail.draft", "mail.send", "events"],
            token: match auth {
                AuthKind::OAuth => TokenKind::Stored,
                _ => TokenKind::None,
            },
            account: Mutex::new(account),
            verify_errors: Mutex::new(VecDeque::new()),
            healths: Mutex::new(VecDeque::new()),
            calls: Mutex::new(Vec::new()),
            mints: Mutex::new(0),
        }
    }

    /// The next `verify` fails with `e`.
    pub fn fail_verify(&self, e: ProviderError) {
        lock(&self.verify_errors).push_back(e);
    }
    /// The next `health` answers `h`.
    pub fn next_health(&self, h: Health) {
        lock(&self.healths).push_back(h);
    }
    pub fn calls(&self) -> Vec<String> {
        lock(&self.calls).clone()
    }
    pub fn set_account(&self, a: AccountInfo) {
        *lock(&self.account) = a;
    }
    fn labels(cx: &ConnCx) -> String {
        cx.with_credential(|c| c.labels().collect::<Vec<_>>().join(","))
            .unwrap_or_else(|e| format!("error:{e}"))
    }
}

#[async_trait]
impl ConnectionProvider for FakeProvider {
    fn id(&self) -> &str {
        self.id
    }
    fn system(&self) -> &str {
        self.system
    }
    fn name(&self) -> &str {
        "Fake"
    }
    fn caps(&self) -> &[&'static str] {
        &self.caps
    }
    fn auth(&self) -> AuthKind {
        self.auth
    }
    fn oauth(&self) -> Option<&OAuthSpec> {
        self.spec.as_ref()
    }
    fn fields(&self) -> &[FieldDef] {
        &self.fields
    }
    fn token_kind(&self) -> TokenKind {
        self.token
    }
    async fn verify(&self, cx: &ConnCx) -> Result<AccountInfo, ProviderError> {
        lock(&self.calls).push(format!("verify:{}", Self::labels(cx)));
        if let Some(e) = lock(&self.verify_errors).pop_front() {
            return Err(e);
        }
        Ok(lock(&self.account).clone())
    }
    async fn health(&self, cx: &ConnCx) -> Health {
        lock(&self.calls).push(format!("health:{}", cx.id()));
        lock(&self.healths).pop_front().unwrap_or(Health::Ok)
    }
    async fn mint(&self, cx: &ConnCx) -> Result<Minted, ProviderError> {
        let n = {
            let mut m = lock(&self.mints);
            *m += 1;
            *m
        };
        lock(&self.calls).push(format!("mint:{}", cx.id()));
        Ok(Minted {
            token: zeroize::Zeroizing::new(format!("minted-{n}")),
            expires_at: cx.connections().modules().clock.now() + 3_600_000,
        })
    }
    async fn revoke(&self, cx: &ConnCx) -> Result<(), ProviderError> {
        lock(&self.calls).push(format!("revoke:{}", cx.id()));
        Ok(())
    }
}
