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

use serde::Serialize;
use steve_modules::contacts::repo::{self as people, Party};
use steve_store::base::Viewer;
use steve_store::channel_spaces::members::{self as repo, Member};
use steve_store::{Read, Store, link_repo};

use crate::ChannelResult;
use crate::spaces::SpaceRow;

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

fn visible(viewer: &Viewer, p: &Party) -> bool {
    p.workspace_id == viewer.ws
        && p.status != "deleted"
        && viewer.sees(&p.visibility, &p.created_by)
}

/// One page of the members of space `id`: owners and admins first, then by name.
pub fn page(
    store: &Store,
    viewer: &Viewer,
    id: &str,
    offset: usize,
    limit: usize,
) -> ChannelResult<MembersPage> {
    store.read(|db| {
        let space = steve_store::channel_spaces::rows::get_in_workspace(db, &viewer.ws, id)?
            .ok_or_else(|| steve_modules::ApiError::not_found("空间"))?;
        let total = repo::count(db, &space.id)?;
        let rows = repo::page(db, &space.id, offset as u64, limit as u64)?;
        let self_org = people::self_org(db, &viewer.ws)?.map(|o| o.id);
        let members = rows
            .into_iter()
            .map(|row| member(db, viewer, &space, self_org.as_deref(), row))
            .collect::<ChannelResult<Vec<_>>>()?;
        Ok(MembersPage {
            total,
            offset,
            limit,
            members,
        })
    })
}

fn member(
    db: &mut impl Read,
    viewer: &Viewer,
    space: &SpaceRow,
    self_org: Option<&str>,
    r: Member,
) -> ChannelResult<MemberOut> {
    let party = steve_store::Contacts::new(db)
        .current_party(&r.party_id)?
        .filter(|p| p.status != "deleted");
    let jobs = match &party {
        Some(p) if p.kind == "contact" => link_repo::current_employers(db, &viewer.ws, &p.id)?,
        _ => Vec::new(),
    };
    let external = match &party {
        _ if !space.has_external => Some(false),
        Some(p) if p.user_id.is_some() || Some(p.id.as_str()) == self_org => Some(false),
        // one of our own app bots (each is its own bot party)
        Some(p)
            if p.contact_kind.as_deref() == Some("bot")
                && steve_store::Connections(db).has_live_party(&p.id)? =>
        {
            Some(false)
        }
        Some(p) if p.kind == "org" => Some(true),
        _ if self_org.is_some_and(|o| jobs.iter().any(|j| j == o)) => Some(false),
        _ if !jobs.is_empty() => Some(true),
        Some(p) if p.rels.iter().any(|r| INTERNAL_RELS.contains(&r.as_str())) => Some(false),
        Some(p) if p.rels.iter().any(|r| EXTERNAL_RELS.contains(&r.as_str())) => Some(true),
        _ => None,
    };
    let shown = party.filter(|p| visible(viewer, p));
    let org = match &shown {
        Some(_) => {
            let mut org = None;
            for j in &jobs {
                if let Some(o) = people::get(db, j)?.filter(|o| visible(viewer, o)) {
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
