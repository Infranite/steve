//! Claiming your own identity into “我”: a connection that proves who signed in (an OAuth user
//! token, [`super::connections::ConnectionProvider::claims_self`]) may find that a channel already
//! saw that person and made an unconfirmed contact for them (a bot copy of your own message, before
//! you authorized). [`claim_self`] merges that contact into “我” (a normal, splittable merge) before
//! the account is attached, instead of the connect failing with “这个账号已经记在「…」名下”.
//! A contact someone confirmed by hand is never merged automatically; the connect then says who
//! holds it. [`super::connections::Connections`] runs it in the connect transaction.

use std::sync::Arc;

use steve_store::Tx;
use steve_store::base::Viewer;

use super::{repo as arepo, service as asvc};
use crate::ApiResult;
use crate::contacts::PartyHooks;
use crate::contacts::merge::merge_tx;
use crate::contacts::profile;
use crate::contacts::repo::Party;

/// May an account on `p` be claimed for “我” without asking: an active contact nobody confirmed by
/// hand — `unconfirmed` (made by `identify`), or made `real` only by a run on the provider's name
/// (`source` chat / sync, no kind or name a person set): the names run promotes the contact a bot
/// made for you some 20 s after your first message, long before you authorize.
pub fn claimable(tx: &Tx, ws: &str, p: &Party) -> ApiResult<bool> {
    if p.kind != "contact" || p.status != "active" || p.is_me() {
        return Ok(false);
    }
    Ok(match p.contact_kind.as_deref() {
        Some("unconfirmed") => true,
        Some("real") if matches!(p.source.as_str(), "chat" | "sync") => {
            !profile::kind_edited(tx, ws, &p.id)? && !profile::name_edited(tx, ws, &p.id)?
        }
        _ => false,
    })
}

/// Before the proven identity `handles` of `system` (the primary handle first, then the others of
/// the same identity) is attached to `me` (“我” of `actor`): merges each claimable contact holding
/// one of them into `me`. Returns the merge ids (one per contact merged). Handles that do not
/// normalize in `system`, or that nobody holds, are skipped; so is a contact `viewer` cannot see.
pub fn claim_self(
    tx: &Tx,
    hooks: &[Arc<dyn PartyHooks>],
    viewer: &Viewer,
    actor: &str,
    me: &Party,
    system: &str,
    handles: &[&str],
) -> ApiResult<Vec<String>> {
    let ws = viewer.ws.as_str();
    let Some(system) = arepo::system(tx, ws, system)? else {
        return Ok(Vec::new());
    };
    let mut merged = Vec::new();
    for raw in handles {
        let Some(h) = asvc::normalize(&system, raw) else {
            continue;
        };
        let Some(account) = arepo::by_handle(tx, ws, &system.family, &h.handle)? else {
            continue;
        };
        let Some(a) = arepo::get(tx, &account)? else {
            continue;
        };
        let Some(p) = asvc::current_party(tx, &a.party_id)? else {
            continue;
        };
        if p.id == me.id || !claimable(tx, ws, &p)? || !viewer.sees(&p.visibility, &p.created_by) {
            continue;
        }
        let id = merge_tx(tx, hooks, viewer, actor, &p.id, &me.id)?;
        tracing::info!(merge = %id, "accounts: your unconfirmed contact merged into 我");
        merged.push(id);
    }
    Ok(merged)
}
