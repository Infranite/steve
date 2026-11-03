//! Domain rules of contacts and organizations (contacts.md §2.1–§2.5, §3). Synchronous; the handlers run
//! them through [`Modules::blocking`]. Every function starts with [`start`], which creates “我” and the
//! workspace's own organization the first time any contacts interface is used (§2.1).

use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value, json};
use steve_store::base::links::{self, LinkKey, NewLink};
use steve_store::base::{self, Level, ObjRef, Viewer, Visibility, tags};
use steve_store::{ChangeEvent, Link, Tx};

use super::Ports;
use super::repo::{
    self, AttrDef, AttrValue, ContactQuery, NewDef, NewParty, NewValue, PARTY, Party, PartyPatch,
};
use crate::{ApiError, ApiResult, Caller, Check, Modules, own};

static KEY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-z][a-z0-9_]{0,31}$").expect("key regex"));
static MONTH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\d{4}-(0[1-9]|1[0-2])$").expect("month regex"));
static DOMAIN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[a-z0-9]([a-z0-9-]*[a-z0-9])?(\.[a-z0-9]([a-z0-9-]*[a-z0-9])?)+$")
        .expect("domain regex")
});

const CONTACT_KINDS: [&str; 3] = ["real", "virtual", "bot"];
const ORG_KINDS: [&str; 4] = ["customer", "supplier", "partner", "other"];
const ATTR_TYPES: [&str; 6] = ["text", "phone", "date", "address", "url", "enum"];
const APPLIES: [&str; 3] = ["contact", "org", "both"];

/// For `PATCH` bodies: a field that is absent stays `None`, an explicit `null` is `Some(None)`.
pub fn nullable<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> Result<Option<Option<T>>, D::Error> {
    Option::<T>::deserialize(d).map(Some)
}

/// The current month as `YYYY-MM` in local time (closing jobs “today”).
pub fn this_month() -> String {
    chrono::Local::now().format("%Y-%m").to_string()
}

pub(crate) fn pref(id: &str) -> ObjRef {
    ObjRef::new(PARTY, id)
}

pub(crate) fn event(tx: &Tx, ws: &str, id: &str) {
    tx.publish(ChangeEvent::Party {
        id: id.into(),
        ws: ws.into(),
    });
}

// ---------------------------------------------------------------- shapes (§3.1)

/// `org` of a `ContactBrief`: the primary current job.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OrgRef {
    pub id: String,
    pub name: String,
    pub title: Option<String>,
    pub dept: Option<String>,
}

/// The fields `ContactBrief` and `Contact` share.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ContactHead {
    pub id: String,
    pub name: String,
    pub aliases: Vec<String>,
    pub kind: Option<String>,
    pub rels: Vec<String>,
    pub important: bool,
    pub org: Option<OrgRef>,
    pub tags: Vec<String>,
    pub visibility: Visibility,
    pub is_me: bool,
    pub avatar: Value,
    pub updated: i64,
}

/// A contact in a list.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ContactBrief {
    #[serde(flatten)]
    pub head: ContactHead,
    pub accounts: i64,
    pub items: i64,
}

/// One job (`works_at` link).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Job {
    pub org_id: String,
    pub org_name: String,
    pub title: Option<String>,
    pub dept: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub source: String,
}

/// An attribute value as the pages show it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Attr {
    pub id: String,
    pub key: String,
    pub label: String,
    pub value: String,
    pub source_label: String,
    pub level: Level,
    pub status: String,
}

impl From<&AttrValue> for Attr {
    fn from(v: &AttrValue) -> Self {
        Self {
            id: v.id.clone(),
            key: v.key.clone(),
            label: v.label.clone(),
            value: v.value.clone(),
            source_label: v.source_label.clone(),
            level: v.level,
            status: v.status.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NameRef {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MergeRef {
    pub id: String,
    pub from_name: String,
    pub created: i64,
    pub splittable: bool,
}

/// A contact's page.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Contact {
    #[serde(flatten)]
    pub head: ContactHead,
    pub note: String,
    pub status: String,
    pub merged_into: Option<String>,
    pub source: String,
    pub revision: i64,
    pub created: i64,
    pub jobs: Vec<Job>,
    pub attrs: Vec<Attr>,
    pub teams: Vec<repo::TeamRef>,
    pub reports_to: Vec<NameRef>,
    pub merges: Vec<MergeRef>,
    /// What is known about them and why the rest is not ([`super::profile::contact_profile`]).
    pub profile: super::profile::ContactProfile,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OrgPerson {
    pub id: String,
    pub name: String,
    pub title: Option<String>,
}

/// An organization; `people` only on the detail.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Org {
    pub id: String,
    pub name: String,
    pub aliases: Vec<String>,
    pub kind: Option<String>,
    pub domains: Vec<String>,
    pub note: String,
    pub tags: Vec<String>,
    pub attrs: Vec<Attr>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub people: Option<Vec<OrgPerson>>,
    pub revision: i64,
    /// Where the name comes from (`name_state`, `name_reason`, `name_scope`, `tenant`).
    #[serde(flatten)]
    pub naming: super::profile::OrgName,
}

/// `Contact | Org` (the answer of `POST /parties/:id/attrs`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum PartyView {
    Contact(Box<Contact>),
    Org(Box<Org>),
}

// ---------------------------------------------------------------- “我”, own organization, ownership

/// Creates “我” for the caller and the workspace's own organization when missing (§2.1), and returns the
/// caller's viewer and “我”.
pub fn start(m: &Modules, who: &Caller) -> ApiResult<(Viewer, Party)> {
    let found = m.store.reader(|c| {
        Ok::<_, ApiError>((
            repo::me_of(c, &who.ws, &who.me)?,
            repo::self_org(c, &who.ws)?.is_some(),
        ))
    })?;
    let me = match found {
        (Some(me), true) => me,
        _ => m.store.transaction(|tx| ensure_base(tx, who))?,
    };
    Ok((m.viewer(who)?, me))
}

/// [`start`]'s write part, also usable inside another transaction.
pub fn ensure_base(tx: &Tx, who: &Caller) -> ApiResult<Party> {
    let me = match repo::me_of(tx, &who.ws, &who.me)? {
        Some(me) => me,
        None => {
            let name = repo::user_name(tx, &who.me)?.unwrap_or_else(|| "我".into());
            let mut p = NewParty::new("contact", &name, &who.me);
            p.contact_kind = Some("real".into());
            p.user_id = Some(who.me.clone());
            p.rels = vec!["me".into()];
            let me = repo::insert(tx, &who.ws, &p)?;
            base::audit(
                tx,
                &who.ws,
                &who.me,
                "party.create",
                &me.id,
                &json!({ "type": "contact", "me": true }),
            )?;
            event(tx, &who.ws, &me.id);
            // ownership.md §3: Steve's bots, personas, the Hub device and connections are “我”'s
            super::stewards::seed_defaults(tx, &who.ws, &me)?;
            me
        }
    };
    ensure_self_org(tx, &who.ws, &who.me)?;
    Ok(me)
}

/// The workspace's own organization (`org_kind = self`), created on first need with the workspace's name.
/// At most one per workspace: callers hold the write transaction.
pub fn ensure_self_org(tx: &Tx, ws: &str, actor: &str) -> ApiResult<Party> {
    if let Some(o) = repo::self_org(tx, ws)? {
        return Ok(o);
    }
    let name = repo::workspace_name(tx, ws)?.unwrap_or_else(|| "本公司".into());
    let mut p = NewParty::new("org", &name, actor);
    p.org_kind = Some("self".into());
    let o = repo::insert(tx, ws, &p)?;
    base::audit(
        tx,
        ws,
        actor,
        "party.create",
        &o.id,
        &json!({ "type": "org", "self": true }),
    )?;
    event(tx, ws, &o.id);
    Ok(o)
}

/// `own_<x>`: a party of `kind` that exists, is in the viewer's workspace, visible, and not deleted;
/// with `active`, also not merged (writes). `404 没有这个<noun>` otherwise.
pub fn own_party(
    c: &rusqlite::Connection,
    viewer: &Viewer,
    id: &str,
    kind: &str,
    noun: &str,
    active: bool,
) -> ApiResult<Party> {
    let p = repo::get(c, id)?
        .filter(|p| p.kind == kind && p.status != "deleted" && (!active || p.status == "active"));
    own(p, viewer, noun)
}

/// `me` in a path means “我”.
fn resolve<'a>(id: &'a str, me: &'a Party) -> &'a str {
    if id == "me" { &me.id } else { id }
}

/// `visibility` of a body: `team:<id>` must name a team of the workspace; `space:` needs the channels
/// module, which is not installed with this slice.
pub(crate) fn check_visibility(
    c: &rusqlite::Connection,
    ws: &str,
    v: &Visibility,
) -> ApiResult<()> {
    match v {
        Visibility::Team(id) if !repo::team_exists(c, ws, id)? => Err(ApiError::not_found("团队")),
        Visibility::Space(_) => Err(ApiError::invalid("visibility")),
        _ => Ok(()),
    }
}

/// Writes a revision and bumps the party's `revision` (§0.7).
/// [`revise`] for other modules' settling code (identification, enrichment): one revision with the
/// given `before` / `after`, then the patch.
pub fn revise_as(
    tx: &Tx,
    p: &Party,
    patch: &PartyPatch,
    before: Value,
    after: Value,
    author: &str,
) -> ApiResult<()> {
    revise(tx, p, patch, before, after, author)
}

pub(crate) fn revise(
    tx: &Tx,
    p: &Party,
    patch: &PartyPatch,
    before: Value,
    after: Value,
    author: &str,
) -> ApiResult<()> {
    let version = p.revision + 1;
    base::revision(
        tx,
        &p.workspace_id,
        PARTY,
        &p.id,
        version,
        &before,
        &after,
        author,
    )?;
    repo::update(tx, &p.id, patch, Some(version))?;
    Ok(())
}

// ---------------------------------------------------------------- building the shapes

fn job_of(c: &rusqlite::Connection, viewer: &Viewer, l: &Link) -> ApiResult<Option<Job>> {
    if l.object_type != PARTY {
        return Ok(None);
    }
    let Some(o) = repo::get(c, &l.object_id)? else {
        return Ok(None);
    };
    if o.workspace_id != viewer.ws || !viewer.sees(&o.visibility, &o.created_by) {
        return Ok(None);
    }
    let s = |k: &str| l.props.get(k).and_then(Value::as_str).map(String::from);
    Ok(Some(Job {
        org_id: o.id,
        org_name: o.name,
        title: s("title"),
        dept: s("dept"),
        from: s("from"),
        to: s("to"),
        source: l.source.clone(),
    }))
}

/// Jobs of a contact: current ones first, each group by `from` descending (unknown last), then newest link.
pub fn jobs(c: &rusqlite::Connection, viewer: &Viewer, contact: &str) -> ApiResult<Vec<Job>> {
    let mut ls = links::from(c, &viewer.ws, &pref(contact), Some("works_at"))?;
    ls.sort_by(|a, b| {
        let k = |l: &Link| {
            (
                l.props.get("to").and_then(Value::as_str).is_some(),
                std::cmp::Reverse(
                    l.props
                        .get("from")
                        .and_then(Value::as_str)
                        .map(String::from),
                ),
                std::cmp::Reverse(l.created),
            )
        };
        k(a).cmp(&k(b))
    });
    let mut out = Vec::new();
    for l in &ls {
        if let Some(j) = job_of(c, viewer, l)? {
            out.push(j);
        }
    }
    Ok(out)
}

fn head(
    c: &rusqlite::Connection,
    viewer: &Viewer,
    p: &Party,
    jobs: &[Job],
) -> ApiResult<ContactHead> {
    let org = jobs.iter().find(|j| j.to.is_none()).map(|j| OrgRef {
        id: j.org_id.clone(),
        name: j.org_name.clone(),
        title: j.title.clone(),
        dept: j.dept.clone(),
    });
    Ok(ContactHead {
        id: p.id.clone(),
        name: p.name.clone(),
        aliases: p.aliases.clone(),
        kind: p.contact_kind.clone(),
        rels: p.rels.clone(),
        important: p.important,
        org,
        tags: tags::of(c, &viewer.ws, &pref(&p.id))?,
        visibility: p.visibility.clone(),
        is_me: p.is_me(),
        avatar: p.avatar.clone(),
        updated: p.updated,
    })
}

pub fn brief(
    c: &rusqlite::Connection,
    viewer: &Viewer,
    ports: &Ports,
    p: &Party,
) -> ApiResult<ContactBrief> {
    let js = jobs(c, viewer, &p.id)?;
    Ok(ContactBrief {
        head: head(c, viewer, p, &js)?,
        accounts: match &ports.handles {
            Some(h) => h.count(c, &viewer.ws, &p.id)?,
            None => 0,
        },
        items: repo::items_count(c, &viewer.ws, &p.id)?,
    })
}

pub fn contact(c: &rusqlite::Connection, viewer: &Viewer, p: &Party) -> ApiResult<Contact> {
    let js = jobs(c, viewer, &p.id)?;
    let visible_orgs: Vec<String> = js.iter().map(|j| j.org_id.clone()).collect();
    let mut reports_to = Vec::new();
    for l in links::from(c, &viewer.ws, &pref(&p.id), Some("reports_to"))? {
        if let Some(b) = repo::get(c, &l.object_id)?.filter(|b| {
            b.workspace_id == viewer.ws
                && b.status != "deleted"
                && viewer.sees(&b.visibility, &b.created_by)
        }) {
            reports_to.push(NameRef {
                id: b.id,
                name: b.name,
            });
        }
    }
    let mut merges = Vec::new();
    for (id, from_name, created, split) in repo::merges_into(c, &viewer.ws, &p.id)? {
        if split.is_none() {
            merges.push(MergeRef {
                id,
                from_name,
                created,
                splittable: merges.is_empty(),
            });
        }
    }
    Ok(Contact {
        head: head(c, viewer, p, &js)?,
        note: p.note.clone(),
        status: p.status.clone(),
        merged_into: p.merged_into.clone(),
        source: p.source.clone(),
        revision: p.revision,
        created: p.created,
        jobs: js,
        attrs: repo::values(c, &viewer.ws, &p.id)?
            .iter()
            .map(Attr::from)
            .collect(),
        teams: repo::teams_of(c, viewer, &p.id)?,
        reports_to,
        merges,
        profile: super::profile::contact_profile(c, &viewer.ws, p, Some(&visible_orgs))?,
    })
}

pub fn org(
    c: &rusqlite::Connection,
    viewer: &Viewer,
    p: &Party,
    with_people: bool,
) -> ApiResult<Org> {
    let people = if with_people {
        Some(
            repo::org_people(c, viewer, &p.id)?
                .into_iter()
                .map(|(id, name, title)| OrgPerson { id, name, title })
                .collect(),
        )
    } else {
        None
    };
    Ok(Org {
        id: p.id.clone(),
        name: p.name.clone(),
        aliases: p.aliases.clone(),
        kind: p.org_kind.clone(),
        domains: repo::domains(c, &viewer.ws, &p.id)?,
        note: p.note.clone(),
        tags: tags::of(c, &viewer.ws, &pref(&p.id))?,
        attrs: repo::values(c, &viewer.ws, &p.id)?
            .iter()
            .map(Attr::from)
            .collect(),
        people,
        revision: p.revision,
        naming: super::profile::org_name(c, &viewer.ws, p)?,
    })
}

fn contact_by_id(m: &Modules, viewer: &Viewer, id: &str) -> ApiResult<Contact> {
    m.store.reader(|c| {
        let p = own_party(c, viewer, id, "contact", "联系人", false)?;
        contact(c, viewer, &p)
    })
}

fn org_by_id(m: &Modules, viewer: &Viewer, id: &str) -> ApiResult<Org> {
    m.store.reader(|c| {
        let p = own_party(c, viewer, id, "org", "组织", false)?;
        org(c, viewer, &p, true)
    })
}

fn party_view(m: &Modules, viewer: &Viewer, p: &Party) -> ApiResult<PartyView> {
    Ok(if p.kind == "org" {
        PartyView::Org(Box::new(org_by_id(m, viewer, &p.id)?))
    } else {
        PartyView::Contact(Box::new(contact_by_id(m, viewer, &p.id)?))
    })
}

// ---------------------------------------------------------------- contacts

/// Query of `GET /contacts` as it arrives (strings, so bad values give `400 参数不对：<field>`).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ContactsParams {
    pub q: Option<String>,
    pub rel: Option<String>,
    pub kind: Option<String>,
    pub org: Option<String>,
    pub tag: Option<String>,
    pub limit: Option<String>,
}

pub fn list_contacts(
    m: &Modules,
    ports: &Ports,
    who: &Caller,
    p: ContactsParams,
) -> ApiResult<Vec<ContactBrief>> {
    let (viewer, _) = start(m, who)?;
    let some = |s: Option<String>| s.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let limit = match some(p.limit) {
        None => 200,
        Some(s) => s
            .parse::<i64>()
            .ok()
            .filter(|n| (1..=500).contains(n))
            .ok_or_else(|| ApiError::invalid("limit"))?,
    };
    let kind = some(p.kind);
    if let Some(k) = &kind {
        Check::new()
            .one_of("kind", k, &["real", "virtual", "unconfirmed"])
            .done()?;
    }
    m.store.reader(|c| {
        let q = some(p.q);
        let handle_ids = match (&q, &ports.handles) {
            (Some(q), Some(h)) => h.parties(c, &who.ws, q)?,
            _ => Vec::new(),
        };
        let f = ContactQuery {
            q,
            rel: some(p.rel),
            kind,
            org: some(p.org),
            tag: some(p.tag),
            handle_ids,
            limit,
        };
        repo::contacts(c, &viewer, &f)?
            .iter()
            .map(|p| brief(c, &viewer, ports, p))
            .collect()
    })
}

/// `GET /contacts/:id` (`me` = “我”).
pub fn get_contact(m: &Modules, who: &Caller, id: &str) -> ApiResult<Contact> {
    let (viewer, me) = start(m, who)?;
    contact_by_id(m, &viewer, resolve(id, &me))
}

/// `GET /me/contact`.
pub fn me_contact(m: &Modules, who: &Caller) -> ApiResult<Contact> {
    get_contact(m, who, "me")
}

fn check_rels(ck: &mut Check, rels: &[String], is_me: bool) {
    ck.list("rels", rels, 5, 20);
    let has_me = rels.iter().any(|r| r == "me");
    ck.that("rels", has_me == is_me);
}

pub(crate) fn dedup(xs: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for x in xs.into_iter().map(|x| x.trim().to_string()) {
        if !out.contains(&x) {
            out.push(x);
        }
    }
    out
}

#[derive(Debug, Clone, Deserialize)]
pub struct NewContact {
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub rels: Vec<String>,
    pub org_id: Option<String>,
    pub title: Option<String>,
    pub kind: Option<String>,
    pub note: Option<String>,
    pub visibility: Option<Visibility>,
}

/// `POST /contacts`.
pub fn create_contact(m: &Modules, who: &Caller, b: NewContact) -> ApiResult<Contact> {
    let (viewer, _) = start(m, who)?;
    let (aliases, rels) = (dedup(b.aliases), dedup(b.rels));
    let mut ck = Check::new();
    ck.len("name", b.name.trim(), 1, 80)
        .list("aliases", &aliases, 10, 80);
    check_rels(&mut ck, &rels, false);
    ck.opt_len("title", b.title.as_deref(), 0, 80)
        .opt_len("note", b.note.as_deref(), 0, 2000);
    if let Some(k) = &b.kind {
        ck.one_of("kind", k, &CONTACT_KINDS);
    }
    ck.done()?;
    let id = m.store.transaction(|tx| {
        let org = match &b.org_id {
            Some(o) => Some(own_party(tx, &viewer, o, "org", "组织", true)?),
            None => None,
        };
        let vis = b.visibility.clone().unwrap_or(Visibility::Workspace);
        check_visibility(tx, &who.ws, &vis)?;
        let mut p = NewParty::new("contact", b.name.trim(), &who.me);
        p.aliases = aliases;
        p.rels = rels;
        p.note = b.note.clone().unwrap_or_default();
        p.contact_kind = Some(b.kind.clone().unwrap_or_else(|| "real".into()));
        p.visibility = vis;
        let p = repo::insert(tx, &who.ws, &p)?;
        if let Some(o) = org {
            let mut l = NewLink::new(pref(&p.id), "works_at", pref(&o.id), "manual");
            l.props = job_props(b.title.as_deref(), None, None);
            links::add(tx, &who.ws, &l)?;
        }
        base::audit(
            tx,
            &who.ws,
            &who.me,
            "party.create",
            &p.id,
            &json!({ "type": "contact" }),
        )?;
        event(tx, &who.ws, &p.id);
        Ok::<_, ApiError>(p.id)
    })?;
    contact_by_id(m, &viewer, &id)
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ContactPatch {
    pub name: Option<String>,
    pub aliases: Option<Vec<String>>,
    pub rels: Option<Vec<String>>,
    pub important: Option<bool>,
    pub note: Option<String>,
    pub kind: Option<String>,
    pub visibility: Option<Visibility>,
    pub tags: Option<Vec<String>>,
}

/// `PATCH /contacts/:id`. “我” keeps `me` in its rels and stays `real`.
pub fn patch_contact(m: &Modules, who: &Caller, id: &str, b: ContactPatch) -> ApiResult<Contact> {
    let (viewer, me) = start(m, who)?;
    let id = resolve(id, &me).to_string();
    let aliases = b.aliases.map(dedup);
    let rels = b.rels.map(dedup);
    m.store.transaction(|tx| {
        let p = own_party(tx, &viewer, &id, "contact", "联系人", true)?;
        let mut ck = Check::new();
        if let Some(n) = &b.name {
            ck.len("name", n.trim(), 1, 80);
        }
        if let Some(a) = &aliases {
            ck.list("aliases", a, 10, 80);
        }
        if let Some(r) = &rels {
            check_rels(&mut ck, r, p.is_me());
        }
        ck.opt_len("note", b.note.as_deref(), 0, 2000);
        if let Some(k) = &b.kind {
            ck.one_of("kind", k, &CONTACT_KINDS)
                .that("kind", !p.is_me() || k == "real");
        }
        if let Some(t) = &b.tags {
            ck.list("tags", t, 50, 64);
        }
        ck.done()?;
        if let Some(v) = &b.visibility {
            check_visibility(tx, &who.ws, v)?;
        }
        let patch = PartyPatch {
            name: b
                .name
                .as_ref()
                .map(|n| n.trim().to_string())
                .filter(|n| *n != p.name),
            aliases: aliases.filter(|a| *a != p.aliases),
            rels: rels.filter(|r| *r != p.rels),
            important: b.important.filter(|i| *i != p.important),
            note: b.note.clone().filter(|n| *n != p.note),
            contact_kind: b
                .kind
                .clone()
                .filter(|k| Some(k) != p.contact_kind.as_ref()),
            visibility: b.visibility.clone().filter(|v| *v != p.visibility),
            ..Default::default()
        };
        let (before, after) = diff(&p, &patch);
        let old_tags = tags::of(tx, &who.ws, &pref(&p.id))?;
        let new_tags = match &b.tags {
            Some(t) => tags::set(tx, &who.ws, &pref(&p.id), t)?,
            None => old_tags.clone(),
        };
        let (mut before, mut after) = (before, after);
        if new_tags != old_tags {
            before.insert("tags".into(), json!(old_tags));
            after.insert("tags".into(), json!(new_tags));
        }
        if !after.is_empty() {
            revise(
                tx,
                &p,
                &patch,
                Value::Object(before),
                Value::Object(after.clone()),
                &who.me,
            )?;
            let fields: Vec<&String> = after.keys().collect();
            base::audit(
                tx,
                &who.ws,
                &who.me,
                "party.update",
                &p.id,
                &json!({ "fields": fields }),
            )?;
            event(tx, &who.ws, &p.id);
        }
        Ok::<_, ApiError>(())
    })?;
    contact_by_id(m, &viewer, &id)
}

/// `{before, after}` of the changed columns of a patch.
fn diff(p: &Party, x: &PartyPatch) -> (Map<String, Value>, Map<String, Value>) {
    let (mut b, mut a) = (Map::new(), Map::new());
    let mut put = |k: &str, old: Value, new: Option<Value>| {
        if let Some(new) = new {
            b.insert(k.into(), old);
            a.insert(k.into(), new);
        }
    };
    put("name", json!(p.name), x.name.as_ref().map(|v| json!(v)));
    put(
        "aliases",
        json!(p.aliases),
        x.aliases.as_ref().map(|v| json!(v)),
    );
    put("rels", json!(p.rels), x.rels.as_ref().map(|v| json!(v)));
    put(
        "important",
        json!(p.important),
        x.important.map(|v| json!(v)),
    );
    put("note", json!(p.note), x.note.as_ref().map(|v| json!(v)));
    put(
        "kind",
        json!(p.contact_kind.as_ref().or(p.org_kind.as_ref())),
        x.contact_kind
            .as_ref()
            .or(x.org_kind.as_ref())
            .map(|v| json!(v)),
    );
    put(
        "visibility",
        json!(p.visibility),
        x.visibility.as_ref().map(|v| json!(v)),
    );
    put(
        "status",
        json!(p.status),
        x.status.as_ref().map(|v| json!(v)),
    );
    (b, a)
}

/// `POST /contacts/:id/confirm`: `unconfirmed → real`; other kinds `409 这个联系人已经确认过了`.
pub fn confirm_contact(m: &Modules, who: &Caller, id: &str) -> ApiResult<Contact> {
    let (viewer, me) = start(m, who)?;
    let id = resolve(id, &me).to_string();
    m.store.transaction(|tx| {
        let p = own_party(tx, &viewer, &id, "contact", "联系人", true)?;
        if p.contact_kind.as_deref() != Some("unconfirmed") {
            return Err(ApiError::conflict("这个联系人已经确认过了"));
        }
        let patch = PartyPatch {
            contact_kind: Some("real".into()),
            ..Default::default()
        };
        revise(
            tx,
            &p,
            &patch,
            json!({ "kind": "unconfirmed" }),
            json!({ "kind": "real" }),
            &who.me,
        )?;
        base::audit(tx, &who.ws, &who.me, "party.confirm", &p.id, &json!({}))?;
        event(tx, &who.ws, &p.id);
        Ok(())
    })?;
    contact_by_id(m, &viewer, &id)
}

/// `DELETE /contacts/:id` (§2.2): the row stays as `deleted`; its attribute values, the memories only
/// about it and its team memberships are physically deleted, other modules follow through
/// [`super::PartyHooks::on_delete`]; links stay. The audit records only counts.
pub fn delete_contact(m: &Modules, ports: &Ports, who: &Caller, id: &str) -> ApiResult<()> {
    let (viewer, me) = start(m, who)?;
    let id = resolve(id, &me).to_string();
    m.store.transaction(|tx| {
        let p = own_party(tx, &viewer, &id, "contact", "联系人", true)?;
        if p.is_me() {
            return Err(ApiError::rule("不能删除你自己"));
        }
        let attrs = repo::values_delete_of(tx, &who.ws, &p.id)?;
        let mems = super::memory::delete_only_about(tx, &who.ws, &p.id)?;
        let teams = repo::memberships_delete_of(tx, &who.ws, &p.id)?;
        super::stewards::forget_party(tx, &who.ws, &p.id)?;
        for h in &ports.hooks {
            h.on_delete(tx, &who.ws, &p.id)?;
        }
        let patch = PartyPatch {
            status: Some("deleted".into()),
            ..Default::default()
        };
        revise(
            tx,
            &p,
            &patch,
            json!({ "status": p.status }),
            json!({ "status": "deleted" }),
            &who.me,
        )?;
        base::audit(
            tx,
            &who.ws,
            &who.me,
            "party.delete",
            &p.id,
            &json!({ "type": "contact", "attrs": attrs, "mems": mems, "teams": teams }),
        )?;
        event(tx, &who.ws, &p.id);
        Ok(())
    })
}

// ---------------------------------------------------------------- jobs (§2.3)

fn job_props(title: Option<&str>, dept: Option<&str>, from: Option<&str>) -> Value {
    json!({ "title": title, "dept": dept, "from": from, "to": null })
}

fn job_key(contact: &str, org: &str) -> LinkKey {
    LinkKey {
        subject: pref(contact),
        predicate: "works_at".into(),
        object: pref(org),
    }
}

fn check_month(ck: &mut Check, field: &str, v: Option<&str>) {
    if let Some(v) = v {
        ck.pattern(field, v, &MONTH);
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct NewJob {
    pub org_id: String,
    pub title: Option<String>,
    pub dept: Option<String>,
    pub from: Option<String>,
}

/// `POST /contacts/:id/jobs`. A closed job at the same organization is reopened, its old interval moved
/// to `props.history` (one link per pair).
pub fn add_job(m: &Modules, who: &Caller, id: &str, b: NewJob) -> ApiResult<Contact> {
    let (viewer, me) = start(m, who)?;
    let id = resolve(id, &me).to_string();
    let mut ck = Check::new();
    ck.opt_len("title", b.title.as_deref(), 0, 80)
        .opt_len("dept", b.dept.as_deref(), 0, 80);
    check_month(&mut ck, "from", b.from.as_deref());
    ck.done()?;
    m.store.transaction(|tx| {
        let p = own_party(tx, &viewer, &id, "contact", "联系人", true)?;
        let o = own_party(tx, &viewer, &b.org_id, "org", "组织", true)?;
        let key = job_key(&p.id, &o.id);
        let mut props = job_props(b.title.as_deref(), b.dept.as_deref(), b.from.as_deref());
        let before = match links::get(tx, &who.ws, &key)? {
            Some(old) if old.props.get("to").is_none_or(Value::is_null) => {
                return Err(ApiError::conflict("已经在这个组织任职"));
            }
            Some(old) => {
                let mut history = old
                    .props
                    .get("history")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let mut past = old.props.clone();
                if let Some(o) = past.as_object_mut() {
                    o.remove("history");
                }
                history.push(past);
                props["history"] = Value::Array(history);
                // a synced job you add back by hand: what you entered (and edited before) stays yours
                let mut manual =
                    super::profile::job_manual_fields(tx, &who.ws, &p.id, &o.id, &old.props)?;
                for (k, v) in [("title", &b.title), ("dept", &b.dept)] {
                    if v.is_some() && !manual.iter().any(|m| m == k) {
                        manual.push(k.to_string());
                    }
                }
                if !manual.is_empty() {
                    props["manual"] = json!(manual);
                }
                links::set_props(tx, &who.ws, &key, &props)?;
                old.props
            }
            None => {
                let mut l = NewLink::new(
                    key.subject.clone(),
                    "works_at",
                    key.object.clone(),
                    "manual",
                );
                l.props = props.clone();
                links::add(tx, &who.ws, &l)?;
                Value::Null
            }
        };
        revise(
            tx,
            &p,
            &PartyPatch::default(),
            json!({ "job": { "org_id": o.id, "props": before } }),
            json!({ "job": { "org_id": o.id, "props": props } }),
            &who.me,
        )?;
        base::audit(
            tx,
            &who.ws,
            &who.me,
            "party.job_add",
            &p.id,
            &json!({ "org_id": o.id }),
        )?;
        event(tx, &who.ws, &p.id);
        Ok::<_, ApiError>(())
    })?;
    contact_by_id(m, &viewer, &id)
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct JobPatch {
    #[serde(default, deserialize_with = "nullable")]
    pub title: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    pub dept: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    pub from: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    pub to: Option<Option<String>>,
}

/// Finds the job link of a contact at an organization the viewer can see; `404 没有这个任职`.
fn own_job(tx: &Tx, viewer: &Viewer, contact: &str, org: &str) -> ApiResult<(LinkKey, Link)> {
    let key = job_key(contact, org);
    let visible = repo::get(tx, org)?
        .is_some_and(|o| o.workspace_id == viewer.ws && viewer.sees(&o.visibility, &o.created_by));
    match links::get(tx, &viewer.ws, &key)? {
        Some(l) if visible => Ok((key, l)),
        _ => Err(ApiError::not_found("任职")),
    }
}

/// `PATCH /contacts/:id/jobs/:org_id`; `to` closes the job, `to: null` reopens it.
pub fn patch_job(
    m: &Modules,
    who: &Caller,
    id: &str,
    org_id: &str,
    b: JobPatch,
) -> ApiResult<Contact> {
    let (viewer, me) = start(m, who)?;
    let id = resolve(id, &me).to_string();
    let mut ck = Check::new();
    ck.opt_len("title", b.title.clone().flatten().as_deref(), 0, 80)
        .opt_len("dept", b.dept.clone().flatten().as_deref(), 0, 80);
    check_month(&mut ck, "from", b.from.clone().flatten().as_deref());
    check_month(&mut ck, "to", b.to.clone().flatten().as_deref());
    ck.done()?;
    m.store.transaction(|tx| {
        let p = own_party(tx, &viewer, &id, "contact", "联系人", true)?;
        let (key, l) = own_job(tx, &viewer, &p.id, org_id)?;
        let mut props = l.props.clone();
        if !props.is_object() {
            props = json!({});
        }
        for (k, v) in [
            ("title", &b.title),
            ("dept", &b.dept),
            ("from", &b.from),
            ("to", &b.to),
        ] {
            if let Some(v) = v {
                props[k] = json!(v);
            }
        }
        // the fields you set: no sync writes them again
        let mut manual = super::profile::manual_fields(&l.props);
        for (k, v) in [("title", &b.title), ("dept", &b.dept)] {
            if v.is_some() && !manual.iter().any(|m| m == k) {
                manual.push(k.to_string());
            }
        }
        if !manual.is_empty() {
            props["manual"] = json!(manual);
        }
        if props != l.props {
            links::set_props(tx, &who.ws, &key, &props)?;
            revise(
                tx,
                &p,
                &PartyPatch::default(),
                json!({ "job": { "org_id": org_id, "props": l.props } }),
                json!({ "job": { "org_id": org_id, "props": props } }),
                &who.me,
            )?;
            base::audit(
                tx,
                &who.ws,
                &who.me,
                "party.job_update",
                &p.id,
                &json!({ "org_id": org_id }),
            )?;
            event(tx, &who.ws, &p.id);
        }
        Ok::<_, ApiError>(())
    })?;
    contact_by_id(m, &viewer, &id)
}

/// `DELETE /contacts/:id/jobs/:org_id` (a job entered by mistake; leaving is `to`).
pub fn delete_job(m: &Modules, who: &Caller, id: &str, org_id: &str) -> ApiResult<()> {
    let (viewer, me) = start(m, who)?;
    let id = resolve(id, &me).to_string();
    m.store.transaction(|tx| {
        let p = own_party(tx, &viewer, &id, "contact", "联系人", true)?;
        let (key, l) = own_job(tx, &viewer, &p.id, org_id)?;
        links::remove(tx, &who.ws, &key)?;
        revise(
            tx,
            &p,
            &PartyPatch::default(),
            json!({ "job": { "org_id": org_id, "props": l.props } }),
            json!({ "job": { "org_id": org_id, "props": null } }),
            &who.me,
        )?;
        base::audit(
            tx,
            &who.ws,
            &who.me,
            "party.job_remove",
            &p.id,
            &json!({ "org_id": org_id }),
        )?;
        event(tx, &who.ws, &p.id);
        Ok(())
    })
}

// ---------------------------------------------------------------- attributes (§2.5)

#[derive(Debug, Clone, Deserialize)]
pub struct NewAttr {
    pub key: String,
    pub label: Option<String>,
    pub value: String,
    pub level: Option<Level>,
}

/// A contact or organization by id for attribute writes (`404 没有这个联系人` / `组织`).
fn own_contact_or_org(c: &rusqlite::Connection, viewer: &Viewer, id: &str) -> ApiResult<Party> {
    let p = repo::get(c, id)?
        .filter(|p| p.status == "active" && (p.kind == "contact" || p.kind == "org"));
    let noun = if p.as_ref().is_some_and(|p| p.kind == "org") {
        "组织"
    } else {
        "联系人"
    };
    own(p, viewer, noun)
}

/// Value checks shared by add and edit: length, and `enum` definitions take one of their options.
fn check_value(ck: &mut Check, def: Option<&AttrDef>, value: &str) {
    ck.len("value", value.trim(), 1, 500);
    if let Some(d) = def.filter(|d| d.kind == "enum" && !d.options.is_empty()) {
        ck.that("value", d.options.iter().any(|o| o == value.trim()));
    }
}

/// `POST /parties/:id/attrs`: a value you entered (`confirmed`, “你填的”). A key without a definition gets
/// a `text` definition of yours; a newer confirmed value makes the older one with the same key + label stale.
pub fn add_attr(m: &Modules, who: &Caller, id: &str, b: NewAttr) -> ApiResult<PartyView> {
    let (viewer, me) = start(m, who)?;
    let id = resolve(id, &me).to_string();
    let p = m.store.transaction(|tx| {
        let p = own_contact_or_org(tx, &viewer, &id)?;
        let def = repo::def_by_key(tx, &who.ws, &b.key)?;
        let mut ck = Check::new();
        ck.pattern("key", &b.key, &KEY)
            .opt_len("label", b.label.as_deref().map(str::trim), 1, 20)
            .that("key", def.as_ref().is_none_or(|d| d.applies_to(&p.kind)));
        check_value(&mut ck, def.as_ref(), &b.value);
        ck.done()?;
        let def = match def {
            Some(d) => d,
            None => {
                let d = repo::def_insert(
                    tx,
                    &who.ws,
                    &NewDef {
                        applies: p.kind.clone(),
                        key: b.key.clone(),
                        label: b
                            .label
                            .as_deref()
                            .map(str::trim)
                            .unwrap_or(&b.key)
                            .to_string(),
                        kind: "text".into(),
                        options: Vec::new(),
                        default_level: b.level.unwrap_or(Level::Conclusion),
                        source: "me".into(),
                        status: "active".into(),
                    },
                )?;
                base::audit(
                    tx,
                    &who.ws,
                    &who.me,
                    "attr_def.create",
                    &d.id,
                    &json!({ "key": d.key }),
                )?;
                d
            }
        };
        let v = repo::value_insert(
            tx,
            &who.ws,
            &NewValue {
                party_id: p.id.clone(),
                key: def.key.clone(),
                label: b
                    .label
                    .as_deref()
                    .map(str::trim)
                    .unwrap_or(&def.label)
                    .to_string(),
                value: b.value.trim().to_string(),
                source: "me".into(),
                source_label: "你填的".into(),
                source_ref: None,
                level: b.level.unwrap_or(def.default_level),
                status: "confirmed".into(),
                decision_id: None,
            },
        )?;
        repo::stale_others(tx, &v)?;
        revise(
            tx,
            &p,
            &PartyPatch::default(),
            json!({ "attr": null }),
            json!({ "attr": { "id": v.id, "key": v.key, "label": v.label } }),
            &who.me,
        )?;
        base::audit(
            tx,
            &who.ws,
            &who.me,
            "attr.set",
            &p.id,
            &json!({ "attr_id": v.id, "key": v.key }),
        )?;
        event(tx, &who.ws, &p.id);
        Ok::<_, ApiError>(p)
    })?;
    party_view(m, &viewer, &p)
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct AttrPatch {
    pub value: Option<String>,
    pub level: Option<Level>,
    pub status: Option<String>,
}

/// An attribute value whose party the viewer owns; `404 没有这个属性`.
fn own_attr(tx: &Tx, viewer: &Viewer, id: &str) -> ApiResult<(AttrValue, Party)> {
    let v = repo::value_get(tx, id)?.filter(|v| v.workspace_id == viewer.ws);
    let p = match &v {
        Some(v) => repo::get(tx, &v.party_id)?.filter(|p| p.status == "active"),
        None => None,
    };
    match (v, p) {
        (Some(v), Some(p)) if viewer.sees(&p.visibility, &p.created_by) => Ok((v, p)),
        _ => Err(ApiError::not_found("属性")),
    }
}

/// `PATCH /attrs/:id`.
pub fn patch_attr(m: &Modules, who: &Caller, id: &str, b: AttrPatch) -> ApiResult<()> {
    let (viewer, _) = start(m, who)?;
    m.store.transaction(|tx| {
        let (v, p) = own_attr(tx, &viewer, id)?;
        let mut ck = Check::new();
        if let Some(val) = &b.value {
            check_value(&mut ck, repo::def_by_key(tx, &who.ws, &v.key)?.as_ref(), val);
        }
        if let Some(s) = &b.status {
            ck.one_of("status", s, &["confirmed", "stale"]);
        }
        ck.done()?;
        let value = b.value.as_deref().map(str::trim);
        repo::value_update(tx, &v.id, value, b.level, b.status.as_deref())?;
        let after = repo::value_get(tx, &v.id)?.ok_or_else(|| ApiError::not_found("属性"))?;
        if after.status == "confirmed" && (b.status.is_some() || b.value.is_some()) {
            repo::stale_others(tx, &after)?;
        }
        let snap = |x: &AttrValue| json!({ "id": x.id, "value": x.value, "level": x.level, "status": x.status });
        revise(tx, &p, &PartyPatch::default(), json!({ "attr": snap(&v) }), json!({ "attr": snap(&after) }), &who.me)?;
        base::audit(tx, &who.ws, &who.me, "attr.update", &p.id, &json!({ "attr_id": v.id, "key": v.key }))?;
        event(tx, &who.ws, &p.id);
        Ok(())
    })
}

/// `DELETE /attrs/:id` (physical).
pub fn delete_attr(m: &Modules, who: &Caller, id: &str) -> ApiResult<()> {
    let (viewer, _) = start(m, who)?;
    m.store.transaction(|tx| {
        let (v, p) = own_attr(tx, &viewer, id)?;
        repo::value_delete(tx, &v.id)?;
        revise(
            tx,
            &p,
            &PartyPatch::default(),
            json!({ "attr": { "id": v.id, "key": v.key, "label": v.label } }),
            json!({ "attr": null }),
            &who.me,
        )?;
        base::audit(
            tx,
            &who.ws,
            &who.me,
            "attr.delete",
            &p.id,
            &json!({ "attr_id": v.id, "key": v.key }),
        )?;
        event(tx, &who.ws, &p.id);
        Ok(())
    })
}

/// `GET /attr-defs`: built-in, then the workspace's.
pub fn list_defs(m: &Modules, who: &Caller) -> ApiResult<Vec<AttrDef>> {
    start(m, who)?;
    m.store.reader(|c| Ok(repo::defs(c, &who.ws)?))
}

#[derive(Debug, Clone, Deserialize)]
pub struct NewDefBody {
    pub applies: String,
    pub key: String,
    pub label: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub options: Vec<String>,
    pub default_level: Level,
}

fn check_options(ck: &mut Check, kind: Option<&str>, options: &[String]) {
    ck.list("options", options, 30, 40);
    if kind == Some("enum") {
        ck.that("options", !options.is_empty());
    }
}

/// `POST /attr-defs`; a key the workspace (or the built-ins) already has → `409 已有这个属性`.
pub fn create_def(m: &Modules, who: &Caller, b: NewDefBody) -> ApiResult<AttrDef> {
    start(m, who)?;
    let options = dedup(b.options);
    let mut ck = Check::new();
    ck.one_of("applies", &b.applies, &APPLIES)
        .pattern("key", &b.key, &KEY)
        .len("label", b.label.trim(), 1, 20)
        .one_of("type", &b.kind, &ATTR_TYPES);
    check_options(&mut ck, Some(&b.kind), &options);
    ck.done()?;
    m.store.transaction(|tx| {
        if repo::def_by_key(tx, &who.ws, &b.key)?.is_some() {
            return Err(ApiError::conflict("已有这个属性"));
        }
        let d = repo::def_insert(
            tx,
            &who.ws,
            &NewDef {
                applies: b.applies.clone(),
                key: b.key.clone(),
                label: b.label.trim().to_string(),
                kind: b.kind.clone(),
                options,
                default_level: b.default_level,
                source: "me".into(),
                status: "active".into(),
            },
        )?;
        base::audit(
            tx,
            &who.ws,
            &who.me,
            "attr_def.create",
            &d.id,
            &json!({ "key": d.key }),
        )?;
        Ok(d)
    })
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct DefPatch {
    pub label: Option<String>,
    pub options: Option<Vec<String>>,
    pub default_level: Option<Level>,
}

/// A definition of the caller's workspace for writes: missing or foreign → 404, built-in → 400.
fn own_def(tx: &Tx, ws: &str, id: &str) -> ApiResult<AttrDef> {
    match repo::def_get(tx, id)? {
        Some(d) if d.builtin => Err(ApiError::rule("内置属性不能修改")),
        Some(d) if d.workspace_id == ws => Ok(d),
        _ => Err(ApiError::not_found("属性")),
    }
}

/// `PATCH /attr-defs/:id`.
pub fn patch_def(m: &Modules, who: &Caller, id: &str, b: DefPatch) -> ApiResult<AttrDef> {
    start(m, who)?;
    let options = b.options.map(dedup);
    m.store.transaction(|tx| {
        let d = own_def(tx, &who.ws, id)?;
        let mut ck = Check::new();
        ck.opt_len("label", b.label.as_deref().map(str::trim), 1, 20);
        if let Some(o) = &options {
            check_options(&mut ck, Some(&d.kind), o);
        }
        ck.done()?;
        repo::def_update(
            tx,
            &d.id,
            b.label.as_deref().map(str::trim),
            options.as_deref(),
            b.default_level,
        )?;
        base::audit(
            tx,
            &who.ws,
            &who.me,
            "attr_def.update",
            &d.id,
            &json!({ "key": d.key }),
        )?;
        repo::def_get(tx, &d.id)?.ok_or_else(|| ApiError::not_found("属性"))
    })
}

/// `DELETE /attr-defs/:id`; still in use → `409 还有 <n> 个联系人在用这个属性`.
pub fn delete_def(m: &Modules, who: &Caller, id: &str) -> ApiResult<()> {
    start(m, who)?;
    m.store.transaction(|tx| {
        let d = own_def(tx, &who.ws, id)?;
        let n = repo::def_usage(tx, &who.ws, &d.key)?;
        if n > 0 {
            return Err(ApiError::conflict(format!("还有 {n} 个联系人在用这个属性")));
        }
        repo::def_delete(tx, &d.id)?;
        base::audit(
            tx,
            &who.ws,
            &who.me,
            "attr_def.delete",
            &d.id,
            &json!({ "key": d.key }),
        )?;
        Ok(())
    })
}

// ---------------------------------------------------------------- organizations (§2.2)

/// Lower case, without a mailbox part or `@`, without a leading `www.`.
pub fn normalize_domain(d: &str) -> String {
    let d = d.trim().to_lowercase();
    let d = d.rsplit('@').next().unwrap_or("");
    d.strip_prefix("www.").unwrap_or(d).to_string()
}

/// Normalized, deduplicated, checked domains; conflicts with another organization → 409.
fn check_domains(tx: &Tx, ws: &str, org: Option<&str>, raw: &[String]) -> ApiResult<Vec<String>> {
    let mut ck = Check::new();
    ck.that("domains", raw.len() <= 20);
    let mut out: Vec<String> = Vec::new();
    for (i, d) in raw.iter().enumerate() {
        let n = normalize_domain(d);
        ck.that(
            &format!("domains.{i}"),
            n.len() <= 253 && DOMAIN.is_match(&n),
        );
        if !out.contains(&n) {
            out.push(n);
        }
    }
    ck.done()?;
    for d in &out {
        if let Some(owner) = repo::domain_owner(tx, ws, d)?.filter(|o| Some(o.as_str()) != org) {
            let name = repo::get(tx, &owner)?.map(|o| o.name).unwrap_or_default();
            return Err(ApiError::conflict(format!("这个域名已经属于「{name}」")));
        }
    }
    Ok(out)
}

/// `GET /orgs` (without `people`).
pub fn list_orgs(m: &Modules, who: &Caller) -> ApiResult<Vec<Org>> {
    let (viewer, _) = start(m, who)?;
    m.store.reader(|c| {
        repo::orgs(c, &viewer)?
            .iter()
            .map(|o| org(c, &viewer, o, false))
            .collect()
    })
}

/// `GET /orgs/:id`.
pub fn get_org(m: &Modules, who: &Caller, id: &str) -> ApiResult<Org> {
    let (viewer, _) = start(m, who)?;
    org_by_id(m, &viewer, id)
}

#[derive(Debug, Clone, Deserialize)]
pub struct NewOrg {
    pub name: String,
    pub kind: Option<String>,
    #[serde(default)]
    pub domains: Vec<String>,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub note: Option<String>,
}

/// `POST /orgs`.
pub fn create_org(m: &Modules, who: &Caller, b: NewOrg) -> ApiResult<Org> {
    let (viewer, _) = start(m, who)?;
    let aliases = dedup(b.aliases);
    let mut ck = Check::new();
    ck.len("name", b.name.trim(), 1, 80)
        .list("aliases", &aliases, 10, 80)
        .opt_len("note", b.note.as_deref(), 0, 2000);
    if let Some(k) = &b.kind {
        ck.one_of("kind", k, &ORG_KINDS);
    }
    ck.done()?;
    let id = m.store.transaction(|tx| {
        let domains = check_domains(tx, &who.ws, None, &b.domains)?;
        let mut p = NewParty::new("org", b.name.trim(), &who.me);
        p.aliases = aliases;
        p.note = b.note.clone().unwrap_or_default();
        p.org_kind = Some(b.kind.clone().unwrap_or_else(|| "other".into()));
        let o = repo::insert(tx, &who.ws, &p)?;
        repo::set_domains(tx, &who.ws, &o.id, &domains)?;
        base::audit(
            tx,
            &who.ws,
            &who.me,
            "party.create",
            &o.id,
            &json!({ "type": "org" }),
        )?;
        event(tx, &who.ws, &o.id);
        Ok::<_, ApiError>(o.id)
    })?;
    org_by_id(m, &viewer, &id)
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct OrgPatch {
    pub name: Option<String>,
    pub kind: Option<String>,
    pub domains: Option<Vec<String>>,
    pub aliases: Option<Vec<String>>,
    pub note: Option<String>,
    pub tags: Option<Vec<String>>,
}

/// `PATCH /orgs/:id`; the own organization keeps its kind (`400 本公司不能改类型`).
pub fn patch_org(m: &Modules, who: &Caller, id: &str, b: OrgPatch) -> ApiResult<Org> {
    let (viewer, _) = start(m, who)?;
    let aliases = b.aliases.map(dedup);
    m.store.transaction(|tx| {
        let o = own_party(tx, &viewer, id, "org", "组织", true)?;
        let mut ck = Check::new();
        if let Some(n) = &b.name {
            ck.len("name", n.trim(), 1, 80);
        }
        if let Some(a) = &aliases {
            ck.list("aliases", a, 10, 80);
        }
        ck.opt_len("note", b.note.as_deref(), 0, 2000);
        if let Some(k) = &b.kind {
            ck.one_of("kind", k, &ORG_KINDS);
        }
        if let Some(t) = &b.tags {
            ck.list("tags", t, 50, 64);
        }
        ck.done()?;
        if o.is_self_org() && b.kind.is_some() {
            return Err(ApiError::rule("本公司不能改类型"));
        }
        let patch = PartyPatch {
            name: b
                .name
                .as_ref()
                .map(|n| n.trim().to_string())
                .filter(|n| *n != o.name),
            aliases: aliases.filter(|a| *a != o.aliases),
            note: b.note.clone().filter(|n| *n != o.note),
            org_kind: b.kind.clone().filter(|k| Some(k) != o.org_kind.as_ref()),
            ..Default::default()
        };
        let (mut before, mut after) = diff(&o, &patch);
        if let Some(raw) = &b.domains {
            let new = check_domains(tx, &who.ws, Some(&o.id), raw)?;
            let old = repo::domains(tx, &who.ws, &o.id)?;
            if new != old {
                repo::set_domains(tx, &who.ws, &o.id, &new)?;
                before.insert("domains".into(), json!(old));
                after.insert("domains".into(), json!(new));
            }
        }
        if let Some(t) = &b.tags {
            let old = tags::of(tx, &who.ws, &pref(&o.id))?;
            let new = tags::set(tx, &who.ws, &pref(&o.id), t)?;
            if new != old {
                before.insert("tags".into(), json!(old));
                after.insert("tags".into(), json!(new));
            }
        }
        if !after.is_empty() {
            revise(
                tx,
                &o,
                &patch,
                Value::Object(before),
                Value::Object(after.clone()),
                &who.me,
            )?;
            let fields: Vec<&String> = after.keys().collect();
            base::audit(
                tx,
                &who.ws,
                &who.me,
                "party.update",
                &o.id,
                &json!({ "type": "org", "fields": fields }),
            )?;
            event(tx, &who.ws, &o.id);
        }
        Ok::<_, ApiError>(())
    })?;
    org_by_id(m, &viewer, id)
}

/// `DELETE /orgs/:id` (§2.2): only the in-row details and the domains go; every open job there is closed
/// this month; the row stays as `deleted`.
pub fn delete_org(m: &Modules, who: &Caller, id: &str) -> ApiResult<()> {
    let (viewer, _) = start(m, who)?;
    m.store.transaction(|tx| {
        let o = own_party(tx, &viewer, id, "org", "组织", true)?;
        if o.is_self_org() {
            return Err(ApiError::rule("本公司不能删除"));
        }
        let month = this_month();
        let mut closed = 0;
        for l in links::to(tx, &who.ws, &pref(&o.id), Some("works_at"))? {
            if l.subject_type != PARTY || !l.props.get("to").is_none_or(Value::is_null) {
                continue;
            }
            let mut props = if l.props.is_object() {
                l.props.clone()
            } else {
                json!({})
            };
            props["to"] = json!(month);
            links::set_props(tx, &who.ws, &job_key(&l.subject_id, &o.id), &props)?;
            repo::touch(tx, &l.subject_id)?;
            closed += 1;
        }
        repo::set_domains(tx, &who.ws, &o.id, &[])?;
        let patch = PartyPatch {
            aliases: Some(Vec::new()),
            note: Some(String::new()),
            status: Some("deleted".into()),
            ..Default::default()
        };
        revise(
            tx,
            &o,
            &patch,
            json!({ "status": o.status, "aliases": o.aliases, "note": o.note }),
            json!({ "status": "deleted", "aliases": [], "note": "" }),
            &who.me,
        )?;
        base::audit(
            tx,
            &who.ws,
            &who.me,
            "party.delete",
            &o.id,
            &json!({ "type": "org", "jobs_closed": closed }),
        )?;
        event(tx, &who.ws, &o.id);
        Ok(())
    })
}
