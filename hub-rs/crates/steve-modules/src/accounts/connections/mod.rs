//! Connections (slice A2 of steve-rs-plan/modules/accounts-vault.md §2.2, §5.2, §9, §10): how Steve
//! is let into an account — the framework every adapter (Gmail/Outlook A3/E2, IMAP E1, Feishu F1/F6,
//! GitHub G1) plugs into.
//!
//! - [`ConnectionProvider`] — what an adapter implements: id, system, capabilities, how it is
//!   authorized ([`AuthKind`]: OAuth code + PKCE and optionally the device flow, credential form, or
//!   managed by its own module), `verify`, `health`, `mint` (short-lived tokens: GitHub installation
//!   tokens, Feishu tenant tokens), `revoke`;
//! - [`oauth`] — a generic OAuth 2.0 client configured by an [`OAuthSpec`] (endpoints, scopes, form
//!   vs JSON bodies, client auth in body vs Basic) and the env clients `STEVE_OAUTH_<P>_CLIENT_ID/SECRET`;
//! - [`ConnCx`] — what an adapter sees: config, scopes, `with_credential`, `secret(key)`,
//!   `access_token()` (refreshed / minted / read as needed), `invalidate_token()` after a 401,
//!   `report(Health)` from real work;
//! - [`Connections`] — the service: connect (every way in ends in [`Connections::connect`]: the
//!   credential goes to the system vault first, then `verify`, then the account (owner “我”, the own
//!   organization or a team) and connection rows in one transaction), refresh (one at a time per
//!   connection, re-read under the lock, new tokens committed to the vault before anyone uses them —
//!   single-use refresh tokens survive), health (`ok → reauth` on auth failure with one open
//!   “<账号> 的授权过期了” decision, `→ error` after 3 failures, backoff 15 min doubling to 6 h),
//!   pause, capabilities, disconnect (revoke, destroy the credential's data key, delete the row);
//! - [`integration`] — the 接入 page's view: providers, their adapters and planned ways in, and per
//!   connection the state of each capability area (`GET /connections/integrations`);
//! - [`pending`] — server-side single-use 10-minute states (also for the GitHub manifest flow);
//! - [`FakeProvider`] — the scripted `FakeAdapter` for tests.
//!
//! Secrets never reach a response, log line, audit detail or error: credentials only in the vault,
//! tokens only in `Zeroizing` strings, bodies with secrets have no `Debug`.
//!
//! Integration (integrator):
//! 1. `let conns = Connections::builder(modules.clone(), vault.clone(),
//!    ConnectionsConfig::from_env(|k| std::env::var(k).ok())).provider(…).usage(…).build();`
//! 2. `connections::register(&mut registry)` before building `Modules` (the `connection` decision);
//! 3. `.merge(connections::routes(conns.clone()))` under `/api` behind the `Caller` layer;
//! 4. `.merge(connections::callback_routes(conns.clone()))` at the root, with the session read but
//!    not required (insert `Caller` when signed in) — redirect URI `<origin>/auth/connect/callback`
//!    for every origin in `STEVE_PUBLIC_URLS`, to be registered at each OAuth provider;
//! 5. `conns.spawn_jobs()` once the runtime is up (`vault.token_refresh` 5 min, `vault.health` 15 min).

mod api;
pub mod cx;
mod decision;
pub mod fake;
pub mod integration;
pub mod oauth;
pub mod pending;
pub mod provider;
pub mod repo;
mod service;

pub use api::{callback_routes, routes};
pub use cx::{AccessToken, ConnCx, StartCx};
pub use decision::ConnectionDecision;
pub use fake::FakeProvider;
pub use integration::{
    CapSpec, CapState, Integration, IntegrationsOut, ScopeSpec, installation_key,
};
pub use oauth::{ClientAuth, OAuthClient, OAuthSpec, TokenSet, TokenStyle};
pub use pending::{PendingStates, STATE_TTL_MS};
pub use provider::{
    AccountInfo, AuthKind, CAPS, ConnectionProvider, FieldDef, Health, Minted, Owner,
    ProviderError, StewardHint, TokenKind,
};
pub use service::{
    AdapterOut, CALLBACK_PATH, Connected, ConnectionOut, ConnectionUsage, Connections,
    ConnectionsBuilder, ConnectionsConfig, DeviceOut, EXTRA_SCOPES, HealthOut, REAUTH_OPTIONS,
    SUBJECT, Usage, UsageRef, error_location,
};

use crate::Registry;

/// Registers the `connection` decision handler.
pub fn register(reg: &mut Registry) {
    reg.decision(SUBJECT, Box::new(ConnectionDecision));
}
