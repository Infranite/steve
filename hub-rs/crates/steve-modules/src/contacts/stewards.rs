//! 归属与负责人: who is responsible for the objects that are not a
//! person's own identity — app bots, service accounts, shared mailboxes, connections, devices, Steve's
//! bots, personas, bot parties and spaces. Whose identity an account is stays `accounts.party_id`.
//!
//! - the `stewards` rows of migration 18 ([`Steward`], [`get`], [`of_object`], [`of_person`] …);
//! - what an object is called ([`object_info`]), a person's holdings ([`holdings`]) and the objects
//!   without a 负责人 ([`unowned`]);
//! - edits by hand go through change records: the operations `steward.set` / `steward.remove`
//!   ([`SetOp`], [`RemoveOp`]) stored and applied as a changeset ([`set`], [`remove`]), undoable for 24
//!   hours through `POST /api/config/:id/undo` like every contact edit;
//! - defaults: [`seed_defaults`] when “我” is first created, [`adopt`] for a new bot, persona or
//!   connection, [`link`] for what a provider tells (source `sync`);
//! - from chat (`steve_steward_propose`): written confirmed at once through [`set_from`] (source
//!   `chat`, the others of that role kept), undoable for 24 hours like a manual edit; [`propose`] and
//!   its `confirm` handler [`StewardConfirm`] stay for proposals opened before;
//! - merges move the rows with the person ([`merge_move`], [`split_back`]).

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use steve_store::base::{Change, Viewer};
use steve_store::{
    Accounts, BotRows, ChannelSources, Connections, Contacts, Decision, DecisionRows, DeviceRows,
    Members, PersonaRows, Read, Stewards, Systems, W, account_rows::Account, now,
};

use super::repo::{NewParty, Party};
use crate::changes::{self, ChangeOp, OpCx, Origin, Plan, Prepared};
use crate::decisions::{self, DecisionCx, DecisionHandler, DecisionOutcome};
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
const SERVICE_ACCOUNTS: [&str; 2] = ["service", "shared_mailbox"];

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

/// [`type_label`] in the request's language, for the pages (`/mcp` answers in zh-CN anyway).
pub fn type_shown(t: &str) -> String {
    use steve_i18n::t;
    match t {
        "account" => t!("modules.stewards.type.account"),
        "connection" => t!("modules.stewards.type.connection"),
        "device" => t!("modules.stewards.type.device"),
        "bot" => t!("modules.stewards.type.bot"),
        "persona" => t!("modules.stewards.type.persona"),
        "party" => t!("modules.stewards.type.party"),
        "space" => t!("modules.stewards.type.space"),
        x => x.to_string(),
    }
}

/// [`role_label`] in the request's language.
pub fn role_shown(r: &str) -> String {
    use steve_i18n::t;
    match r {
        "owner" => t!("modules.stewards.role.owner"),
        "maintainer" => t!("modules.stewards.role.maintainer"),
        "user" => t!("modules.stewards.role.user"),
        x => x.to_string(),
    }
}

/// `accounts.kind` as the pages say it, in the request's language.
pub fn account_kind_shown(k: &str) -> String {
    use steve_i18n::t;
    match k {
        "personal" => t!("modules.account_kind.personal"),
        "work_email" => t!("modules.account_kind.work_email"),
        "app_bot" => t!("modules.account_kind.app_bot"),
        "shared_mailbox" => t!("modules.account_kind.shared_mailbox"),
        "bank_card" => t!("modules.account_kind.bank_card"),
        "service" => t!("modules.account_kind.service"),
        "phone" => t!("modules.account_kind.phone"),
        "bot" => t!("modules.account_kind.bot"),
        "calendar" => t!("modules.account_kind.calendar"),
        "root" => t!("modules.account_kind.root"),
        _ => t!("modules.account_kind.other"),
    }
}

/// `accounts.kind` as the pages say it (zh-CN).
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
pub use steve_store::{Steward, StewardKey as Key};

pub fn get(c: &mut impl Read, ws: &str, key: &Key) -> ApiResult<Option<Steward>> {
    Ok(Stewards(c).get(ws, key)?)
}

pub fn of_object(c: &mut impl Read, ws: &str, kind: &str, id: &str) -> ApiResult<Vec<Steward>> {
    Ok(Stewards(c).of_object(ws, kind, id)?)
}

pub fn of_person(c: &mut impl Read, ws: &str, id: &str) -> ApiResult<Vec<Steward>> {
    Ok(Stewards(c).of_person(ws, id)?)
}

/// Inserts or replaces the row with `s`'s key (keeping its `created` when it existed).
pub fn put(tx: &mut W<'_>, s: &Steward) -> ApiResult<()> {
    Stewards(tx).put(s)?;
    tx.emit(crate::accounts::service::party_changed(
        &s.workspace_id,
        &s.person_id,
    ));
    Ok(())
}

pub fn delete(tx: &mut W<'_>, ws: &str, k: &Key) -> ApiResult<bool> {
    let removed = Stewards(tx).delete(ws, k)?;
    tx.emit(crate::accounts::service::party_changed(ws, &k.person_id));
    Ok(removed)
}

/// Does the object have any steward row at all (confirmed or proposed)? Defaults only go to objects
/// without one, so a 负责人 removed by hand is not put back.
pub fn has_any(c: &mut impl Read, ws: &str, kind: &str, id: &str) -> ApiResult<bool> {
    Ok(Stewards(c).has_any(ws, kind, id)?)
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
            type_label: type_shown(t),
            name,
            kind: None,
            detail: String::new(),
            party_id: None,
        }
    }
}

/// A system's name as the pages show it (a built-in one in the request's language).
fn system_name(c: &mut impl Read, ws: &str, system: &str) -> ApiResult<String> {
    Ok(Systems(c)
        .get(ws, system)?
        .map(|row| crate::shown::system_name(system, row.builtin, &row.name))
        .unwrap_or_else(|| system.to_string()))
}

fn account_info(c: &mut impl Read, ws: &str, row: Account) -> ApiResult<ObjectInfo> {
    let name = if row.display_name.trim().is_empty() {
        row.handle_display.clone()
    } else {
        row.display_name
    };
    let mut o = ObjectInfo::new("account", &row.id, name);
    o.detail = format!(
        "{} · {} · {}",
        system_name(c, ws, &row.system_id)?,
        account_kind_shown(&row.kind),
        row.handle_display
    );
    o.kind = Some(row.kind);
    o.party_id = Some(row.party_id);
    Ok(o)
}

fn connection_info(c: &mut impl Read, row: steve_store::ConnRow) -> ApiResult<Option<ObjectInfo>> {
    let Some(account) = Accounts(c).get(&row.account_id)? else {
        return Ok(None);
    };
    let name = if account.display_name.trim().is_empty() {
        account.handle_display.clone()
    } else {
        account.display_name
    };
    let mut o = ObjectInfo::new("connection", &row.id, name);
    o.detail = format!("{} · {}", row.adapter, account.handle_display);
    o.kind = Some(row.adapter);
    o.party_id = Some(account.party_id);
    Ok(Some(o))
}

fn device_info(row: steve_store::DeviceIdentity) -> ObjectInfo {
    let mut o = ObjectInfo::new("device", &row.id, row.name);
    o.detail = row
        .roles
        .into_iter()
        .map(|role| {
            if role == "执行节点" {
                steve_i18n::t!("modules.stewards.device_runner")
            } else {
                role
            }
        })
        .collect::<Vec<_>>()
        .join(&steve_i18n::t!("modules.error.list_sep"));
    o
}

fn bot_info(row: steve_store::BotRef) -> ObjectInfo {
    let mut o = ObjectInfo::new("bot", &row.id, row.name);
    o.detail =
        steve_store::seeded::shown(&row.role, steve_store::seeded::BOT[0].1).unwrap_or(row.role);
    o
}

fn persona_info(row: steve_store::PersonaIdentity) -> ObjectInfo {
    let mut o = ObjectInfo::new("persona", &row.id, row.name);
    let intro = steve_store::seeded::shown(&row.intro, steve_store::seeded::PERSONA[0].1)
        .unwrap_or(row.intro);
    o.detail = intro.chars().take(60).collect();
    o
}

fn space_info(id: &str, name: String, kind: String) -> ObjectInfo {
    let mut o = ObjectInfo::new("space", id, name);
    o.kind = Some(kind);
    o
}

fn party_info(p: Party) -> ObjectInfo {
    let mut o = ObjectInfo::new("party", &p.id, p.name);
    if p.contact_kind.as_deref() != Some("bot") {
        o.type_label = match p.kind.as_str() {
            "org" => steve_i18n::t!("modules.stewards.type.org"),
            "team" => steve_i18n::t!("modules.stewards.type.team"),
            _ => steve_i18n::t!("modules.stewards.type.contact"),
        };
    }
    o.kind = p.contact_kind.or(p.org_kind);
    o.party_id = Some(p.id);
    o
}

pub fn object_info(
    c: &mut impl Read,
    ws: &str,
    kind: &str,
    id: &str,
) -> ApiResult<Option<ObjectInfo>> {
    Ok(match kind {
        "account" => Accounts(c)
            .get(id)?
            .filter(|a| a.workspace_id == ws && a.status != "deleted")
            .map(|a| account_info(c, ws, a))
            .transpose()?,
        "connection" => {
            let row = Connections(c)
                .get(id)?
                .filter(|row| row.workspace_id == ws && row.status != "revoked");
            match row {
                Some(row) => connection_info(c, row)?,
                None => None,
            }
        }
        "party" => Contacts::new(c)
            .party(id)?
            .filter(|p| p.workspace_id == ws && p.status == "active")
            .map(party_info),
        "bot" | "device" | "persona" | "space" => named_objects(c, ws, kind)?
            .into_iter()
            .find(|o| o.object_id == id),
        _ => None,
    })
}

pub(super) fn named_objects(c: &mut impl Read, ws: &str, kind: &str) -> ApiResult<Vec<ObjectInfo>> {
    Ok(match kind {
        "bot" => BotRows(c).owned_in(ws)?.into_iter().map(bot_info).collect(),
        "device" => DeviceRows(c)
            .identities()?
            .into_iter()
            .map(device_info)
            .collect(),
        "persona" => PersonaRows(c)
            .identities(ws)?
            .into_iter()
            .map(persona_info)
            .collect(),
        "space" => ChannelSources(c)
            .named_spaces(ws)?
            .into_iter()
            .map(|(id, name, kind)| space_info(&id, name, kind))
            .collect(),
        _ => Vec::new(),
    })
}

// ---------------------------------------------------------------- people

/// A person who can be a steward: an active, visible contact that is not a bot. `me` is the viewer's
/// “我”. `404 没有这个联系人` otherwise.
pub fn person(c: &mut impl Read, viewer: &Viewer, id: &str) -> ApiResult<Party> {
    let p = if id == "me" {
        Contacts::new(c).me(&viewer.ws, &viewer.user)?
    } else {
        Contacts::new(c).party(id)?
    };
    let p = crate::own(
        p.filter(|p| p.kind == "contact" && p.status == "active"),
        viewer,
        "联系人",
    )?;
    if p.contact_kind.as_deref() == Some("bot") {
        return Err(ApiError::rule(steve_i18n::t!(
            "modules.stewards.bot_cannot_own"
        )));
    }
    Ok(p)
}

fn name_of(c: &mut impl Read, id: &str) -> ApiResult<String> {
    Ok(Contacts::new(c)
        .party(id)?
        .map(|p| p.name)
        .unwrap_or_else(|| id.to_string()))
}

/// “我” of a workspace's first owner (whom defaults go to when no user is at hand).
pub fn owner_me(c: &mut impl Read, ws: &str) -> ApiResult<Option<Party>> {
    let user = Members::new(c).owner(ws)?;
    match user {
        Some(u) => Ok(Contacts::new(c)
            .me(ws, &u)?
            .filter(|p| p.status == "active")),
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

fn steward_out(c: &mut impl Read, s: &Steward) -> ApiResult<StewardOut> {
    let p = Contacts::new(c).party(&s.person_id)?;
    Ok(StewardOut {
        person_id: s.person_id.clone(),
        person_name: p.as_ref().map(|p| p.name.clone()).unwrap_or_default(),
        is_me: p.as_ref().is_some_and(Party::is_me),
        role: s.role.clone(),
        role_label: role_shown(&s.role),
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

pub fn object_stewards(
    c: &mut impl Read,
    ws: &str,
    object: ObjectInfo,
) -> ApiResult<ObjectStewards> {
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

/// `GET /people/:id/holdings`: 名下 — own identities by system, then what the person is
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
pub fn accounts_of(c: &mut impl Read, ws: &str, party: &str) -> ApiResult<Vec<AccountGroup>> {
    let mut rows = Accounts(c).of_party(ws, party)?;
    rows.retain(|row| row.status != "deleted");
    rows.sort_by(|a, b| {
        a.system_id
            .cmp(&b.system_id)
            .then_with(|| a.created.cmp(&b.created))
    });
    let mut out: Vec<AccountGroup> = Vec::new();
    for row in rows {
        let Account {
            id,
            system_id: system,
            handle_display: handle,
            display_name: name,
            kind,
            ..
        } = row;
        let a = AccountRef {
            id,
            handle,
            name,
            kind_label: account_kind_shown(&kind),
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

pub fn holdings_of(c: &mut impl Read, viewer: &Viewer, p: &Party) -> ApiResult<Holdings> {
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
            role_label: role_shown(&s.role),
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
    m.store.read::<_, steve_store::StoreError>(|c| {
        Ok((|| {
            let p = super::service::own_party_in(c, &viewer, id, "contact", "联系人", false)?;
            holdings_of(c, &viewer, &p)
        })())
    })?
}

/// The objects nobody confirmed as 负责人: Steve's bots, personas, devices,
/// connections, bot parties and service / shared-mailbox accounts.
pub fn unowned_in(c: &mut impl Read, ws: &str) -> ApiResult<Vec<ObjectInfo>> {
    let owned = Stewards(c).confirmed_objects(ws)?;
    let mut out = Vec::new();
    for kind in ["bot", "persona", "device"] {
        out.extend(named_objects(c, ws, kind)?);
    }
    for connection in Connections(c)
        .list(ws)?
        .into_iter()
        .filter(|row| row.status != "revoked")
    {
        out.extend(connection_info(c, connection)?);
    }
    let mut parties = Contacts::new(c).active(&Viewer::bare(ws, "steve"), &["contact"], true)?;
    parties.retain(|p| p.contact_kind.as_deref() == Some("bot"));
    parties.sort_by(|a, b| a.name.cmp(&b.name));
    out.extend(parties.into_iter().map(party_info));
    for row in Accounts(c).service_identities(ws, &SERVICE_ACCOUNTS)? {
        out.push(account_info(c, ws, row)?);
    }
    out.retain(|object| !owned.contains(&(object.object_type.clone(), object.object_id.clone())));
    Ok(out)
}

/// `GET /stewards/unowned`.
pub fn unowned(m: &Modules, who: &Caller) -> ApiResult<Vec<ObjectInfo>> {
    super::service::start(m, who)?;
    m.store
        .read::<_, steve_store::StoreError>(|c| Ok(unowned_in(c, &who.ws)))?
}

/// `GET /stewards/:type/:id`.
pub fn list_object(
    m: &Modules,
    who: &Caller,
    object_type: &str,
    id: &str,
) -> ApiResult<ObjectStewards> {
    super::service::start(m, who)?;
    m.store.read::<_, steve_store::StoreError>(|c| {
        Ok((|| {
            let o = own_object(c, &who.ws, object_type, id)?;
            object_stewards(c, &who.ws, o)
        })())
    })?
}

/// The object or `404 没有这个<type label>` (`400` for an unknown type).
pub fn own_object(
    c: &mut impl Read,
    ws: &str,
    object_type: &str,
    id: &str,
) -> ApiResult<ObjectInfo> {
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

fn name_list(c: &mut impl Read, rows: &[&Steward]) -> ApiResult<Vec<String>> {
    rows.iter().map(|s| name_of(c, &s.person_id)).collect()
}

/// The stewards on a preview line: "A、B", or “（空）”.
fn shown_names(v: &[String]) -> String {
    if v.is_empty() {
        steve_i18n::t!("modules.stewards.none")
    } else {
        v.join(&steve_i18n::t!("modules.error.list_sep"))
    }
}

fn names(c: &mut impl Read, rows: &[&Steward]) -> ApiResult<String> {
    Ok(shown_names(&name_list(c, rows)?))
}

fn set_op(o: &ObjectInfo, person: &str, role: &str, source: &str) -> Value {
    json!({ "op": "steward.set", "object_type": o.object_type, "object_id": o.object_id, "person_id": person, "role": role, "source": source })
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
    origin: Origin,
) -> ApiResult<Option<String>> {
    if plan.ops.is_empty() {
        return Ok(None);
    }
    let prepared = Prepared {
        target_id: Some(href_person.to_string()),
        revision: None,
        title: steve_i18n::stored(|| {
            steve_i18n::t!(
                "modules.stewards.line",
                name = o.name,
                role = role_shown("owner")
            )
        }),
        href: format!("#/contacts/{href_person}"),
        subject: format!("the stewards of {} \"{}\"", o.object_type, o.name),
        fields: Vec::new(),
        ops: Vec::new(),
        free_text: None,
        context: Value::Null,
    };
    let p = changes::store_plan(m, who, "contact", &prepared, plan, origin)?;
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
    set_from(m, who, object_type, id, b, "manual", Origin::default())
}

/// [`set`] with the row's `source` (`manual`, `chat`) and the changeset's origin.
pub fn set_from(
    m: &Modules,
    who: &Caller,
    object_type: &str,
    id: &str,
    b: SetBody,
    source: &str,
    origin: Origin,
) -> ApiResult<Changed> {
    let (viewer, _) = super::service::start(m, who)?;
    let role = check_role(b.role.as_deref())?;
    // the preview line is stored with the changeset: zh-CN (changes::stored)
    let (o, plan, person) = changes::stored(|| {
        m.store.read::<_, steve_store::StoreError>(|c| {
            Ok((|| {
                let o = own_object(c, &who.ws, object_type, id)?;
                let p = person(c, &viewer, b.person_id.trim())?;
                if o.object_type == "party" && o.object_id == p.id {
                    return Err(ApiError::rule(steve_i18n::t!(
                        "modules.stewards.not_own_steward"
                    )));
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
                    plan.ops.push(set_op(&o, &p.id, &role, source));
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
                    let mut to = name_list(c, &after)?;
                    to.push(p.name.clone());
                    let to = shown_names(&to);
                    plan.changes.push(Change::new(
                        steve_i18n::t!(
                            "modules.stewards.line",
                            name = o.name,
                            role = role_shown(&role)
                        ),
                        names(c, &same)?,
                        to,
                    ));
                }
                Ok((o, plan, p))
            })())
        })?
    })?;
    let undo = record(m, who, &o, &person.id, plan, origin)?;
    let now = m
        .store
        .read::<_, steve_store::StoreError>(|c| Ok(object_stewards(c, &who.ws, o)))??;
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
    let (o, plan, href) = changes::stored(|| {
        m.store.read::<_, steve_store::StoreError>(|c| {
            Ok((|| {
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
                        .filter(|s| {
                            s.role == role && !gone.iter().any(|g| g.person_id == s.person_id)
                        })
                        .collect();
                    plan.changes.push(Change::new(
                        steve_i18n::t!(
                            "modules.stewards.line",
                            name = o.name,
                            role = role_shown(&role)
                        ),
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
            })())
        })?
    })?;
    let undo = record(m, who, &o, &href, plan, Origin::default())?;
    let now = m
        .store
        .read::<_, steve_store::StoreError>(|c| Ok(object_stewards(c, &who.ws, o)))??;
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

fn restore(tx: &mut W<'_>, before: &Value) -> ApiResult<()> {
    if before.is_null() {
        return Ok(());
    }
    let s: Steward = serde_json::from_value(before.clone())?;
    if Contacts::new(tx).party(&s.person_id)?.is_some() {
        put(tx, &s)?;
    }
    Ok(())
}

/// `steward.set {object_type, object_id, person_id, role, source}`: a confirmed row. Undo puts back
/// what was there (nothing, or the proposed row).
pub struct SetOp;

impl ChangeOp for SetOp {
    fn validate(&self, w: &mut steve_store::W<'_>, cx: &OpCx, op: &Value) -> ApiResult<()> {
        let c = w;
        let k = key_of(op)?;
        own_object(c, cx.ws, &k.object_type, &k.object_id)?;
        check_role(Some(&k.role))?;
        let viewer = Viewer::bare(cx.ws, cx.actor);
        person(c, &viewer, &k.person_id).map(|_| ())
    }

    fn apply(&self, w: &mut steve_store::W<'_>, cx: &OpCx, op: &Value) -> ApiResult<Value> {
        let tx = w;
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
                && DecisionRows(tx).get(d)?.is_some_and(|d| d.status == "open")
            {
                DecisionRows(tx).settle(
                    d,
                    &json!({ "choice": "yes", "manual": true }),
                    "resolved",
                    &Value::Null,
                )?;
            }
        }
        put(tx, &s)?;
        tx.audit(cx.ws,
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

    fn undo(
        &self,
        w: &mut steve_store::W<'_>,
        cx: &OpCx,
        op: &Value,
        before: &Value,
    ) -> ApiResult<()> {
        let tx = w;
        let k = key_of(op)?;
        delete(tx, cx.ws, &k)?;
        restore(tx, before)
    }
}

/// `steward.remove {object_type, object_id, person_id, role}`. Undo puts the row back as it was.
pub struct RemoveOp;

impl ChangeOp for RemoveOp {
    fn apply(&self, w: &mut steve_store::W<'_>, cx: &OpCx, op: &Value) -> ApiResult<Value> {
        let tx = w;
        let k = key_of(op)?;
        let before = get(tx, cx.ws, &k)?;
        delete(tx, cx.ws, &k)?;
        tx.audit(cx.ws,
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

    fn undo(
        &self,
        w: &mut steve_store::W<'_>,
        _cx: &OpCx,
        _op: &Value,
        before: &Value,
    ) -> ApiResult<()> {
        let tx = w;
        restore(tx, before)
    }
}

// ---------------------------------------------------------------- defaults

/// Links `object` to `person` unless that row exists already; a proposed row is confirmed. Returns
/// whether something was written. For system writes (connect, sync, seed), audited as `steward.link`.
pub fn link(
    tx: &mut W<'_>,
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
    tx.audit(
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
    tx: &mut W<'_>,
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
        Some(u) => Contacts::new(tx)
            .me(ws, u)?
            .filter(|p| p.status == "active"),
        None => owner_me(tx, ws)?,
    };
    match me {
        Some(me) => link(tx, ws, object_type, object_id, &me.id, source, None),
        None => Ok(false),
    }
}

/// [`adopt`] in every workspace, for objects that belong to none (devices): each workspace's owner
/// gets it, where “我” exists and nobody has it yet.
pub fn adopt_everywhere(tx: &mut W<'_>, object_type: &str, object_id: &str) -> ApiResult<usize> {
    let wss = steve_store::Workspaces(tx).ids()?;
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
pub fn seed_defaults(tx: &mut W<'_>, ws: &str, me: &Party) -> ApiResult<usize> {
    let mut n = 0;
    let bots = BotRows(tx)
        .owned_in(ws)?
        .into_iter()
        .map(|b| b.id)
        .collect::<Vec<_>>();
    let personas = PersonaRows(tx)
        .identities(ws)?
        .into_iter()
        .map(|p| p.id)
        .collect();
    let hub = DeviceRows(tx)
        .identities()?
        .into_iter()
        .filter(|d| d.id == "hub")
        .map(|d| d.id)
        .collect();
    for (t, ids) in [("bot", bots), ("persona", personas), ("device", hub)] {
        for id in ids {
            if !has_any(tx, ws, t, &id)? && link(tx, ws, t, &id, &me.id, "seed", None)? {
                n += 1;
            }
        }
    }
    for row in Connections(tx)
        .list(ws)?
        .into_iter()
        .filter(|row| row.status != "revoked")
    {
        if has_any(tx, ws, "connection", &row.id)? {
            continue;
        }
        let Some(account) = Accounts(tx).get(&row.account_id)? else {
            continue;
        };
        if let Some(p) = connection_owner(tx, ws, &account.party_id, &row.created_by)?
            && link(tx, ws, "connection", &row.id, &p, "seed", None)?
        {
            n += 1;
        }
    }
    Ok(n)
}

/// Whose a connection is: the person whose account it is, else the connecting user's “我”.
pub fn connection_owner(
    c: &mut impl Read,
    ws: &str,
    account_party: &str,
    created_by: &str,
) -> ApiResult<Option<String>> {
    if let Some(p) = Contacts::new(c).party(account_party)?.filter(|p| {
        p.kind == "contact"
            && p.status == "active"
            && !matches!(p.contact_kind.as_deref(), Some("bot" | "virtual"))
    }) {
        return Ok(Some(p.id));
    }
    Ok(Contacts::new(c)
        .me(ws, created_by)?
        .filter(|p| p.status == "active")
        .map(|p| p.id))
}

/// A new bot party (`contact_kind = bot`) for a non-person actor: an app bot on connect, another
/// bot seen in a chat. Nobody owns it yet.
pub fn new_bot_party(
    w: &mut steve_store::W<'_>,
    ws: &str,
    actor: &str,
    name: &str,
    source: &str,
) -> ApiResult<Party> {
    let name: String = name.trim().chars().take(80).collect();
    let name = if name.is_empty() {
        "机器人".to_string()
    } else {
        name
    };
    let mut p = NewParty::new("contact", &name, actor);
    p.contact_kind = Some("bot".into());
    p.source = source.into();
    let p = w.contacts().insert(ws, &p)?;
    w.audit(
        ws,
        actor,
        "party.create",
        &p.id,
        &json!({ "type": "contact", "bot": true }),
    )?;
    w.emit(crate::accounts::service::party_changed(ws, &p.id));
    Ok(p)
}

// ---------------------------------------------------------------- proposals from chat

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
    tx: &mut W<'_>,
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
    let d = DecisionRows(tx).open(&d)?;
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
    fn resolve(&self, tx: &mut W<'_>, cx: &DecisionCx, d: &Decision) -> ApiResult<DecisionOutcome> {
        let s = Stewards(tx)
            .by_decision(&d.id)?
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
        Ok(result.into())
    }

    fn before_undo(&self, tx: &mut W<'_>, d: &Decision) -> ApiResult<()> {
        if let Some(v) = d.result.get("steward") {
            let mut s: Steward = serde_json::from_value(v.clone())?;
            s.status = "proposed".into();
            if Contacts::new(tx).party(&s.person_id)?.is_some() {
                put(tx, &s)?;
            }
            return Ok(());
        }
        if let Some(mut s) = Stewards(tx).by_decision(&d.id)? {
            s.status = "proposed".into();
            s.confidence = CHAT_CONFIDENCE;
            put(tx, &s)?;
        }
        Ok(())
    }

    fn expire(&self, tx: &mut W<'_>, d: &Decision) -> ApiResult<()> {
        if let Some(s) = Stewards(tx)
            .by_decision(&d.id)?
            .filter(|s| s.status == "proposed")
        {
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
pub fn merge_move(tx: &mut W<'_>, ws: &str, from: &str, into: &str) -> ApiResult<MovedStewards> {
    let w = tx;
    let mut mv = MovedStewards::default();
    for s in Stewards(w).of_person(ws, from)? {
        let mut k = s.key();
        k.person_id = into.into();
        if Stewards(w).get(ws, &k)?.is_some() {
            Stewards(w).delete(ws, &s.key())?;
            w.emit(crate::accounts::service::party_changed(ws, from));
            mv.dropped.push(s);
        } else {
            Stewards(w).move_person(ws, &s.key(), into)?;
            mv.moved.push(k);
        }
    }
    for s in Stewards(w).of_object(ws, "party", from)? {
        let mut k = s.key();
        k.object_id = into.into();
        if k.person_id == into || Stewards(w).get(ws, &k)?.is_some() {
            Stewards(w).delete(ws, &s.key())?;
            w.emit(crate::accounts::service::party_changed(ws, &s.person_id));
            mv.objects_dropped.push(s);
        } else {
            Stewards(w).move_object(ws, &s.key(), into)?;
            mv.objects.push(k);
        }
    }
    w.emit(crate::accounts::service::party_changed(ws, into));
    Ok(mv)
}

/// Undoes [`merge_move`] for rows still where the merge put them.
pub fn split_back(
    tx: &mut W<'_>,
    ws: &str,
    from: &str,
    into: &str,
    mv: &MovedStewards,
) -> ApiResult<()> {
    let w = tx;
    for k in &mv.moved {
        let mut dest = k.clone();
        dest.person_id = from.into();
        if Stewards(w).get(ws, &dest)?.is_none() {
            Stewards(w).move_person(ws, k, from)?;
        }
    }
    for k in &mv.objects {
        let mut dest = k.clone();
        dest.object_id = from.into();
        if Stewards(w).get(ws, &dest)?.is_none() {
            Stewards(w).move_object(ws, k, from)?;
        }
    }
    for s in mv.dropped.iter().chain(mv.objects_dropped.iter()) {
        if Stewards(w).get(ws, &s.key())?.is_none() {
            Stewards(w).put(s)?;
            w.emit(crate::accounts::service::party_changed(ws, &s.person_id));
        }
    }
    w.emit(crate::accounts::service::party_changed(ws, from));
    w.emit(crate::accounts::service::party_changed(ws, into));
    Ok(())
}

// ---------------------------------------------------------------- for cards and tools

/// The confirmed owners' names of an object (`张三、李四`), empty if none.
pub fn owner_names(
    c: &mut impl Read,
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
pub fn party_owner_names(c: &mut impl Read, ws: &str, party: &str) -> ApiResult<Vec<String>> {
    let mut out = owner_names(c, ws, "party", party)?;
    for account in Accounts(c)
        .of_party(ws, party)?
        .into_iter()
        .filter(|a| a.status != "deleted")
    {
        let a = account.id;
        for n in owner_names(c, ws, "account", &a)? {
            if !out.contains(&n) {
                out.push(n);
            }
        }
    }
    Ok(out)
}
