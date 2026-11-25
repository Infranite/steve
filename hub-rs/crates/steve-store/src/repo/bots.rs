//! Bots (owned by the user, members of workspaces) and their memories.

use super::Cached;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::{Sets, all, exists, one, revision, to_json};
use crate::types::double;
use crate::{
    Avatar, Bot, BotWorkspace, ExecutionSurface, Memory, Result, Store, StoreError, now, uid,
};

impl Store {
    pub fn bots(&self) -> Bots<'_> {
        Bots(self)
    }
    pub fn memories(&self) -> Memories<'_> {
        Memories(self)
    }
}

/// What a new or seeded bot may set; unset fields get the repo.ts seed defaults
/// (`role ""`, `rules ""`, `persona ""`, `proactive 1`, `accept_mode "inherit"`, `perm_mode "default"`, `avatar {}`).
/// Like TS, `prompt` and `device` cannot be set at creation, only by `patch`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct NewBot {
    pub name: String,
    pub harness: String,
    pub role: Option<String>,
    pub specialties: Option<Vec<String>>,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub rules: Option<String>,
    pub persona: Option<String>,
    pub proactive: Option<i64>,
    pub accept_mode: Option<String>,
    pub perm_mode: Option<String>,
    pub avatar: Option<Avatar>,
    pub default_project: Option<String>,
    pub fallback_surfaces: Option<Vec<ExecutionSurface>>,
}

/// The patchable bot fields, in the order repo.ts lists them (this is also the key order of the revision).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct BotPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub specialties: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    #[serde(
        default,
        deserialize_with = "double",
        skip_serializing_if = "Option::is_none"
    )]
    pub model: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "double",
        skip_serializing_if = "Option::is_none"
    )]
    pub reasoning_effort: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rules: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proactive: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accept_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub perm_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<Avatar>,
    #[serde(
        default,
        deserialize_with = "double",
        skip_serializing_if = "Option::is_none"
    )]
    pub default_project: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "double",
        skip_serializing_if = "Option::is_none"
    )]
    pub prompt: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "double",
        skip_serializing_if = "Option::is_none"
    )]
    pub device: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_surfaces: Option<Vec<ExecutionSurface>>,
}

pub struct Bots<'a>(&'a Store);

const BOT: &str = "SELECT * FROM bots WHERE id = ? AND deleted = 0";

impl Bots<'_> {
    /// Non-deleted bots, main first then by name; with `ws`, only members of that workspace.
    pub fn list(&self, ws: Option<&str>) -> Result<Vec<Bot>> {
        self.0.read_conn(|c| match ws {
            Some(ws) => all(
                c,
                "SELECT b.* FROM bots b JOIN bot_workspaces m ON m.bot_id = b.id WHERE m.workspace_id = ? AND b.deleted = 0 ORDER BY b.main DESC, b.name",
                [ws],
                Bot::from_row,
            ),
            None => all(c, "SELECT * FROM bots WHERE deleted = 0 ORDER BY main DESC, name", [], Bot::from_row),
        })
    }

    /// A non-deleted bot.
    pub fn get(&self, id: &str) -> Result<Option<Bot>> {
        self.0.read_conn(|c| one(c, BOT, [id], Bot::from_row))
    }

    /// The stored `owner` column for `id`, including soft-deleted bots (`None` if no row exists).
    pub fn owner(&self, id: &str) -> Result<Option<Option<String>>> {
        self.0.read_conn(|c| {
            one(c, "SELECT owner FROM bots WHERE id = ?", [id], |r| {
                r.get::<_, Option<String>>(0)
            })
        })
    }

    /// The main bot (Steve).
    pub fn main(&self) -> Result<Option<Bot>> {
        self.0.read_conn(|c| {
            one(
                c,
                "SELECT * FROM bots WHERE main = 1 AND deleted = 0 ORDER BY updated LIMIT 1",
                [],
                Bot::from_row,
            )
        })
    }

    pub fn workspaces(&self, id: &str) -> Result<Vec<BotWorkspace>> {
        self.0.read_conn(|c| {
            all(
                c,
                "SELECT workspace_id, duty FROM bot_workspaces WHERE bot_id = ?",
                [id],
                |r| {
                    Ok(BotWorkspace {
                        workspace_id: r.get(0)?,
                        duty: r.get(1)?,
                    })
                },
            )
        })
    }

    pub fn in_workspace(&self, id: &str, ws: &str) -> Result<bool> {
        self.0.read_conn(|c| {
            exists(
                c,
                "SELECT 1 FROM bot_workspaces WHERE bot_id = ? AND workspace_id = ?",
                [id, ws],
            )
        })
    }

    /// Ordered fallback execution surfaces configured for Bot `id`.
    pub fn fallback_surfaces(&self, id: &str) -> Result<Vec<ExecutionSurface>> {
        self.0.read_conn(|c| fallback_surfaces_on(c, id))
    }

    /// Ordered execution-surface candidates for `bot`: primary surface first, followed by
    /// configured fallbacks without duplicates.
    pub fn execution_surfaces(&self, bot: &Bot) -> Result<Vec<ExecutionSurface>> {
        let mut out = vec![ExecutionSurface {
            harness: bot.harness.clone(),
            model: bot.model.clone(),
            reasoning_effort: bot.reasoning_effort.clone(),
        }];
        for surface in self.fallback_surfaces(&bot.id)? {
            if !surface.harness.trim().is_empty() && !out.contains(&surface) {
                out.push(surface);
            }
        }
        Ok(out)
    }

    /// Makes the bot a member of exactly `wss` (existing duties are kept).
    pub fn set_workspaces(&self, id: &str, wss: &[String]) -> Result<()> {
        self.0.write_conn(|w| {
            let transaction = w.tx()?;
            let tx = transaction.connection();
            let marks = if wss.is_empty() { "''".to_string() } else { vec!["?"; wss.len()].join(",") };
            let mut ps: Vec<&str> = vec![id];
            ps.extend(wss.iter().map(String::as_str));
            tx.run(
                &format!("DELETE FROM bot_workspaces WHERE bot_id = ? AND workspace_id NOT IN ({marks})"),
                rusqlite::params_from_iter(ps),
            )?;
            for ws in wss {
                tx.run("INSERT OR IGNORE INTO bot_workspaces (bot_id, workspace_id, created) VALUES (?, ?, ?)", params![id, ws, now()])?;
            }
            transaction.commit()?;
            if let Some(ws) = crate::repo::one(
                w.c,
                "SELECT workspace_id FROM bots WHERE id = ?",
                [id],
                |r| r.get::<_, String>(0),
            )? {
                w.publish(steve_types::DomainEvent::from(
                    steve_types::ConfigEvent::Bot {
                        ws: ws.into(),
                        id: id.into(),
                    },
                ));
            }
            Ok(())
        })
    }

    /// Seed-only insert: never overwrites what the user changed. Also joins `ws`. No event.
    pub fn seed(&self, id: &str, main: bool, b: &NewBot, ws: &str) -> Result<()> {
        self.0.write_conn(|w| seed(w.c, id, main, b, ws))
    }

    /// A new (non-main) bot in `ws`; owned by `author` and audited as `bot.create`.
    pub fn create(&self, ws: &str, b: &NewBot, author: &str) -> Result<Option<Bot>> {
        self.0.write_conn(|w| {
            let id = uid("bot");
            seed(w.c, &id, false, b, ws)?;
            if !author.is_empty() {
                w.c.run(
                    "UPDATE bots SET owner = ? WHERE id = ? AND main = 0",
                    params![author, id],
                )?;
            }
            w.c.run(
                "INSERT INTO audit (workspace_id, ts, actor, action, target, detail) VALUES (?, ?, ?, ?, ?, ?)",
                params![ws, now(), author, "bot.create", id, json!({"name": b.name}).to_string()],
            )?;
            w.publish(steve_types::DomainEvent::from(
                steve_types::ConfigEvent::Bot {
                    ws: ws.into(),
                    id: id.clone().into(),
                },
            ));
            one(w.c, BOT, [&id], Bot::from_row)
        })
    }

    /// Fails with `没有这个 Bot`. Bumps `revision` when something changed and always records
    /// a revision row `{before, after}` with the given keys.
    pub fn patch(&self, id: &str, p: &BotPatch, author: &str) -> Result<Option<Bot>> {
        self.0.write_conn(|w| {
            let cur =
                one(w.c, BOT, [id], Bot::from_row)?.ok_or(StoreError::NotFound("没有这个 Bot"))?;
            let avatar = p.avatar.as_ref().map(to_json).transpose()?;
            let mut s = Sets::default();
            s.val("name", p.name.clone())
                .val("role", p.role.clone())
                .val(
                    "specialties",
                    p.specialties.as_ref().map(to_json).transpose()?,
                )
                .val("harness", p.harness.clone())
                .null("model", p.model.clone())
                .null("reasoning_effort", p.reasoning_effort.clone())
                .val("rules", p.rules.clone())
                .val("persona", p.persona.clone())
                .val("proactive", p.proactive)
                .val("accept_mode", p.accept_mode.clone())
                .val("perm_mode", p.perm_mode.clone())
                .val("avatar", avatar)
                .null("default_project", p.default_project.clone())
                .null("prompt", p.prompt.clone())
                .null("device", p.device.clone())
                .val("enabled", p.enabled);
            let cur_fallbacks = if p.fallback_surfaces.is_some() {
                Some(fallback_surfaces_on(w.c, id)?)
            } else {
                None
            };
            if let Some(fallbacks) = &p.fallback_surfaces {
                write_fallback_surfaces(w.c, id, fallbacks)?;
            }
            if !s.is_empty() {
                w.c.run(
                    &format!(
                        "UPDATE bots SET {}, revision = revision + 1, updated = ? WHERE id = ?",
                        s.sql()
                    ),
                    rusqlite::params_from_iter(s.params([now().into(), id.to_string().into()])),
                )?;
            } else if p.fallback_surfaces.is_some() {
                w.c.run(
                    "UPDATE bots SET revision = revision + 1, updated = ? WHERE id = ?",
                    params![now(), id],
                )?;
            }
            let after = match serde_json::to_value(p)? {
                Value::Object(m) => m,
                _ => Map::new(),
            };
            let mut cur_json = serde_json::to_value(&cur)?;
            if let (Some(prev_fs), Value::Object(obj)) = (cur_fallbacks, &mut cur_json) {
                obj.insert("fallback_surfaces".into(), serde_json::to_value(prev_fs)?);
            }
            let before: Map<String, Value> = after
                .keys()
                .map(|k| (k.clone(), cur_json.get(k).cloned().unwrap_or(Value::Null)))
                .collect();
            revision(
                w.c,
                &cur.workspace_id,
                "bot",
                id,
                cur.revision + 1,
                &json!({ "before": before, "after": after }),
                author,
            )?;
            w.publish(steve_types::DomainEvent::from(
                steve_types::ConfigEvent::Bot {
                    ws: cur.workspace_id.clone().into(),
                    id: id.into(),
                },
            ));
            one(w.c, BOT, [id], Bot::from_row)
        })
    }

    /// Soft delete; the main bot cannot be removed.
    pub fn remove(&self, id: &str) -> Result<()> {
        self.0.write(|w| crate::bots::remove(w, id))
    }

    pub fn restore(&self, id: &str) -> Result<()> {
        self.0.write_conn(|w| {
            w.c.run(
                "UPDATE bots SET deleted = 0, enabled = 1, updated = ? WHERE id = ?",
                params![now(), id],
            )?;
            if let Some(ws) = crate::repo::one(
                w.c,
                "SELECT workspace_id FROM bots WHERE id = ?",
                [id],
                |r| r.get::<_, String>(0),
            )? {
                w.publish(steve_types::DomainEvent::from(
                    steve_types::ConfigEvent::Bot {
                        ws: ws.into(),
                        id: id.into(),
                    },
                ));
            }
            Ok(())
        })
    }

    /// The raw SQL `seed()` in orchestrator.ts runs after seeding `steve` and `dev`: link `dev` to the
    /// Steve project if it has none, and give both their colours if their avatar is still `{}`. No event.
    pub fn seed_fixups(&self, steve_project: Option<&str>) -> Result<()> {
        self.0.write_conn(|w| {
            if let Some(p) = steve_project {
                w.c.run("UPDATE bots SET default_project = ? WHERE id = 'dev' AND default_project IS NULL", [p])?;
            }
            w.c.run(r##"UPDATE bots SET avatar = '{"color":"#7aa7c7","round":70}' WHERE id = 'steve' AND avatar = '{}'"##, [])?;
            w.c.run(r##"UPDATE bots SET avatar = '{"color":"#8c7ae6","round":45}' WHERE id = 'dev' AND avatar = '{}'"##, [])?;
            Ok(())
        })
    }
}

fn fallback_surfaces_on(c: &rusqlite::Connection, id: &str) -> Result<Vec<ExecutionSurface>> {
    all(
        c,
        "SELECT harness, model, reasoning_effort FROM bot_fallback_surfaces WHERE bot_id = ? ORDER BY ordinal ASC",
        [id],
        |r| {
            Ok(ExecutionSurface {
                harness: r.get(0)?,
                model: r.get(1)?,
                reasoning_effort: r.get(2)?,
            })
        },
    )
}

fn write_fallback_surfaces(
    c: &rusqlite::Connection,
    id: &str,
    surfaces: &[ExecutionSurface],
) -> Result<()> {
    c.run("DELETE FROM bot_fallback_surfaces WHERE bot_id = ?", [id])?;
    for (ordinal, surface) in surfaces.iter().enumerate() {
        c.run(
            "INSERT INTO bot_fallback_surfaces (bot_id, ordinal, harness, model, reasoning_effort) VALUES (?, ?, ?, ?, ?)",
            params![
                id,
                ordinal as i64,
                surface.harness,
                surface.model,
                surface.reasoning_effort,
            ],
        )?;
    }
    Ok(())
}

fn seed(c: &rusqlite::Connection, id: &str, main: bool, b: &NewBot, ws: &str) -> Result<()> {
    let avatar = to_json(b.avatar.as_ref().unwrap_or(&Avatar::default()))?;
    let inserted = c
        .prepare_cached(
            "INSERT INTO bots (id, workspace_id, name, role, main, harness, model, reasoning_effort, rules, persona, proactive, accept_mode, perm_mode, avatar, default_project, specialties, enabled, revision, updated)
      VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1, 1, ?) ON CONFLICT(id) DO NOTHING",
        )?
        .execute(params![
            id,
            ws,
            b.name,
            b.role.as_deref().unwrap_or(""),
            main as i64,
            b.harness,
            b.model,
            b.reasoning_effort,
            b.rules.as_deref().unwrap_or(""),
            b.persona.as_deref().unwrap_or(""),
            b.proactive.unwrap_or(1),
            b.accept_mode.as_deref().unwrap_or("inherit"),
            b.perm_mode.as_deref().unwrap_or("default"),
            avatar,
            b.default_project,
            to_json(&b.specialties.as_deref().unwrap_or_default())?,
            now(),
        ])?;
    if inserted > 0
        && let Some(fallbacks) = b.fallback_surfaces.as_deref()
        && !fallbacks.is_empty()
    {
        write_fallback_surfaces(c, id, fallbacks)?;
    }
    c.run(
        "INSERT OR IGNORE INTO bot_workspaces (bot_id, workspace_id, created) VALUES (?, ?, ?)",
        params![id, ws, now()],
    )?;
    // who edits it (migration 050): its workspace's owner; the main bot's is the instance admin
    c.run(
        "UPDATE bots SET owner = (SELECT m.user_id FROM workspace_members m WHERE m.workspace_id = bots.workspace_id AND m.role = 'owner' ORDER BY m.created LIMIT 1)
          WHERE id = ? AND main = 0 AND owner IS NULL",
        [id],
    )?;
    Ok(())
}

pub struct Memories<'a>(&'a Store);

impl Memories<'_> {
    pub fn get(&self, id: &str) -> Result<Option<Memory>> {
        self.0.read_conn(|c| {
            one(
                c,
                "SELECT * FROM bot_memories WHERE id = ?",
                [id],
                Memory::from_row,
            )
        })
    }

    /// Newest first.
    pub fn list(&self, bot: &str, ws: &str) -> Result<Vec<Memory>> {
        self.0.read_conn(|c| {
            all(c, "SELECT * FROM bot_memories WHERE bot_id = ? AND workspace_id = ? ORDER BY created DESC", [bot, ws], Memory::from_row)
        })
    }

    pub fn count(&self, bot: &str, ws: &str) -> Result<i64> {
        self.0.read_conn(|c| {
            Ok(c.prepare_cached(
                "SELECT count(*) n FROM bot_memories WHERE bot_id = ? AND workspace_id = ?",
            )?
            .query_row([bot, ws], |r| r.get(0))?)
        })
    }

    pub fn add(&self, bot: &str, ws: &str, text: &str, source: &str) -> Result<String> {
        self.0.write_conn(|w| {
            let id = uid("mem");
            w.c.run(
                "INSERT INTO bot_memories (id, bot_id, workspace_id, text, source, created) VALUES (?, ?, ?, ?, ?, ?)",
                params![id, bot, ws, text, source, now()],
            )?;
            w.publish(steve_types::DomainEvent::from(
                steve_types::ConfigEvent::Bot {
                    ws: ws.into(),
                    id: bot.into(),
                },
            ));
            Ok(id)
        })
    }

    pub fn edit(&self, id: &str, text: &str) -> Result<()> {
        self.0.write_conn(|w| {
            let bot: Option<(String, String)> = one(
                w.c,
                "UPDATE bot_memories SET text = ? WHERE id = ? RETURNING bot_id, workspace_id",
                [text, id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            if let Some((bot, ws)) = bot {
                w.publish(steve_types::DomainEvent::from(
                    steve_types::ConfigEvent::Bot {
                        ws: ws.into(),
                        id: bot.into(),
                    },
                ));
            }
            Ok(())
        })
    }

    pub fn remove(&self, id: &str) -> Result<()> {
        self.0.write_conn(|w| {
            let bot: Option<(String, String)> = one(
                w.c,
                "DELETE FROM bot_memories WHERE id = ? RETURNING bot_id, workspace_id",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            if let Some((bot, ws)) = bot {
                w.publish(steve_types::DomainEvent::from(
                    steve_types::ConfigEvent::Bot {
                        ws: ws.into(),
                        id: bot.into(),
                    },
                ));
            }
            Ok(())
        })
    }

    /// Forgets everything bots remember in `ws`; publishes `bot` with id `*`.
    pub fn clear(&self, ws: &str) -> Result<()> {
        self.0.write_conn(|w| {
            w.c.run("DELETE FROM bot_memories WHERE workspace_id = ?", [ws])?;
            w.publish(steve_types::DomainEvent::from(
                steve_types::ConfigEvent::Bot {
                    ws: ws.into(),
                    id: "*".into(),
                },
            ));
            Ok(())
        })
    }

    /// All memories of one bot in every workspace (`DELETE /api/bots/:id` with `memory: "delete"`). No event.
    pub fn remove_of_bot(&self, bot: &str) -> Result<()> {
        self.0.write_conn(|w| {
            w.c.run("DELETE FROM bot_memories WHERE bot_id = ?", [bot])?;
            Ok(())
        })
    }

    /// Memories of one bot scoped strictly to `ws` (workspace removal or scoped global delete).
    pub fn remove_of_bot_in_workspace(&self, bot: &str, ws: &str) -> Result<()> {
        self.0.write_conn(|w| {
            w.c.run(
                "DELETE FROM bot_memories WHERE bot_id = ? AND workspace_id = ?",
                params![bot, ws],
            )?;
            Ok(())
        })
    }
}
