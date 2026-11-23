//! The library's skills: Markdown instructions (`SKILL.md`: a `name`, a `description` of when to
//! use it, then the body) the harness of a Bot assigned one reads where the harness adapter declares
//! it reads skills (`steve_store::caps::skills`). Which harnesses read skills, and where, is the
//! adapter's data (`AdapterProperties::skills`), never decided here.
//!
//! - `GET /capabilities/skills` → `SkillOut[]` (each with `bots: [{id, enabled}]`)
//! - `POST /capabilities/skills {name, description, content?, bot?}` → `SkillOut`
//! - `PATCH /capabilities/skills/:id {…any of those}` → `SkillOut`
//! - `DELETE /capabilities/skills/:id` → `{ok:true}`
//!
//! A name is 1–64 of `a-z`, `0-9` and single `-` between them, once per workspace; a description is
//! one line of at most 1024 characters; a body at most 32 KiB, and a Bot's switched-on skills at
//! most 128 KiB together (what a session's Node is given in one message). Every change is audited
//! (`skill.create|update|delete`, names only) and publishes the change event of every Bot using it.

use std::sync::LazyLock;

use axum::Json;
use axum::extract::{Path, State};
use regex::Regex;
use serde::Deserialize;
use serde_json::{Value, json};
use steve_store::caps::Assignments;
use steve_store::caps::skills::{Skill, Skills};
use steve_store::{Read, now, uid};

use super::Capabilities;
use crate::vault::VaultResult;
use crate::{ApiError, ApiResult, Body, Caller, Check};

/// Bytes of one skill's body, and of a Bot's enabled skills together.
pub const MAX_CONTENT: usize = 32 * 1024;
pub const MAX_TOTAL: usize = 128 * 1024;
/// Characters of a description.
pub const MAX_DESCRIPTION: usize = 1024;

static NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-z0-9]+(-[a-z0-9]+)*$").unwrap());

/// A new skill, or the fields a `PATCH` changes.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SkillIn {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
    /// On create: also assign it to this Bot.
    #[serde(default)]
    pub bot: Option<String>,
}

pub(super) async fn list_h(
    State(s): State<Capabilities>,
    who: Caller,
) -> VaultResult<Json<Vec<Value>>> {
    let svc = s.clone();
    Ok(Json(
        s.vault
            .blocking(move |_| Ok(svc.skills_list(&who.ws)?))
            .await?,
    ))
}

pub(super) async fn create_h(
    State(s): State<Capabilities>,
    who: Caller,
    Body(b): Body<SkillIn>,
) -> VaultResult<Json<Value>> {
    let svc = s.clone();
    Ok(Json(
        s.vault
            .blocking(move |_| Ok(svc.skill_create(&who.ws, &who.me, b)?))
            .await?,
    ))
}

pub(super) async fn update_h(
    State(s): State<Capabilities>,
    who: Caller,
    Path(sid): Path<String>,
    Body(b): Body<SkillIn>,
) -> VaultResult<Json<Value>> {
    let svc = s.clone();
    Ok(Json(
        s.vault
            .blocking(move |_| Ok(svc.skill_update(&who.ws, &who.me, &sid, b)?))
            .await?,
    ))
}

pub(super) async fn remove_h(
    State(s): State<Capabilities>,
    who: Caller,
    Path(sid): Path<String>,
) -> VaultResult<Json<Value>> {
    let svc = s.clone();
    s.vault
        .blocking(move |_| Ok(svc.skill_delete(&who.ws, &who.me, &sid)?))
        .await?;
    Ok(Json(json!({ "ok": true })))
}

fn skill_of(c: &mut impl Read, ws: &str, sid: &str) -> ApiResult<Skill> {
    Skills(c)
        .get(sid)?
        .filter(|s| s.workspace_id == ws)
        .ok_or_else(|| ApiError::not_found("技能"))
}

pub(crate) fn out(s: &Skill) -> Value {
    json!({
        "id": s.id, "name": s.name, "description": s.description,
        "content": s.content, "created": s.created, "updated": s.updated,
    })
}

fn check(s: &Skill) -> ApiResult<()> {
    let mut c = Check::new();
    c.that("name", s.name.len() <= 64 && NAME.is_match(&s.name));
    c.that(
        "description",
        !s.description.is_empty()
            && s.description.chars().count() <= MAX_DESCRIPTION
            && !s.description.contains(['\n', '\r', '\0']),
    );
    c.that(
        "content",
        s.content.len() <= MAX_CONTENT && !s.content.contains('\0'),
    );
    c.done()
}

fn out_with_users(c: &mut impl Read, s: &Skill) -> ApiResult<Value> {
    let mut v = out(s);
    v["bots"] = super::users(c, "skill", &s.id)?;
    Ok(v)
}

/// Room in `bot`'s switched-on skills for `entry` switched on, its body `content` when given (else
/// as stored): `Err` when they would hold more than [`MAX_TOTAL`].
pub(crate) fn room_for(
    c: &mut impl Read,
    bot: &str,
    entry: &str,
    content: Option<usize>,
) -> ApiResult<()> {
    let mut total = 0;
    for (s, on) in Skills(c).of_bot(bot)? {
        if on && s.id != entry {
            total += s.content.len();
        }
    }
    let own = match content {
        Some(n) => n,
        None => Skills(c).get(entry)?.map_or(0, |s| s.content.len()),
    };
    if total + own > MAX_TOTAL {
        return Err(ApiError::rule(steve_i18n::t!(
            "modules.caps.skills_too_large",
            kib = MAX_TOTAL / 1024
        )));
    }
    Ok(())
}

impl Capabilities {
    pub fn skills_list(&self, ws: &str) -> ApiResult<Vec<Value>> {
        self.read(|c| {
            Skills(c)
                .list(ws)?
                .iter()
                .map(|s| out_with_users(c, s))
                .collect()
        })
    }

    pub fn skill_create(&self, ws: &str, me: &str, b: SkillIn) -> ApiResult<Value> {
        let at = now();
        let blank = Skill {
            id: uid("bskill"),
            workspace_id: ws.into(),
            name: String::new(),
            description: String::new(),
            content: String::new(),
            created: at,
            updated: at,
        };
        if b.name.is_none() || b.description.is_none() {
            let mut c = Check::new();
            c.that("name", b.name.is_some());
            c.that("description", b.description.is_some());
            c.done()?;
        }
        self.skill_save(ws, me, blank, b, true)
    }

    pub fn skill_update(&self, ws: &str, me: &str, sid: &str, b: SkillIn) -> ApiResult<Value> {
        let old = self.read(|c| skill_of(c, ws, sid))?;
        self.skill_save(ws, me, old, b, false)
    }

    pub fn skill_delete(&self, ws: &str, me: &str, sid: &str) -> ApiResult<()> {
        self.vault.store().write(|tx| {
            let old = skill_of(tx, ws, sid)?;
            super::publish_users(tx, "skill", sid)?;
            Skills(tx).delete(sid)?;
            tx.audit(ws, me, "skill.delete", sid, &json!({"name": old.name}))?;
            Ok(())
        })
    }

    /// Merges `b` into `old`, checks it and writes it in one transaction.
    fn skill_save(
        &self,
        ws: &str,
        me: &str,
        old: Skill,
        b: SkillIn,
        fresh: bool,
    ) -> ApiResult<Value> {
        let assign_to = b.bot.clone();
        let mut s = old.clone();
        if let Some(n) = b.name {
            s.name = n.trim().to_string();
        }
        if let Some(d) = b.description {
            s.description = d.trim().to_string();
        }
        if let Some(c) = b.content {
            s.content = c;
        }
        check(&s)?;
        s.updated = now();
        self.vault.store().write(|tx| {
            if fresh && Skills(tx).list(ws)?.len() >= super::MAX_LIBRARY {
                return Err(ApiError::rule(steve_i18n::t!(
                    "modules.caps.library_full",
                    max = super::MAX_LIBRARY
                )));
            }
            if Skills(tx).name_taken(ws, &s.name, Some(&s.id))? {
                return Err(ApiError::conflict(steve_i18n::t!(
                    "modules.caps.skill_name_taken",
                    name = s.name
                )));
            }
            for a in Assignments(tx).of_entry("skill", &s.id)? {
                if super::clashes(tx, &a.bot_id, "skill", &s.id, &s.name)? {
                    return Err(ApiError::conflict(steve_i18n::t!(
                        "modules.caps.name_used",
                        name = s.name
                    )));
                }
                // a body that grows must still fit every Bot that has it on
                if a.enabled && s.content.len() > old.content.len() {
                    room_for(tx, &a.bot_id, &s.id, Some(s.content.len()))?;
                }
            }
            if fresh {
                Skills(tx).insert(&s)?;
            } else {
                Skills(tx).update(&s)?;
            }
            tx.audit(
                ws,
                me,
                if fresh {
                    "skill.create"
                } else {
                    "skill.update"
                },
                &s.id,
                &json!({"name": s.name}),
            )?;
            if let (true, Some(bot)) = (fresh, assign_to.as_deref()) {
                super::assign_in(tx, ws, me, bot, "skill", &s.id, true)?;
            }
            super::publish_users(tx, "skill", &s.id)?;
            out_with_users(tx, &s)
        })
    }
}
