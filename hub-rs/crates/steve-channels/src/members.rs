//! `GET /spaces/:id/members?offset=&limit=` (the space page's 成员): the
//! space's `space_members`, each with the contact `identify` attached it to and whether it is
//! external.
//!
//! - The space must be the viewer's to see (`404 没有这个空间`).
//! - A contact the viewer may not see (someone's private contact) is `contact: null`; only the
//!   account's own name and handle are shown.
//! - `external`: `false` for “我”, the own organization, our own bots, people with a current job
//!   (`works_at`, written by the directory sync) at the own organization or marked 同事 / 上级,
//!   and everybody in a space without external members; `true` for people working at another
//!   organization, marked 客户 / 合作方 / 供应商, and for other organizations; `null` (unknown)
//!   otherwise.

use rusqlite::{Connection, params};
use serde::Serialize;
use steve_modules::accounts::service::{current_party, is_ours};
use steve_modules::contacts::repo::{self as people, Party};
use steve_modules::contacts::service::own_party;
use steve_store::base::Viewer;

use crate::ChannelResult;
use crate::spaces::{self, SpaceRow};

pub const PAGE: usize = 50;
/// Relations to you that make a contact one of us / not one of us (when no job says so).
const INTERNAL_RELS: [&str; 2] = ["colleague", "manager"];
const EXTERNAL_RELS: [&str; 3] = ["customer", "partner", "supplier"];
pub const PAGE_MAX: usize = 200;

/// The contact behind a member's account.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ContactRef {
    pub id: String,
    /// `contact | org | team`
    #[serde(rename = "type")]
    pub kind: String,
    pub name: String,
    /// `real | virtual | unconfirmed` for contacts.
    pub contact_kind: Option<String>,
    /// It is “我” of some user.
    pub me: bool,
}

/// `{id, name}` of an organization.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OrgRef {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MemberOut {
    pub account_id: String,
    pub name: String,
    pub handle: String,
    /// `owner | admin | member | bot`
    pub role: String,
    pub joined: i64,
    pub contact: Option<ContactRef>,
    /// Where the contact works now (the first current `works_at`), when visible.
    pub org: Option<OrgRef>,
    pub external: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MembersPage {
    pub total: i64,
    pub offset: usize,
    pub limit: usize,
    pub members: Vec<MemberOut>,
}

struct Row {
    account_id: String,
    name: String,
    handle: String,
    role: String,
    joined: i64,
    party_id: String,
}

/// Current `works_at` organizations of a contact, oldest link first.
fn jobs(c: &Connection, ws: &str, party: &str) -> ChannelResult<Vec<String>> {
    let mut st = c.prepare_cached(
        "SELECT object_id FROM links WHERE workspace_id = ? AND subject_type = 'party' AND subject_id = ?
            AND predicate = 'works_at' AND object_type = 'party' AND json_extract(props, '$.to') IS NULL
          ORDER BY created, object_id",
    )?;
    let rows = st
        .query_map(params![ws, party], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn visible(c: &Connection, viewer: &Viewer, p: &Party) -> bool {
    own_party(c, viewer, &p.id, &p.kind, "联系人", false).is_ok()
}

/// One page of the members of space `id`: owners and admins first, then by name.
pub fn page(
    c: &Connection,
    viewer: &Viewer,
    id: &str,
    offset: usize,
    limit: usize,
) -> ChannelResult<MembersPage> {
    let space = spaces::own(c, viewer, id)?;
    let total: i64 = c.query_row(
        "SELECT count(*) FROM space_members WHERE space_id = ?",
        [&space.id],
        |r| r.get(0),
    )?;
    let mut st = c.prepare_cached(
        "SELECT m.account_id, CASE WHEN a.display_name <> '' THEN a.display_name ELSE a.handle_display END,
                a.handle_display, m.role, m.joined, a.party_id
           FROM space_members m JOIN accounts a ON a.id = m.account_id
          WHERE m.space_id = ?
          ORDER BY CASE m.role WHEN 'owner' THEN 0 WHEN 'admin' THEN 1 WHEN 'bot' THEN 3 ELSE 2 END,
                   CASE WHEN a.display_name <> '' THEN a.display_name ELSE a.handle_display END, m.account_id
          LIMIT ? OFFSET ?",
    )?;
    let rows = st
        .query_map(params![space.id, limit as i64, offset as i64], |r| {
            Ok(Row {
                account_id: r.get(0)?,
                name: r.get(1)?,
                handle: r.get(2)?,
                role: r.get(3)?,
                joined: r.get(4)?,
                party_id: r.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(st);
    let self_org = people::self_org(c, &viewer.ws)?.map(|o| o.id);
    let members = rows
        .into_iter()
        .map(|r| member(c, viewer, &space, self_org.as_deref(), r))
        .collect::<ChannelResult<Vec<_>>>()?;
    Ok(MembersPage {
        total,
        offset,
        limit,
        members,
    })
}

fn member(
    c: &Connection,
    viewer: &Viewer,
    space: &SpaceRow,
    self_org: Option<&str>,
    r: Row,
) -> ChannelResult<MemberOut> {
    let party = current_party(c, &r.party_id)?.filter(|p| p.status != "deleted");
    let jobs = match &party {
        Some(p) if p.kind == "contact" => jobs(c, &viewer.ws, &p.id)?,
        _ => Vec::new(),
    };
    let external = match &party {
        _ if !space.has_external => Some(false),
        Some(p) if p.user_id.is_some() || Some(p.id.as_str()) == self_org => Some(false),
        // one of our own app bots (each is its own bot party)
        Some(p) if is_ours(c, p)? => Some(false),
        Some(p) if p.kind == "org" => Some(true),
        _ if self_org.is_some_and(|o| jobs.iter().any(|j| j == o)) => Some(false),
        _ if !jobs.is_empty() => Some(true),
        Some(p) if p.rels.iter().any(|r| INTERNAL_RELS.contains(&r.as_str())) => Some(false),
        Some(p) if p.rels.iter().any(|r| EXTERNAL_RELS.contains(&r.as_str())) => Some(true),
        _ => None,
    };
    let shown = party.filter(|p| visible(c, viewer, p));
    let org = match &shown {
        Some(_) => {
            let mut org = None;
            for j in &jobs {
                if let Some(o) = people::get(c, j)?.filter(|o| visible(c, viewer, o)) {
                    org = Some(OrgRef {
                        id: o.id,
                        name: o.name,
                    });
                    break;
                }
            }
            org
        }
        None => None,
    };
    Ok(MemberOut {
        account_id: r.account_id,
        name: r.name,
        handle: r.handle,
        role: r.role,
        joined: r.joined,
        contact: shown.map(|p| ContactRef {
            me: p.user_id.is_some(),
            id: p.id,
            kind: p.kind,
            name: p.name,
            contact_kind: p.contact_kind,
        }),
        org,
        external,
    })
}
