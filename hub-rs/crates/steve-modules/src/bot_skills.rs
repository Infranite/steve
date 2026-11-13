//! A Bot's skills: Markdown instructions (`SKILL.md`: a `name`, a `description` of when to use it,
//! then the body) its harness reads where the harness adapter declares it reads skills (migration
//! 63, `steve_store::bot_skills`). Which harnesses read skills, and where, is the adapter's data
//! (`AdapterProperties::skills`), never decided here.
//!
//! - `GET /bots/:id/skills` → `SkillOut[]`
//! - `POST /bots/:id/skills {name, description, content?, enabled?}` → `SkillOut`
//! - `PATCH /bots/:id/skills/:sid {…any of those}` → `SkillOut` (`{enabled}` alone switches it)
//! - `DELETE /bots/:id/skills/:sid` → `{ok:true}`
//!
//! A name is 1–64 of `a-z`, `0-9` and single `-` between them, once per Bot; a description is one
//! line of at most 1024 characters; a body at most 32 KiB, and a Bot's enabled skills at most
//! 128 KiB together (what a session's Node is given in one message). Every change is audited
//! (`bot_skill.create|update|enable|disable|delete`, names only) and publishes the Bot's change
//! event. A change reaches a session the next time its harness opens.

use std::sync::LazyLock;

use axum::extract::{Path, State};
use axum::routing::{get, patch};
use axum::{Json, Router};
use regex::Regex;
use rusqlite::{Connection, OptionalExtension};
use serde::Deserialize;
use serde_json::{Value, json};
use steve_store::bot_skills::{self as repo, Skill};
use steve_store::{ChangeEvent, Store, base, now, uid};

use crate::{ApiError, ApiResult, Body, Caller, Check};

/// Skills per Bot.
pub const MAX_SKILLS: usize = 32;
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
    #[serde(default)]
    pub enabled: Option<bool>,
}

#[derive(Clone)]
pub struct BotSkills {
    store: Store,
}

pub fn routes<S: Clone + Send + Sync + 'static>(svc: BotSkills) -> Router<S> {
    Router::new()
        .route("/bots/{id}/skills", get(list).post(create))
        .route("/bots/{id}/skills/{sid}", patch(update).delete(remove))
        .with_state(svc)
}

async fn blocking<R: Send + 'static>(
    f: impl FnOnce() -> ApiResult<R> + Send + 'static,
) -> ApiResult<R> {
    tokio::task::spawn_blocking(steve_i18n::carry(f))
        .await
        .map_err(|e| ApiError::Internal(format!("task failed: {e}")))?
}

async fn list(
    State(s): State<BotSkills>,
    who: Caller,
    Path(id): Path<String>,
) -> ApiResult<Json<Vec<Value>>> {
    blocking(move || s.list(&who.ws, &id)).await.map(Json)
}

async fn create(
    State(s): State<BotSkills>,
    who: Caller,
    Path(id): Path<String>,
    Body(b): Body<SkillIn>,
) -> ApiResult<Json<Value>> {
    blocking(move || s.create(&who.ws, &who.me, &id, b))
        .await
        .map(Json)
}

async fn update(
    State(s): State<BotSkills>,
    who: Caller,
    Path((id, sid)): Path<(String, String)>,
    Body(b): Body<SkillIn>,
) -> ApiResult<Json<Value>> {
    blocking(move || s.update(&who.ws, &who.me, &id, &sid, b))
        .await
        .map(Json)
}

async fn remove(
    State(s): State<BotSkills>,
    who: Caller,
    Path((id, sid)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    blocking(move || s.delete(&who.ws, &who.me, &id, &sid)).await?;
    Ok(Json(json!({ "ok": true })))
}

/// The Bot, if `ws` sees it (its home or one it serves), not deleted.
fn own_bot(c: &Connection, ws: &str, bot: &str) -> ApiResult<()> {
    let seen: Option<i64> = c
        .query_row(
            "SELECT 1 FROM bots b WHERE b.id = ?1 AND b.deleted = 0 AND (b.workspace_id = ?2
               OR EXISTS (SELECT 1 FROM bot_workspaces w WHERE w.bot_id = b.id AND w.workspace_id = ?2))",
            [bot, ws],
            |r| r.get(0),
        )
        .optional()?;
    seen.map(|_| ()).ok_or_else(|| ApiError::not_found("Bot"))
}

fn skill_of(c: &Connection, bot: &str, sid: &str) -> ApiResult<Skill> {
    repo::get(c, sid)?
        .filter(|s| s.bot_id == bot)
        .ok_or_else(|| ApiError::not_found("技能"))
}

fn out(s: &Skill) -> Value {
    json!({
        "id": s.id, "bot_id": s.bot_id, "name": s.name, "description": s.description,
        "content": s.content, "enabled": s.enabled, "created": s.created, "updated": s.updated,
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

/// The bytes of `bot`'s enabled skills with `s` as it is about to be saved.
fn enabled_total(c: &Connection, bot: &str, s: &Skill) -> ApiResult<usize> {
    let others: usize = repo::list(c, bot)?
        .iter()
        .filter(|x| x.enabled && x.id != s.id)
        .map(|x| x.content.len())
        .sum();
    Ok(others + if s.enabled { s.content.len() } else { 0 })
}

impl BotSkills {
    pub fn new(store: Store) -> Self {
        BotSkills { store }
    }

    pub fn list(&self, ws: &str, bot: &str) -> ApiResult<Vec<Value>> {
        self.store.reader(|c| {
            own_bot(c, ws, bot)?;
            Ok(repo::list(c, bot)?.iter().map(out).collect())
        })
    }

    pub fn create(&self, ws: &str, me: &str, bot: &str, b: SkillIn) -> ApiResult<Value> {
        let at = now();
        let blank = Skill {
            id: uid("bskill"),
            workspace_id: ws.into(),
            bot_id: bot.into(),
            name: String::new(),
            description: String::new(),
            content: String::new(),
            enabled: true,
            created: at,
            updated: at,
        };
        if b.name.is_none() || b.description.is_none() {
            let mut c = Check::new();
            c.that("name", b.name.is_some());
            c.that("description", b.description.is_some());
            c.done()?;
        }
        self.save(ws, me, bot, blank, b, true)
    }

    pub fn update(&self, ws: &str, me: &str, bot: &str, sid: &str, b: SkillIn) -> ApiResult<Value> {
        let old = self.store.reader(|c| {
            own_bot(c, ws, bot)?;
            skill_of(c, bot, sid)
        })?;
        self.save(ws, me, bot, old, b, false)
    }

    pub fn delete(&self, ws: &str, me: &str, bot: &str, sid: &str) -> ApiResult<()> {
        self.store.transaction(|tx| {
            own_bot(tx, ws, bot)?;
            let old = skill_of(tx, bot, sid)?;
            repo::delete(tx, sid)?;
            base::audit(
                tx,
                ws,
                me,
                "bot_skill.delete",
                sid,
                &json!({"bot": bot, "name": old.name}),
            )?;
            tx.publish(ChangeEvent::Bot { id: bot.into() });
            Ok(())
        })
    }

    /// Merges `b` into `old`, checks it and writes it in one transaction.
    fn save(
        &self,
        ws: &str,
        me: &str,
        bot: &str,
        old: Skill,
        b: SkillIn,
        fresh: bool,
    ) -> ApiResult<Value> {
        let only_switch = !fresh
            && b.name.is_none()
            && b.description.is_none()
            && b.content.is_none()
            && b.enabled.is_some();
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
        if let Some(e) = b.enabled {
            s.enabled = e;
        }
        check(&s)?;
        s.updated = now();
        self.store.transaction(|tx| {
            own_bot(tx, ws, bot)?;
            if fresh && repo::list(tx, bot)?.len() >= MAX_SKILLS {
                return Err(ApiError::rule(steve_i18n::t!(
                    "modules.bot_skills.too_many",
                    max = MAX_SKILLS
                )));
            }
            if repo::name_taken(tx, bot, &s.name, Some(&s.id))? {
                return Err(ApiError::conflict(steve_i18n::t!(
                    "modules.bot_skills.name_taken",
                    name = s.name
                )));
            }
            // a switch off, or a change that does not grow them, is always allowed
            let total = enabled_total(tx, bot, &s)?;
            let grows = s.enabled && (!old.enabled || fresh || s.content.len() > old.content.len());
            if grows && total > MAX_TOTAL {
                return Err(ApiError::rule(steve_i18n::t!(
                    "modules.bot_skills.too_large",
                    kib = MAX_TOTAL / 1024
                )));
            }
            if fresh {
                repo::insert(tx, &s)?;
            } else {
                repo::update(tx, &s)?;
            }
            let verb = match (fresh, only_switch, s.enabled) {
                (true, _, _) => "bot_skill.create",
                (false, true, true) => "bot_skill.enable",
                (false, true, false) => "bot_skill.disable",
                (false, false, _) => "bot_skill.update",
            };
            base::audit(
                tx,
                ws,
                me,
                verb,
                &s.id,
                &json!({"bot": bot, "name": s.name, "enabled": s.enabled}),
            )?;
            tx.publish(ChangeEvent::Bot { id: bot.into() });
            Ok(())
        })?;
        Ok(out(&s))
    }
}
