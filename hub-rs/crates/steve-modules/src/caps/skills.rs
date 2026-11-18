//! The library's skills: Markdown instructions (`SKILL.md`: a `name`, a `description` of when to
//! use it, then the body) the harness of a Bot assigned one reads where the harness adapter declares
//! it reads skills (`steve_store::caps::skills`). Which harnesses read skills, and where, is the
//! adapter's data (`AdapterProperties::skills`), never decided here.
//!
//! - `GET /capabilities/skills` → `SkillOut[]` (each with `bots: [{id, enabled}]`)
//! - `POST /capabilities/skills {name, description, content?, bot?}` → `SkillOut`
//! - `PATCH /capabilities/skills/:id {base_version, name?, description?, content?, note?}` saves a draft
//! - `GET /capabilities/skills/:id/versions[/:version]` lists versions or reads one
//! - `PUT /capabilities/skills/:id/publication {version, publication, rollback?}` switches the published version
//! - `DELETE /capabilities/skills/:id` → `{ok:true}`
//!
//! A name is 1–64 of `a-z`, `0-9` and single `-` between them, once per workspace; a description is
//! one line of at most 1024 characters; a body at most 32 KiB, and a Bot's switched-on published skills at
//! most 128 KiB together (what a session's Node is given in one message). Every change is audited
//! (`skill.create|update|publish|rollback|delete`, no Markdown) and publishes the change event of every Bot using it.

use std::sync::LazyLock;

use axum::Json;
use axum::extract::{Path, State};
use regex::Regex;
use rusqlite::{Connection, OptionalExtension};
use serde::Deserialize;
use serde_json::{Value, json};
use steve_store::caps::skills::{self as repo, Skill};
use steve_store::{base, now, uid};

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
    pub base_version: Option<i64>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PublishIn {
    pub version: i64,
    pub publication: i64,
    #[serde(default)]
    pub rollback: bool,
}

pub(super) async fn versions_h(
    State(s): State<Capabilities>,
    who: Caller,
    Path(id): Path<String>,
) -> VaultResult<Json<Value>> {
    let svc = s.clone();
    s.vault
        .blocking(move |_| Ok(svc.skill_versions(&who.ws, &id)?))
        .await
        .map(Json)
}

pub(super) async fn version_h(
    State(s): State<Capabilities>,
    who: Caller,
    Path((id, n)): Path<(String, i64)>,
) -> VaultResult<Json<repo::Version>> {
    let svc = s.clone();
    s.vault
        .blocking(move |_| Ok(svc.skill_version(&who.ws, &id, n)?))
        .await
        .map(Json)
}

pub(super) async fn publish_h(
    State(s): State<Capabilities>,
    who: Caller,
    Path(id): Path<String>,
    Body(b): Body<PublishIn>,
) -> VaultResult<Json<Value>> {
    let svc = s.clone();
    s.vault
        .blocking(move |_| Ok(svc.skill_publish(&who.ws, &who.me, &id, b)?))
        .await
        .map(Json)
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

fn skill_of(c: &Connection, ws: &str, sid: &str) -> ApiResult<Skill> {
    repo::get(c, sid)?
        .filter(|s| s.workspace_id == ws)
        .ok_or_else(|| ApiError::not_found("技能"))
}

pub(crate) fn out(c: &Connection, s: &Skill) -> ApiResult<Value> {
    let h = repo::history(c, &s.id)?;
    Ok(json!({
        "id": s.id, "name": s.name, "description": s.description,
        "content": s.content, "created": s.created, "updated": s.updated,
        "head_version": h.head_version, "published_version": h.published_version,
        "publication": h.publication,
    }))
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

fn out_with_users(c: &Connection, s: &Skill) -> ApiResult<Value> {
    let mut v = out(c, s)?;
    v["bots"] = super::users(c, "skill", &s.id)?;
    Ok(v)
}

/// Room in `bot`'s switched-on skills for `entry` switched on, its body `content` when given (else
/// as stored): `Err` when they would hold more than [`MAX_TOTAL`].
pub(crate) fn room_for(
    c: &Connection,
    bot: &str,
    entry: &str,
    content: Option<usize>,
) -> ApiResult<()> {
    let ws = repo::get(c, entry)?.map_or(String::new(), |s| s.workspace_id);
    let total: usize = repo::published_for_bot(c, &ws, bot)?
        .into_iter()
        .filter(|s| s.id != entry)
        .map(|s| s.version.content.len())
        .sum();
    let own = match content {
        Some(n) => n,
        None => repo::published(c, entry)?.map_or(0, |v| v.content.len()),
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
        self.vault.store().reader(|c| {
            repo::list(c, ws)?
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
        self.skill_save(ws, me, Some(blank), "", b)
    }

    pub fn skill_update(&self, ws: &str, me: &str, sid: &str, b: SkillIn) -> ApiResult<Value> {
        if b.base_version.is_none() {
            return Err(ApiError::invalid("base_version"));
        }
        self.skill_save(ws, me, None, sid, b)
    }

    pub fn skill_versions(&self, ws: &str, sid: &str) -> ApiResult<Value> {
        self.vault.store().reader(|c| {
            skill_of(c, ws, sid)?;
            Ok(json!(repo::version_summaries(c, sid)?))
        })
    }

    pub fn skill_version(&self, ws: &str, sid: &str, n: i64) -> ApiResult<repo::Version> {
        self.vault.store().reader(|c| {
            skill_of(c, ws, sid)?;
            repo::version(c, sid, n)?.ok_or_else(version_missing)
        })
    }

    pub fn skill_publish(&self, ws: &str, me: &str, sid: &str, b: PublishIn) -> ApiResult<Value> {
        self.vault.store().transaction(|tx| {
            writable(tx, ws, me)?;
            let s = skill_of(tx, ws, sid)?;
            let h = repo::history(tx, sid)?;
            if h.publication != b.publication {
                return Err(changed());
            }
            let v = repo::version(tx, sid, b.version)?.ok_or_else(version_missing)?;
            if b.rollback && v.first_published_at.is_none() {
                return Err(ApiError::rule(steve_i18n::t!(
                    "modules.caps.skill_not_published"
                )));
            }
            if h.published_version == Some(b.version) {
                return out_with_users(tx, &s);
            }
            ensure_name(tx, ws, sid, &v.name)?;
            for a in steve_store::caps::of_entry(tx, "skill", sid)? {
                if a.enabled {
                    room_for(tx, &a.bot_id, sid, Some(v.content.len()))?;
                }
            }
            let at = now();
            repo::publish(tx, sid, b.version, at)?;
            repo::prune(tx, at)?;
            base::audit(
                tx,
                ws,
                me,
                if b.rollback {
                    "skill.rollback"
                } else {
                    "skill.publish"
                },
                sid,
                &json!({"name":v.name, "from":h.published_version, "version":b.version}),
            )?;
            super::publish_users(tx, "skill", sid)?;
            out_with_users(tx, &s)
        })
    }

    pub fn skill_delete(&self, ws: &str, me: &str, sid: &str) -> ApiResult<()> {
        self.vault.store().transaction(|tx| {
            writable(tx, ws, me)?;
            let old = skill_of(tx, ws, sid)?;
            super::publish_users(tx, "skill", sid)?;
            repo::delete(tx, sid)?;
            base::audit(tx, ws, me, "skill.delete", sid, &json!({"name": old.name}))?;
            Ok(())
        })
    }

    fn skill_save(
        &self,
        ws: &str,
        me: &str,
        new: Option<Skill>,
        sid: &str,
        b: SkillIn,
    ) -> ApiResult<Value> {
        let note = b.note.as_deref().unwrap_or("").trim();
        if note.chars().count() > 500 || note.contains('\0') {
            return Err(ApiError::invalid("note"));
        }
        self.vault.store().transaction(|tx| {
            writable(tx, ws, me)?;
            let fresh = new.is_some();
            let (mut s, n) = match new {
                Some(s) => (s, 1),
                None => {
                    let s = skill_of(tx, ws, sid)?;
                    let h = repo::history(tx, sid)?;
                    if b.base_version != Some(h.head_version) {
                        return Err(changed());
                    }
                    (s, h.head_version + 1)
                }
            };
            if let Some(name) = b.name {
                s.name = name.trim().to_string();
            }
            if let Some(d) = b.description {
                s.description = d.trim().to_string();
            }
            if let Some(c) = b.content {
                s.content = c;
            }
            check(&s)?;
            s.updated = now();
            if fresh && repo::list(tx, ws)?.len() >= super::MAX_LIBRARY {
                return Err(ApiError::rule(steve_i18n::t!(
                    "modules.caps.library_full",
                    max = super::MAX_LIBRARY
                )));
            }
            ensure_name(tx, ws, &s.id, &s.name)?;
            if fresh {
                repo::insert(tx, &s)?;
            } else {
                repo::update(tx, &s)?;
            }
            repo::save_version(tx, &s, n, me, note)?;
            repo::prune(tx, s.updated)?;
            base::audit(
                tx,
                ws,
                me,
                if fresh {
                    "skill.create"
                } else {
                    "skill.update"
                },
                &s.id,
                &json!({"name":s.name, "version":n}),
            )?;
            if fresh && let Some(bot) = &b.bot {
                super::assign_in(tx, ws, me, bot, "skill", &s.id, true)?;
            }
            super::publish_users(tx, "skill", &s.id)?;
            out_with_users(tx, &s)
        })
    }
}

fn writable(c: &Connection, ws: &str, me: &str) -> ApiResult<()> {
    let member = c
        .query_row(
            "SELECT 1 FROM workspace_members WHERE workspace_id=? AND user_id=?",
            [ws, me],
            |_| Ok(()),
        )
        .optional()?;
    member.ok_or_else(|| ApiError::forbidden(steve_i18n::t!("modules.caps.skill_write_denied")))
}

fn ensure_name(c: &Connection, ws: &str, id: &str, name: &str) -> ApiResult<()> {
    if repo::name_taken(c, ws, name, Some(id))? {
        return Err(ApiError::conflict(steve_i18n::t!(
            "modules.caps.skill_name_taken",
            name = name
        )));
    }
    Ok(())
}

fn changed() -> ApiError {
    ApiError::conflict(steve_i18n::t!("modules.caps.skill_changed"))
}

fn version_missing() -> ApiError {
    ApiError::not_found(steve_i18n::t!("modules.caps.skill_version"))
}
