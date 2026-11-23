//! External personas: the “face” a Bot shows in channels. Every
//! workspace gets a default persona “Steve” the first time it uses channels; it cannot be deleted
//! and is the last fallback of every bot account (see [`crate::bindings`]).
//!
//! Personas are workspace objects: every member of the workspace sees them.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use steve_modules::{ApiError, Check};
use steve_store::base::Viewer;
use steve_store::channel_personas as repo;
pub use steve_store::channel_personas::{
    Persona as PersonaRow, Reveal, default_persona, get, list,
};
use steve_store::seeded;
use steve_store::{BotRows, Read, W};

use crate::ChannelResult;
use crate::bindings;

/// Name of the default persona.
pub const DEFAULT_NAME: &str = "Steve";
/// `introOf` when nothing is revealed.
pub const NOTHING_REVEALED: &str = "（不透露身份，只回答问题本身）";

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

impl RevealPatch {
    fn apply(&self, mut reveal: Reveal) -> Reveal {
        reveal.name = self.name.unwrap_or(reveal.name);
        reveal.role = self.role.unwrap_or(reveal.role);
        reveal.ai = self.ai.unwrap_or(reveal.ai);
        reveal.steve = self.steve.unwrap_or(reveal.steve);
        reveal.owner = self.owner.unwrap_or(reveal.owner);
        reveal
    }
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

/// The external owner name, never the private nickname used when talking to the owner.
pub fn owner_name(r: &mut impl steve_store::Read, ws: &str) -> ChannelResult<String> {
    owner_name_in(r, ws, None, None)
}

/// External acknowledgments and handoffs use the same effective persona/space scope as drafts.
pub fn owner_name_for_space(r: &mut impl Read, ws: &str, space: &str) -> ChannelResult<String> {
    let persona = match steve_store::channel_spaces::rows::get_in_workspace(r, ws, space)? {
        Some(row) => bindings::effective(r, &row)?,
        None => None,
    };
    owner_name_in(
        r,
        ws,
        persona.as_ref().map(|e| e.persona.id.as_str()),
        Some(space),
    )
}

pub fn owner_name_in(
    r: &mut impl steve_store::Read,
    ws: &str,
    persona: Option<&str>,
    space: Option<&str>,
) -> ChannelResult<String> {
    owner_address(r, ws, true, persona, space)
}

/// Only for a conversation already verified to contain the owner alone.
pub fn owner_internal_name(
    r: &mut impl steve_store::Read,
    ws: &str,
    connection: &str,
) -> ChannelResult<String> {
    let user = crate::compose::owner_user(r, ws, connection)?;
    Ok(
        steve_modules::contacts::profile::address(r, ws, &user, false, None, None)?
            .unwrap_or_else(|| "你".into()),
    )
}

fn owner_address(
    r: &mut impl steve_store::Read,
    ws: &str,
    external: bool,
    persona: Option<&str>,
    space: Option<&str>,
) -> ChannelResult<String> {
    let Some(id) = steve_store::Members::new(r).first(ws)? else {
        return Ok("你".into());
    };
    let name = steve_store::UserRows(r)
        .name(&id)?
        .unwrap_or_else(|| "你".into());
    Ok(
        steve_modules::contacts::profile::address(r, ws, &id, external, persona, space)?
            .unwrap_or(name),
    )
}

/// A live persona of this workspace, else `404 没有这个对外身份`.
pub fn own(r: &mut impl Read, ws: &str, id: &str) -> ChannelResult<PersonaRow> {
    get(r, id)?
        .filter(|p| p.workspace_id == ws && !p.deleted)
        .ok_or_else(|| ApiError::not_found("对外身份"))
}

/// Seeds the default persona “Steve” on first use of channels. `None` when the workspace has
/// no Bot yet (nothing to put behind it); seeded on the next call then.
pub fn ensure_default(w: &mut W<'_>, ws: &str, now: i64) -> ChannelResult<Option<PersonaRow>> {
    if let Some(p) = default_persona(w, ws)? {
        return Ok(Some(p));
    }
    let Some((bot, soul)) = BotRows(w).persona_seed(ws)? else {
        return Ok(None);
    };
    let p = repo::create(
        w,
        &repo::NewPersona {
            workspace: ws,
            name: DEFAULT_NAME,
            bot: &bot,
            intro: &seeded::persona_intro(),
            soul: &soul,
            reveal: Reveal::ALL,
            signature: "",
            is_default: true,
            now,
        },
    )?;
    steve_modules::contacts::stewards::adopt(w, ws, "persona", &p.id, None, "seed")?;
    event(w, ws, &p.id);
    Ok(Some(p))
}

/// The Bot is in this workspace and live.
fn own_bot(r: &mut impl Read, ws: &str, bot: &str) -> ChannelResult<()> {
    if BotRows(r).local(ws, bot)?.is_none() {
        return Err(ApiError::not_found("Bot"));
    }
    Ok(())
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

/// Creates a persona: avatar seeded by its name, reveal defaults to [`Reveal::NEW`]. Seeds the
/// default persona first, so a workspace's first persona never becomes the only one.
pub fn create(
    w: &mut W<'_>,
    viewer: &Viewer,
    n: &NewPersona,
    now: i64,
) -> ChannelResult<PersonaRow> {
    check_fields(
        Some(&n.name),
        n.intro.as_deref(),
        n.soul.as_deref(),
        n.signature.as_deref(),
    )?;
    let ws = &viewer.ws;
    own_bot(w, ws, &n.bot_id)?;
    ensure_default(w, ws, now)?;
    let name = n.name.trim();
    let reveal = n
        .reveal
        .as_ref()
        .map_or(Reveal::NEW, |patch| patch.apply(Reveal::NEW));
    let p = repo::create(
        w,
        &repo::NewPersona {
            workspace: ws,
            name,
            bot: &n.bot_id,
            intro: n.intro.as_deref().unwrap_or("").trim(),
            soul: n.soul.as_deref().unwrap_or("").trim(),
            reveal,
            signature: n.signature.as_deref().unwrap_or("").trim(),
            is_default: false,
            now,
        },
    )?;
    let id = &p.id;
    // The person who creates a persona is its steward.
    steve_modules::contacts::stewards::adopt(w, ws, "persona", id, Some(&viewer.user), "seed")?;
    steve_store::audit_repo::revise(
        w,
        ws,
        "persona",
        id,
        1,
        &Value::Null,
        &p.snapshot(),
        &viewer.user,
    )?;
    steve_store::audit_repo::record(
        w,
        ws,
        &viewer.user,
        "persona.create",
        id,
        &json!({"bot_id": p.bot_id}),
    )?;
    event(w, ws, id);
    Ok(p)
}

/// Patches a persona: a revision, audit `persona.patch`, SSE `persona`.
pub fn patch(
    w: &mut W<'_>,
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
    let before = own(w, ws, id)?;
    if let Some(b) = &p.bot_id {
        own_bot(w, ws, b)?;
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
        after.reveal = v.apply(after.reveal);
    }
    if let Some(v) = &p.signature {
        after.signature = v.trim().to_string();
    }
    after.revision = before.revision + 1;
    after.updated = now;
    repo::update(w, &after)?;
    steve_store::audit_repo::revise(
        w,
        ws,
        "persona",
        id,
        after.revision,
        &before.snapshot(),
        &after.snapshot(),
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
    steve_store::audit_repo::record(
        w,
        ws,
        &viewer.user,
        "persona.patch",
        id,
        &json!({ "fields": fields }),
    )?;
    event(w, ws, id);
    Ok(after)
}

pub use steve_store::channel_personas::bindings::Moved;

/// Deletes a persona: the default one cannot be deleted; accounts using it as their default
/// switch to the default persona, spaces bound to it go back to following their account;
/// `deleted=1`.
pub fn delete(w: &mut W<'_>, viewer: &Viewer, id: &str, now: i64) -> ChannelResult<Moved> {
    let ws = &viewer.ws;
    let p = own(w, ws, id)?;
    if p.is_default {
        return Err(ApiError::rule(steve_i18n::t!(
            "channels.error.default_persona_kept"
        )));
    }
    let fallback = ensure_default(w, ws, now)?.ok_or_else(|| ApiError::not_found("对外身份"))?;
    let moved = repo::bindings::release(w, ws, id, &fallback.id, now)?;
    repo::retire(w, id, now)?;
    steve_store::audit_repo::revise(
        w,
        ws,
        "persona",
        id,
        p.revision + 1,
        &p.snapshot(),
        &json!({"deleted": true}),
        &viewer.user,
    )?;
    steve_store::audit_repo::record(
        w,
        ws,
        &viewer.user,
        "persona.delete",
        id,
        &json!({"accounts": moved.accounts, "spaces": moved.spaces}),
    )?;
    event(w, ws, id);
    Ok(moved)
}

/// SSE `{type:"persona", id, ws}`.
pub fn event(w: &mut W<'_>, ws: &str, id: &str) {
    w.emit(steve_types::DomainEvent::from(
        steve_types::ChannelsEvent::Persona {
            id: id.into(),
            ws: ws.into(),
        },
    ));
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

/// The `Persona` shape.
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

/// A persona's `field` as shown: still as seeded, in the request's language ([`steve_store::seeded`]).
fn shown(v: &str, field: &str) -> String {
    let keys = seeded::PERSONA
        .iter()
        .find(|(f, _)| *f == field)
        .map_or(&[][..], |(_, k)| *k);
    seeded::shown(v, keys).unwrap_or_else(|| v.to_owned())
}

/// A persona's `field` as a page sent it: one that is a seed shown in another language is stored as seeded.
pub(crate) fn sent(v: Option<String>, field: &str) -> Option<String> {
    let keys = seeded::PERSONA
        .iter()
        .find(|(f, _)| *f == field)
        .map_or(&[][..], |(_, k)| *k);
    v.map(|v| seeded::stored(&v, keys).unwrap_or(v))
}

pub fn out(
    r: &mut impl steve_store::Read,
    viewer: &Viewer,
    p: &PersonaRow,
) -> ChannelResult<PersonaOut> {
    let owner = owner_name_in(r, &p.workspace_id, Some(&p.id), None)?;
    Ok(PersonaOut {
        id: p.id.clone(),
        name: p.name.clone(),
        avatar: p.avatar.clone(),
        bot_id: p.bot_id.clone(),
        // seeded text in the reader's language; the preview is what recipients are sent
        intro: shown(&p.intro, "intro"),
        soul: shown(&p.soul, "soul"),
        reveal: p.reveal,
        signature: p.signature.clone(),
        is_default: p.is_default,
        intro_preview: intro_of(&p.name, &p.intro, &p.reveal, &owner),
        used_by: bindings::used_by(r, viewer, p)?,
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
