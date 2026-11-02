//! Creating the bot's Feishu / Lark app from Steve: the official one-scan bootstrap (the
//! `PersonalAgent` app registration), re-implemented natively after larksuite/cli
//! `internal/auth/app_registration.go` (f31194b) and `@larksuite/openclaw-lark-tools` 1.0.47.
//! No CLI, wrapper or runtime of theirs is used; their code is only the protocol reference.
//!
//! The protocol (`POST {accounts}/oauth/v1/app/registration`, form-encoded):
//! - `action=begin, archetype=PersonalAgent, auth_method=client_secret, request_user_info=open_id
//!   tenant_brand` → `device_code`, `user_code`, `verification_uri_complete`, `expire_in` (legacy
//!   `expires_in`, default 600 s), `interval` (default 5 s). Always sent to **Feishu**'s accounts
//!   host; the brand asked for only picks the confirmation page's host.
//! - `action=poll, device_code` → `client_id` + `client_secret` (+ `user_info{open_id,
//!   tenant_brand}`) when confirmed; else `error`: `authorization_pending` (again), `slow_down`
//!   (+5 s, at most 60), `access_denied`, `expired_token` / `invalid_grant`, anything else (failed).
//!   The first `user_info.tenant_brand` that differs from the polled brand switches the polled host,
//!   once, and polls again at once. One deadline (`expires_at`, fixed at begin) bounds everything;
//!   a resume never extends it.
//!
//! Steve's side ([`Registrations`], table `chan_app_registrations`, migration 23):
//! - one active attempt per person and workspace; asking again returns it;
//! - the issued secret is sealed outside any transaction and written in the same transaction that
//!   moves the row from `pending` to `credentials_saved` (so a cancel committed first leaves no
//!   credential, and a credential always has its row);
//! - binding verifies that credential and creates a **new** `feishu_app` connection on it
//!   ([`Connections::bind_existing`]): a failed check keeps the credential (`credentials_saved`
//!   with an error, `retry` checks again, never a second begin), an App ID that already has a
//!   connection here is refused, and the row turns `succeeded` in the binding transaction only if
//!   it is still `verifying` (a cancel committed first rolls the connection back);
//! - every route and the background work act as the initiator; nobody else sees an attempt.
//!
//! Never returned, logged or sent in an event: `device_code`, `client_secret`. Errors are ours,
//! short and fixed; the provider's text is not passed on.

mod protocol;
mod routes;
mod service;

pub use protocol::{ARCHETYPE, Bases, Begun, Brand, PATH, Poll, Secret, classify, parse_begin};
pub use routes::routes;
pub use service::{Kind, Registrations, state};
