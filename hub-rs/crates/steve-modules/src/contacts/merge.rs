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
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use steve_store::base::links::{self, LinkKey, MovedLink};
use steve_store::base::{self, Level, ObjRef, Viewer, mems, tags};
use steve_store::{Cached, Decision, Tx, now, uid};

use super::repo::{self, AttrDef, AttrValue, PARTY, Party, PartyPatch};
use super::service::{self as svc, ContactBrief};
use super::{PartyHooks, Ports};
use crate::decisions::{self, DecisionCx, DecisionHandler};
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

/// A `merges` row.
#[derive(Debug, Clone, PartialEq)]
pub struct MergeRow {
    pub id: String,
    pub workspace_id: String,
    pub from_id: String,
    pub into_id: String,
    pub moved: Moved,
    pub actor: String,
    pub created: i64,
    pub split: Option<i64>,
}

fn merge_get(c: &Connection, id: &str) -> ApiResult<Option<MergeRow>> {
    let row = c
        .prepare_cached(
            "SELECT id, workspace_id, from_id, into_id, moved, actor, created, split FROM merges WHERE id = ?",
        )?
        .query_row([id], |r| {
            Ok((
                MergeRow {
                    id: r.get(0)?,
                    workspace_id: r.get(1)?,
                    from_id: r.get(2)?,
                    into_id: r.get(3)?,
                    moved: Moved::default(),
                    actor: r.get(5)?,
                    created: r.get(6)?,
                    split: r.get(7)?,
                },
                r.get::<_, String>(4)?,
            ))
        })
        .optional()?;
    Ok(row.map(|(mut m, moved)| {
        m.moved = serde_json::from_str(&moved).unwrap_or_default();
        m
    }))
}

/// Is there a merge into `into` newer than `m` that is not split yet?
fn later_merge(c: &Connection, m: &MergeRow) -> ApiResult<bool> {
    Ok(c.prepare_cached(
        "SELECT 1 FROM merges WHERE workspace_id = ? AND into_id = ? AND split IS NULL AND id != ?
           AND (created > ? OR (created = ? AND rowid > (SELECT rowid FROM merges WHERE id = ?)))",
    )?
    .exists(params![
        m.workspace_id,
        m.into_id,
        m.id,
        m.created,
        m.created,
        m.id
    ])?)
}

fn set_merged(tx: &Tx, id: &str, into: Option<&str>) -> ApiResult<()> {
    tx.run(
        "UPDATE parties SET merged_into = ?, updated = ? WHERE id = ?",
        params![into, now(), id],
    )?;
    Ok(())
}

// ---------------------------------------------------------------- merge

fn own_contact(tx: &Tx, viewer: &Viewer, id: &str) -> ApiResult<Party> {
    svc::own_party(tx, viewer, id, "contact", "联系人", false)
}

/// Merges `from` into `into` inside `tx` and returns the merge id. Both must be active contacts of
/// the viewer's workspace, visible to them, not the same, and `from` not “我”.
pub fn merge_tx(
    tx: &Tx,
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
        return Err(ApiError::rule("不能合并到自己"));
    }
    if from.is_me() {
        return Err(ApiError::rule("不能把你自己合并到别人"));
    }
    if from.status != "active" || into.status != "active" {
        return Err(ApiError::conflict("这个联系人已经合并过了"));
    }
    let (fr, ir) = (pref(&from.id), pref(&into.id));
    let mut moved = Moved::default();

    // Attribute values: same key + label + value as one of `into`'s stays on `from`.
    let theirs = repo::values(tx, ws, &into.id)?;
    for v in repo::values(tx, ws, &from.id)? {
        if theirs
            .iter()
            .any(|t| t.key == v.key && t.label == v.label && t.value == v.value)
        {
            continue;
        }
        tx.run(
            "UPDATE attr_values SET party_id = ?, updated = ? WHERE id = ?",
            params![into.id, now(), v.id],
        )?;
        moved.attrs.push(v.id);
    }

    // Links: remember the ones `into` already has (retarget drops them), then move the rest.
    let mut mine = links::from(tx, ws, &fr, None)?;
    mine.extend(links::to(tx, ws, &fr, None)?);
    let swap = |r: &ObjRef| if *r == fr { ir.clone() } else { r.clone() };
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
        if after != key && links::get(tx, ws, &after)?.is_some() {
            moved.links_dropped.push(DroppedLink {
                key,
                props: l.props,
                source: l.source,
                confidence: l.confidence,
                created: l.created,
            });
        }
    }
    moved.links = links::retarget(tx, ws, &fr, &ir)?;

    // Memories and tags: those `into` already had are “shared”.
    let about_into: Vec<String> = mem_ids_about(tx, ws, &into.id)?;
    for id in mems::retarget_about(tx, ws, PARTY, &from.id, &into.id, None)? {
        if about_into.contains(&id) {
            moved.mems_shared.push(id);
        } else {
            moved.mems.push(id);
        }
    }
    let tags_into = tags::of(tx, ws, &ir)?;
    for id in tags::retarget(tx, ws, PARTY, &from.id, &into.id, None)? {
        if tags_into.contains(&id) {
            moved.tags_shared.push(id);
        } else {
            moved.tags.push(id);
        }
    }

    // Team memberships.
    for mut m in memberships(tx, ws, &from.id)? {
        let has = tx
            .prepare_cached("SELECT 1 FROM team_members WHERE team_id = ? AND contact_id = ?")?
            .exists(params![m.team_id, into.id])?;
        if has {
            tx.run(
                "DELETE FROM team_members WHERE team_id = ? AND contact_id = ?",
                params![m.team_id, from.id],
            )?;
            m.dropped = true;
        } else {
            tx.run(
                "UPDATE team_members SET contact_id = ? WHERE team_id = ? AND contact_id = ?",
                params![into.id, m.team_id, from.id],
            )?;
        }
        svc::event(tx, ws, &m.team_id);
        moved.teams.push(m);
    }

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
    repo::touch(tx, &into.id)?;

    let id = uid("mg");
    tx.run(
        "INSERT INTO merges (id, workspace_id, from_id, into_id, moved, actor, created) VALUES (?, ?, ?, ?, ?, ?, ?)",
        params![id, ws, from.id, into.id, serde_json::to_string(&moved)?, actor, now()],
    )?;
    settle_proposals(tx, ws, &from.id, &into.id)?;
    base::audit(
        tx,
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
    svc::event(tx, ws, &from.id);
    svc::event(tx, ws, &into.id);
    Ok(id)
}

fn mem_ids_about(c: &Connection, ws: &str, party: &str) -> ApiResult<Vec<String>> {
    Ok(c.prepare_cached(
        "SELECT mem_id FROM mem_about WHERE workspace_id = ? AND object_type = 'party' AND object_id = ?",
    )?
    .query_map(params![ws, party], |r| r.get(0))?
    .collect::<rusqlite::Result<_>>()?)
}

fn memberships(c: &Connection, ws: &str, contact: &str) -> ApiResult<Vec<MovedMember>> {
    Ok(c.prepare_cached(
        "SELECT team_id, role, source, joined FROM team_members WHERE workspace_id = ? AND contact_id = ? ORDER BY team_id",
    )?
    .query_map(params![ws, contact], |r| {
        Ok(MovedMember {
            team_id: r.get(0)?,
            role: r.get(1)?,
            source: r.get(2)?,
            joined: r.get(3)?,
            dropped: false,
        })
    })?
    .collect::<rusqlite::Result<_>>()?)
}

/// After `from` is merged: the open proposal of this very pair becomes `merged` (its card resolved with
/// `merge`), other open proposals involving `from` are moot and expire.
fn settle_proposals(tx: &Tx, ws: &str, from: &str, into: &str) -> ApiResult<()> {
    let open: Vec<(String, String, String, Option<String>)> = tx
        .prepare_cached(
            "SELECT id, from_id, into_id, decision_id FROM merge_proposals
             WHERE workspace_id = ? AND status = 'open' AND (from_id = ?2 OR into_id = ?2)",
        )
        .and_then(|mut st| {
            st.query_map(params![ws, from], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?
            .collect()
        })?;
    for (id, f, i, decision) in open {
        let (status, d_status, result) = if f == from && i == into {
            ("merged", "resolved", json!({ "choice": "merge" }))
        } else {
            ("expired", "expired", json!({ "expired": true }))
        };
        set_proposal(tx, &id, status)?;
        if let Some(d) = decision {
            base::decisions::settle(tx, &d, &result, d_status, &Value::Null)?;
        }
    }
    Ok(())
}

fn set_proposal(tx: &Tx, id: &str, status: &str) -> ApiResult<()> {
    let resolved = (status != "open").then(now);
    tx.run(
        "UPDATE merge_proposals SET status = ?, resolved = ? WHERE id = ?",
        params![status, resolved, id],
    )?;
    Ok(())
}

// ---------------------------------------------------------------- split

/// Splits merge `id` inside `tx`: only the latest unsplit merge into its `into` can be split
/// (`409 先拆开之后的合并`, also while `into` is itself merged into someone else). Moves back what is
/// still on `into`.
pub fn split_tx(
    tx: &Tx,
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
    let from = repo::get(tx, &m.from_id)?.ok_or_else(|| ApiError::not_found("合并记录"))?;
    if m.split.is_some() {
        return Err(ApiError::conflict("这次合并已经拆开了"));
    }
    if into.status != "active" || later_merge(tx, &m)? {
        return Err(ApiError::conflict("先拆开之后的合并"));
    }
    if from.status != "merged" || from.merged_into.as_deref() != Some(into.id.as_str()) {
        return Err(ApiError::conflict("这个联系人已经不在合并状态"));
    }
    let mv = &m.moved;

    for h in hooks.iter().rev() {
        let v = mv.hooks.get(h.name()).cloned().unwrap_or(Value::Null);
        h.on_split(tx, ws, &from.id, &into.id, &v)?;
    }
    super::stewards::split_back(tx, ws, &from.id, &into.id, &mv.stewards)?;

    for t in &mv.teams {
        if t.dropped {
            tx.run(
                "INSERT OR IGNORE INTO team_members (workspace_id, team_id, contact_id, role, source, joined)
                 SELECT ?, ?, ?, ?, ?, ? WHERE EXISTS (SELECT 1 FROM parties WHERE id = ?2)",
                params![ws, t.team_id, from.id, t.role, t.source, t.joined],
            )?;
        } else {
            tx.run(
                "UPDATE OR IGNORE team_members SET contact_id = ? WHERE team_id = ? AND contact_id = ?",
                params![from.id, t.team_id, into.id],
            )?;
        }
        svc::event(tx, ws, &t.team_id);
    }

    if !mv.tags.is_empty() {
        tags::retarget(tx, ws, PARTY, &into.id, &from.id, Some(&mv.tags))?;
    }
    for tag in &mv.tags_shared {
        tx.run(
            "INSERT OR IGNORE INTO entity_tags (workspace_id, tag_id, object_type, object_id, created)
             SELECT ?, ?, 'party', ?, ? WHERE EXISTS (SELECT 1 FROM tags WHERE id = ?2)",
            params![ws, tag, from.id, now()],
        )?;
    }

    if !mv.mems.is_empty() {
        mems::retarget_about(tx, ws, PARTY, &into.id, &from.id, Some(&mv.mems))?;
    }
    for mem in &mv.mems_shared {
        if mems::get(tx, mem)?.is_some() {
            tx.run(
                "INSERT OR IGNORE INTO mem_about (mem_id, workspace_id, object_type, object_id) VALUES (?, ?, 'party', ?)",
                params![mem, ws, from.id],
            )?;
        }
    }

    links::restore(tx, ws, &mv.links)?;
    for l in &mv.links_dropped {
        if links::get(tx, ws, &l.key)?.is_none() {
            tx.run(
                "INSERT OR IGNORE INTO links (workspace_id, subject_type, subject_id, predicate, object_type, object_id, props, source, confidence, created) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                params![
                    ws,
                    l.key.subject.kind,
                    l.key.subject.id,
                    l.key.predicate,
                    l.key.object.kind,
                    l.key.object.id,
                    serde_json::to_string(&l.props)?,
                    l.source,
                    l.confidence,
                    l.created
                ],
            )?;
        }
    }
    for a in &mv.attrs {
        tx.run(
            "UPDATE attr_values SET party_id = ?, updated = ? WHERE id = ? AND party_id = ?",
            params![from.id, now(), a, into.id],
        )?;
    }

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
    repo::touch(tx, &into.id)?;
    tx.run(
        "UPDATE merges SET split = ? WHERE id = ?",
        params![now(), m.id],
    )?;
    base::audit(
        tx,
        ws,
        actor,
        "party.split",
        &from.id,
        &json!({ "into": into.id, "merge": m.id }),
    )?;
    svc::event(tx, ws, &from.id);
    svc::event(tx, ws, &into.id);
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
fn first_account(c: &rusqlite::Connection, party: &str) -> ApiResult<Option<PendingAccount>> {
    Ok(c.prepare_cached(
        "SELECT a.system_id, coalesce(s.name, a.system_id), a.handle_display FROM accounts a
               LEFT JOIN systems s ON s.id = a.system_id AND s.workspace_id IN ('*', a.workspace_id)
              WHERE a.party_id = ? AND a.status = 'active' ORDER BY a.created, a.id LIMIT 1",
    )?
    .query_row([party], |r| {
        Ok(PendingAccount {
            system: r.get(0)?,
            system_name: r.get(1)?,
            handle: r.get(2)?,
        })
    })
    .optional()?)
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
        .transaction(|tx| merge_tx(tx, &ports.hooks, &viewer, &who.me, &from, &into))?;
    let into = m.store.reader(|c| {
        let p = svc::own_party(c, &viewer, &into, "contact", "联系人", false)?;
        svc::contact(c, &viewer, &p)
    })?;
    Ok(Merged { merge_id, into })
}

/// `POST /merges/:id/split`.
pub fn split(m: &Modules, ports: &Ports, who: &Caller, id: &str) -> ApiResult<()> {
    let (viewer, _) = svc::start(m, who)?;
    m.store
        .transaction(|tx| split_tx(tx, &ports.hooks, &viewer, &who.me, id))
}

/// `(id, from_id, into_id, account_id, reason, decision_id)` of an open proposal.
type ProposalRow = (
    String,
    String,
    String,
    Option<String>,
    String,
    Option<String>,
);

/// `GET /merges`: open proposals whose two contacts are still active and visible, and the unconfirmed
/// contacts.
pub fn pending(m: &Modules, ports: &Ports, who: &Caller) -> ApiResult<Pending> {
    let (viewer, _) = svc::start(m, who)?;
    m.store.reader(|c| {
        let rows: Vec<ProposalRow> = c
            .prepare_cached(
                "SELECT id, from_id, into_id, account_id, reason, decision_id FROM merge_proposals
                 WHERE workspace_id = ? AND status = 'open' ORDER BY created DESC, rowid DESC",
            )?
            .query_map([&viewer.ws], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            })?
            .collect::<rusqlite::Result<_>>()?;
        let mut proposals = Vec::new();
        for (id, f, i, account, reason, decision_id) in rows {
            let (Ok(f), Ok(i)) = (
                svc::own_party(c, &viewer, &f, "contact", "联系人", true),
                svc::own_party(c, &viewer, &i, "contact", "联系人", true),
            ) else {
                continue;
            };
            let account = match account {
                Some(a) => crate::accounts::repo::get(c, &a)?.map(|a| ProposalAccount {
                    id: a.id,
                    handle: a.handle_display,
                    system: a.system_id,
                }),
                None => None,
            };
            proposals.push(Proposal {
                id,
                from: svc::brief(c, &viewer, ports, &f)?,
                into: svc::brief(c, &viewer, ports, &i)?,
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
        let parties = repo::contacts(c, &viewer, &q)?;
        let ids: Vec<String> = parties.iter().map(|p| p.id.clone()).collect();
        let mut seen: std::collections::HashMap<String, crate::contacts::Seen> = match &ports.spaces
        {
            Some(s) => s.seen(c, &viewer, &ids)?.into_iter().collect(),
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
            let account = first_account(c, &p.id)?;
            let nameless = crate::accounts::looks_like_handle(&p.name)
                || account.as_ref().is_some_and(|a| a.handle == p.name);
            let label = if nameless {
                format!(
                    "未知{}用户",
                    account
                        .as_ref()
                        .map(|a| a.system_name.as_str())
                        .unwrap_or("")
                )
            } else {
                p.name.clone()
            };
            unconfirmed.push(PendingContact {
                brief: svc::brief(c, &viewer, ports, p)?,
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
    })
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

fn proposal(c: &Connection, id: &str) -> ApiResult<Option<(String, String, String, String)>> {
    Ok(c.prepare_cached(
        "SELECT workspace_id, from_id, into_id, status FROM merge_proposals WHERE id = ?",
    )?
    .query_row([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
    .optional()?)
}

impl DecisionHandler for PartyMerge {
    fn resolve(&self, tx: &Tx, cx: &DecisionCx, d: &Decision) -> ApiResult<Value> {
        let (ws, from, into, status) = proposal(tx, &d.subject_id)?
            .filter(|p| p.0 == cx.ws)
            .ok_or_else(|| ApiError::not_found("合并提议"))?;
        if status != "open" {
            return Err(ApiError::conflict("这个提议已经处理过了"));
        }
        let mut result = decisions::choice_result(cx);
        match cx.choice {
            "merge" => {
                // Marked first so that merge_tx leaves this decision to the dispatcher.
                set_proposal(tx, &d.subject_id, "merged")?;
                let viewer = Viewer::load(tx, &ws, cx.actor)?;
                let id = merge_tx(tx, &self.hooks, &viewer, cx.actor, &from, &into)?;
                result["merge_id"] = json!(id);
            }
            _ => set_proposal(tx, &d.subject_id, "rejected")?,
        }
        Ok(result)
    }

    fn before_undo(&self, tx: &Tx, d: &Decision) -> ApiResult<()> {
        let Some((ws, _, _, _)) = proposal(tx, &d.subject_id)? else {
            return Ok(());
        };
        if let Some(mg) = d.result.get("merge_id").and_then(Value::as_str) {
            let actor = d_actor(tx, mg)?;
            let viewer = Viewer::load(tx, &ws, &actor)?;
            split_tx(tx, &self.hooks, &viewer, &actor, mg)?;
        }
        set_proposal(tx, &d.subject_id, "open")
    }
}

fn d_actor(c: &Connection, merge: &str) -> ApiResult<String> {
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
    fn resolve(&self, tx: &Tx, cx: &DecisionCx, d: &Decision) -> ApiResult<Value> {
        let v = repo::value_get(tx, &d.subject_id)?
            .filter(|v| v.workspace_id == cx.ws)
            .ok_or_else(|| ApiError::not_found("属性"))?;
        let mut result = decisions::choice_result(cx);
        if cx.choice == "yes" {
            let staled: Vec<String> = tx
                .prepare_cached(
                    "SELECT id FROM attr_values WHERE workspace_id = ? AND party_id = ? AND key = ? AND label = ? AND status = 'confirmed' AND id != ?",
                )?
                .query_map(
                    params![v.workspace_id, v.party_id, v.key, v.label, v.id],
                    |r| r.get(0),
                )?
                .collect::<rusqlite::Result<_>>()?;
            repo::value_update(tx, &v.id, None, None, Some("confirmed"))?;
            repo::stale_others(tx, &v)?;
            result["staled"] = json!(staled);
        } else {
            repo::value_delete(tx, &v.id)?;
            result["value"] = value_snapshot(&v);
        }
        repo::touch(tx, &v.party_id)?;
        svc::event(tx, cx.ws, &v.party_id);
        Ok(result)
    }

    fn before_undo(&self, tx: &Tx, d: &Decision) -> ApiResult<()> {
        if let Some(s) = d.result.get("value") {
            let str_of = |k: &str| s.get(k).and_then(Value::as_str);
            let party = str_of("party_id").unwrap_or_default();
            if repo::get(tx, party)?.is_none() {
                return Ok(());
            }
            tx.run(
                "INSERT OR IGNORE INTO attr_values (id, workspace_id, party_id, key, label, value, source, source_label, source_ref, level, status, decision_id, created, updated)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'candidate', ?, ?, ?)",
                params![
                    d.subject_id,
                    d.workspace_id,
                    party,
                    str_of("key"),
                    str_of("label"),
                    str_of("value"),
                    str_of("source"),
                    str_of("source_label").unwrap_or_default(),
                    str_of("source_ref"),
                    str_of("level").unwrap_or(Level::Reasoning.code()),
                    d.id,
                    s.get("created").and_then(Value::as_i64).unwrap_or_else(now),
                    now()
                ],
            )?;
            svc::event(tx, &d.workspace_id, party);
            return Ok(());
        }
        let Some(v) = repo::value_get(tx, &d.subject_id)? else {
            return Ok(());
        };
        repo::value_update(tx, &v.id, None, None, Some("candidate"))?;
        for id in d
            .result
            .get("staled")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            tx.run(
                "UPDATE attr_values SET status = 'confirmed', updated = ? WHERE id = ? AND status = 'stale'",
                params![now(), id],
            )?;
        }
        svc::event(tx, &d.workspace_id, &v.party_id);
        Ok(())
    }

    /// 30 days without an answer: the candidate is dropped.
    fn expire(&self, tx: &Tx, d: &Decision) -> ApiResult<()> {
        if let Some(v) = repo::value_get(tx, &d.subject_id)?.filter(|v| v.status == "candidate") {
            repo::value_delete(tx, &v.id)?;
            svc::event(tx, &d.workspace_id, &v.party_id);
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
    fn resolve(&self, tx: &Tx, cx: &DecisionCx, d: &Decision) -> ApiResult<Value> {
        let x = repo::def_get(tx, &d.subject_id)?
            .filter(|x| x.workspace_id == cx.ws)
            .ok_or_else(|| ApiError::not_found("属性"))?;
        let mut result = decisions::choice_result(cx);
        if cx.choice == "yes" {
            tx.run(
                "UPDATE attr_defs SET status = 'active' WHERE id = ?",
                [&x.id],
            )?;
        } else {
            repo::def_delete(tx, &x.id)?;
            result["def"] = def_snapshot(&x);
        }
        Ok(result)
    }

    fn before_undo(&self, tx: &Tx, d: &Decision) -> ApiResult<()> {
        if let Some(s) = d.result.get("def") {
            let str_of = |k: &str| s.get(k).and_then(Value::as_str);
            tx.run(
                "INSERT OR IGNORE INTO attr_defs (id, workspace_id, applies, key, label, type, options, default_level, source, status, created)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 'proposed', ?)",
                params![
                    d.subject_id,
                    d.workspace_id,
                    str_of("applies"),
                    str_of("key"),
                    str_of("label"),
                    str_of("type"),
                    serde_json::to_string(s.get("options").unwrap_or(&json!([])))?,
                    str_of("default_level"),
                    str_of("source"),
                    s.get("created").and_then(Value::as_i64).unwrap_or_else(now)
                ],
            )?;
            return Ok(());
        }
        tx.run(
            "UPDATE attr_defs SET status = 'proposed' WHERE id = ? AND status = 'active'",
            [&d.subject_id],
        )?;
        Ok(())
    }

    fn expire(&self, tx: &Tx, d: &Decision) -> ApiResult<()> {
        tx.run(
            "DELETE FROM attr_defs WHERE id = ? AND status = 'proposed'",
            [&d.subject_id],
        )?;
        Ok(())
    }
}
