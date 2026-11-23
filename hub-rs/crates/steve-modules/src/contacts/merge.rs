//! Merging and splitting contacts, and the decisions the contacts module opens (slice P4).
//!
//! - [`merge`] moves everything of `from` onto `into` in one transaction — attribute values (exact
//!   duplicates stay behind), links at either end (jobs, `reports_to`, people on items), `mem_about`,
//!   team memberships, `entity_tags`, and whatever the [`PartyHooks`] move (accounts) — and records each
//!   moved key in `merges.moved`, so that
//! - [`split`] moves back exactly those rows that are still on `into`; whatever you changed by hand
//!   after the merge (an account moved elsewhere, a deleted attribute, a link's props) stays as it is.
//! - Decision handlers: `party_merge` (`merge` / `not_same` on a merge proposal), `attr_value` and
//!   `attr_def` (`yes` / `no` on a candidate value or a proposed definition); each can be undone.
//!
//! Integration (integrator): `contacts::merge::register(&mut registry, &ports)` with the same ports the
//! contacts router gets (the handler needs the [`PartyHooks`]). The routes are merged by
//! [`super::routes`].

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use steve_store::W;
use steve_store::base::links::{LinkKey, Links, MovedLink};
use steve_store::base::{Level, ObjRef, Viewer};
use steve_store::knowledge::Memories;
use steve_store::{
    Attributes, ContactMerge, ContactMerges, Decision, DecisionRows, Link, MembershipRow,
    MergeProposals, TagRows, Teams, now, uid,
};

use super::repo::{self, AttrDef, AttrValue, PARTY, Party, PartyPatch};
use super::service::{self as svc, ContactBrief};
use super::{PartyHooks, Ports};
use crate::decisions::{self, DecisionCx, DecisionHandler, DecisionOutcome};
use crate::{ApiError, ApiResult, Body, Caller, Modules, Registry};

fn pref(id: &str) -> ObjRef {
    ObjRef::new(PARTY, id)
}

// ---------------------------------------------------------------- what a merge moved

/// A link of `from` dropped because `into` already had the same one; a split puts it back.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DroppedLink {
    pub key: LinkKey,
    pub props: Value,
    pub source: String,
    pub confidence: f64,
    pub created: i64,
}

/// A team membership of `from`. `dropped` = `into` was already in the team, so the row was deleted
/// rather than moved.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MovedMember {
    pub team_id: String,
    pub role: String,
    pub source: String,
    pub joined: i64,
    #[serde(default)]
    pub dropped: bool,
}

/// `merges.moved`. The `*_shared` / `*_dropped` lists are what `into` already had: the split restores
/// `from`'s side without taking `into`'s away.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Moved {
    /// Attribute value ids moved to `into` (exact duplicates of `into`'s values stay on `from`).
    pub attrs: Vec<String>,
    pub links: Vec<MovedLink>,
    pub links_dropped: Vec<DroppedLink>,
    /// Memories moved from `from` to `into`.
    pub mems: Vec<String>,
    /// Memories that were about both.
    pub mems_shared: Vec<String>,
    pub teams: Vec<MovedMember>,
    pub tags: Vec<String>,
    pub tags_shared: Vec<String>,
    /// Steward rows (they move like accounts).
    pub stewards: super::stewards::MovedStewards,
    /// [`PartyHooks::on_merge`] results by [`PartyHooks::name`].
    pub hooks: Map<String, Value>,
}

type MergeRow = ContactMerge<Moved>;

fn merge_get(tx: &mut W<'_>, id: &str) -> ApiResult<Option<MergeRow>> {
    Ok(ContactMerges(tx).get(id)?)
}

fn later_merge(tx: &mut W<'_>, merge: &MergeRow) -> ApiResult<bool> {
    Ok(ContactMerges(tx).has_later(merge)?)
}

fn set_merged(tx: &mut W<'_>, id: &str, into: Option<&str>) -> ApiResult<()> {
    Ok(tx.contacts().set_merged_into(id, into)?)
}

// ---------------------------------------------------------------- merge

fn own_contact(tx: &mut W<'_>, viewer: &Viewer, id: &str) -> ApiResult<Party> {
    svc::own_party_in(tx, viewer, id, "contact", "联系人", false)
}

fn merge_attributes(
    tx: &mut W<'_>,
    ws: &str,
    from: &Party,
    into: &Party,
    moved: &mut Moved,
) -> ApiResult<()> {
    let mut attrs = Attributes(tx);
    let theirs = attrs.values(ws, &into.id)?;
    for v in attrs.values(ws, &from.id)? {
        if theirs
            .iter()
            .any(|t| t.key == v.key && t.label == v.label && t.value == v.value)
        {
            continue;
        }
        attrs.move_value(&v.id, &from.id, &into.id)?;
        moved.attrs.push(v.id);
    }
    Ok(())
}

fn merge_links(
    tx: &mut W<'_>,
    ws: &str,
    fr: &ObjRef,
    ir: &ObjRef,
    moved: &mut Moved,
) -> ApiResult<()> {
    let mut links = Links(tx);
    // Links: remember the ones `into` already has (retarget drops them), then move the rest.
    let mut mine = links.from(ws, fr, None)?;
    mine.extend(links.to(ws, fr, None)?);
    let swap = |r: &ObjRef| if *r == *fr { ir.clone() } else { r.clone() };
    for l in mine {
        let key = LinkKey {
            subject: ObjRef::new(&l.subject_type, &l.subject_id),
            predicate: l.predicate.clone(),
            object: ObjRef::new(&l.object_type, &l.object_id),
        };
        let after = LinkKey {
            subject: swap(&key.subject),
            predicate: key.predicate.clone(),
            object: swap(&key.object),
        };
        if after != key && links.get(ws, &after)?.is_some() {
            moved.links_dropped.push(DroppedLink {
                key,
                props: l.props,
                source: l.source,
                confidence: l.confidence,
                created: l.created,
            });
        }
    }
    moved.links = links.retarget(ws, fr, ir)?;
    Ok(())
}

fn merge_memories_and_tags(
    tx: &mut W<'_>,
    ws: &str,
    from: &Party,
    ir: &ObjRef,
    moved: &mut Moved,
) -> ApiResult<()> {
    let fr = pref(&from.id);
    let mut memories = Memories(tx);
    let prior = memories.about_ids(ws, ir)?;
    for id in memories.move_subjects(ws, &fr, ir, None)? {
        if prior.contains(&id) {
            moved.mems_shared.push(id);
        } else {
            moved.mems.push(id);
        }
    }
    let mut tags = TagRows(tx);
    let prior = tags.of(ws, ir)?;
    for id in tags.move_between(ws, &fr, ir, None)? {
        if prior.contains(&id) {
            moved.tags_shared.push(id);
        } else {
            moved.tags.push(id);
        }
    }
    Ok(())
}

fn merge_memberships(
    tx: &mut W<'_>,
    ws: &str,
    from: &Party,
    into: &Party,
    moved: &mut Moved,
) -> ApiResult<()> {
    let rows = Teams(tx).memberships(ws, &from.id)?;
    for row in rows {
        let mut teams = Teams(tx);
        let dropped = teams.has_member(&row.team, &into.id)?;
        if dropped {
            teams.remove_member(ws, &row.team, &from.id)?;
        } else {
            teams.move_member(&row.team, &from.id, &into.id)?;
        }
        tx.emit(crate::accounts::service::party_changed(ws, &row.team));
        moved.teams.push(MovedMember {
            team_id: row.team,
            role: row.role,
            source: row.source,
            joined: row.joined,
            dropped,
        });
    }
    Ok(())
}

/// Merges `from` into `into` inside `tx` and returns the merge id. Both must be active contacts of
/// the viewer's workspace, visible to them, not the same, and `from` not “我”.
pub fn merge_tx(
    tx: &mut W<'_>,
    hooks: &[Arc<dyn PartyHooks>],
    viewer: &Viewer,
    actor: &str,
    from_id: &str,
    into_id: &str,
) -> ApiResult<String> {
    let ws = viewer.ws.as_str();
    let from = own_contact(tx, viewer, from_id)?;
    let into = own_contact(tx, viewer, into_id)?;
    if from.id == into.id {
        return Err(ApiError::rule(steve_i18n::t!("modules.merge.into_self")));
    }
    if from.is_me() {
        return Err(ApiError::rule(steve_i18n::t!(
            "modules.merge.self_into_other"
        )));
    }
    if from.status != "active" || into.status != "active" {
        return Err(ApiError::conflict(steve_i18n::t!(
            "modules.merge.already_merged"
        )));
    }
    let (fr, ir) = (pref(&from.id), pref(&into.id));
    let mut moved = Moved::default();

    merge_attributes(tx, ws, &from, &into, &mut moved)?;

    merge_links(tx, ws, &fr, &ir, &mut moved)?;

    merge_memories_and_tags(tx, ws, &from, &ir, &mut moved)?;

    merge_memberships(tx, ws, &from, &into, &mut moved)?;

    moved.stewards = super::stewards::merge_move(tx, ws, &from.id, &into.id)?;

    for h in hooks {
        let v = h.on_merge(tx, ws, &from.id, &into.id)?;
        moved.hooks.insert(h.name().to_string(), v);
    }

    svc::revise(
        tx,
        &from,
        &PartyPatch {
            status: Some("merged".into()),
            ..Default::default()
        },
        json!({ "status": from.status, "merged_into": null }),
        json!({ "status": "merged", "merged_into": into.id }),
        actor,
    )?;
    set_merged(tx, &from.id, Some(&into.id))?;
    tx.contacts().touch(&into.id)?;

    let id = uid("mg");
    ContactMerges(tx).insert(&ContactMerge {
        id: id.clone(),
        workspace_id: ws.into(),
        from_id: from.id.clone(),
        into_id: into.id.clone(),
        moved: &moved,
        actor: actor.into(),
        created: now(),
        split: None,
    })?;
    settle_proposals(tx, ws, &from.id, &into.id)?;
    tx.audit(
        ws,
        actor,
        "party.merge",
        &from.id,
        &json!({
            "into": into.id, "merge": id,
            "attrs": moved.attrs.len(), "links": moved.links.len(), "mems": moved.mems.len(),
            "teams": moved.teams.len(), "tags": moved.tags.len(),
        }),
    )?;
    tx.emit(crate::accounts::service::party_changed(ws, &from.id));
    tx.emit(crate::accounts::service::party_changed(ws, &into.id));
    Ok(id)
}

/// After `from` is merged: the open proposal of this very pair becomes `merged` (its card resolved with
/// `merge`), other open proposals involving `from` are moot and expire.
fn settle_proposals(tx: &mut W<'_>, ws: &str, from: &str, into: &str) -> ApiResult<()> {
    for p in MergeProposals(tx).involving(ws, from)? {
        let (status, d_status, result) = if p.from == from && p.into == into {
            ("merged", "resolved", json!({"choice":"merge"}))
        } else {
            ("expired", "expired", json!({"expired":true}))
        };
        MergeProposals(tx).set_status(&p.id, status)?;
        if let Some(d) = p.decision {
            DecisionRows(tx).settle(&d, &result, d_status, &Value::Null)?;
        }
    }
    Ok(())
}

fn set_proposal(tx: &mut W<'_>, id: &str, status: &str) -> ApiResult<()> {
    Ok(MergeProposals(tx).set_status(id, status)?)
}

// ---------------------------------------------------------------- split

fn restore_memberships(
    tx: &mut W<'_>,
    ws: &str,
    from: &Party,
    into: &Party,
    mv: &Moved,
) -> ApiResult<()> {
    for member in &mv.teams {
        if member.dropped {
            if tx.contacts().party(&member.team_id)?.is_some() {
                Teams(tx).restore_member(
                    ws,
                    &from.id,
                    &MembershipRow {
                        team: member.team_id.clone(),
                        role: member.role.clone(),
                        source: member.source.clone(),
                        joined: member.joined,
                    },
                )?;
            }
        } else if !Teams(tx).has_member(&member.team_id, &from.id)? {
            Teams(tx).move_member(&member.team_id, &into.id, &from.id)?;
        }
        tx.emit(crate::accounts::service::party_changed(ws, &member.team_id));
    }
    Ok(())
}

fn restore_memories_and_tags(
    tx: &mut W<'_>,
    ws: &str,
    from: &Party,
    into: &Party,
    mv: &Moved,
) -> ApiResult<()> {
    let (fr, ir) = (pref(&from.id), pref(&into.id));
    let mut tags = TagRows(tx);
    if !mv.tags.is_empty() {
        tags.move_between(ws, &ir, &fr, Some(&mv.tags))?;
    }
    for tag in &mv.tags_shared {
        if tags.contains(ws, tag)? {
            tags.attach(ws, tag, &fr)?;
        }
    }
    let mut memories = Memories(tx);
    if !mv.mems.is_empty() {
        memories.move_subjects(ws, &ir, &fr, Some(&mv.mems))?;
    }
    for mem in &mv.mems_shared {
        if memories.get(mem)?.is_some() {
            memories.add_subject(ws, mem, &fr)?;
        }
    }
    Ok(())
}

fn restore_links_and_attributes(
    tx: &mut W<'_>,
    ws: &str,
    from: &Party,
    into: &Party,
    mv: &Moved,
) -> ApiResult<()> {
    let mut links = Links(tx);
    links.restore(ws, &mv.links)?;
    for l in &mv.links_dropped {
        links.insert_preserved(
            ws,
            &l.key,
            &Link {
                workspace_id: ws.into(),
                subject_type: l.key.subject.kind.clone(),
                subject_id: l.key.subject.id.clone(),
                predicate: l.key.predicate.clone(),
                object_type: l.key.object.kind.clone(),
                object_id: l.key.object.id.clone(),
                props: l.props.clone(),
                source: l.source.clone(),
                confidence: l.confidence,
                created: l.created,
            },
        )?;
    }
    for id in &mv.attrs {
        Attributes(tx).move_value(id, &into.id, &from.id)?;
    }
    Ok(())
}

/// Splits merge `id` inside `tx`: only the latest unsplit merge into its `into` can be split
/// (`409 先拆开之后的合并`, also while `into` is itself merged into someone else). Moves back what is
/// still on `into`.
pub fn split_tx(
    tx: &mut W<'_>,
    hooks: &[Arc<dyn PartyHooks>],
    viewer: &Viewer,
    actor: &str,
    id: &str,
) -> ApiResult<()> {
    let ws = viewer.ws.as_str();
    let m = merge_get(tx, id)?
        .filter(|m| m.workspace_id == ws)
        .ok_or_else(|| ApiError::not_found("合并记录"))?;
    let into = own_contact(tx, viewer, &m.into_id).map_err(|_| ApiError::not_found("合并记录"))?;
    let from = tx
        .contacts()
        .party(&m.from_id)?
        .ok_or_else(|| ApiError::not_found("合并记录"))?;
    if m.split.is_some() {
        return Err(ApiError::conflict(steve_i18n::t!(
            "modules.merge.already_split"
        )));
    }
    if into.status != "active" || later_merge(tx, &m)? {
        return Err(ApiError::conflict(steve_i18n::t!(
            "modules.merge.split_later_first"
        )));
    }
    if from.status != "merged" || from.merged_into.as_deref() != Some(into.id.as_str()) {
        return Err(ApiError::conflict(steve_i18n::t!(
            "modules.merge.not_merged"
        )));
    }
    let mv = &m.moved;

    for h in hooks.iter().rev() {
        let v = mv.hooks.get(h.name()).cloned().unwrap_or(Value::Null);
        h.on_split(tx, ws, &from.id, &into.id, &v)?;
    }
    super::stewards::split_back(tx, ws, &from.id, &into.id, &mv.stewards)?;

    restore_memberships(tx, ws, &from, &into, mv)?;

    restore_memories_and_tags(tx, ws, &from, &into, mv)?;

    restore_links_and_attributes(tx, ws, &from, &into, mv)?;

    svc::revise(
        tx,
        &from,
        &PartyPatch {
            status: Some("active".into()),
            ..Default::default()
        },
        json!({ "status": "merged", "merged_into": into.id }),
        json!({ "status": "active", "merged_into": null }),
        actor,
    )?;
    set_merged(tx, &from.id, None)?;
    tx.contacts().touch(&into.id)?;
    ContactMerges(tx).split(&m.id)?;
    tx.audit(
        ws,
        actor,
        "party.split",
        &from.id,
        &json!({ "into": into.id, "merge": m.id }),
    )?;
    tx.emit(crate::accounts::service::party_changed(ws, &from.id));
    tx.emit(crate::accounts::service::party_changed(ws, &into.id));
    Ok(())
}

// ---------------------------------------------------------------- REST

/// `POST /contacts/:id/merge`.
#[derive(Debug, Clone, Deserialize)]
pub struct MergeBody {
    pub into: String,
}

/// `{merge_id, into: Contact}`.
#[derive(Debug, Clone, Serialize)]
pub struct Merged {
    pub merge_id: String,
    pub into: svc::Contact,
}

/// One open proposal of `GET /merges`.
#[derive(Debug, Clone, Serialize)]
pub struct Proposal {
    pub id: String,
    pub from: ContactBrief,
    pub into: ContactBrief,
    pub account: Option<ProposalAccount>,
    pub reason: String,
    pub decision_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProposalAccount {
    pub id: String,
    pub handle: String,
    pub system: String,
}

/// `GET /merges`: `{proposals, unconfirmed, quiet}` (需要你判断).
#[derive(Debug, Clone, Serialize)]
pub struct Pending {
    pub proposals: Vec<Proposal>,
    /// Most recently active first; people only seen in spaces you muted are left out.
    pub unconfirmed: Vec<PendingContact>,
    /// How many were left out for that.
    pub quiet: usize,
}

/// An unconfirmed contact with what tells it apart.
#[derive(Debug, Clone, Serialize)]
pub struct PendingContact {
    #[serde(flatten)]
    pub brief: ContactBrief,
    /// What to call it: its name, or “未知飞书用户” while the name is still a raw handle.
    pub label: String,
    /// No name yet (`label` is the placeholder).
    pub nameless: bool,
    /// Its first account: `{system, system_name, handle}`.
    pub account: Option<PendingAccount>,
    pub seen: Option<crate::contacts::Seen>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PendingAccount {
    pub system: String,
    pub system_name: String,
    pub handle: String,
}

/// The first account of a party: `(system id, system name, handle_display)`.
fn first_account(
    r: &mut impl steve_store::Read,
    ws: &str,
    party: &str,
) -> ApiResult<Option<PendingAccount>> {
    let Some(account) = steve_store::Accounts(r)
        .of_party(ws, party)?
        .into_iter()
        .filter(|account| account.status == "active")
        .min_by(|a, b| a.created.cmp(&b.created).then_with(|| a.id.cmp(&b.id)))
    else {
        return Ok(None);
    };
    let system = account.system_id;
    let system_name = steve_store::Systems(r)
        .get(ws, &system)?
        .map(|row| crate::shown::system_name(&system, row.builtin, &row.name))
        .unwrap_or_else(|| system.clone());
    Ok(Some(PendingAccount {
        system,
        system_name,
        handle: account.handle_display,
    }))
}

fn me_or<'a>(id: &'a str, me: &'a Party) -> &'a str {
    if id == "me" { &me.id } else { id }
}

/// `POST /contacts/:id/merge {into}` (`me` allowed as `into`, refused as `id`).
pub fn merge(
    m: &Modules,
    ports: &Ports,
    who: &Caller,
    id: &str,
    b: MergeBody,
) -> ApiResult<Merged> {
    let (viewer, me) = svc::start(m, who)?;
    let (from, into) = (me_or(id, &me).to_string(), me_or(&b.into, &me).to_string());
    let merge_id = m
        .store
        .write(|tx| merge_tx(tx, &ports.hooks, &viewer, &who.me, &from, &into))?;
    let into = m.store.read::<_, steve_store::StoreError>(|r| {
        Ok(
            svc::own_party_in(r, &viewer, &into, "contact", "联系人", false)
                .and_then(|p| svc::contact(r, &viewer, &p)),
        )
    })??;
    Ok(Merged { merge_id, into })
}

/// `POST /merges/:id/split`.
pub fn split(m: &Modules, ports: &Ports, who: &Caller, id: &str) -> ApiResult<()> {
    let (viewer, _) = svc::start(m, who)?;
    m.store
        .write(|tx| split_tx(tx, &ports.hooks, &viewer, &who.me, id))
}

/// `GET /merges`: open proposals whose two contacts are still active and visible, and the unconfirmed
/// contacts.
pub fn pending(m: &Modules, ports: &Ports, who: &Caller) -> ApiResult<Pending> {
    let (viewer, _) = svc::start(m, who)?;
    m.store.read::<_, steve_store::StoreError>(|r| {
        Ok((|| {
            let rows = MergeProposals(r).pending(&viewer.ws)?;
            let mut proposals = Vec::new();
            for row in rows {
                let (id, f, i, account, reason, decision_id) = (
                    row.id,
                    row.from,
                    row.into,
                    row.account,
                    row.reason,
                    row.decision,
                );
                let (Ok(f), Ok(i)) = (
                    svc::own_party_in(r, &viewer, &f, "contact", "联系人", true),
                    svc::own_party_in(r, &viewer, &i, "contact", "联系人", true),
                ) else {
                    continue;
                };
                let account = match account {
                    Some(a) => steve_store::Accounts(r).get(&a)?.map(|a| ProposalAccount {
                        id: a.id,
                        handle: a.handle_display,
                        system: a.system_id,
                    }),
                    None => None,
                };
                proposals.push(Proposal {
                    id,
                    from: svc::brief(r, &viewer, ports, &f)?,
                    into: svc::brief(r, &viewer, ports, &i)?,
                    account,
                    reason,
                    decision_id,
                });
            }
            let q = repo::ContactQuery {
                kind: Some("unconfirmed".into()),
                limit: 500,
                ..Default::default()
            };
            let parties = r.contacts().contacts(&viewer, &q)?;
            let ids: Vec<String> = parties.iter().map(|p| p.id.clone()).collect();
            let mut seen: std::collections::HashMap<String, crate::contacts::Seen> =
                match &ports.spaces {
                    Some(s) => s.seen(&m.store, &viewer, &ids)?.into_iter().collect(),
                    None => Default::default(),
                };
            let mut unconfirmed = Vec::new();
            let mut quiet = 0;
            for p in &parties {
                let seen = seen.remove(&p.id);
                if seen.as_ref().is_some_and(|s| s.quiet) {
                    quiet += 1;
                    continue;
                }
                let account = first_account(r, &viewer.ws, &p.id)?;
                let nameless = crate::accounts::looks_like_handle(&p.name)
                    || account.as_ref().is_some_and(|a| a.handle == p.name);
                let label = if nameless {
                    match account.as_ref() {
                        Some(a) => {
                            steve_i18n::t!("modules.merge.unknown_user", system = a.system_name)
                        }
                        None => steve_i18n::t!("modules.merge.unknown_user_plain"),
                    }
                } else {
                    p.name.clone()
                };
                unconfirmed.push(PendingContact {
                    brief: svc::brief(r, &viewer, ports, p)?,
                    label,
                    nameless,
                    account,
                    seen,
                });
            }
            // the most recently active first, then the named ones
            unconfirmed.sort_by(|a, b| {
                let at = |x: &PendingContact| x.seen.as_ref().and_then(|s| s.last_at).unwrap_or(0);
                at(b).cmp(&at(a)).then(a.nameless.cmp(&b.nameless))
            });
            Ok(Pending {
                proposals,
                unconfirmed,
                quiet,
            })
        })())
    })?
}

type P = Extension<Arc<Ports>>;

/// `GET /merges`, `POST /contacts/:id/merge`, `POST /merges/:id/split`; merged into [`super::routes`]
/// (which adds the [`Ports`] extension).
pub(crate) fn routes() -> Router<Modules> {
    Router::new()
        .route("/merges", get(get_pending))
        .route("/contacts/{id}/merge", post(post_merge))
        .route("/merges/{id}/split", post(post_split))
}

async fn get_pending(
    State(m): State<Modules>,
    Extension(ports): P,
    who: Caller,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| Ok(Json(serde_json::to_value(pending(m, &ports, &who)?)?)))
        .await
}

async fn post_merge(
    State(m): State<Modules>,
    Extension(ports): P,
    who: Caller,
    Path(id): Path<String>,
    Body(b): Body<MergeBody>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| Ok(Json(serde_json::to_value(merge(m, &ports, &who, &id, b)?)?)))
        .await
}

async fn post_split(
    State(m): State<Modules>,
    Extension(ports): P,
    who: Caller,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| {
        split(m, &ports, &who, &id)?;
        Ok(Json(json!({ "ok": true })))
    })
    .await
}

// ---------------------------------------------------------------- decision handlers

/// Registers the `party_merge`, `attr_value` and `attr_def` decision handlers. `ports` = the ports the
/// contacts router gets (merging through a decision moves accounts too).
pub fn register(reg: &mut Registry, ports: &Ports) {
    reg.decision(
        "party_merge",
        Box::new(PartyMerge {
            hooks: ports.hooks.clone(),
        }),
    )
    .decision("attr_value", Box::new(AttrValueConfirm))
    .decision("attr_def", Box::new(AttrDefConfirm))
    .decision(
        super::stewards::SUBJECT,
        Box::new(super::stewards::StewardConfirm),
    );
}

/// `merge` → merge the proposal's pair; `not_same` → `rejected` (the pair is never proposed again).
/// Undo: split the merge (only while it is the latest), or reopen the proposal.
pub struct PartyMerge {
    hooks: Vec<Arc<dyn PartyHooks>>,
}

fn proposal(tx: &mut W<'_>, id: &str) -> ApiResult<Option<(String, String, String, String)>> {
    Ok(MergeProposals(tx)
        .get(id)?
        .map(|p| (p.ws, p.from, p.into, p.status)))
}

impl DecisionHandler for PartyMerge {
    fn resolve(&self, tx: &mut W<'_>, cx: &DecisionCx, d: &Decision) -> ApiResult<DecisionOutcome> {
        let (ws, from, into, status) = proposal(tx, &d.subject_id)?
            .filter(|p| p.0 == cx.ws)
            .ok_or_else(|| ApiError::not_found("合并提议"))?;
        if status != "open" {
            return Err(ApiError::conflict(steve_i18n::t!(
                "modules.merge.proposal_handled"
            )));
        }
        let mut result = decisions::choice_result(cx);
        match cx.choice {
            "merge" => {
                // Marked first so that merge_tx leaves this decision to the dispatcher.
                set_proposal(tx, &d.subject_id, "merged")?;
                let viewer = Viewer::bare(&ws, cx.actor);
                let id = merge_tx(tx, &self.hooks, &viewer, cx.actor, &from, &into)?;
                result["merge_id"] = json!(id);
            }
            _ => set_proposal(tx, &d.subject_id, "rejected")?,
        }
        Ok(result.into())
    }

    fn before_undo(&self, tx: &mut W<'_>, d: &Decision) -> ApiResult<()> {
        let Some((ws, _, _, _)) = proposal(tx, &d.subject_id)? else {
            return Ok(());
        };
        if let Some(mg) = d.result.get("merge_id").and_then(Value::as_str) {
            let actor = d_actor(tx, mg)?;
            let viewer = Viewer::bare(&ws, &actor);
            split_tx(tx, &self.hooks, &viewer, &actor, mg)?;
        }
        set_proposal(tx, &d.subject_id, "open")
    }
}

fn d_actor(c: &mut W<'_>, merge: &str) -> ApiResult<String> {
    merge_get(c, merge)?
        .map(|m| m.actor)
        .ok_or_else(|| ApiError::not_found("合并记录"))
}

fn value_snapshot(v: &AttrValue) -> Value {
    json!({
        "id": v.id, "party_id": v.party_id, "key": v.key, "label": v.label, "value": v.value,
        "source": v.source, "source_label": v.source_label, "source_ref": v.source_ref,
        "level": v.level.code(), "created": v.created,
    })
}

/// `yes` → the candidate value is `confirmed` and older confirmed values of the same `key + label` go
/// `stale`; `no` → deleted. Undo puts it back as a candidate (restoring a deleted one, un-staling what
/// the confirmation staled).
pub struct AttrValueConfirm;

impl DecisionHandler for AttrValueConfirm {
    fn resolve(&self, tx: &mut W<'_>, cx: &DecisionCx, d: &Decision) -> ApiResult<DecisionOutcome> {
        let v = Attributes(tx)
            .value(&d.subject_id)?
            .filter(|v| v.workspace_id == cx.ws)
            .ok_or_else(|| ApiError::not_found("属性"))?;
        let mut result = decisions::choice_result(cx);
        if cx.choice == "yes" {
            let mut attrs = Attributes(tx);
            attrs.update_value(&v.id, None, None, Some("confirmed"))?;
            let staled = attrs.stale_siblings(&v)?;
            result["staled"] = json!(staled);
        } else {
            Attributes(tx).delete_value(&v.id)?;
            result["value"] = value_snapshot(&v);
        }
        tx.contacts().touch(&v.party_id)?;
        tx.emit(crate::accounts::service::party_changed(cx.ws, &v.party_id));
        Ok(result.into())
    }

    fn before_undo(&self, tx: &mut W<'_>, d: &Decision) -> ApiResult<()> {
        if let Some(s) = d.result.get("value") {
            let str_of = |k: &str| s.get(k).and_then(Value::as_str);
            let party = str_of("party_id").unwrap_or_default();
            if tx.contacts().party(party)?.is_none() {
                return Ok(());
            }
            if let (Some(key), Some(label), Some(value), Some(source)) = (
                str_of("key"),
                str_of("label"),
                str_of("value"),
                str_of("source"),
            ) {
                Attributes(tx).restore_value(&AttrValue {
                    id: d.subject_id.clone(),
                    workspace_id: d.workspace_id.clone(),
                    party_id: party.into(),
                    key: key.into(),
                    label: label.into(),
                    value: value.into(),
                    source: source.into(),
                    source_label: str_of("source_label").unwrap_or_default().into(),
                    source_ref: str_of("source_ref").map(String::from),
                    level: Level::from_db(str_of("level").unwrap_or(Level::Reasoning.code())),
                    status: "candidate".into(),
                    decision_id: Some(d.id.clone()),
                    created: s.get("created").and_then(Value::as_i64).unwrap_or_else(now),
                    updated: now(),
                })?;
            }
            tx.emit(crate::accounts::service::party_changed(
                &d.workspace_id,
                party,
            ));
            return Ok(());
        }
        let Some(v) = Attributes(tx).value(&d.subject_id)? else {
            return Ok(());
        };
        Attributes(tx).update_value(&v.id, None, None, Some("candidate"))?;
        for id in d
            .result
            .get("staled")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            Attributes(tx).restore_confirmed(id)?;
        }
        tx.emit(crate::accounts::service::party_changed(
            &d.workspace_id,
            &v.party_id,
        ));
        Ok(())
    }

    /// 30 days without an answer: the candidate is dropped.
    fn expire(&self, tx: &mut W<'_>, d: &Decision) -> ApiResult<()> {
        if let Some(v) = Attributes(tx)
            .value(&d.subject_id)?
            .filter(|v| v.status == "candidate")
        {
            Attributes(tx).delete_value(&v.id)?;
            tx.emit(crate::accounts::service::party_changed(
                &d.workspace_id,
                &v.party_id,
            ));
        }
        Ok(())
    }
}

fn def_snapshot(x: &AttrDef) -> Value {
    json!({
        "applies": x.applies, "key": x.key, "label": x.label, "type": x.kind, "options": x.options,
        "default_level": x.default_level.code(), "source": x.source, "created": x.created,
    })
}

/// `yes` → the proposed definition becomes `active`; `no` → deleted. Undo makes it `proposed` again.
pub struct AttrDefConfirm;

impl DecisionHandler for AttrDefConfirm {
    fn resolve(&self, tx: &mut W<'_>, cx: &DecisionCx, d: &Decision) -> ApiResult<DecisionOutcome> {
        let x = Attributes(tx)
            .definition(&d.subject_id)?
            .filter(|x| x.workspace_id == cx.ws)
            .ok_or_else(|| ApiError::not_found("属性"))?;
        let mut result = decisions::choice_result(cx);
        if cx.choice == "yes" {
            Attributes(tx).set_definition_status(&x.id, "active", None)?;
        } else {
            Attributes(tx).delete_definition(&x.id)?;
            result["def"] = def_snapshot(&x);
        }
        Ok(result.into())
    }

    fn before_undo(&self, tx: &mut W<'_>, d: &Decision) -> ApiResult<()> {
        if let Some(s) = d.result.get("def") {
            let str_of = |k: &str| s.get(k).and_then(Value::as_str);
            if let (Some(applies), Some(key), Some(label), Some(kind), Some(level), Some(source)) = (
                str_of("applies"),
                str_of("key"),
                str_of("label"),
                str_of("type"),
                str_of("default_level"),
                str_of("source"),
            ) {
                Attributes(tx).restore_definition(&AttrDef {
                    id: d.subject_id.clone(),
                    workspace_id: d.workspace_id.clone(),
                    applies: applies.into(),
                    key: key.into(),
                    label: label.into(),
                    kind: kind.into(),
                    options: s
                        .get("options")
                        .cloned()
                        .and_then(|v| serde_json::from_value(v).ok())
                        .unwrap_or_default(),
                    default_level: Level::from_db(level),
                    source: source.into(),
                    status: "proposed".into(),
                    builtin: d.workspace_id == "*",
                    created: s.get("created").and_then(Value::as_i64).unwrap_or_else(now),
                })?;
            }
            return Ok(());
        }
        Ok(Attributes(tx).set_definition_status(&d.subject_id, "proposed", Some("active"))?)
    }

    fn expire(&self, tx: &mut W<'_>, d: &Decision) -> ApiResult<()> {
        Ok(Attributes(tx).delete_proposal(&d.subject_id)?)
    }
}
