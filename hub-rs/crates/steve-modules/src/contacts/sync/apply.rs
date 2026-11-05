//! Applying one snapshot in one transaction: departments → org-chart teams of the own
//! organization, people → contacts and accounts, membership, leads, `works_at` and `reports_to`.
//!
//! Rules beyond the spec's sentence, and why:
//! - A department that disappears is dissolved and loses its (synced) members; when it comes back its
//!   team is reactivated (same id, so the tags, purpose, bound group and memories you added survive).
//! - A person not yet known is matched first by their directory handle (`identify`), then by one of
//!   their emails already recorded on a contact, and only then becomes a new contact — so the
//!   directory attaches to the Sarah you already have instead of creating a second one.
//! - Only what the sync wrote is changed by the sync: names of contacts it created, its own `works_at`
//!   and `reports_to` links. Your edits are left alone.
//! - People who left are taken out of every team and their job at the own organization is closed;
//!   the contact, its accounts and memories stay.
//! - An empty snapshot while teams exist is refused: it means lost permissions, not a company with
//!   no one in it.

use std::collections::{BTreeSet, HashMap};

use serde::Serialize;
use serde_json::{Map, Value, json};
use steve_store::Tx;
use steve_store::base::links::{self, LinkKey, NewLink};
use steve_store::base::{self, Viewer};

use super::super::repo::{self as prepo, NewParty, Party, PartyPatch};
use super::super::service::{ensure_self_org, event, pref, revise, this_month};
use super::super::teams::repo::{self as trepo, Details};
use super::DirConnection;
use super::adapter::{DirUser, Snapshot};
use super::repo::{self, SyncedTeam};
use crate::accounts::repo::{self as arepo, System};
use crate::accounts::service::{self as asvc, Record, current_party};
use crate::accounts::{ExtraHandle, Hints, identify};
use crate::{ApiError, ApiResult};

/// `400` when a snapshot is empty but the connection has synced teams before.
pub const EMPTY_SNAPSHOT: &str = "通讯录是空的，这次没有同步（可能是通讯录权限范围被收回了）";

/// What one sync changed (also the detail of the `team.sync` audit).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SyncReport {
    /// Member rows added / removed across all teams.
    pub added: usize,
    pub removed: usize,
    /// Teams dissolved because their department is gone.
    pub dissolved: usize,
    /// New org-chart teams.
    pub teams: usize,
    /// Teams whose department came back.
    pub restored: usize,
    /// Teams or sync-created contacts renamed.
    pub renamed: usize,
    /// Teams moved under another parent.
    pub moved: usize,
    /// New contacts.
    pub contacts: usize,
    /// People who left, whose job at the own organization was closed now.
    pub departed: usize,
}

const MAX_TEAM_NAME: usize = 40;
const MAX_DEPT_PATH: usize = 80;
/// Path segments shown at most: the root side is dropped first.
const MAX_DEPT_DEPTH: usize = 6;

/// Department id → its path from the top of the visible tree, “A / B / C”.
pub fn dept_paths(s: &Snapshot) -> HashMap<&str, String> {
    let by_id: HashMap<&str, &super::adapter::Department> =
        s.departments.iter().map(|d| (d.id.as_str(), d)).collect();
    let mut out = HashMap::new();
    for d in &s.departments {
        let mut names = vec![d.name.trim()];
        let mut seen = BTreeSet::from([d.id.as_str()]);
        let mut cur = d.parent_id.as_deref();
        while let Some(p) = cur.and_then(|p| by_id.get(p)) {
            if !seen.insert(p.id.as_str()) {
                break; // a cycle in a broken snapshot
            }
            names.push(p.name.trim());
            cur = p.parent_id.as_deref();
        }
        names.retain(|n| !n.is_empty());
        names.reverse();
        if names.len() > MAX_DEPT_DEPTH {
            names.drain(..names.len() - MAX_DEPT_DEPTH);
        }
        out.insert(d.id.as_str(), names.join(" / "));
    }
    out
}
const MAX_NAME: usize = 80;

fn clip(s: &str, n: usize) -> String {
    s.trim().chars().take(n).collect()
}

/// Drops entries without an id and repeated ids (the first wins).
fn clean(s: &Snapshot) -> Snapshot {
    let mut seen = BTreeSet::new();
    let departments = s
        .departments
        .iter()
        .filter(|d| !d.id.trim().is_empty() && seen.insert(d.id.clone()))
        .cloned()
        .collect();
    let mut seen = BTreeSet::new();
    let users = s
        .users
        .iter()
        .filter(|u| !u.id.trim().is_empty() && seen.insert(u.id.clone()))
        .cloned()
        .collect();
    Snapshot { departments, users }
}

/// Applies `snapshot` of `conn` whose people are handles of `system_id`.
pub fn apply(
    tx: &Tx,
    conn: &DirConnection,
    system_id: &str,
    snapshot: &Snapshot,
    now: i64,
) -> ApiResult<SyncReport> {
    let ws = conn.workspace_id.as_str();
    let actor = conn.owner.as_str();
    let snap = clean(snapshot);
    let existing = repo::chart_teams(tx, ws, &conn.id)?;
    if snap.departments.is_empty()
        && snap.users.is_empty()
        && existing.iter().any(|t| t.party.status == "active")
    {
        return Err(ApiError::rule(EMPTY_SNAPSHOT));
    }
    asvc::ensure_seeds(tx, ws)?;
    let system = asvc::own_system(tx, ws, system_id)?;
    let own = ensure_self_org(tx, ws, actor)?;
    let mut r = SyncReport::default();
    let mut touched: BTreeSet<String> = BTreeSet::new();

    let (team_of, dissolved) = teams(tx, conn, &own, &snap, &existing, now, &mut r, &mut touched)?;

    // people
    let label = format!("{} · 同步", conn.label);
    let mut contact_of: HashMap<String, String> = HashMap::new();
    for u in snap.users.iter().filter(|u| !u.departed) {
        if let Some(pid) = person(tx, conn, &system, u, &label, &mut r, &mut touched)? {
            contact_of.insert(u.id.clone(), pid);
        }
    }
    let mut departed: Vec<String> = Vec::new();
    for u in snap.users.iter().filter(|u| u.departed) {
        if let Some(p) = known(tx, ws, &system, u)?
            && !contact_of.values().any(|c| *c == p.id)
            && !departed.contains(&p.id)
        {
            departed.push(p.id);
        }
    }

    // jobs at the own organization, with the whole department path (“基础架构 / 存储 / NoSQL”)
    let paths = dept_paths(&snap);
    for u in snap.users.iter().filter(|u| !u.departed) {
        let Some(pid) = contact_of.get(&u.id) else {
            continue;
        };
        let dept = u
            .department_ids
            .iter()
            .find_map(|d| paths.get(d.as_str()))
            .map(|n| clip(n, MAX_DEPT_PATH));
        let title = u
            .title
            .as_deref()
            .map(|t| clip(t, MAX_NAME))
            .filter(|t| !t.is_empty());
        if open_job(tx, ws, pid, &own.id, title.as_deref(), dept.as_deref())? {
            touched.insert(pid.clone());
        }
    }
    for pid in &departed {
        if close_job(tx, ws, pid, &own.id)? {
            r.departed += 1;
            touched.insert(pid.clone());
        }
    }

    // members and leads; a dissolved department has nobody in it any more
    for team in &dissolved {
        members(tx, ws, team, &[], &mut r)?;
    }
    for d in &snap.departments {
        let Some(team) = team_of.get(&d.id) else {
            continue;
        };
        let mut want: Vec<(String, &str)> = Vec::new();
        if let Some(l) = d.leader_id.as_ref().and_then(|l| contact_of.get(l)) {
            want.push((l.clone(), "lead"));
        }
        for u in snap
            .users
            .iter()
            .filter(|u| !u.departed && u.department_ids.contains(&d.id))
        {
            if let Some(c) = contact_of.get(&u.id)
                && !want.iter().any(|(w, _)| w == c)
            {
                want.push((c.clone(), "member"));
            }
        }
        if members(tx, ws, team, &want, &mut r)? {
            touched.insert(team.clone());
        }
    }

    // reports_to
    for u in snap.users.iter().filter(|u| !u.departed) {
        let Some(pid) = contact_of.get(&u.id) else {
            continue;
        };
        let lead = u
            .leader_id
            .as_ref()
            .and_then(|l| contact_of.get(l))
            .filter(|l| *l != pid);
        if reports_to(tx, ws, pid, lead.map(String::as_str))? {
            touched.insert(pid.clone());
        }
    }
    for pid in &departed {
        if reports_to(tx, ws, pid, None)? {
            touched.insert(pid.clone());
        }
    }

    for id in &touched {
        event(tx, ws, id);
    }
    let mut detail = serde_json::to_value(&r)?;
    detail["connection"] = json!(conn.id);
    base::audit(tx, ws, actor, "team.sync", &conn.id, &detail)?;
    Ok(r)
}

/// Upserts a team per department, sets parents, dissolves the vanished ones; returns department id →
/// team id, and the teams dissolved now.
#[allow(clippy::too_many_arguments)]
fn teams(
    tx: &Tx,
    conn: &DirConnection,
    own: &Party,
    snap: &Snapshot,
    existing: &[SyncedTeam],
    now: i64,
    r: &mut SyncReport,
    touched: &mut BTreeSet<String>,
) -> ApiResult<(HashMap<String, String>, Vec<String>)> {
    let ws = conn.workspace_id.as_str();
    let actor = conn.owner.as_str();
    let mut team_of: HashMap<String, String> = HashMap::new();
    let mut dissolved = Vec::new();
    for d in &snap.departments {
        let mut name = clip(&d.name, MAX_TEAM_NAME);
        if name.is_empty() {
            name = "未命名部门".into();
        }
        match existing.iter().find(|t| t.external_id == d.id) {
            Some(t) => {
                let (mut before, mut after) = (Map::new(), Map::new());
                let mut patch = PartyPatch::default();
                if t.party.name != name {
                    before.insert("name".into(), json!(t.party.name));
                    after.insert("name".into(), json!(name));
                    patch.name = Some(name.clone());
                    r.renamed += 1;
                }
                if t.party.status != "active" {
                    before.insert("status".into(), json!(t.party.status));
                    after.insert("status".into(), json!("active"));
                    patch.status = Some("active".into());
                    r.restored += 1;
                }
                if !after.is_empty() {
                    revise(
                        tx,
                        &t.party,
                        &patch,
                        Value::Object(before),
                        Value::Object(after),
                        actor,
                    )?;
                    touched.insert(t.party.id.clone());
                }
                team_of.insert(d.id.clone(), t.party.id.clone());
            }
            None => {
                let mut p = NewParty::new("team", &name, actor);
                p.source = "sync".into();
                p.source_ref = Some(d.id.clone());
                let p = prepo::insert(tx, ws, &p)?;
                trepo::details_insert(
                    tx,
                    ws,
                    &p.id,
                    &Details {
                        kind: "chart".into(),
                        org_id: Some(own.id.clone()),
                        sync_connection: Some(conn.id.clone()),
                        external_id: Some(d.id.clone()),
                        synced: Some(now),
                        ..Default::default()
                    },
                )?;
                r.teams += 1;
                touched.insert(p.id.clone());
                team_of.insert(d.id.clone(), p.id);
            }
        }
    }
    for d in &snap.departments {
        let team = &team_of[&d.id];
        let parent = d
            .parent_id
            .as_ref()
            .and_then(|p| team_of.get(p))
            .filter(|p| *p != team);
        let old = existing
            .iter()
            .find(|t| t.party.id == *team)
            .and_then(|t| t.parent_id.as_ref());
        if parent != old {
            repo::set_parent(tx, team, parent.map(String::as_str))?;
            if existing.iter().any(|t| t.party.id == *team) {
                r.moved += 1;
            }
            touched.insert(team.clone());
        }
        repo::set_synced(tx, team, now)?;
    }
    for t in existing
        .iter()
        .filter(|t| t.party.status == "active" && !team_of.values().any(|v| *v == t.party.id))
    {
        revise(
            tx,
            &t.party,
            &PartyPatch {
                status: Some("dissolved".into()),
                ..Default::default()
            },
            json!({ "status": t.party.status }),
            json!({ "status": "dissolved" }),
            actor,
        )?;
        r.dissolved += 1;
        dissolved.push(t.party.id.clone());
        touched.insert(t.party.id.clone());
    }
    Ok((team_of, dissolved))
}

/// The contact already behind a directory person (by any of their handles), if it is active.
fn known(tx: &Tx, ws: &str, system: &System, u: &DirUser) -> ApiResult<Option<Party>> {
    for h in std::iter::once(&u.id).chain(u.extra_ids.iter()) {
        if let Some(a) = arepo::by_handle(tx, ws, &system.family, h.trim())?
            && let Some(acc) = arepo::get(tx, &a)?
        {
            return Ok(current_party(tx, &acc.party_id)?.filter(|p| p.status == "active"));
        }
    }
    Ok(None)
}

/// The one active contact owning one of these emails, if exactly one does.
fn by_email(tx: &Tx, ws: &str, emails: &[String]) -> ApiResult<Option<Party>> {
    let mail = asvc::own_system(tx, ws, "mail")?;
    let mut found: Option<Party> = None;
    for e in emails {
        let Some(h) = asvc::normalize(&mail, e) else {
            continue;
        };
        let Some(a) = arepo::by_handle(tx, ws, &mail.family, &h.handle)? else {
            continue;
        };
        let Some(p) = arepo::get(tx, &a)?
            .map(|acc| current_party(tx, &acc.party_id))
            .transpose()?
            .flatten()
            .filter(|p| p.status == "active" && p.kind == "contact")
        else {
            continue;
        };
        match &found {
            Some(f) if f.id != p.id => return Ok(None),
            _ => found = Some(p),
        }
    }
    Ok(found)
}

/// Finds or creates the contact of a directory person; `None` for the own organization's accounts.
fn person(
    tx: &Tx,
    conn: &DirConnection,
    system: &System,
    u: &DirUser,
    label: &str,
    r: &mut SyncReport,
    touched: &mut BTreeSet<String>,
) -> ApiResult<Option<String>> {
    let ws = conn.workspace_id.as_str();
    let actor = conn.owner.as_str();
    let name = clip(&u.name, MAX_NAME);
    let name = if name.is_empty() { u.id.clone() } else { name };
    let extra: Vec<ExtraHandle> = u
        .extra_ids
        .iter()
        .filter(|h| !h.trim().is_empty())
        .map(|h| ExtraHandle {
            handle: h.trim().into(),
            connection_id: Some(conn.id.clone()),
        })
        .collect();
    let mut new = false;
    let party_id = match known(tx, ws, system, u)? {
        Some(_) => None,
        None => by_email(tx, ws, &u.emails)?.map(|p| p.id),
    };
    let party_id = match party_id {
        // the directory account joins the contact its email belongs to
        Some(pid) => {
            let viewer = Viewer::load(tx, ws, actor)?;
            let a = asvc::record_tx(
                tx,
                &viewer,
                actor,
                &Record {
                    system_id: system.id.clone(),
                    party_id: pid.clone(),
                    kind: "personal".into(),
                    handle: u.id.clone(),
                    display_name: name.clone(),
                    verified: true,
                    confidence: 1.0,
                    source: "sync".into(),
                    source_label: label.into(),
                },
            )?;
            for x in &extra {
                arepo::add_handle(
                    tx,
                    ws,
                    &system.family,
                    &x.handle,
                    &a.id,
                    x.connection_id.as_deref(),
                )?;
            }
            pid
        }
        None => {
            let hints = Hints {
                display_name: Some(name.clone()),
                extra,
                source: Some("sync".into()),
                source_label: Some(label.into()),
                ..Default::default()
            };
            let got = identify(tx, ws, actor, &system.id, &u.id, &hints)?;
            if got.is_ours {
                return Ok(None);
            }
            let p = prepo::get(tx, &got.party_id)?;
            new = got.created
                && p.as_ref().is_some_and(|p| {
                    p.source == "identify" && p.contact_kind.as_deref() == Some("unconfirmed")
                });
            got.party_id
        }
    };
    let Some(p) = prepo::get(tx, &party_id)?.filter(|p| p.status == "active") else {
        return Ok(None);
    };
    if new {
        repo::set_source(tx, &p.id, "sync", &u.id)?;
        r.contacts += 1;
        touched.insert(p.id.clone());
    }
    // the directory is an exact source: an unconfirmed contact becomes real
    let mut patch = PartyPatch::default();
    let (mut before, mut after) = (Map::new(), Map::new());
    if p.contact_kind.as_deref() == Some("unconfirmed") {
        if !new {
            repo::set_source(tx, &p.id, "sync", &u.id)?;
        }
        before.insert("kind".into(), json!("unconfirmed"));
        after.insert("kind".into(), json!("real"));
        patch.contact_kind = Some("real".into());
    }
    // the sync's own contacts follow the directory; others only while their name is still a raw
    // handle (identification fell back to `ou_…`) — a name you gave stays, also on the sync's own
    let raw = crate::accounts::looks_like_handle(&p.name)
        || p.name.trim() == u.id
        || u.extra_ids.iter().any(|x| x.trim() == p.name.trim());
    if !new
        && !p.is_me()
        && p.name != name
        && !crate::accounts::looks_like_handle(&name)
        && (raw || p.source == "sync")
        && !crate::contacts::profile::name_edited(tx, ws, &p.id)?
    {
        before.insert("name".into(), json!(p.name));
        after.insert("name".into(), json!(name));
        patch.name = Some(name.clone());
        r.renamed += 1;
    }
    if !after.is_empty() {
        after.insert("source".into(), json!("sync"));
        if new {
            prepo::update(tx, &p.id, &patch, None)?;
        } else {
            revise(
                tx,
                &p,
                &patch,
                Value::Object(before),
                Value::Object(after),
                actor,
            )?;
        }
        touched.insert(p.id.clone());
    }
    // emails become mail accounts of the contact unless someone already has them
    if !u.emails.is_empty() {
        let mail = asvc::own_system(tx, ws, "mail")?;
        let viewer = Viewer::load(tx, ws, actor)?;
        for e in &u.emails {
            let Some(h) = asvc::normalize(&mail, e) else {
                continue;
            };
            if arepo::by_handle(tx, ws, &mail.family, &h.handle)?.is_some() {
                continue;
            }
            asvc::record_tx(
                tx,
                &viewer,
                actor,
                &Record {
                    system_id: mail.id.clone(),
                    party_id: p.id.clone(),
                    kind: "work_email".into(),
                    handle: h.handle,
                    display_name: name.clone(),
                    verified: true,
                    confidence: 1.0,
                    source: "sync".into(),
                    source_label: label.into(),
                },
            )?;
            touched.insert(p.id.clone());
        }
    }
    Ok(Some(p.id))
}

fn job_key(contact: &str, org: &str) -> LinkKey {
    LinkKey {
        subject: pref(contact),
        predicate: "works_at".into(),
        object: pref(org),
    }
}

fn is_open(l: &steve_store::Link) -> bool {
    l.props.get("to").is_none_or(Value::is_null)
}

/// The synced job at the own organization: created, updated (title, department) or reopened after a
/// return. A job you entered yourself is left as it is. Returns whether something changed.
fn open_job(
    tx: &Tx,
    ws: &str,
    contact: &str,
    org: &str,
    title: Option<&str>,
    dept: Option<&str>,
) -> ApiResult<bool> {
    let key = job_key(contact, org);
    match links::get(tx, ws, &key)? {
        None => {
            let mut l = NewLink::new(key.subject, "works_at", key.object, "sync");
            l.props = json!({ "title": title, "dept": dept, "from": this_month(), "to": null });
            Ok(links::add(tx, ws, &l)?)
        }
        Some(l) if l.source != "sync" => Ok(false),
        Some(l) if is_open(&l) => {
            let mut props = l.props.clone();
            if !props.is_object() {
                props = json!({});
            }
            // a field you edited stays yours
            let manual =
                crate::contacts::profile::job_manual_fields(tx, ws, contact, org, &l.props)?;
            if !manual.iter().any(|m| m == "title") {
                props["title"] = json!(title);
            }
            if !manual.iter().any(|m| m == "dept") {
                props["dept"] = json!(dept);
            }
            if props == l.props {
                return Ok(false);
            }
            Ok(links::set_props(tx, ws, &key, &props)?)
        }
        Some(l) => {
            let mut history = l
                .props
                .get("history")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let mut past = l.props.clone();
            if let Some(o) = past.as_object_mut() {
                o.remove("history");
            }
            history.push(past);
            // back again: what you set on the job stays yours
            let manual =
                crate::contacts::profile::job_manual_fields(tx, ws, contact, org, &l.props)?;
            let keep = |k: &str, synced: Option<&str>| {
                if manual.iter().any(|m| m == k) {
                    l.props[k].clone()
                } else {
                    json!(synced)
                }
            };
            let mut props = json!({ "title": keep("title", title), "dept": keep("dept", dept), "from": this_month(), "to": null, "history": history });
            if !manual.is_empty() {
                props["manual"] = json!(manual);
            }
            Ok(links::set_props(tx, ws, &key, &props)?)
        }
    }
}

/// Closes the synced job at the own organization (`to` = this month). Returns whether it was open.
fn close_job(tx: &Tx, ws: &str, contact: &str, org: &str) -> ApiResult<bool> {
    let key = job_key(contact, org);
    match links::get(tx, ws, &key)? {
        Some(l) if l.source == "sync" && is_open(&l) => {
            let mut props = l.props.clone();
            if !props.is_object() {
                props = json!({});
            }
            props["to"] = json!(this_month());
            Ok(links::set_props(tx, ws, &key, &props)?)
        }
        _ => Ok(false),
    }
}

/// Makes a team's member rows exactly `want` (`source = sync`). Returns whether a row changed.
fn members(
    tx: &Tx,
    ws: &str,
    team: &str,
    want: &[(String, &str)],
    r: &mut SyncReport,
) -> ApiResult<bool> {
    let have = repo::member_rows(tx, ws, team)?;
    let mut changed = false;
    for (c, role, _) in &have {
        match want.iter().find(|(w, _)| w == c) {
            None => {
                trepo::member_remove(tx, ws, team, c)?;
                r.removed += 1;
                changed = true;
            }
            Some((_, w)) if w != role => {
                trepo::member_role(tx, ws, team, c, w)?;
                changed = true;
            }
            Some(_) => {}
        }
    }
    for (c, role) in want {
        if !have.iter().any(|(h, _, _)| h == c) {
            trepo::member_add(tx, ws, team, c, role, "sync")?;
            r.added += 1;
            changed = true;
        }
    }
    Ok(changed)
}

/// Makes the synced `reports_to` links of `contact` exactly `lead` (links you added stay). Returns
/// whether a link changed.
fn reports_to(tx: &Tx, ws: &str, contact: &str, lead: Option<&str>) -> ApiResult<bool> {
    let mut changed = false;
    let mut have_lead = false;
    for l in links::from(tx, ws, &pref(contact), Some("reports_to"))? {
        if Some(l.object_id.as_str()) == lead {
            have_lead = true;
            continue;
        }
        if l.source == "sync" {
            links::remove(
                tx,
                ws,
                &LinkKey {
                    subject: pref(contact),
                    predicate: "reports_to".into(),
                    object: pref(&l.object_id),
                },
            )?;
            changed = true;
        }
    }
    if let (Some(lead), false) = (lead, have_lead) {
        let l = NewLink::new(pref(contact), "reports_to", pref(lead), "sync");
        changed |= links::add(tx, ws, &l)?;
    }
    Ok(changed)
}
