//! People on items (slice P3): the `links` between items and parties, the
//! `@联系人` of `POST /api/thread`, the guess from names in the text when there is no `@`, a party's
//! items and its timeline (with [`TimelineSource`] for what other modules add).
//!
//! steve-core calls (integrator), right after `createItem` in `POST /api/thread`:
//! - with `mentions`: [`link_mentions`], then appends [`mention_note`] to Steve's reply;
//! - without: [`infer_item_people`].

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::json;
use steve_store::Tx;
use steve_store::base::links::{self, LinkKey, NewLink};
use steve_store::base::{self, ObjRef, Viewer};

use super::Ports;
use super::people_repo::{self as prepo, ItemHead, ROLES};
use super::repo::{self, PARTY, Party};
use super::service::{event, own_party, pref, start};
use crate::{ApiError, ApiResult, Caller, Check, Modules};

/// `object_type` of items in `links`.
pub const ITEM: &str = "item";
/// Confidence of a link guessed from a name in the text (“Steve 推断”).
pub const INFERRED: f64 = 0.6;
/// At most this many `mentions` on one message.
pub const MAX_MENTIONS: usize = 20;

/// The Chinese name of a role, for timeline lines.
pub fn role_label(role: &str) -> &'static str {
    match role {
        "raised_by" => "提出人",
        "waiting_on" => "在等",
        "about" => "关于",
        "attendee" => "参与者",
        _ => "相关",
    }
}

/// [`role_label`] in the request's language, for what the pages show.
pub fn role_shown(role: &str) -> String {
    use steve_i18n::t;
    match role {
        "raised_by" => t!("modules.item_people.role.raised_by"),
        "waiting_on" => t!("modules.item_people.role.waiting_on"),
        "about" => t!("modules.item_people.role.about"),
        "attendee" => t!("modules.item_people.role.attendee"),
        _ => t!("modules.item_people.role.other"),
    }
}

fn item_ref(id: &str) -> ObjRef {
    ObjRef::new(ITEM, id)
}

fn key(item: &str, role: &str, party: &str) -> LinkKey {
    LinkKey {
        subject: item_ref(item),
        predicate: role.into(),
        object: pref(party),
    }
}

/// The item ownership check: exists and is in the caller's workspace. `404 没有这个事项`.
pub fn own_item(c: &Connection, ws: &str, id: &str) -> ApiResult<ItemHead> {
    prepo::item(c, id)?
        .filter(|i| i.workspace_id == ws)
        .ok_or_else(|| ApiError::not_found("事项"))
}

/// The name a reference to this party shows: deleted contacts and dissolved teams are named as such.
pub fn display_name(p: &Party) -> String {
    match (p.kind.as_str(), p.status.as_str()) {
        ("team", "dissolved") => "已解散的团队".into(),
        ("org", "deleted") => "已删除的组织".into(),
        (_, "deleted") => "已删除的联系人".into(),
        _ => p.name.clone(),
    }
}

/// [`display_name`] in the request's language, for an API answer (timeline lines keep the zh-CN one).
pub fn shown_name(p: &Party) -> String {
    match (p.kind.as_str(), p.status.as_str()) {
        ("team", "dissolved") => steve_i18n::t!("modules.item_people.dissolved_team"),
        ("org", "deleted") => steve_i18n::t!("modules.item_people.deleted_org"),
        (_, "deleted") => steve_i18n::t!("modules.item_people.deleted_contact"),
        _ => p.name.clone(),
    }
}

/// A party referenced by a link: in the workspace and visible, any status.
fn visible_party(c: &Connection, viewer: &Viewer, id: &str) -> ApiResult<Option<Party>> {
    Ok(repo::get(c, id)?
        .filter(|p| p.workspace_id == viewer.ws && viewer.sees(&p.visibility, &p.created_by)))
}

// ---------------------------------------------------------------- /items/:id/people

/// One person (or organization, team) on an item.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ItemPerson {
    pub party_id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub role: String,
    pub source: String,
    pub confidence: f64,
}

fn people_of(c: &Connection, viewer: &Viewer, item: &str) -> ApiResult<Vec<ItemPerson>> {
    let mut out = Vec::new();
    for l in links::from(c, &viewer.ws, &item_ref(item), None)? {
        if l.object_type != PARTY || !ROLES.contains(&l.predicate.as_str()) {
            continue;
        }
        if let Some(p) = visible_party(c, viewer, &l.object_id)? {
            out.push(ItemPerson {
                party_id: p.id.clone(),
                name: shown_name(&p),
                kind: p.kind,
                role: l.predicate,
                source: l.source,
                confidence: l.confidence,
            });
        }
    }
    Ok(out)
}

/// `GET /items/:id/people`, in the order the links were made.
pub fn list(m: &Modules, who: &Caller, item: &str) -> ApiResult<Vec<ItemPerson>> {
    let (viewer, _) = start(m, who)?;
    m.store.reader(|c| {
        own_item(c, &who.ws, item)?;
        people_of(c, &viewer, item)
    })
}

#[derive(Debug, Clone, Deserialize)]
pub struct AddBody {
    pub party_id: String,
    pub role: String,
}

/// An active party of any type the viewer owns, `404 没有这个联系人` (or 组织, 团队).
fn own_active(c: &Connection, viewer: &Viewer, id: &str) -> ApiResult<Party> {
    let (kind, noun) = match repo::get(c, id)?.map(|p| p.kind).as_deref() {
        Some("org") => ("org", "组织"),
        Some("team") => ("team", "团队"),
        _ => ("contact", "联系人"),
    };
    own_party(c, viewer, id, kind, noun, true)
}

/// `POST /items/:id/people`: links the party in `role` (`source = manual`); an existing link is left
/// as it is. Timeline `关联了 <名字>（<角色>）`.
pub fn add(m: &Modules, who: &Caller, item: &str, b: AddBody) -> ApiResult<Vec<ItemPerson>> {
    let (viewer, _) = start(m, who)?;
    Check::new().one_of("role", &b.role, &ROLES).done()?;
    m.store.transaction(|tx| {
        let i = own_item(tx, &who.ws, item)?;
        let p = own_active(tx, &viewer, &b.party_id)?;
        let mut l = NewLink::new(item_ref(&i.id), &b.role, pref(&p.id), "manual");
        l.confidence = 1.0;
        if links::add(tx, &who.ws, &l)? {
            prepo::timeline_add(
                tx,
                &i.id,
                &who.me,
                "sys",
                &format!("关联了 {}（{}）", p.name, role_label(&b.role)),
            )?;
            base::audit(
                tx,
                &who.ws,
                &who.me,
                "item.party_add",
                &i.id,
                &json!({ "party_id": p.id, "role": b.role }),
            )?;
            event(tx, &who.ws, &p.id);
        }
        people_of(tx, &viewer, &i.id)
    })
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct RemoveParams {
    pub role: Option<String>,
}

/// `DELETE /items/:id/people/:party_id?role=`: one role, or every role when `role` is absent.
/// Timeline `移除了 <名字>` when something was removed.
pub fn remove(
    m: &Modules,
    who: &Caller,
    item: &str,
    party: &str,
    q: RemoveParams,
) -> ApiResult<()> {
    let (viewer, _) = start(m, who)?;
    let role = q.role.filter(|r| !r.is_empty());
    if let Some(r) = &role {
        Check::new().one_of("role", r, &ROLES).done()?;
    }
    m.store.transaction(|tx| {
        let i = own_item(tx, &who.ws, item)?;
        let p = visible_party(tx, &viewer, party)?.ok_or_else(|| ApiError::not_found("联系人"))?;
        let roles: Vec<&str> = match &role {
            Some(r) => vec![r.as_str()],
            None => ROLES.to_vec(),
        };
        let mut removed = Vec::new();
        for r in roles {
            if links::remove(tx, &who.ws, &key(&i.id, r, &p.id))? {
                removed.push(r);
            }
        }
        if !removed.is_empty() {
            prepo::timeline_add(
                tx,
                &i.id,
                &who.me,
                "sys",
                &format!("移除了 {}", display_name(&p)),
            )?;
            base::audit(
                tx,
                &who.ws,
                &who.me,
                "item.party_remove",
                &i.id,
                &json!({ "party_id": p.id, "roles": removed }),
            )?;
            event(tx, &who.ws, &p.id);
        }
        Ok(())
    })
}

// ---------------------------------------------------------------- @ and inference on POST /thread

/// One entry of `POST /api/thread {mentions}`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Mention {
    /// `party` | `bot`
    pub kind: String,
    pub id: String,
}

/// Validates `mentions` before the item is created: at most 20, each `party` or `bot` with an id.
/// `400 参数不对：mentions[.n.kind]`.
pub fn check_mentions(ms: &[Mention]) -> ApiResult<()> {
    let mut ck = Check::new();
    ck.that("mentions", ms.len() <= MAX_MENTIONS);
    for (n, x) in ms.iter().enumerate() {
        ck.one_of(&format!("mentions.{n}.kind"), &x.kind, &["party", "bot"]);
        ck.len(&format!("mentions.{n}.id"), &x.id, 1, 64);
    }
    ck.done()
}

/// Links every `party` mention to the new item as `related` (`source = mention`, confidence 1) and
/// returns the names linked, in order. Parties that are gone or not visible are skipped; `bot`
/// mentions are steve-core's (hand the item to that bot).
pub fn link_mentions(
    m: &Modules,
    who: &Caller,
    item: &str,
    ms: &[Mention],
) -> ApiResult<Vec<String>> {
    let (viewer, _) = start(m, who)?;
    m.store.transaction(|tx| {
        let i = own_item(tx, &who.ws, item)?;
        let mut names = Vec::new();
        let mut seen = BTreeSet::new();
        for x in ms.iter().filter(|x| x.kind == "party") {
            if !seen.insert(x.id.clone()) {
                continue;
            }
            let Ok(p) = own_active(tx, &viewer, &x.id) else {
                continue;
            };
            link_related(tx, &who.ws, &i.id, &p.id, "mention", 1.0)?;
            names.push(p.name.clone());
        }
        Ok(names)
    })
}

/// What Steve's reply gets appended when mentions were linked: `关联了 A、B`. `None` for none.
pub fn mention_note(names: &[String]) -> Option<String> {
    (!names.is_empty()).then(|| format!("关联了 {}", names.join("、")))
}

fn link_related(
    tx: &Tx,
    ws: &str,
    item: &str,
    party: &str,
    source: &str,
    confidence: f64,
) -> ApiResult<bool> {
    let mut l = NewLink::new(item_ref(item), "related", pref(party), source);
    l.confidence = confidence;
    let added = links::add(tx, ws, &l)?;
    if added {
        event(tx, ws, party);
    }
    Ok(added)
}

/// The names a party is known by for `@` and inference: its name and aliases, at least 2 characters,
/// never “我”.
pub fn match_names(p: &Party) -> Vec<&str> {
    std::iter::once(p.name.as_str())
        .chain(p.aliases.iter().map(String::as_str))
        .map(str::trim)
        .filter(|n| n.chars().count() >= 2 && *n != "我")
        .collect()
}

/// The parties named in `text`, by exact substring of a name or alias. A name inside a longer matched
/// name does not count where it only occurs as part of it (“李雷雷” does not also name “李雷”); a name
/// two parties share names neither (no guessing). Returns party ids in candidate order.
pub fn named_in(text: &str, candidates: &[Party]) -> Vec<String> {
    let mut by_name: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for p in candidates {
        for n in match_names(p) {
            by_name.entry(n).or_default().insert(p.id.as_str());
        }
    }
    let hit: Vec<&str> = by_name
        .keys()
        .copied()
        .filter(|n| text.contains(n))
        .collect();
    let mut found = BTreeSet::new();
    for a in &hit {
        let mut rest = text.to_string();
        for b in hit.iter().filter(|b| b.len() > a.len() && b.contains(a)) {
            rest = rest.replace(b, "\u{0}");
        }
        if !rest.contains(a) {
            continue;
        }
        if let Some(ids) = by_name.get(a).filter(|ids| ids.len() == 1) {
            found.extend(ids.iter().copied());
        }
    }
    candidates
        .iter()
        .filter(|p| found.contains(p.id.as_str()))
        .map(|p| p.id.clone())
        .collect()
}

/// `infer_item_people(ws, item_id, text)`: for a message without `mentions`, links the contacts
/// and teams the text names unambiguously as `related` (`source = infer`, confidence 0.6). Only parties
/// the sender can see; never “我”, never unconfirmed contacts; existing links are left alone. Returns
/// the names newly linked.
pub fn infer_item_people(
    m: &Modules,
    who: &Caller,
    item: &str,
    text: &str,
) -> ApiResult<Vec<String>> {
    let (viewer, _) = start(m, who)?;
    let candidates: Vec<Party> = m.store.reader(|c| {
        Ok::<_, ApiError>(
            prepo::parties(c, &viewer, &["contact", "team"], true)?
                .into_iter()
                .filter(|p| !p.is_me())
                .collect(),
        )
    })?;
    let ids = named_in(text, &candidates);
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    m.store.transaction(|tx| {
        let i = own_item(tx, &who.ws, item)?;
        let mut names = Vec::new();
        for p in candidates.iter().filter(|p| ids.contains(&p.id)) {
            if link_related(tx, &who.ws, &i.id, &p.id, "infer", INFERRED)? {
                names.push(p.name.clone());
            }
        }
        Ok(names)
    })
}

// ---------------------------------------------------------------- a party's items and timeline

/// One row of `GET /parties/:id/items`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PartyItem {
    pub item_id: String,
    pub title: String,
    pub role: String,
    pub source: String,
    pub confidence: f64,
}

/// The item links of a party, newest first, one per (item, role). Teams: unarchived items of
/// their members (with sub-teams for org-chart teams) ∪ items linked to the team itself.
fn item_links(c: &Connection, ws: &str, p: &Party) -> ApiResult<Vec<prepo::ItemLink>> {
    let mut rows = prepo::item_links_to(c, ws, std::slice::from_ref(&p.id), false)?;
    if p.kind == "team" {
        let members = if prepo::is_chart(c, &p.id)? {
            prepo::team_member_ids(c, ws, &p.id)?
        } else {
            super::teams::repo::member_rows(c, ws, &p.id)?
                .into_iter()
                .map(|(id, _)| id)
                .collect()
        };
        rows.extend(prepo::item_links_to(c, ws, &members, true)?);
        rows.sort_by_key(|l| std::cmp::Reverse(l.created));
    }
    let mut seen = BTreeSet::new();
    rows.retain(|l| seen.insert((l.item_id.clone(), l.role.clone())));
    Ok(rows)
}

fn own_listed(c: &Connection, viewer: &Viewer, id: &str, me: &Party) -> ApiResult<Party> {
    let id = if id == "me" { me.id.as_str() } else { id };
    let (kind, noun) = match repo::get(c, id)?.map(|p| p.kind).as_deref() {
        Some("org") => ("org", "组织"),
        Some("team") => ("team", "团队"),
        _ => ("contact", "联系人"),
    };
    own_party(c, viewer, id, kind, noun, false)
}

/// `GET /parties/:id/items`.
pub fn party_items(m: &Modules, who: &Caller, id: &str) -> ApiResult<Vec<PartyItem>> {
    let (viewer, me) = start(m, who)?;
    m.store.reader(|c| {
        let p = own_listed(c, &viewer, id, &me)?;
        Ok(item_links(c, &who.ws, &p)?
            .into_iter()
            .map(|l| PartyItem {
                item_id: l.item_id,
                title: l.title,
                role: l.role,
                source: l.source,
                confidence: l.confidence,
            })
            .collect())
    })
}

/// One line of a party's “往来” timeline.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TimelineEntry {
    pub ts: i64,
    /// `mail` | `message` | `meeting` | `item` | `event`
    pub kind: String,
    pub text: String,
    pub href: String,
}

/// What another module adds to a party's timeline (channels: messages; signals: mail, meetings, events).
pub trait TimelineSource: Send + Sync {
    /// Entries about `party` older than `before` (all when `None`), newest first, at most `limit`.
    fn entries(
        &self,
        c: &Connection,
        viewer: &Viewer,
        party: &str,
        before: Option<i64>,
        limit: usize,
    ) -> ApiResult<Vec<TimelineEntry>>;
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct TimelineParams {
    pub before: Option<String>,
    pub limit: Option<String>,
}

/// `GET /parties/:id/timeline?before=&limit=` (limit ≤100, default 50): this module's item lines (when
/// the link was made) merged with every [`TimelineSource`], newest first.
pub fn timeline(
    m: &Modules,
    ports: &Ports,
    who: &Caller,
    id: &str,
    q: TimelineParams,
) -> ApiResult<Vec<TimelineEntry>> {
    let (viewer, me) = start(m, who)?;
    let num =
        |s: Option<String>, field: &str, ok: &dyn Fn(i64) -> bool| -> ApiResult<Option<i64>> {
            match s.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) {
                None => Ok(None),
                Some(s) => s
                    .parse::<i64>()
                    .ok()
                    .filter(|n| ok(*n))
                    .map(Some)
                    .ok_or_else(|| ApiError::invalid(field)),
            }
        };
    let before = num(q.before, "before", &|n| n > 0)?;
    let limit = num(q.limit, "limit", &|n| (1..=100).contains(&n))?.unwrap_or(50) as usize;
    m.store.reader(|c| {
        let p = own_listed(c, &viewer, id, &me)?;
        let mut out: Vec<TimelineEntry> = item_links(c, &who.ws, &p)?
            .into_iter()
            .filter(|l| before.is_none_or(|b| l.created < b))
            .map(|l| TimelineEntry {
                ts: l.created,
                kind: "item".into(),
                text: steve_i18n::t!(
                    "modules.item_people.timeline_item",
                    id = l.item_id,
                    title = l.title,
                    role = role_shown(&l.role)
                ),
                href: format!("#/items/{}", l.item_id),
            })
            .take(limit)
            .collect();
        for s in &ports.timelines {
            out.extend(s.entries(c, &viewer, &p.id, before, limit)?);
        }
        out.sort_by_key(|e| std::cmp::Reverse(e.ts));
        out.truncate(limit);
        Ok(out)
    })
}
