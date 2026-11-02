//! 归属与负责人 (steve-rs-plan/modules/ownership.md): who is responsible for the objects that are not a
//! person's own identity — app bots, service accounts, shared mailboxes, connections, devices, Steve's
//! bots, personas, bot parties and spaces. Whose identity an account is stays `accounts.party_id`.
//!
//! - the `stewards` rows of migration 18 ([`Steward`], [`get`], [`of_object`], [`of_person`] …);
//! - what an object is called ([`object_info`]), a person's holdings ([`holdings`]) and the objects
//!   without a 负责人 ([`unowned`]);
//! - edits by hand go through change records: the operations `steward.set` / `steward.remove`
//!   ([`SetOp`], [`RemoveOp`]) stored and applied as a changeset ([`set`], [`remove`]), undoable for 24
//!   hours through `POST /api/config/:id/undo` like every contact edit;
//! - defaults (§3): [`seed_defaults`] when “我” is first created, [`adopt`] for a new bot, persona or
//!   connection, [`link`] for what a provider tells (source `sync`);
//! - proposals from chat (§3): [`propose`] writes a `proposed` row and opens a `confirm` decision whose
//!   handler is [`StewardConfirm`];
//! - merges move the rows with the person ([`merge_move`], [`split_back`]).

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use steve_store::base::{self, Change, Viewer};
use steve_store::{Cached, ChangeEvent, Decision, Tx, now};

use super::repo::{self, NewParty, Party};
use crate::changes::{self, ChangeOp, OpCx, Origin, Plan, Prepared};
use crate::decisions::{self, DecisionCx, DecisionHandler};
use crate::{ApiError, ApiResult, Caller, Modules};

/// `object_type` codes and how the pages name them.
pub const OBJECT_TYPES: [(&str, &str); 7] = [
    ("account", "账号"),
    ("connection", "接入"),
    ("device", "设备"),
    ("bot", "Bot"),
    ("persona", "身份"),
    ("party", "机器人"),
    ("space", "群"),
];
/// `role` codes, `owner` first.
pub const ROLES: [(&str, &str); 3] = [
    ("owner", "负责人"),
    ("maintainer", "维护人"),
    ("user", "使用人"),
];
/// The `subject_type` of a proposed steward's decision.
pub const SUBJECT: &str = "steward";
/// Confidence of a steward proposed from chat.
pub const CHAT_CONFIDENCE: f64 = 0.6;
/// Account kinds that are not somebody's own identity: listed under 没有负责人 when nobody is.
const SERVICE_ACCOUNTS: &str = "'service','shared_mailbox'";

pub fn type_label(t: &str) -> &str {
    OBJECT_TYPES
        .iter()
        .find(|(c, _)| *c == t)
        .map(|(_, l)| *l)
        .unwrap_or(t)
}

pub fn role_label(r: &str) -> &str {
    ROLES
        .iter()
        .find(|(c, _)| *c == r)
        .map(|(_, l)| *l)
        .unwrap_or(r)
}

/// `accounts.kind` as the pages say it.
pub fn account_kind_label(k: &str) -> &str {
    match k {
        "personal" => "个人账号",
        "work_email" => "工作邮箱",
        "app_bot" => "应用机器人",
        "shared_mailbox" => "共享邮箱",
        "bank_card" => "银行卡",
        "service" => "服务账号",
        "phone" => "电话",
        "bot" => "机器人",
        "calendar" => "日历",
        "root" => "根账号",
        _ => "其他",
    }
}

// ---------------------------------------------------------------- rows

/// A `stewards` row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Steward {
    pub workspace_id: String,
    pub object_type: String,
    pub object_id: String,
    pub person_id: String,
    pub role: String,
    /// `manual` | `connect` | `sync` | `chat` | `inferred` | `seed`
    pub source: String,
    pub confidence: f64,
    /// `confirmed` | `proposed`
    pub status: String,
    pub source_ref: Option<String>,
    pub decision_id: Option<String>,
    pub created: i64,
    pub updated: i64,
}

impl Steward {
    /// A confirmed row created now.
    pub fn new(
        ws: &str,
        object_type: &str,
        object_id: &str,
        person: &str,
        role: &str,
        source: &str,
    ) -> Self {
        let t = now();
        Self {
            workspace_id: ws.into(),
            object_type: object_type.into(),
            object_id: object_id.into(),
            person_id: person.into(),
            role: role.into(),
            source: source.into(),
            confidence: 1.0,
            status: "confirmed".into(),
            source_ref: None,
            decision_id: None,
            created: t,
            updated: t,
        }
    }

    fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            workspace_id: r.get(0)?,
            object_type: r.get(1)?,
            object_id: r.get(2)?,
            person_id: r.get(3)?,
            role: r.get(4)?,
            source: r.get(5)?,
            confidence: r.get(6)?,
            status: r.get(7)?,
            source_ref: r.get(8)?,
            decision_id: r.get(9)?,
            created: r.get(10)?,
            updated: r.get(11)?,
        })
    }

    pub fn key(&self) -> Key {
        Key {
            object_type: self.object_type.clone(),
            object_id: self.object_id.clone(),
            person_id: self.person_id.clone(),
            role: self.role.clone(),
        }
    }
}

/// The primary key of a row within a workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Key {
    pub object_type: String,
    pub object_id: String,
    pub person_id: String,
    pub role: String,
}

const COLS: &str = "workspace_id, object_type, object_id, person_id, role, source, confidence, status, source_ref, decision_id, created, updated";

pub fn get(c: &Connection, ws: &str, k: &Key) -> ApiResult<Option<Steward>> {
    Ok(c.prepare_cached(&format!(
        "SELECT {COLS} FROM stewards WHERE workspace_id = ? AND object_type = ? AND object_id = ? AND person_id = ? AND role = ?"
    ))?
    .query_row(
        params![ws, k.object_type, k.object_id, k.person_id, k.role],
        Steward::from_row,
    )
    .optional()?)
}

/// The rows of an object, owners first, then by age.
pub fn of_object(
    c: &Connection,
    ws: &str,
    object_type: &str,
    object_id: &str,
) -> ApiResult<Vec<Steward>> {
    Ok(c.prepare_cached(&format!(
        "SELECT {COLS} FROM stewards WHERE workspace_id = ? AND object_type = ? AND object_id = ?
         ORDER BY CASE role WHEN 'owner' THEN 0 WHEN 'maintainer' THEN 1 ELSE 2 END, created"
    ))?
    .query_map(params![ws, object_type, object_id], Steward::from_row)?
    .collect::<rusqlite::Result<_>>()?)
}

/// The rows naming a person.
pub fn of_person(c: &Connection, ws: &str, person: &str) -> ApiResult<Vec<Steward>> {
    Ok(c.prepare_cached(&format!(
        "SELECT {COLS} FROM stewards WHERE workspace_id = ? AND person_id = ? ORDER BY object_type, created"
    ))?
    .query_map(params![ws, person], Steward::from_row)?
    .collect::<rusqlite::Result<_>>()?)
}

pub fn by_decision(c: &Connection, decision: &str) -> ApiResult<Option<Steward>> {
    Ok(c.prepare_cached(&format!(
        "SELECT {COLS} FROM stewards WHERE decision_id = ?"
    ))?
    .query_row([decision], Steward::from_row)
    .optional()?)
}

/// Inserts or replaces the row with `s`'s key (keeping its `created` when it existed).
pub fn put(tx: &Tx, s: &Steward) -> ApiResult<()> {
    tx.run(
        "INSERT INTO stewards (workspace_id, object_type, object_id, person_id, role, source, confidence, status, source_ref, decision_id, created, updated)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT (workspace_id, object_type, object_id, person_id, role) DO UPDATE SET
           source = excluded.source, confidence = excluded.confidence, status = excluded.status,
           source_ref = excluded.source_ref, decision_id = excluded.decision_id, updated = excluded.updated",
        params![
            s.workspace_id,
            s.object_type,
            s.object_id,
            s.person_id,
            s.role,
            s.source,
            s.confidence,
            s.status,
            s.source_ref,
            s.decision_id,
            s.created,
            s.updated
        ],
    )?;
    changed(tx, &s.workspace_id, &s.person_id);
    Ok(())
}

pub fn delete(tx: &Tx, ws: &str, k: &Key) -> ApiResult<bool> {
    let n = tx.run(
        "DELETE FROM stewards WHERE workspace_id = ? AND object_type = ? AND object_id = ? AND person_id = ? AND role = ?",
        params![ws, k.object_type, k.object_id, k.person_id, k.role],
    )?;
    changed(tx, ws, &k.person_id);
    Ok(n > 0)
}

/// The person's page (and a bot party's) shows the change.
fn changed(tx: &Tx, ws: &str, person: &str) {
    tx.publish(ChangeEvent::Party {
        id: person.into(),
        ws: ws.into(),
    });
}

/// Does the object have any steward row at all (confirmed or proposed)? Defaults only go to objects
/// without one, so a 负责人 removed by hand is not put back.
pub fn has_any(c: &Connection, ws: &str, object_type: &str, object_id: &str) -> ApiResult<bool> {
    Ok(c.prepare_cached(
        "SELECT 1 FROM stewards WHERE workspace_id = ? AND object_type = ? AND object_id = ?",
    )?
    .exists(params![ws, object_type, object_id])?)
}

// ---------------------------------------------------------------- objects

/// An object as the pages and tools name it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ObjectInfo {
    pub object_type: String,
    pub object_id: String,
    /// “账号”, “设备” …
    pub type_label: String,
    pub name: String,
    /// The finer kind: account kind, adapter, `contact_kind`, space kind.
    pub kind: Option<String>,
    /// One line under the name (“飞书 · 应用机器人 · cli_a1”).
    pub detail: String,
    /// For accounts and connections: whose identity the account is (`accounts.party_id`).
    pub party_id: Option<String>,
}

impl ObjectInfo {
    fn new(t: &str, id: &str, name: String) -> Self {
        Self {
            object_type: t.into(),
            object_id: id.into(),
            type_label: type_label(t).into(),
            name,
            kind: None,
            detail: String::new(),
            party_id: None,
        }
    }
}

fn system_name(c: &Connection, ws: &str, system: &str) -> ApiResult<String> {
    Ok(c.prepare_cached(
        "SELECT name FROM systems WHERE id = ? AND workspace_id IN (?, '*') ORDER BY workspace_id = '*' LIMIT 1",
    )?
    .query_row(params![system, ws], |r| r.get(0))
    .optional()?
    .unwrap_or_else(|| system.to_string()))
}

/// The object `object_type`/`object_id` in `ws`, or `None` when it does not exist there (deleted bots
/// and personas, merged parties and revoked connections count as gone).
pub fn object_info(
    c: &Connection,
    ws: &str,
    object_type: &str,
    id: &str,
) -> ApiResult<Option<ObjectInfo>> {
    let info = match object_type {
        "account" => c
            .prepare_cached(
                "SELECT kind, handle_display, display_name, party_id, system_id FROM accounts
                 WHERE id = ? AND workspace_id = ? AND status != 'deleted'",
            )?
            .query_row(params![id, ws], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                ))
            })
            .optional()?
            .map(|(kind, handle, display, party, system)| -> ApiResult<ObjectInfo> {
                let name = if display.trim().is_empty() { handle.clone() } else { display };
                let mut o = ObjectInfo::new("account", id, name);
                o.detail = format!(
                    "{} · {} · {}",
                    system_name(c, ws, &system)?,
                    account_kind_label(&kind),
                    handle
                );
                o.kind = Some(kind);
                o.party_id = Some(party);
                Ok(o)
            })
            .transpose()?,
        "connection" => c
            .prepare_cached(
                "SELECT c.adapter, a.handle_display, a.display_name, a.party_id FROM connections c
                 JOIN accounts a ON a.id = c.account_id
                 WHERE c.id = ? AND c.workspace_id = ? AND c.status != 'revoked'",
            )?
            .query_row(params![id, ws], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })
            .optional()?
            .map(|(adapter, handle, display, party)| {
                let name = if display.trim().is_empty() { handle.clone() } else { display };
                let mut o = ObjectInfo::new("connection", id, name);
                o.detail = format!("{adapter} · {handle}");
                o.kind = Some(adapter);
                o.party_id = Some(party);
                o
            }),
        "device" => c
            .prepare_cached("SELECT name, roles FROM devices WHERE id = ?")?
            .query_row([id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .optional()?
            .map(|(name, roles)| {
                let mut o = ObjectInfo::new("device", id, name);
                o.detail = serde_json::from_str::<Vec<String>>(&roles)
                    .unwrap_or_default()
                    .join("、");
                o
            }),
        "bot" => c
            .prepare_cached(
                "SELECT name, role FROM bots WHERE id = ?1 AND deleted = 0
                   AND (workspace_id = ?2 OR EXISTS (SELECT 1 FROM bot_workspaces WHERE bot_id = ?1 AND workspace_id = ?2))",
            )?
            .query_row(params![id, ws], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .optional()?
            .map(|(name, role)| {
                let mut o = ObjectInfo::new("bot", id, name);
                o.detail = role;
                o
            }),
        "persona" => c
            .prepare_cached(
                "SELECT name, intro FROM personas WHERE id = ? AND workspace_id = ? AND deleted = 0",
            )?
            .query_row(params![id, ws], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .optional()?
            .map(|(name, intro)| {
                let mut o = ObjectInfo::new("persona", id, name);
                o.detail = intro.chars().take(60).collect();
                o
            }),
        "party" => repo::get(c, id)?
            .filter(|p| p.workspace_id == ws && p.status == "active")
            .map(|p| {
                let mut o = ObjectInfo::new("party", id, p.name.clone());
                if p.contact_kind.as_deref() != Some("bot") {
                    o.type_label = match p.kind.as_str() {
                        "org" => "组织",
                        "team" => "团队",
                        _ => "联系人",
                    }
                    .into();
                }
                o.kind = p.contact_kind.clone().or(p.org_kind.clone());
                o.party_id = Some(p.id);
                o
            }),
        "space" => c
            .prepare_cached("SELECT name, kind FROM spaces WHERE id = ? AND workspace_id = ? AND status != 'archived'")?
            .query_row(params![id, ws], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .optional()?
            .map(|(name, kind)| {
                let mut o = ObjectInfo::new("space", id, name);
                o.kind = Some(kind);
                o
            }),
        _ => None,
    };
    Ok(info)
}

// ---------------------------------------------------------------- people

/// A person who can be a steward: an active, visible contact that is not a bot. `me` is the viewer's
/// “我”. `404 没有这个联系人` otherwise.
pub fn person(c: &Connection, viewer: &Viewer, id: &str) -> ApiResult<Party> {
    let p = if id == "me" {
        repo::me_of(c, &viewer.ws, &viewer.user)?
    } else {
        repo::get(c, id)?
    };
    let p = crate::own(
        p.filter(|p| p.kind == "contact" && p.status == "active"),
        viewer,
        "联系人",
    )?;
    if p.contact_kind.as_deref() == Some("bot") {
        return Err(ApiError::rule("机器人不能做负责人"));
    }
    Ok(p)
}

fn name_of(c: &Connection, id: &str) -> ApiResult<String> {
    Ok(repo::get(c, id)?
        .map(|p| p.name)
        .unwrap_or_else(|| id.to_string()))
}

/// “我” of a workspace's first owner (whom defaults go to when no user is at hand).
pub fn owner_me(c: &Connection, ws: &str) -> ApiResult<Option<Party>> {
    let user: Option<String> = c
        .prepare_cached(
            "SELECT user_id FROM workspace_members WHERE workspace_id = ? AND role = 'owner' ORDER BY created LIMIT 1",
        )?
        .query_row([ws], |r| r.get(0))
        .optional()?;
    match user {
        Some(u) => Ok(repo::me_of(c, ws, &u)?.filter(|p| p.status == "active")),
        None => Ok(None),
    }
}

// ---------------------------------------------------------------- shapes

/// One steward of an object as the pages show it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StewardOut {
    pub person_id: String,
    pub person_name: String,
    pub is_me: bool,
    pub role: String,
    pub role_label: String,
    pub source: String,
    pub status: String,
    pub confidence: f64,
    pub decision_id: Option<String>,
    pub updated: i64,
}

fn steward_out(c: &Connection, s: &Steward) -> ApiResult<StewardOut> {
    let p = repo::get(c, &s.person_id)?;
    Ok(StewardOut {
        person_id: s.person_id.clone(),
        person_name: p.as_ref().map(|p| p.name.clone()).unwrap_or_default(),
        is_me: p.as_ref().is_some_and(Party::is_me),
        role: s.role.clone(),
        role_label: role_label(&s.role).into(),
        source: s.source.clone(),
        status: s.status.clone(),
        confidence: s.confidence,
        decision_id: s.decision_id.clone(),
        updated: s.updated,
    })
}

/// `GET /stewards/:type/:id`: the object and its stewards.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ObjectStewards {
    pub object: ObjectInfo,
    pub stewards: Vec<StewardOut>,
}

pub fn object_stewards(c: &Connection, ws: &str, object: ObjectInfo) -> ApiResult<ObjectStewards> {
    let rows = of_object(c, ws, &object.object_type, &object.object_id)?;
    let mut stewards = Vec::with_capacity(rows.len());
    for s in &rows {
        stewards.push(steward_out(c, s)?);
    }
    Ok(ObjectStewards { object, stewards })
}

/// One account of a person.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AccountRef {
    pub id: String,
    pub handle: String,
    pub name: String,
    pub kind: String,
    pub kind_label: String,
}

/// A person's accounts in one system.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AccountGroup {
    pub system_id: String,
    pub system: String,
    pub accounts: Vec<AccountRef>,
}

/// Something a person is a steward of.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Holding {
    pub object: ObjectInfo,
    pub role: String,
    pub role_label: String,
    pub source: String,
    pub status: String,
    pub decision_id: Option<String>,
    /// The other stewards of the same object.
    pub others: Vec<StewardOut>,
}

/// `GET /people/:id/holdings`: 名下 (§4) — own identities by system, then what the person is
/// responsible for.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Holdings {
    pub person_id: String,
    pub person_name: String,
    pub is_bot: bool,
    pub accounts: Vec<AccountGroup>,
    pub responsible: Vec<Holding>,
    /// For a bot party: its stewards.
    pub stewards: Vec<StewardOut>,
}

/// A party's accounts, grouped by system in the order the systems first appear.
pub fn accounts_of(c: &Connection, ws: &str, party: &str) -> ApiResult<Vec<AccountGroup>> {
    let rows: Vec<(String, String, String, String, String)> = c
        .prepare_cached(
            "SELECT id, system_id, handle_display, display_name, kind FROM accounts
             WHERE workspace_id = ? AND party_id = ? AND status != 'deleted' ORDER BY system_id, created",
        )?
        .query_map(params![ws, party], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut out: Vec<AccountGroup> = Vec::new();
    for (id, system, handle, name, kind) in rows {
        let a = AccountRef {
            id,
            handle,
            name,
            kind_label: account_kind_label(&kind).into(),
            kind,
        };
        match out.iter_mut().find(|g| g.system_id == system) {
            Some(g) => g.accounts.push(a),
            None => out.push(AccountGroup {
                system: system_name(c, ws, &system)?,
                system_id: system,
                accounts: vec![a],
            }),
        }
    }
    Ok(out)
}

pub fn holdings_of(c: &Connection, viewer: &Viewer, p: &Party) -> ApiResult<Holdings> {
    let ws = viewer.ws.as_str();
    let mut responsible = Vec::new();
    for s in of_person(c, ws, &p.id)? {
        let Some(object) = object_info(c, ws, &s.object_type, &s.object_id)? else {
            continue;
        };
        let others = of_object(c, ws, &s.object_type, &s.object_id)?
            .iter()
            .filter(|o| o.person_id != p.id)
            .map(|o| steward_out(c, o))
            .collect::<ApiResult<_>>()?;
        responsible.push(Holding {
            object,
            role_label: role_label(&s.role).into(),
            role: s.role,
            source: s.source,
            status: s.status,
            decision_id: s.decision_id,
            others,
        });
    }
    let stewards = of_object(c, ws, "party", &p.id)?
        .iter()
        .map(|s| steward_out(c, s))
        .collect::<ApiResult<_>>()?;
    Ok(Holdings {
        person_id: p.id.clone(),
        person_name: p.name.clone(),
        is_bot: p.contact_kind.as_deref() == Some("bot"),
        accounts: accounts_of(c, ws, &p.id)?,
        responsible,
        stewards,
    })
}

/// `GET /people/:id/holdings` (any visible contact, bot parties included).
pub fn holdings(m: &Modules, who: &Caller, id: &str) -> ApiResult<Holdings> {
    let (viewer, me) = super::service::start(m, who)?;
    let id = if id == "me" { me.id.as_str() } else { id };
    m.store.reader(|c| {
        let p = super::service::own_party(c, &viewer, id, "contact", "联系人", false)?;
        holdings_of(c, &viewer, &p)
    })
}

/// The objects nobody confirmed as 负责人 (§4 “没有负责人”): Steve's bots, personas, devices,
/// connections, bot parties and service / shared-mailbox accounts.
pub fn unowned_in(c: &Connection, ws: &str) -> ApiResult<Vec<ObjectInfo>> {
    let none = |t: &str, col: &str| {
        format!(
            "NOT EXISTS (SELECT 1 FROM stewards s WHERE s.workspace_id = ?1 AND s.object_type = '{t}' AND s.object_id = {col} AND s.role = 'owner' AND s.status = 'confirmed')"
        )
    };
    let queries = [
        (
            "bot",
            format!(
                "SELECT b.id FROM bots b WHERE b.deleted = 0 AND (b.workspace_id = ?1 OR EXISTS (SELECT 1 FROM bot_workspaces w WHERE w.bot_id = b.id AND w.workspace_id = ?1)) AND {} ORDER BY b.main DESC, b.name",
                none("bot", "b.id")
            ),
        ),
        (
            "persona",
            format!(
                "SELECT x.id FROM personas x WHERE x.workspace_id = ?1 AND x.deleted = 0 AND {} ORDER BY x.is_default DESC, x.name",
                none("persona", "x.id")
            ),
        ),
        (
            "device",
            format!(
                "SELECT d.id FROM devices d WHERE {} ORDER BY d.created",
                none("device", "d.id")
            ),
        ),
        (
            "connection",
            format!(
                "SELECT c.id FROM connections c WHERE c.workspace_id = ?1 AND c.status != 'revoked' AND {} ORDER BY c.created",
                none("connection", "c.id")
            ),
        ),
        (
            "party",
            format!(
                "SELECT p.id FROM parties p WHERE p.workspace_id = ?1 AND p.type = 'contact' AND p.contact_kind = 'bot' AND p.status = 'active' AND {} ORDER BY p.name",
                none("party", "p.id")
            ),
        ),
        (
            "account",
            format!(
                "SELECT a.id FROM accounts a WHERE a.workspace_id = ?1 AND a.status != 'deleted' AND a.kind IN ({SERVICE_ACCOUNTS}) AND {} ORDER BY a.created",
                none("account", "a.id")
            ),
        ),
    ];
    let mut out = Vec::new();
    for (t, sql) in queries {
        let ids: Vec<String> = c
            .prepare_cached(&sql)?
            .query_map([ws], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        for id in ids {
            if let Some(o) = object_info(c, ws, t, &id)? {
                out.push(o);
            }
        }
    }
    Ok(out)
}

/// `GET /stewards/unowned`.
pub fn unowned(m: &Modules, who: &Caller) -> ApiResult<Vec<ObjectInfo>> {
    super::service::start(m, who)?;
    m.store.reader(|c| unowned_in(c, &who.ws))
}

/// `GET /stewards/:type/:id`.
pub fn list_object(
    m: &Modules,
    who: &Caller,
    object_type: &str,
    id: &str,
) -> ApiResult<ObjectStewards> {
    super::service::start(m, who)?;
    m.store.reader(|c| {
        let o = own_object(c, &who.ws, object_type, id)?;
        object_stewards(c, &who.ws, o)
    })
}

/// The object or `404 没有这个<type label>` (`400` for an unknown type).
pub fn own_object(c: &Connection, ws: &str, object_type: &str, id: &str) -> ApiResult<ObjectInfo> {
    if !OBJECT_TYPES.iter().any(|(t, _)| *t == object_type) {
        return Err(ApiError::invalid("object_type"));
    }
    object_info(c, ws, object_type, id)?.ok_or_else(|| ApiError::not_found(type_label(object_type)))
}

// ---------------------------------------------------------------- edits by hand (change records)

/// `PUT /stewards/:type/:id`.
#[derive(Debug, Clone, Deserialize)]
pub struct SetBody {
    pub person_id: String,
    #[serde(default)]
    pub role: Option<String>,
    /// Replace the other stewards of the same role (default `true`: the chip sets *the* 负责人).
    #[serde(default)]
    pub replace: Option<bool>,
}

/// `DELETE /stewards/:type/:id?person_id=&role=`: one person, or everyone of the role (default owner).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RemoveQuery {
    #[serde(default)]
    pub person_id: Option<String>,
    #[serde(default)]
    pub role: Option<String>,
}

/// The answer of a change: `undo` is the changeset to pass to `POST /api/config/:id/undo`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Changed {
    pub ok: bool,
    pub undo: Option<String>,
    #[serde(flatten)]
    pub now: ObjectStewards,
}

fn check_role(role: Option<&str>) -> ApiResult<String> {
    let r = role.unwrap_or("owner");
    if ROLES.iter().any(|(c, _)| *c == r) {
        Ok(r.to_string())
    } else {
        Err(ApiError::invalid("role"))
    }
}

fn names(c: &Connection, rows: &[&Steward]) -> ApiResult<String> {
    let mut v = Vec::new();
    for s in rows {
        v.push(name_of(c, &s.person_id)?);
    }
    Ok(if v.is_empty() {
        "（空）".into()
    } else {
        v.join("、")
    })
}

fn set_op(o: &ObjectInfo, person: &str, role: &str) -> Value {
    json!({ "op": "steward.set", "object_type": o.object_type, "object_id": o.object_id, "person_id": person, "role": role, "source": "manual" })
}

fn remove_op(s: &Steward) -> Value {
    json!({ "op": "steward.remove", "object_type": s.object_type, "object_id": s.object_id, "person_id": s.person_id, "role": s.role })
}

/// Stores the plan as a changeset and applies it (one record, one undo).
fn record(
    m: &Modules,
    who: &Caller,
    o: &ObjectInfo,
    href_person: &str,
    plan: Plan,
) -> ApiResult<Option<String>> {
    if plan.ops.is_empty() {
        return Ok(None);
    }
    let prepared = Prepared {
        target_id: Some(href_person.to_string()),
        revision: None,
        title: format!("「{}」的{}", o.name, role_label("owner")),
        href: format!("#/contacts/{href_person}"),
        subject: format!("the stewards of {} \"{}\"", o.object_type, o.name),
        fields: Vec::new(),
        ops: Vec::new(),
        free_text: None,
        context: Value::Null,
    };
    let p = changes::store_plan(m, who, "contact", &prepared, plan, Origin::default())?;
    match changes::apply(m, who, &p.id) {
        Some(r) => r.map(|_| Some(p.id)),
        None => Err(ApiError::Internal("steward changeset vanished".into())),
    }
}

/// `PUT /stewards/:type/:id {person_id, role?, replace?}`.
pub fn set(
    m: &Modules,
    who: &Caller,
    object_type: &str,
    id: &str,
    b: SetBody,
) -> ApiResult<Changed> {
    let (viewer, _) = super::service::start(m, who)?;
    let role = check_role(b.role.as_deref())?;
    let (o, plan, person) = m.store.reader(|c| {
        let o = own_object(c, &who.ws, object_type, id)?;
        let p = person(c, &viewer, b.person_id.trim())?;
        if o.object_type == "party" && o.object_id == p.id {
            return Err(ApiError::rule("不能做自己的负责人"));
        }
        let rows = of_object(c, &who.ws, object_type, id)?;
        let same: Vec<&Steward> = rows.iter().filter(|s| s.role == role).collect();
        let mut plan = Plan::default();
        if b.replace.unwrap_or(true) {
            for s in same.iter().filter(|s| s.person_id != p.id) {
                plan.ops.push(remove_op(s));
            }
        }
        let current = same.iter().find(|s| s.person_id == p.id);
        if current.is_none_or(|s| s.status != "confirmed") {
            plan.ops.push(set_op(&o, &p.id, &role));
        }
        if !plan.ops.is_empty() {
            let after: Vec<&Steward> = if b.replace.unwrap_or(true) {
                Vec::new()
            } else {
                same.iter()
                    .filter(|s| s.person_id != p.id)
                    .copied()
                    .collect()
            };
            let mut to = names(c, &after)?;
            to = if to == "（空）" {
                p.name.clone()
            } else {
                format!("{to}、{}", p.name)
            };
            plan.changes.push(Change::new(
                format!("「{}」的{}", o.name, role_label(&role)),
                names(c, &same)?,
                to,
            ));
        }
        Ok((o, plan, p))
    })?;
    let undo = record(m, who, &o, &person.id, plan)?;
    let now = m.store.reader(|c| object_stewards(c, &who.ws, o))?;
    Ok(Changed {
        ok: true,
        undo,
        now,
    })
}

/// `DELETE /stewards/:type/:id`.
pub fn remove(
    m: &Modules,
    who: &Caller,
    object_type: &str,
    id: &str,
    q: RemoveQuery,
) -> ApiResult<Changed> {
    let (_, me) = super::service::start(m, who)?;
    let role = check_role(q.role.as_deref())?;
    let (o, plan, href) = m.store.reader(|c| {
        let o = own_object(c, &who.ws, object_type, id)?;
        let rows = of_object(c, &who.ws, object_type, id)?;
        let person = q
            .person_id
            .as_deref()
            .map(|p| if p == "me" { me.id.as_str() } else { p });
        let gone: Vec<&Steward> = rows
            .iter()
            .filter(|s| s.role == role && person.is_none_or(|p| p == s.person_id))
            .collect();
        let mut plan = Plan::default();
        for s in &gone {
            plan.ops.push(remove_op(s));
        }
        if !gone.is_empty() {
            let left: Vec<&Steward> = rows
                .iter()
                .filter(|s| s.role == role && !gone.iter().any(|g| g.person_id == s.person_id))
                .collect();
            plan.changes.push(Change::new(
                format!("「{}」的{}", o.name, role_label(&role)),
                names(
                    c,
                    &rows.iter().filter(|s| s.role == role).collect::<Vec<_>>(),
                )?,
                names(c, &left)?,
            ));
        }
        let href = gone
            .first()
            .map(|s| s.person_id.clone())
            .unwrap_or(me.id.clone());
        Ok::<_, ApiError>((o, plan, href))
    })?;
    let undo = record(m, who, &o, &href, plan)?;
    let now = m.store.reader(|c| object_stewards(c, &who.ws, o))?;
    Ok(Changed {
        ok: true,
        undo,
        now,
    })
}

fn key_of(op: &Value) -> ApiResult<Key> {
    let s = |k: &str| {
        op.get(k)
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| ApiError::invalid(k))
    };
    Ok(Key {
        object_type: s("object_type")?,
        object_id: s("object_id")?,
        person_id: s("person_id")?,
        role: s("role")?,
    })
}

fn restore(tx: &Tx, before: &Value) -> ApiResult<()> {
    if before.is_null() {
        return Ok(());
    }
    let s: Steward = serde_json::from_value(before.clone())?;
    if repo::get(tx, &s.person_id)?.is_some() {
        put(tx, &s)?;
    }
    Ok(())
}

/// `steward.set {object_type, object_id, person_id, role, source}`: a confirmed row. Undo puts back
/// what was there (nothing, or the proposed row).
pub struct SetOp;

impl ChangeOp for SetOp {
    fn validate(&self, c: &Connection, cx: &OpCx, op: &Value) -> ApiResult<()> {
        let k = key_of(op)?;
        own_object(c, cx.ws, &k.object_type, &k.object_id)?;
        check_role(Some(&k.role))?;
        let viewer = Viewer::load(c, cx.ws, cx.actor)?;
        person(c, &viewer, &k.person_id).map(|_| ())
    }

    fn apply(&self, tx: &Tx, cx: &OpCx, op: &Value) -> ApiResult<Value> {
        let k = key_of(op)?;
        let before = get(tx, cx.ws, &k)?;
        let source = op.get("source").and_then(Value::as_str).unwrap_or("manual");
        let mut s = Steward::new(
            cx.ws,
            &k.object_type,
            &k.object_id,
            &k.person_id,
            &k.role,
            source,
        );
        if let Some(b) = &before {
            s.created = b.created;
            // a proposal confirmed by hand: its card is settled too
            if let Some(d) = b.decision_id.as_deref().filter(|_| b.status == "proposed")
                && base::decisions::get(tx, d)?.is_some_and(|d| d.status == "open")
            {
                base::decisions::settle(
                    tx,
                    d,
                    &json!({ "choice": "yes", "manual": true }),
                    "resolved",
                    &Value::Null,
                )?;
            }
        }
        put(tx, &s)?;
        base::audit(
            tx,
            cx.ws,
            cx.actor,
            "steward.set",
            &k.object_id,
            &json!({ "object_type": k.object_type, "person": k.person_id, "role": k.role, "changeset": cx.changeset_id }),
        )?;
        Ok(before
            .map(serde_json::to_value)
            .transpose()?
            .unwrap_or(Value::Null))
    }

    fn undo(&self, tx: &Tx, cx: &OpCx, op: &Value, before: &Value) -> ApiResult<()> {
        let k = key_of(op)?;
        delete(tx, cx.ws, &k)?;
        restore(tx, before)
    }
}

/// `steward.remove {object_type, object_id, person_id, role}`. Undo puts the row back as it was.
pub struct RemoveOp;

impl ChangeOp for RemoveOp {
    fn apply(&self, tx: &Tx, cx: &OpCx, op: &Value) -> ApiResult<Value> {
        let k = key_of(op)?;
        let before = get(tx, cx.ws, &k)?;
        delete(tx, cx.ws, &k)?;
        base::audit(
            tx,
            cx.ws,
            cx.actor,
            "steward.remove",
            &k.object_id,
            &json!({ "object_type": k.object_type, "person": k.person_id, "role": k.role, "changeset": cx.changeset_id }),
        )?;
        Ok(before
            .map(serde_json::to_value)
            .transpose()?
            .unwrap_or(Value::Null))
    }

    fn undo(&self, tx: &Tx, _cx: &OpCx, _op: &Value, before: &Value) -> ApiResult<()> {
        restore(tx, before)
    }
}

// ---------------------------------------------------------------- defaults (§3)

/// Links `object` to `person` unless that row exists already; a proposed row is confirmed. Returns
/// whether something was written. For system writes (connect, sync, seed), audited as `steward.link`.
pub fn link(
    tx: &Tx,
    ws: &str,
    object_type: &str,
    object_id: &str,
    person_id: &str,
    source: &str,
    source_ref: Option<&str>,
) -> ApiResult<bool> {
    let k = Key {
        object_type: object_type.into(),
        object_id: object_id.into(),
        person_id: person_id.into(),
        role: "owner".into(),
    };
    if get(tx, ws, &k)?.is_some_and(|s| s.status == "confirmed") {
        return Ok(false);
    }
    let mut s = Steward::new(ws, object_type, object_id, person_id, "owner", source);
    s.source_ref = source_ref.map(str::to_string);
    put(tx, &s)?;
    base::audit(
        tx,
        ws,
        "steve",
        "steward.link",
        object_id,
        &json!({ "object_type": object_type, "person": person_id, "source": source }),
    )?;
    Ok(true)
}

/// A new object gets “我” of `user` (or of the workspace's owner) as 负责人, unless it has stewards
/// already. Nothing happens while that “我” does not exist yet: [`seed_defaults`] links it when it is
/// created.
pub fn adopt(
    tx: &Tx,
    ws: &str,
    object_type: &str,
    object_id: &str,
    user: Option<&str>,
    source: &str,
) -> ApiResult<bool> {
    if has_any(tx, ws, object_type, object_id)? {
        return Ok(false);
    }
    let me = match user {
        Some(u) => repo::me_of(tx, ws, u)?.filter(|p| p.status == "active"),
        None => owner_me(tx, ws)?,
    };
    match me {
        Some(me) => link(tx, ws, object_type, object_id, &me.id, source, None),
        None => Ok(false),
    }
}

/// [`adopt`] in every workspace, for objects that belong to none (devices): each workspace's owner
/// gets it, where “我” exists and nobody has it yet.
pub fn adopt_everywhere(tx: &Tx, object_type: &str, object_id: &str) -> ApiResult<usize> {
    let wss: Vec<String> = tx
        .prepare_cached("SELECT id FROM workspaces ORDER BY created")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut n = 0;
    for ws in wss {
        if adopt(tx, &ws, object_type, object_id, None, "seed")? {
            n += 1;
        }
    }
    Ok(n)
}

/// The backfill of migration 18 for one workspace, run when its “我” (`me`) is created: Steve's bots,
/// personas and the Hub device go to `me`; connections to the person whose account it is, else to the
/// connecting user's “我”. Objects that have any steward already are left alone.
pub fn seed_defaults(tx: &Tx, ws: &str, me: &Party) -> ApiResult<usize> {
    let mut n = 0;
    let bots: Vec<String> = tx
        .prepare_cached(
            "SELECT b.id FROM bots b WHERE b.deleted = 0
               AND (b.workspace_id = ?1 OR EXISTS (SELECT 1 FROM bot_workspaces w WHERE w.bot_id = b.id AND w.workspace_id = ?1))",
        )?
        .query_map([ws], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let personas: Vec<String> = tx
        .prepare_cached("SELECT id FROM personas WHERE workspace_id = ? AND deleted = 0")?
        .query_map([ws], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let hub: Vec<String> = tx
        .prepare_cached("SELECT id FROM devices WHERE id = 'hub'")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for (t, ids) in [("bot", bots), ("persona", personas), ("device", hub)] {
        for id in ids {
            if !has_any(tx, ws, t, &id)? && link(tx, ws, t, &id, &me.id, "seed", None)? {
                n += 1;
            }
        }
    }
    let conns: Vec<(String, String, String)> = tx
        .prepare_cached(
            "SELECT c.id, a.party_id, c.created_by FROM connections c JOIN accounts a ON a.id = c.account_id
             WHERE c.workspace_id = ? AND c.status != 'revoked'",
        )?
        .query_map([ws], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (id, party, by) in conns {
        if has_any(tx, ws, "connection", &id)? {
            continue;
        }
        if let Some(p) = connection_owner(tx, ws, &party, &by)?
            && link(tx, ws, "connection", &id, &p, "seed", None)?
        {
            n += 1;
        }
    }
    Ok(n)
}

/// Whose a connection is (§3): the person whose account it is, else the connecting user's “我”.
pub fn connection_owner(
    c: &Connection,
    ws: &str,
    account_party: &str,
    created_by: &str,
) -> ApiResult<Option<String>> {
    if let Some(p) = repo::get(c, account_party)?.filter(|p| {
        p.kind == "contact"
            && p.status == "active"
            && !matches!(p.contact_kind.as_deref(), Some("bot" | "virtual"))
    }) {
        return Ok(Some(p.id));
    }
    Ok(repo::me_of(c, ws, created_by)?
        .filter(|p| p.status == "active")
        .map(|p| p.id))
}

/// A new bot party (`contact_kind = bot`, §1) for a non-person actor: an app bot on connect, another
/// bot seen in a chat. Nobody owns it yet.
pub fn new_bot_party(tx: &Tx, ws: &str, actor: &str, name: &str, source: &str) -> ApiResult<Party> {
    let name: String = name.trim().chars().take(80).collect();
    let name = if name.is_empty() {
        "机器人".to_string()
    } else {
        name
    };
    let mut p = NewParty::new("contact", &name, actor);
    p.contact_kind = Some("bot".into());
    p.source = source.into();
    let p = repo::insert(tx, ws, &p)?;
    base::audit(
        tx,
        ws,
        actor,
        "party.create",
        &p.id,
        &json!({ "type": "contact", "bot": true }),
    )?;
    super::service::event(tx, ws, &p.id);
    Ok(p)
}

// ---------------------------------------------------------------- proposals from chat (§3)

/// What [`propose`] did.
#[derive(Debug, Clone, PartialEq)]
pub enum Proposed {
    /// A proposed row and its decision.
    Opened(String),
    /// That person is already a confirmed steward in that role.
    Already,
    /// Already proposed, waiting for the user.
    Pending(String),
}

/// A proposed steward (`source = chat`) plus a `confirm` decision the user answers with one click.
#[allow(clippy::too_many_arguments)]
pub fn propose(
    tx: &Tx,
    ws: &str,
    o: &ObjectInfo,
    person: &Party,
    role: &str,
    basis: &str,
    requested_by: &str,
    source_ref: Option<&str>,
) -> ApiResult<Proposed> {
    let k = Key {
        object_type: o.object_type.clone(),
        object_id: o.object_id.clone(),
        person_id: person.id.clone(),
        role: role.into(),
    };
    if let Some(s) = get(tx, ws, &k)? {
        return Ok(match (s.status.as_str(), s.decision_id) {
            ("proposed", Some(d)) => Proposed::Pending(d),
            _ => Proposed::Already,
        });
    }
    let subject = format!(
        "{}:{}:{}:{}",
        k.object_type, k.object_id, k.person_id, k.role
    );
    let mut d = decisions::confirm(
        ws,
        SUBJECT,
        &subject,
        &format!("「{}」的{}是 {}？", o.name, role_label(role), person.name),
        requested_by,
    );
    let basis = basis.trim();
    if !basis.is_empty() {
        d.draft = Some(basis.chars().take(500).collect());
    }
    let d = base::decisions::open(tx, &d)?;
    let mut s = Steward::new(ws, &k.object_type, &k.object_id, &k.person_id, role, "chat");
    s.status = "proposed".into();
    s.confidence = CHAT_CONFIDENCE;
    s.source_ref = source_ref.map(str::to_string);
    s.decision_id = Some(d.id.clone());
    put(tx, &s)?;
    Ok(Proposed::Opened(d.id))
}

/// `confirm` on a proposed steward: `yes` → confirmed; `no` → deleted. Undo makes it proposed again.
pub struct StewardConfirm;

impl DecisionHandler for StewardConfirm {
    fn resolve(&self, tx: &Tx, cx: &DecisionCx, d: &Decision) -> ApiResult<Value> {
        let s = by_decision(tx, &d.id)?
            .filter(|s| s.workspace_id == cx.ws)
            .ok_or_else(|| ApiError::not_found("负责人提议"))?;
        let mut result = decisions::choice_result(cx);
        if cx.choice == "yes" {
            let mut c = s.clone();
            c.status = "confirmed".into();
            c.confidence = 1.0;
            c.updated = now();
            put(tx, &c)?;
        } else {
            delete(tx, &s.workspace_id, &s.key())?;
            result["steward"] = serde_json::to_value(&s)?;
        }
        Ok(result)
    }

    fn before_undo(&self, tx: &Tx, d: &Decision) -> ApiResult<()> {
        if let Some(v) = d.result.get("steward") {
            let mut s: Steward = serde_json::from_value(v.clone())?;
            s.status = "proposed".into();
            if repo::get(tx, &s.person_id)?.is_some() {
                put(tx, &s)?;
            }
            return Ok(());
        }
        if let Some(mut s) = by_decision(tx, &d.id)? {
            s.status = "proposed".into();
            s.confidence = CHAT_CONFIDENCE;
            put(tx, &s)?;
        }
        Ok(())
    }

    fn expire(&self, tx: &Tx, d: &Decision) -> ApiResult<()> {
        if let Some(s) = by_decision(tx, &d.id)?.filter(|s| s.status == "proposed") {
            delete(tx, &s.workspace_id, &s.key())?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------- merges

/// What a merge did to the steward rows (in `merges.moved.stewards`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MovedStewards {
    /// Rows of `from` as a steward, now `into`'s (keys after the move).
    pub moved: Vec<Key>,
    /// Rows of `from` deleted because `into` already had the same one.
    pub dropped: Vec<Steward>,
    /// Rows about `from` as an object (a bot party), now about `into` (keys after the move).
    pub objects: Vec<Key>,
    pub objects_dropped: Vec<Steward>,
}

/// Moves `from`'s steward rows to `into` inside the merge transaction.
pub fn merge_move(tx: &Tx, ws: &str, from: &str, into: &str) -> ApiResult<MovedStewards> {
    let mut mv = MovedStewards::default();
    for s in of_person(tx, ws, from)? {
        let mut k = s.key();
        k.person_id = into.into();
        if get(tx, ws, &k)?.is_some() {
            delete(tx, ws, &s.key())?;
            mv.dropped.push(s);
        } else {
            tx.run(
                "UPDATE stewards SET person_id = ? WHERE workspace_id = ? AND object_type = ? AND object_id = ? AND person_id = ? AND role = ?",
                params![into, ws, s.object_type, s.object_id, from, s.role],
            )?;
            mv.moved.push(k);
        }
    }
    for s in of_object(tx, ws, "party", from)? {
        let mut k = s.key();
        k.object_id = into.into();
        if k.person_id == into || get(tx, ws, &k)?.is_some() {
            delete(tx, ws, &s.key())?;
            mv.objects_dropped.push(s);
        } else {
            tx.run(
                "UPDATE stewards SET object_id = ? WHERE workspace_id = ? AND object_type = 'party' AND object_id = ? AND person_id = ? AND role = ?",
                params![into, ws, from, s.person_id, s.role],
            )?;
            mv.objects.push(k);
        }
    }
    changed(tx, ws, into);
    Ok(mv)
}

/// Undoes [`merge_move`] for the rows still where the merge put them.
pub fn split_back(tx: &Tx, ws: &str, from: &str, into: &str, mv: &MovedStewards) -> ApiResult<()> {
    for k in &mv.moved {
        tx.run(
            "UPDATE OR IGNORE stewards SET person_id = ? WHERE workspace_id = ? AND object_type = ? AND object_id = ? AND person_id = ? AND role = ?",
            params![from, ws, k.object_type, k.object_id, into, k.role],
        )?;
    }
    for k in &mv.objects {
        tx.run(
            "UPDATE OR IGNORE stewards SET object_id = ? WHERE workspace_id = ? AND object_type = 'party' AND object_id = ? AND person_id = ? AND role = ?",
            params![from, ws, into, k.person_id, k.role],
        )?;
    }
    for s in mv.dropped.iter().chain(mv.objects_dropped.iter()) {
        if get(tx, ws, &s.key())?.is_none() {
            put(tx, s)?;
        }
    }
    changed(tx, ws, from);
    changed(tx, ws, into);
    Ok(())
}

/// A deleted contact is nobody's steward any more, and a deleted bot party has none.
pub fn forget_party(tx: &Tx, ws: &str, party: &str) -> ApiResult<usize> {
    Ok(tx.run(
        "DELETE FROM stewards WHERE workspace_id = ?1 AND (person_id = ?2 OR (object_type = 'party' AND object_id = ?2))",
        params![ws, party],
    )?)
}

// ---------------------------------------------------------------- for cards and tools

/// The confirmed owners' names of an object (`张三、李四`), empty if none.
pub fn owner_names(
    c: &Connection,
    ws: &str,
    object_type: &str,
    object_id: &str,
) -> ApiResult<Vec<String>> {
    let mut out = Vec::new();
    for s in of_object(c, ws, object_type, object_id)?
        .into_iter()
        .filter(|s| s.role == "owner" && s.status == "confirmed")
    {
        let n = name_of(c, &s.person_id)?;
        if !out.contains(&n) {
            out.push(n);
        }
    }
    Ok(out)
}

/// The owners of a party: its own stewards, then those of its accounts (a bot's app account).
pub fn party_owner_names(c: &Connection, ws: &str, party: &str) -> ApiResult<Vec<String>> {
    let mut out = owner_names(c, ws, "party", party)?;
    let accounts: Vec<String> = c
        .prepare_cached("SELECT id FROM accounts WHERE workspace_id = ? AND party_id = ? AND status != 'deleted'")?
        .query_map(params![ws, party], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for a in accounts {
        for n in owner_names(c, ws, "account", &a)? {
            if !out.contains(&n) {
                out.push(n);
            }
        }
    }
    Ok(out)
}
