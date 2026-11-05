//! External personas: the “face” a Bot shows in channels. Every
//! workspace gets a default persona “Steve” the first time it uses channels; it cannot be deleted
//! and is the last fallback of every bot account (see [`crate::bindings`]).
//!
//! Personas are workspace objects: every member of the workspace sees them.

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use steve_modules::{ApiError, Check};
use steve_store::base::{self, Viewer};
use steve_store::{ChangeEvent, Tx, uid};

use crate::ChannelResult;
use crate::bindings;

/// Name of the default persona.
pub const DEFAULT_NAME: &str = "Steve";
/// `introOf` when nothing is revealed.
pub const NOTHING_REVEALED: &str = "（不透露身份，只回答问题本身）";

/// What a persona tells about itself. Missing keys in stored JSON read as `true`, the column
/// default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reveal {
    #[serde(default = "yes")]
    pub name: bool,
    #[serde(default = "yes")]
    pub role: bool,
    #[serde(default = "yes")]
    pub ai: bool,
    #[serde(default = "yes")]
    pub steve: bool,
    #[serde(default = "yes")]
    pub owner: bool,
}

fn yes() -> bool {
    true
}

impl Reveal {
    /// Everything, the default persona's setting.
    pub const ALL: Reveal = Reveal {
        name: true,
        role: true,
        ai: true,
        steve: true,
        owner: true,
    };
    /// A new persona's default: name, role and “is an AI assistant”.
    pub const NEW: Reveal = Reveal {
        name: true,
        role: true,
        ai: true,
        steve: false,
        owner: false,
    };

    fn apply(mut self, p: &RevealPatch) -> Self {
        self.name = p.name.unwrap_or(self.name);
        self.role = p.role.unwrap_or(self.role);
        self.ai = p.ai.unwrap_or(self.ai);
        self.steve = p.steve.unwrap_or(self.steve);
        self.owner = p.owner.unwrap_or(self.owner);
        self
    }
}

/// A partial `reveal` in a request body; missing keys keep their value.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RevealPatch {
    pub name: Option<bool>,
    pub role: Option<bool>,
    pub ai: Option<bool>,
    pub steve: Option<bool>,
    pub owner: Option<bool>,
}

/// One `personas` row.
#[derive(Debug, Clone, PartialEq)]
pub struct PersonaRow {
    pub id: String,
    pub workspace_id: String,
    pub name: String,
    pub avatar: Value,
    pub bot_id: String,
    pub intro: String,
    pub soul: String,
    pub reveal: Reveal,
    pub signature: String,
    pub is_default: bool,
    pub deleted: bool,
    pub revision: i64,
    pub created: i64,
    pub updated: i64,
}

const COLS: &str = "id, workspace_id, name, avatar, bot_id, intro, soul, reveal, signature, is_default, deleted, revision, created, updated";

fn row(r: &rusqlite::Row) -> rusqlite::Result<PersonaRow> {
    let avatar: String = r.get(3)?;
    let reveal: String = r.get(7)?;
    Ok(PersonaRow {
        id: r.get(0)?,
        workspace_id: r.get(1)?,
        name: r.get(2)?,
        avatar: serde_json::from_str(&avatar).unwrap_or_else(|_| json!({})),
        bot_id: r.get(4)?,
        intro: r.get(5)?,
        soul: r.get(6)?,
        reveal: serde_json::from_str(&reveal).unwrap_or(Reveal::ALL),
        signature: r.get(8)?,
        is_default: r.get::<_, i64>(9)? != 0,
        deleted: r.get::<_, i64>(10)? != 0,
        revision: r.get(11)?,
        created: r.get(12)?,
        updated: r.get(13)?,
    })
}

/// The answer to “你是谁？” (prototype `introOf`, ported verbatim): the revealed parts joined with
/// “，”. The role part is the intro without its leading “我是…，” and trailing “。”.
pub fn intro_of(name: &str, intro: &str, reveal: &Reveal, owner: &str) -> String {
    let mut bits: Vec<String> = Vec::new();
    if reveal.name {
        bits.push(format!("我是{name}"));
    }
    if reveal.role {
        bits.push(role_of(intro));
    }
    if reveal.ai {
        bits.push("我是 AI 助手".into());
    }
    if reveal.steve {
        bits.push("由 Steve 驱动".into());
    }
    if reveal.owner {
        bits.push(format!("归 {owner} 管理"));
    }
    if bits.is_empty() {
        return NOTHING_REVEALED.into();
    }
    let parts: Vec<&str> = bits
        .iter()
        .map(String::as_str)
        .filter(|s| !s.is_empty())
        .collect();
    format!("{}。", parts.join("，"))
}

/// `intro.replace(/^我是[^，,。]*[，,]?/, "").replace(/。$/, "")`.
fn role_of(intro: &str) -> String {
    let mut s = intro;
    if let Some(rest) = s.strip_prefix("我是") {
        let cut = rest.find(['，', ',', '。']).unwrap_or(rest.len());
        let rest = &rest[cut..];
        s = rest
            .strip_prefix('，')
            .or_else(|| rest.strip_prefix(','))
            .unwrap_or(rest);
    }
    s.strip_suffix('。').unwrap_or(s).to_string()
}

/// How the persona refers to the owner (`<主人称呼>`): the workspace owner's confirmed “偏好的称呼”
/// on their own contact card, else their user name.
pub fn owner_name(c: &Connection, ws: &str) -> ChannelResult<String> {
    let name: Option<String> = c
        .query_row(
            "SELECT coalesce(
                (SELECT av.value FROM parties p JOIN attr_values av ON av.party_id = p.id AND av.workspace_id = p.workspace_id
                   WHERE p.workspace_id = ?1 AND p.user_id = u.id AND av.key = 'call' AND av.status = 'confirmed' AND av.value <> ''
                   ORDER BY av.updated DESC LIMIT 1),
                u.name)
             FROM workspace_members m JOIN users u ON u.id = m.user_id
             WHERE m.workspace_id = ?1 ORDER BY (m.role = 'owner') DESC, m.created, u.id LIMIT 1",
            [ws],
            |r| r.get(0),
        )
        .optional()?;
    Ok(name.unwrap_or_else(|| "你".into()))
}

pub fn get(c: &Connection, id: &str) -> ChannelResult<Option<PersonaRow>> {
    Ok(c.query_row(
        &format!("SELECT {COLS} FROM personas WHERE id = ?"),
        [id],
        row,
    )
    .optional()?)
}

/// `own_persona(ws, id)`: a live persona of this workspace, else `404 没有这个对外身份`.
pub fn own(c: &Connection, ws: &str, id: &str) -> ChannelResult<PersonaRow> {
    get(c, id)?
        .filter(|p| p.workspace_id == ws && !p.deleted)
        .ok_or_else(|| ApiError::not_found("对外身份"))
}

/// Live personas of a workspace: the default first, then by creation.
pub fn list(c: &Connection, ws: &str) -> ChannelResult<Vec<PersonaRow>> {
    let mut st = c.prepare_cached(&format!(
        "SELECT {COLS} FROM personas WHERE workspace_id = ? AND deleted = 0 ORDER BY is_default DESC, created, id"
    ))?;
    let rows = st
        .query_map([ws], row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// The workspace's default persona, if seeded.
pub fn default_persona(c: &Connection, ws: &str) -> ChannelResult<Option<PersonaRow>> {
    Ok(c.query_row(
        &format!(
            "SELECT {COLS} FROM personas WHERE workspace_id = ? AND is_default = 1 AND deleted = 0 ORDER BY created LIMIT 1"
        ),
        [ws],
        row,
    )
    .optional()?)
}

/// The Bot behind the default persona: `steve` if it is in this workspace, else the main Bot, else
/// the first Bot of the workspace. With its speaking style (`bots.persona`) for the soul.
fn seed_bot(c: &Connection, ws: &str) -> ChannelResult<Option<(String, String)>> {
    Ok(c.query_row(
        "SELECT b.id, b.persona FROM bots b JOIN bot_workspaces w ON w.bot_id = b.id AND w.workspace_id = ?
          WHERE b.deleted = 0 ORDER BY (b.id = 'steve') DESC, b.main DESC, w.created, b.id LIMIT 1",
        [ws],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()?)
}

/// Seeds the default persona “Steve” on first use of channels. `None` when the workspace has
/// no Bot yet (nothing to put behind it); seeded on the next call then.
pub fn ensure_default(tx: &Tx, ws: &str, now: i64) -> ChannelResult<Option<PersonaRow>> {
    if let Some(p) = default_persona(tx, ws)? {
        return Ok(Some(p));
    }
    let Some((bot, soul)) = seed_bot(tx, ws)? else {
        return Ok(None);
    };
    let owner = owner_name(tx, ws)?;
    let id = uid("per");
    tx.execute(
        "INSERT INTO personas (id, workspace_id, name, avatar, bot_id, intro, soul, reveal, signature, is_default, created, updated)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, '', 1, ?, ?)",
        params![
            id,
            ws,
            DEFAULT_NAME,
            json!({"seed": DEFAULT_NAME}).to_string(),
            bot,
            format!("我是 Steve，{owner} 的 AI 助手，帮他接收和分派事情。"),
            soul,
            serde_json::to_string(&Reveal::ALL)?,
            now,
            now
        ],
    )?;
    steve_modules::contacts::stewards::adopt(tx, ws, "persona", &id, None, "seed")?;
    event(tx, ws, &id);
    get(tx, &id)
}

/// `ownBot`: the Bot is in this workspace and not deleted, else `404 没有这个 Bot`.
fn own_bot(c: &Connection, ws: &str, bot: &str) -> ChannelResult<()> {
    let ok: bool = c.query_row(
        "SELECT EXISTS (SELECT 1 FROM bots b JOIN bot_workspaces w ON w.bot_id = b.id AND w.workspace_id = ?1 WHERE b.id = ?2 AND b.deleted = 0)",
        params![ws, bot],
        |r| r.get(0),
    )?;
    if ok {
        Ok(())
    } else {
        Err(ApiError::not_found("Bot"))
    }
}

/// `POST /personas`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewPersona {
    pub name: String,
    pub bot_id: String,
    #[serde(default)]
    pub intro: Option<String>,
    #[serde(default)]
    pub soul: Option<String>,
    #[serde(default)]
    pub reveal: Option<RevealPatch>,
    #[serde(default)]
    pub signature: Option<String>,
}

/// `PATCH /personas/:id`: all optional.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersonaPatch {
    pub name: Option<String>,
    pub bot_id: Option<String>,
    pub intro: Option<String>,
    pub soul: Option<String>,
    pub reveal: Option<RevealPatch>,
    pub signature: Option<String>,
}

fn check_fields(
    name: Option<&str>,
    intro: Option<&str>,
    soul: Option<&str>,
    signature: Option<&str>,
) -> ChannelResult<()> {
    Check::new()
        .opt_len("name", name.map(str::trim), 1, 20)
        .opt_len("intro", intro, 0, 400)
        .opt_len("soul", soul, 0, 2000)
        .opt_len("signature", signature, 0, 60)
        .done()
}

fn snap(p: &PersonaRow) -> Value {
    json!({
        "name": p.name, "bot_id": p.bot_id, "intro": p.intro, "soul": p.soul,
        "reveal": p.reveal, "signature": p.signature,
    })
}

/// Creates a persona: avatar seeded by its name, reveal defaults to [`Reveal::NEW`]. Seeds the
/// default persona first, so a workspace's first persona never becomes the only one.
pub fn create(tx: &Tx, viewer: &Viewer, n: &NewPersona, now: i64) -> ChannelResult<PersonaRow> {
    check_fields(
        Some(&n.name),
        n.intro.as_deref(),
        n.soul.as_deref(),
        n.signature.as_deref(),
    )?;
    let ws = &viewer.ws;
    own_bot(tx, ws, &n.bot_id)?;
    ensure_default(tx, ws, now)?;
    let name = n.name.trim();
    let reveal = Reveal::NEW.apply(&n.reveal.clone().unwrap_or_default());
    let id = uid("per");
    tx.execute(
        "INSERT INTO personas (id, workspace_id, name, avatar, bot_id, intro, soul, reveal, signature, is_default, created, updated)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 0, ?, ?)",
        params![
            id,
            ws,
            name,
            json!({"seed": name}).to_string(),
            n.bot_id,
            n.intro.as_deref().unwrap_or("").trim(),
            n.soul.as_deref().unwrap_or("").trim(),
            serde_json::to_string(&reveal)?,
            n.signature.as_deref().unwrap_or("").trim(),
            now,
            now
        ],
    )?;
    let p = own(tx, ws, &id)?;
    // the person who creates a persona is its 负责人
    steve_modules::contacts::stewards::adopt(tx, ws, "persona", &id, Some(&viewer.user), "seed")?;
    base::revision(
        tx,
        ws,
        "persona",
        &id,
        1,
        &Value::Null,
        &snap(&p),
        &viewer.user,
    )?;
    base::audit(
        tx,
        ws,
        &viewer.user,
        "persona.create",
        &id,
        &json!({"bot_id": p.bot_id}),
    )?;
    event(tx, ws, &id);
    Ok(p)
}

/// Patches a persona: a revision, audit `persona.patch`, SSE `persona`.
pub fn patch(
    tx: &Tx,
    viewer: &Viewer,
    id: &str,
    p: &PersonaPatch,
    now: i64,
) -> ChannelResult<PersonaRow> {
    check_fields(
        p.name.as_deref(),
        p.intro.as_deref(),
        p.soul.as_deref(),
        p.signature.as_deref(),
    )?;
    let ws = &viewer.ws;
    let before = own(tx, ws, id)?;
    if let Some(b) = &p.bot_id {
        own_bot(tx, ws, b)?;
    }
    let mut after = before.clone();
    if let Some(v) = &p.name {
        after.name = v.trim().to_string();
    }
    if let Some(v) = &p.bot_id {
        after.bot_id = v.clone();
    }
    if let Some(v) = &p.intro {
        after.intro = v.trim().to_string();
    }
    if let Some(v) = &p.soul {
        after.soul = v.trim().to_string();
    }
    if let Some(v) = &p.reveal {
        after.reveal = after.reveal.apply(v);
    }
    if let Some(v) = &p.signature {
        after.signature = v.trim().to_string();
    }
    after.revision = before.revision + 1;
    after.updated = now;
    tx.execute(
        "UPDATE personas SET name = ?, bot_id = ?, intro = ?, soul = ?, reveal = ?, signature = ?, revision = ?, updated = ? WHERE id = ?",
        params![
            after.name,
            after.bot_id,
            after.intro,
            after.soul,
            serde_json::to_string(&after.reveal)?,
            after.signature,
            after.revision,
            now,
            id
        ],
    )?;
    base::revision(
        tx,
        ws,
        "persona",
        id,
        after.revision,
        &snap(&before),
        &snap(&after),
        &viewer.user,
    )?;
    let fields: Vec<&str> = [
        ("name", p.name.is_some()),
        ("bot_id", p.bot_id.is_some()),
        ("intro", p.intro.is_some()),
        ("soul", p.soul.is_some()),
        ("reveal", p.reveal.is_some()),
        ("signature", p.signature.is_some()),
    ]
    .into_iter()
    .filter_map(|(k, on)| on.then_some(k))
    .collect();
    base::audit(
        tx,
        ws,
        &viewer.user,
        "persona.patch",
        id,
        &json!({ "fields": fields }),
    )?;
    event(tx, ws, id);
    Ok(after)
}

/// What a deletion moved: account defaults switched to the default persona, space bindings dropped.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Moved {
    pub accounts: usize,
    pub spaces: usize,
}

/// Deletes a persona: the default one cannot be deleted; accounts using it as their default
/// switch to the default persona, spaces bound to it go back to following their account;
/// `deleted=1`.
pub fn delete(tx: &Tx, viewer: &Viewer, id: &str, now: i64) -> ChannelResult<Moved> {
    let ws = &viewer.ws;
    let p = own(tx, ws, id)?;
    if p.is_default {
        return Err(ApiError::rule("默认身份不能删除"));
    }
    let fallback = ensure_default(tx, ws, now)?.ok_or_else(|| ApiError::not_found("对外身份"))?;
    let moved = bindings::release(tx, ws, id, &fallback.id, now)?;
    tx.execute(
        "UPDATE personas SET deleted = 1, revision = revision + 1, updated = ? WHERE id = ?",
        params![now, id],
    )?;
    base::revision(
        tx,
        ws,
        "persona",
        id,
        p.revision + 1,
        &snap(&p),
        &json!({"deleted": true}),
        &viewer.user,
    )?;
    base::audit(
        tx,
        ws,
        &viewer.user,
        "persona.delete",
        id,
        &json!({"accounts": moved.accounts, "spaces": moved.spaces}),
    )?;
    event(tx, ws, id);
    Ok(moved)
}

/// The Bot behind personas was deleted (`deleteBot` in steve-core calls this with the heir): the
/// personas keep their face and get the heir as their brain. Returns how many changed.
pub fn on_bot_deleted(tx: &Tx, ws: &str, bot: &str, heir: &str, now: i64) -> ChannelResult<usize> {
    let mut st = tx.prepare_cached(&format!(
        "SELECT {COLS} FROM personas WHERE workspace_id = ? AND bot_id = ? AND deleted = 0 ORDER BY id"
    ))?;
    let rows = st
        .query_map(params![ws, bot], row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(st);
    for p in &rows {
        let id = &p.id;
        tx.execute(
            "UPDATE personas SET bot_id = ?, revision = revision + 1, updated = ? WHERE id = ?",
            params![heir, now, id],
        )?;
        let after = PersonaRow {
            bot_id: heir.into(),
            ..p.clone()
        };
        base::revision(
            tx,
            ws,
            "persona",
            id,
            p.revision + 1,
            &snap(p),
            &snap(&after),
            "steve",
        )?;
        base::audit(
            tx,
            ws,
            "steve",
            "persona.bot_moved",
            id,
            &json!({"from": bot, "to": heir}),
        )?;
        event(tx, ws, id);
    }
    Ok(rows.len())
}

/// SSE `{type:"persona", id, ws}`.
pub fn event(tx: &Tx, ws: &str, id: &str) {
    tx.publish(ChangeEvent::Persona {
        id: id.into(),
        ws: ws.into(),
    });
}

// ---------------------------------------------------------------- output

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UsedAccount {
    pub account_id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UsedSpace {
    pub id: String,
    pub name: String,
    /// Bound to this persona on its own (not inherited from the account).
    pub own: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct UsedBy {
    pub accounts: Vec<UsedAccount>,
    pub spaces: Vec<UsedSpace>,
}

/// The `Persona` shape of §5.1.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PersonaOut {
    pub id: String,
    pub name: String,
    pub avatar: Value,
    pub bot_id: String,
    pub intro: String,
    pub soul: String,
    pub reveal: Reveal,
    pub signature: String,
    pub is_default: bool,
    pub intro_preview: String,
    pub used_by: UsedBy,
    pub revision: i64,
}

pub fn out(c: &Connection, viewer: &Viewer, p: &PersonaRow) -> ChannelResult<PersonaOut> {
    let owner = owner_name(c, &p.workspace_id)?;
    Ok(PersonaOut {
        id: p.id.clone(),
        name: p.name.clone(),
        avatar: p.avatar.clone(),
        bot_id: p.bot_id.clone(),
        intro: p.intro.clone(),
        soul: p.soul.clone(),
        reveal: p.reveal,
        signature: p.signature.clone(),
        is_default: p.is_default,
        intro_preview: intro_of(&p.name, &p.intro, &p.reveal, &owner),
        used_by: bindings::used_by(c, viewer, p)?,
        revision: p.revision,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(name: bool, role: bool, ai: bool, steve: bool, owner: bool) -> Reveal {
        Reveal {
            name,
            role,
            ai,
            steve,
            owner,
        }
    }

    #[test]
    fn intro_of_matches_the_prototype() {
        let oncall = "我是值班小运，负责运维群的告警排查和部署答疑。";
        assert_eq!(
            intro_of("值班小运", oncall, &Reveal::NEW, "Alice"),
            "我是值班小运，负责运维群的告警排查和部署答疑，我是 AI 助手。"
        );
        assert_eq!(
            intro_of(
                "Steve",
                "我是 Steve，Alice 的 AI 助手，帮他接收和分派事情。",
                &Reveal::ALL,
                "Alice"
            ),
            "我是Steve，Alice 的 AI 助手，帮他接收和分派事情，我是 AI 助手，由 Steve 驱动，归 Alice 管理。"
        );
        assert_eq!(
            intro_of(
                "需求官",
                "我是需求官，帮大家把想法整理成需求。",
                &r(false, true, false, false, true),
                "Alice"
            ),
            "帮大家把想法整理成需求，归 Alice 管理。"
        );
        assert_eq!(
            intro_of("x", oncall, &r(false, false, false, false, false), "P"),
            NOTHING_REVEALED
        );
        // an intro without the “我是…” prefix is kept whole, minus its final “。”
        assert_eq!(
            intro_of(
                "小助理",
                "回答常见问题。",
                &r(false, true, false, false, false),
                "P"
            ),
            "回答常见问题。"
        );
        // ASCII comma, and an intro that is only the name
        assert_eq!(role_of("我是小运,负责值班"), "负责值班");
        assert_eq!(role_of("我是小运。"), "");
        // an empty role is skipped, not joined as an empty part
        assert_eq!(
            intro_of(
                "小运",
                "我是小运。",
                &r(true, true, true, false, false),
                "P"
            ),
            "我是小运，我是 AI 助手。"
        );
        assert_eq!(
            intro_of("小运", "", &r(false, true, false, false, false), "P"),
            "。"
        );
    }

    #[test]
    fn reveal_reads_missing_keys_as_true() {
        let r: Reveal = serde_json::from_str(r#"{"steve":false}"#).expect("reveal");
        assert_eq!(
            r,
            Reveal {
                steve: false,
                ..Reveal::ALL
            }
        );
    }
}
