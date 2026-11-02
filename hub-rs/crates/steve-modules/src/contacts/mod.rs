//! Contacts, organizations and teams (slices P1, P2, P3 of steve-rs-plan/modules/contacts.md): “我”, contacts,
//! the workspace's own organization, organizations with domains, jobs (`works_at` links), attribute
//! definitions and values with disclosure levels, virtual and org-chart teams ([`teams`]); P3: memories
//! about parties ([`memory`]), people on items, `@` and the guess from names ([`item_people`]), the `@`
//! menu and ⌘K ([`mentions`]), the agents' MCP tools ([`mcp`]) and contact cards for prompts ([`cards`]);
//! ownership (归属与负责人, ownership.md): [`stewards`] and its routes [`stewards_api`].
//!
//! The build plan names the crate `steve-people`; P1 and P2 landed here in steve-modules, so P3 lives next
//! to them.
//!
//! - [`repo`]: the SQL (tables of migration 4, owned by steve-store);
//! - [`service`]: the domain rules, callable from the REST handlers, MCP tools and drafts alike;
//! - [`api`]: the REST handlers; [`routes`] is what the integrator mounts under `/api`.
//!
//! Other modules plug in through [`Ports`]: [`PartyHooks`] (accounts and channel members follow a
//! deleted or merged contact inside the same transaction) and [`HandleSearch`] (account handles in
//! `GET /contacts?q=` and the account counts of `ContactBrief`), [`SpacePort`] (a team's bound group) and
//! [`TeamSync`] (the directory sync behind `POST /teams/sync`).
//!
//! Integration (integrator): `.merge(steve_modules::contacts::routes(ports))` on the `/api` router,
//! behind the layer that inserts [`crate::Caller`].

pub mod api;
pub mod cards;
pub mod changes;
pub mod item_people;
pub mod mcp;
pub mod memory;
pub mod mentions;
pub mod merge;
pub mod ownership_mcp;
pub mod people_repo;
pub mod repo;
pub mod service;
pub mod stewards;
pub mod stewards_api;
pub mod sync;
pub mod teams;

use std::sync::Arc;

use rusqlite::Connection;
use serde_json::Value;
use steve_store::Tx;

use crate::ApiResult;

pub use api::routes;
pub use cards::{ContactCard, contact_cards, item_contact_cards};
pub use item_people::{
    Mention, TimelineEntry, TimelineSource, check_mentions, infer_item_people, link_mentions,
    mention_note,
};
pub use teams::{Seen, SeenMessage, SeenSpace, SpacePort, TeamSync};

/// Callbacks into other modules when a contact goes away or is merged (§0.1 point 8). Every method runs
/// inside the contacts transaction; an error rolls the whole operation back.
pub trait PartyHooks: Send + Sync {
    /// The key under which [`PartyHooks::on_merge`]'s result is kept in `merges.moved.hooks`.
    fn name(&self) -> &str;

    /// A contact was deleted (the accounts module deletes its accounts).
    fn on_delete(&self, tx: &Tx, ws: &str, party_id: &str) -> ApiResult<()> {
        let _ = (tx, ws, party_id);
        Ok(())
    }

    /// `from` is being merged into `into`; returns what was moved, for the split.
    fn on_merge(&self, tx: &Tx, ws: &str, from: &str, into: &str) -> ApiResult<Value> {
        let _ = (tx, ws, from, into);
        Ok(Value::Null)
    }

    /// A merge is being split; `moved` is what [`PartyHooks::on_merge`] returned.
    fn on_split(&self, tx: &Tx, ws: &str, from: &str, into: &str, moved: &Value) -> ApiResult<()> {
        let _ = (tx, ws, from, into, moved);
        Ok(())
    }
}

/// Account handles, provided by the accounts module (accounts-vault.md). Without it, `q` matches only
/// names, aliases, organizations and titles, and `accounts` counts are 0.
pub trait HandleSearch: Send + Sync {
    /// Party ids of `ws` owning an account whose handle contains `q` (case-insensitive).
    fn parties(&self, c: &Connection, ws: &str, q: &str) -> ApiResult<Vec<String>>;

    /// Number of accounts of a party.
    fn count(&self, c: &Connection, ws: &str, party_id: &str) -> ApiResult<i64>;

    /// The account handles of a party as agents may see them (`sarah@acme.com`, `飞书 Sarah`), for
    /// `steve_contacts_find`. Default: none.
    fn handles(&self, c: &Connection, ws: &str, party_id: &str) -> ApiResult<Vec<String>> {
        let _ = (c, ws, party_id);
        Ok(Vec::new())
    }
}

/// What other modules plug into the contacts module. Built once at startup by the integrator.
#[derive(Clone, Default)]
pub struct Ports {
    pub hooks: Vec<Arc<dyn PartyHooks>>,
    pub handles: Option<Arc<dyn HandleSearch>>,
    /// The channels module's spaces; without it a team's `space_id` can only be `null`.
    pub spaces: Option<Arc<dyn SpacePort>>,
    /// The directory sync (slice P6); without it `POST /teams/sync` answers `400`.
    pub sync: Option<Arc<dyn TeamSync>>,
    /// What other modules add to `GET /parties/:id/timeline` (messages, mail, meetings, events).
    pub timelines: Vec<Arc<dyn TimelineSource>>,
}
