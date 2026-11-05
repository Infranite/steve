//! Teams: virtual teams you
//! build and edit, org-chart teams that follow the directory sync (only `purpose`, `space_id` and tags are
//! local), headcounts over the sub-tree, and `team:<id>` as a visibility scope (the scope itself is
//! [`steve_store::base::Viewer::load`]).
//!
//! Other modules plug in through [`super::Ports`]: [`SpacePort`] (the channels module checks `space_id`)
//! and [`TeamSync`] (the directory sync of slice P6 queues `POST /teams/sync` and names its connection).

pub mod api;
pub mod repo;

use chrono::TimeZone;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use steve_store::Tx;
use steve_store::base::{self, Viewer, Visibility, tags};

use super::Ports;
use super::repo::{self as prepo, NewParty, Party, PartyPatch};
use super::service::{check_visibility, event, jobs, nullable, own_party, pref, revise, start};
use crate::{ApiError, ApiResult, Caller, Check, Modules};
use repo::Details;

/// `400` for every change of an org-chart team beyond `purpose`, `space_id` and tags (verbatim).
pub const CHART_READONLY: &str = "组织架构团队跟随同步，不能改这一项";
/// `400` of `POST /teams/sync` without a directory connection (verbatim).
pub const NO_DIRECTORY: &str = "还没有接入能读通讯录的账号";

/// [`CHART_READONLY`] in the request's language.
pub fn chart_readonly() -> String {
    steve_i18n::t!("modules.teams.chart_readonly")
}

const ROLES: [&str; 2] = ["lead", "member"];

/// Spaces (groups) of the channels module, for `PATCH /teams/:id {space_id}`. Without it only `null` is
/// accepted.
pub trait SpacePort: Send + Sync {
    /// Is `space_id` a space of the viewer's workspace they can see?
    fn visible(&self, c: &Connection, viewer: &Viewer, space_id: &str) -> ApiResult<bool>;

    /// Where each of these contacts was seen (the spaces the viewer can see, the last message), for
    /// telling unknown people apart in 待确认. Contacts never seen are left out.
    fn seen(
        &self,
        c: &Connection,
        viewer: &Viewer,
        parties: &[String],
    ) -> ApiResult<Vec<(String, Seen)>> {
        let _ = (c, viewer, parties);
        Ok(Vec::new())
    }
}

/// Where a contact was seen in the channels.
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct Seen {
    /// Most recently active first.
    pub spaces: Vec<SeenSpace>,
    /// Their last message the viewer may see.
    pub last: Option<SeenMessage>,
    /// Messages from them.
    pub messages: i64,
    /// When they were last active (message or joining), Unix ms.
    pub last_at: Option<i64>,
    /// Every space they were seen in is one you asked not to be disturbed by (不提醒 / 屏蔽): they
    /// don't need your judgement.
    pub quiet: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SeenSpace {
    pub id: String,
    pub name: String,
    /// `group | dm | channel | …`
    pub kind: String,
    /// `realtime | quiet | blocked`
    pub attention: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SeenMessage {
    pub text: String,
    pub created: i64,
    pub space_id: String,
    pub space_name: String,
}

/// The directory sync (slice P6, `people.directory_sync`), as far as the team routes need it.
pub trait TeamSync: Send + Sync {
    /// Display name of a directory connection (“飞书通讯录”) for `GET /teams`'s `sync.label`.
    fn label(&self, c: &Connection, ws: &str, connection_id: &str) -> ApiResult<Option<String>>;

    /// Queues one sync of `connection_id` (or of every directory connection) and returns the job id;
    /// `Ok(None)` when the workspace has no connection with `directory.read`. An unknown connection id
    /// is the implementation's `404 没有这个连接`.
    fn schedule(
        &self,
        c: &Connection,
        ws: &str,
        actor: &str,
        connection_id: Option<&str>,
    ) -> ApiResult<Option<String>>;
}

// ---------------------------------------------------------------- shapes

/// A member as the team page shows it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TeamMember {
    pub contact_id: String,
    pub name: String,
    /// `lead` | `member`
    pub role: String,
    /// The contact's primary current job is at an organization other than the workspace's own.
    pub external: bool,
    /// `manual` | `sync` | `chat`
    pub source: String,
}

/// A team. `members` only on the detail; `depth` only in the org-chart list.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Team {
    pub id: String,
    pub name: String,
    /// `chart` | `virtual`
    pub kind: String,
    /// `active` | `dissolved`
    pub status: String,
    pub parent_id: Option<String>,
    pub org_id: Option<String>,
    pub purpose: String,
    pub space_id: Option<String>,
    pub tags: Vec<String>,
    pub visibility: Visibility,
    pub source_label: String,
    pub headcount: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub members: Option<Vec<TeamMember>>,
    pub children: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub depth: Option<usize>,
    pub revision: i64,
}

/// The last directory sync, for the “组织架构” header.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SyncInfo {
    pub connection_id: String,
    pub label: String,
    pub at: i64,
}

/// One directory connection's last sync, for the 组织架构 header: what the app may see and why it
/// failed (“应用只能看到 N 个部门”).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DirectoryStatus {
    pub connection_id: String,
    pub label: String,
    #[serde(flatten)]
    pub state: crate::contacts::sync::DirectoryState,
    /// One sentence for the header when the view is partial or the sync failed.
    pub note: Option<String>,
}

/// `GET /teams`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TeamList {
    pub chart: Vec<Team>,
    #[serde(rename = "virtual")]
    pub virtual_: Vec<Team>,
    pub sync: Option<SyncInfo>,
    pub directory: Vec<DirectoryStatus>,
}

/// The header sentence of a directory state.
pub fn directory_note(s: &crate::contacts::sync::DirectoryState) -> Option<String> {
    if !s.missing_scopes.is_empty() {
        return Some(steve_i18n::t!(
            "modules.teams.note.missing_scopes",
            scopes = s
                .missing_scopes
                .join(&steve_i18n::t!("modules.error.list_sep"))
        ));
    }
    if let Some(e) = &s.error {
        return Some(steve_i18n::t!("modules.teams.note.sync_failed", error = e));
    }
    match s.scope {
        Some(sc) if !sc.whole => Some(match (sc.departments, sc.users) {
            (0, 0) => steve_i18n::t!("modules.teams.note.scope_empty"),
            (d, 0) => steve_i18n::t!("modules.teams.note.scope_departments", departments = d),
            (0, u) => steve_i18n::t!("modules.teams.note.scope_users", users = u),
            (d, u) => steve_i18n::t!("modules.teams.note.scope_both", departments = d, users = u),
        }),
        _ => None,
    }
}

fn local(ms: i64, fmt: &str) -> String {
    chrono::Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|t| t.format(fmt).to_string())
        .unwrap_or_default()
}

fn sync_label(c: &Connection, ports: &Ports, ws: &str, conn: &str) -> ApiResult<String> {
    let l = match &ports.sync {
        Some(s) => s.label(c, ws, conn)?,
        None => None,
    };
    Ok(l.unwrap_or_else(|| steve_i18n::t!("modules.teams.directory")))
}

/// “你建的 · 10-08”, “飞书通讯录 · 10-31 06:00 同步” (prototype `people-data.ts`).
fn source_label(c: &Connection, ports: &Ports, p: &Party, d: &Details) -> ApiResult<String> {
    if let (Some(conn), true) = (&d.sync_connection, d.is_chart()) {
        let label = sync_label(c, ports, &p.workspace_id, conn)?;
        return Ok(match d.synced {
            Some(t) => steve_i18n::t!(
                "modules.teams.source.synced_at",
                label = label,
                at = local(t, "%m-%d %H:%M")
            ),
            None => label,
        });
    }
    let who = match p.source.as_str() {
        "sync" => steve_i18n::t!("modules.teams.source.sync"),
        "chat" => steve_i18n::t!("modules.teams.source.chat"),
        "steve" => steve_i18n::t!("modules.teams.source.steve"),
        _ => steve_i18n::t!("modules.teams.source.me"),
    };
    Ok(steve_i18n::t!(
        "modules.teams.source.created_at",
        who = who,
        at = local(p.created, "%m-%d")
    ))
}

fn members(c: &Connection, viewer: &Viewer, team: &str) -> ApiResult<Vec<TeamMember>> {
    let own = prepo::self_org(c, &viewer.ws)?.map(|o| o.id);
    let mut out = Vec::new();
    for m in repo::members(c, &viewer.ws, team)? {
        if !viewer.sees(&m.contact.visibility, &m.contact.created_by) {
            continue;
        }
        let primary = jobs(c, viewer, &m.contact.id)?
            .into_iter()
            .find(|j| j.to.is_none());
        out.push(TeamMember {
            external: !m.contact.is_me()
                && primary.is_some_and(|j| Some(&j.org_id) != own.as_ref()),
            contact_id: m.contact.id,
            name: m.contact.name,
            role: m.role,
            source: m.source,
        });
    }
    Ok(out)
}

/// Builds the `Team` shape; `with_members` for the detail.
pub fn team(
    c: &Connection,
    viewer: &Viewer,
    ports: &Ports,
    p: &Party,
    d: &Details,
    with_members: bool,
) -> ApiResult<Team> {
    Ok(Team {
        id: p.id.clone(),
        name: p.name.clone(),
        kind: d.kind.clone(),
        status: p.status.clone(),
        parent_id: d.parent_id.clone(),
        org_id: d.org_id.clone(),
        purpose: d.purpose.clone(),
        space_id: d.space_id.clone(),
        tags: tags::of(c, &viewer.ws, &pref(&p.id))?,
        visibility: p.visibility.clone(),
        source_label: source_label(c, ports, p, d)?,
        headcount: repo::headcount(c, &viewer.ws, &p.id, d.is_chart())?,
        members: if with_members {
            Some(members(c, viewer, &p.id)?)
        } else {
            None
        },
        children: repo::children(c, viewer, &p.id)?,
        depth: None,
        revision: p.revision,
    })
}

/// A team the viewer owns; `active` for writes. `404 没有这个团队`.
pub fn own_team(
    c: &Connection,
    viewer: &Viewer,
    id: &str,
    active: bool,
) -> ApiResult<(Party, Details)> {
    let p = own_party(c, viewer, id, "team", "团队", active)?;
    let d = repo::details(c, &p.id)?;
    Ok((p, d))
}

fn team_by_id(m: &Modules, viewer: &Viewer, ports: &Ports, id: &str) -> ApiResult<Team> {
    m.store.reader(|c| {
        let (p, d) = own_team(c, viewer, id, false)?;
        team(c, viewer, ports, &p, &d, true)
    })
}

/// Org-chart teams in tree order (parents before children, siblings by name), each with its depth. A
/// team whose parent is not in the list (dissolved, invisible) starts a tree of its own.
fn tree_order(chart: Vec<Team>) -> Vec<Team> {
    let ids: Vec<String> = chart.iter().map(|t| t.id.clone()).collect();
    let is_root = |t: &Team| t.parent_id.as_ref().is_none_or(|p| !ids.contains(p));
    let mut out: Vec<Team> = Vec::with_capacity(chart.len());
    let mut rest = chart;
    // `rest` is sorted by name already; a stack keeps siblings in that order
    let mut stack: Vec<(Team, usize)> = Vec::new();
    let roots: Vec<Team> = {
        let (r, others): (Vec<Team>, Vec<Team>) = rest.into_iter().partition(is_root);
        rest = others;
        r
    };
    stack.extend(roots.into_iter().rev().map(|t| (t, 0)));
    while let Some((mut t, depth)) = stack.pop() {
        let (kids, others): (Vec<Team>, Vec<Team>) = rest
            .into_iter()
            .partition(|k| k.parent_id.as_deref() == Some(t.id.as_str()));
        rest = others;
        stack.extend(kids.into_iter().rev().map(|k| (k, depth + 1)));
        t.depth = Some(depth);
        out.push(t);
    }
    // a parent cycle leaves teams unreached: show them flat rather than drop them
    out.extend(rest.into_iter().map(|mut t| {
        t.depth = Some(0);
        t
    }));
    out
}

// ---------------------------------------------------------------- reads

/// `GET /teams`: `{chart (tree order, with depth), virtual, sync}`, without members.
pub fn list_teams(m: &Modules, ports: &Ports, who: &Caller) -> ApiResult<TeamList> {
    let (viewer, _) = start(m, who)?;
    m.store.reader(|c| {
        let (mut chart, mut virtual_) = (Vec::new(), Vec::new());
        for (p, d) in repo::teams(c, &viewer)? {
            let t = team(c, &viewer, ports, &p, &d, false)?;
            if d.is_chart() {
                chart.push(t);
            } else {
                virtual_.push(t);
            }
        }
        let sync = match repo::last_sync(c, &who.ws)? {
            Some((conn, at)) => Some(SyncInfo {
                label: sync_label(c, ports, &who.ws, &conn)?,
                connection_id: conn,
                at,
            }),
            None => None,
        };
        let mut directory = Vec::new();
        for (conn, raw) in crate::contacts::sync::repo::directory_states(c, &who.ws)? {
            let Ok(state) = serde_json::from_str::<crate::contacts::sync::DirectoryState>(&raw)
            else {
                continue;
            };
            directory.push(DirectoryStatus {
                label: sync_label(c, ports, &who.ws, &conn)?,
                note: directory_note(&state),
                connection_id: conn,
                state,
            });
        }
        Ok(TeamList {
            chart: tree_order(chart),
            virtual_,
            sync,
            directory,
        })
    })
}

/// `GET /teams/:id` (dissolved teams too, so references can show “已解散的团队”).
pub fn get_team(m: &Modules, ports: &Ports, who: &Caller, id: &str) -> ApiResult<Team> {
    let (viewer, _) = start(m, who)?;
    team_by_id(m, &viewer, ports, id)
}

// ---------------------------------------------------------------- writes

#[derive(Debug, Clone, Deserialize)]
pub struct MemberIn {
    pub contact_id: String,
    pub role: Option<String>,
}

/// Checks a member list (`field.<i>.role`, duplicates at `field.<i>.contact_id`) and resolves each
/// contact (`404 没有这个联系人`). With `me_leads`, “我” without a role becomes `lead`.
fn resolve_members(
    tx: &Tx,
    viewer: &Viewer,
    me: &Party,
    field: &str,
    xs: &[MemberIn],
    me_leads: bool,
) -> ApiResult<Vec<(String, String)>> {
    let mut ck = Check::new();
    ck.that(field, xs.len() <= 500);
    let mut seen: Vec<&str> = Vec::new();
    for (i, x) in xs.iter().enumerate() {
        if let Some(r) = &x.role {
            ck.one_of(&format!("{field}.{i}.role"), r, &ROLES);
        }
        ck.that(
            &format!("{field}.{i}.contact_id"),
            !seen.contains(&x.contact_id.as_str()),
        );
        seen.push(&x.contact_id);
    }
    ck.done()?;
    let mut out = Vec::with_capacity(xs.len());
    for x in xs {
        let id = if x.contact_id == "me" {
            me.id.as_str()
        } else {
            x.contact_id.as_str()
        };
        let c = own_party(tx, viewer, id, "contact", "联系人", true)?;
        let role = match &x.role {
            Some(r) => r.clone(),
            None if me_leads && c.id == me.id => "lead".into(),
            None => "member".into(),
        };
        out.push((c.id, role));
    }
    Ok(out)
}

/// `space_id` through the channels module; without it only `null`.
fn check_space(
    c: &Connection,
    viewer: &Viewer,
    ports: &Ports,
    space: Option<&str>,
) -> ApiResult<()> {
    match (space, &ports.spaces) {
        (None, _) => Ok(()),
        (Some(_), None) => Err(ApiError::invalid("space_id")),
        (Some(s), Some(p)) if p.visible(c, viewer, s)? => Ok(()),
        (Some(_), Some(_)) => Err(ApiError::not_found("群")),
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct NewTeam {
    pub name: String,
    pub purpose: Option<String>,
    pub members: Vec<MemberIn>,
    pub visibility: Option<Visibility>,
}

/// `POST /teams`: a virtual team of at least two members (outside contacts allowed).
pub fn create_team(m: &Modules, ports: &Ports, who: &Caller, b: NewTeam) -> ApiResult<Team> {
    let (viewer, me) = start(m, who)?;
    let mut ck = Check::new();
    ck.len("name", b.name.trim(), 1, 40)
        .opt_len("purpose", b.purpose.as_deref(), 0, 400)
        .that("members", b.members.len() >= 2);
    ck.done()?;
    let id = m.store.transaction(|tx| {
        let members = resolve_members(tx, &viewer, &me, "members", &b.members, true)?;
        let vis = b.visibility.clone().unwrap_or(Visibility::Workspace);
        check_visibility(tx, &who.ws, &vis)?;
        let mut p = NewParty::new("team", b.name.trim(), &who.me);
        p.visibility = vis;
        let p = prepo::insert(tx, &who.ws, &p)?;
        repo::details_insert(
            tx,
            &who.ws,
            &p.id,
            &Details {
                kind: "virtual".into(),
                purpose: b.purpose.clone().unwrap_or_default(),
                ..Default::default()
            },
        )?;
        for (cid, role) in &members {
            repo::member_add(tx, &who.ws, &p.id, cid, role, "manual")?;
        }
        base::audit(
            tx,
            &who.ws,
            &who.me,
            "team.create",
            &p.id,
            &json!({ "kind": "virtual", "members": members.len() }),
        )?;
        event(tx, &who.ws, &p.id);
        Ok::<_, ApiError>(p.id)
    })?;
    team_by_id(m, &viewer, ports, &id)
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct TeamPatch {
    pub name: Option<String>,
    pub purpose: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    pub space_id: Option<Option<String>>,
    pub tags: Option<Vec<String>>,
    pub visibility: Option<Visibility>,
}

/// `PATCH /teams/:id`. Org-chart teams take only `purpose`, `space_id` and tags; a different name or
/// visibility gets [`CHART_READONLY`].
pub fn patch_team(
    m: &Modules,
    ports: &Ports,
    who: &Caller,
    id: &str,
    b: TeamPatch,
) -> ApiResult<Team> {
    let (viewer, _) = start(m, who)?;
    m.store.transaction(|tx| {
        let (p, d) = own_team(tx, &viewer, id, true)?;
        let mut ck = Check::new();
        if let Some(n) = &b.name {
            ck.len("name", n.trim(), 1, 40);
        }
        ck.opt_len("purpose", b.purpose.as_deref(), 0, 400);
        if let Some(t) = &b.tags {
            ck.list("tags", t, 50, 64);
        }
        ck.done()?;
        let name = b
            .name
            .as_ref()
            .map(|n| n.trim().to_string())
            .filter(|n| *n != p.name);
        let visibility = b.visibility.clone().filter(|v| *v != p.visibility);
        if d.is_chart() && (name.is_some() || visibility.is_some()) {
            return Err(ApiError::rule(chart_readonly()));
        }
        if let Some(v) = &visibility {
            check_visibility(tx, &who.ws, v)?;
        }
        let purpose = b.purpose.clone().filter(|x| *x != d.purpose);
        let space = b.space_id.clone().filter(|s| *s != d.space_id);
        if let Some(s) = &space {
            check_space(tx, &viewer, ports, s.as_deref())?;
        }
        let (mut before, mut after) = (Map::new(), Map::new());
        let mut put = |k: &str, old: Value, new: Value| {
            before.insert(k.into(), old);
            after.insert(k.into(), new);
        };
        if let Some(n) = &name {
            put("name", json!(p.name), json!(n));
        }
        if let Some(v) = &visibility {
            put("visibility", json!(p.visibility), json!(v));
        }
        if let Some(x) = &purpose {
            put("purpose", json!(d.purpose), json!(x));
        }
        if let Some(s) = &space {
            put("space_id", json!(d.space_id), json!(s));
        }
        if let Some(t) = &b.tags {
            let old = tags::of(tx, &who.ws, &pref(&p.id))?;
            let new = tags::set(tx, &who.ws, &pref(&p.id), t)?;
            if new != old {
                put("tags", json!(old), json!(new));
            }
        }
        if after.is_empty() {
            return Ok(());
        }
        repo::details_update(
            tx,
            &p.id,
            purpose.as_deref(),
            space.as_ref().map(|s| s.as_deref()),
        )?;
        let patch = PartyPatch {
            name,
            visibility,
            ..Default::default()
        };
        let fields: Vec<String> = after.keys().cloned().collect();
        revise(
            tx,
            &p,
            &patch,
            Value::Object(before),
            Value::Object(after),
            &who.me,
        )?;
        base::audit(
            tx,
            &who.ws,
            &who.me,
            "team.update",
            &p.id,
            &json!({ "fields": fields }),
        )?;
        event(tx, &who.ws, &p.id);
        Ok::<_, ApiError>(())
    })?;
    team_by_id(m, &viewer, ports, id)
}

/// `PUT /teams/:id/members`: replaces the members of a virtual team; the revision records only what
/// changed (`before`: removed and re-roled rows as they were, `after`: added and re-roled rows).
pub fn put_members(
    m: &Modules,
    ports: &Ports,
    who: &Caller,
    id: &str,
    b: Vec<MemberIn>,
) -> ApiResult<Team> {
    let (viewer, me) = start(m, who)?;
    m.store.transaction(|tx| {
        let (p, d) = own_team(tx, &viewer, id, true)?;
        if d.is_chart() {
            return Err(ApiError::rule(chart_readonly()));
        }
        let new = resolve_members(tx, &viewer, &me, "members", &b, false)?;
        let old = repo::member_rows(tx, &who.ws, &p.id)?;
        let row = |c: &str, r: &str| json!({ "contact_id": c, "role": r });
        let (mut was, mut now) = (Vec::new(), Vec::new());
        for (c, r) in &old {
            match new.iter().find(|(n, _)| n == c) {
                None => {
                    repo::member_remove(tx, &who.ws, &p.id, c)?;
                    was.push(row(c, r));
                }
                Some((_, nr)) if nr != r => {
                    repo::member_role(tx, &who.ws, &p.id, c, nr)?;
                    was.push(row(c, r));
                    now.push(row(c, nr));
                }
                Some(_) => {}
            }
        }
        for (c, r) in &new {
            if !old.iter().any(|(o, _)| o == c) {
                repo::member_add(tx, &who.ws, &p.id, c, r, "manual")?;
                now.push(row(c, r));
            }
        }
        if was.is_empty() && now.is_empty() {
            return Ok(());
        }
        revise(
            tx,
            &p,
            &PartyPatch::default(),
            json!({ "members": was }),
            json!({ "members": now }),
            &who.me,
        )?;
        base::audit(
            tx,
            &who.ws,
            &who.me,
            "team.members",
            &p.id,
            &json!({ "count": new.len() }),
        )?;
        event(tx, &who.ws, &p.id);
        Ok::<_, ApiError>(())
    })?;
    team_by_id(m, &viewer, ports, id)
}

/// `DELETE /teams/:id`: a virtual team becomes `dissolved` (members and references stay).
pub fn dissolve_team(m: &Modules, who: &Caller, id: &str) -> ApiResult<()> {
    let (viewer, _) = start(m, who)?;
    m.store.transaction(|tx| {
        let (p, d) = own_team(tx, &viewer, id, true)?;
        if d.is_chart() {
            return Err(ApiError::rule(chart_readonly()));
        }
        let patch = PartyPatch {
            status: Some("dissolved".into()),
            ..Default::default()
        };
        revise(
            tx,
            &p,
            &patch,
            json!({ "status": p.status }),
            json!({ "status": "dissolved" }),
            &who.me,
        )?;
        base::audit(tx, &who.ws, &who.me, "team.dissolve", &p.id, &json!({}))?;
        event(tx, &who.ws, &p.id);
        Ok(())
    })
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SyncBody {
    pub connection_id: Option<String>,
}

/// `POST /teams/sync`: queues a directory sync through [`TeamSync`]; returns the job id.
pub fn sync_teams(m: &Modules, ports: &Ports, who: &Caller, b: SyncBody) -> ApiResult<String> {
    start(m, who)?;
    let Some(sync) = &ports.sync else {
        return Err(ApiError::rule(steve_i18n::t!("modules.teams.no_directory")));
    };
    m.store
        .reader(|c| sync.schedule(c, &who.ws, &who.me, b.connection_id.as_deref()))?
        .ok_or_else(|| ApiError::rule(steve_i18n::t!("modules.teams.no_directory")))
}
