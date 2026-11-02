//! Bots (owned by the user, members of workspaces) and their memories.

use super::Cached;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::{Sets, all, exists, one, revision, to_json};
use crate::types::double;
use crate::{Avatar, Bot, BotWorkspace, ChangeEvent, Memory, Result, Store, StoreError, now, uid};

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
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub rules: Option<String>,
    pub persona: Option<String>,
    pub proactive: Option<i64>,
    pub accept_mode: Option<String>,
    pub perm_mode: Option<String>,
    pub avatar: Option<Avatar>,
    pub default_project: Option<String>,
}

/// The patchable bot fields, in the order repo.ts lists them (this is also the key order of the revision).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct BotPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
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
}

pub struct Bots<'a>(&'a Store);

const BOT: &str = "SELECT * FROM bots WHERE id = ? AND deleted = 0";

impl Bots<'_> {
    /// Non-deleted bots, main first then by name; with `ws`, only members of that workspace.
    pub fn list(&self, ws: Option<&str>) -> Result<Vec<Bot>> {
        self.0.read(|c| match ws {
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
        self.0.read(|c| one(c, BOT, [id], Bot::from_row))
    }

    /// The main bot (Steve).
    pub fn main(&self) -> Result<Option<Bot>> {
        self.0.read(|c| {
            one(
                c,
                "SELECT * FROM bots WHERE main = 1 AND deleted = 0 ORDER BY updated LIMIT 1",
                [],
                Bot::from_row,
            )
        })
    }

    pub fn workspaces(&self, id: &str) -> Result<Vec<BotWorkspace>> {
        self.0.read(|c| {
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
        self.0.read(|c| {
            exists(
                c,
                "SELECT 1 FROM bot_workspaces WHERE bot_id = ? AND workspace_id = ?",
                [id, ws],
            )
        })
    }

    /// Makes the bot a member of exactly `wss` (existing duties are kept).
    pub fn set_workspaces(&self, id: &str, wss: &[String]) -> Result<()> {
        self.0.write(|w| {
            let tx = w.c.unchecked_transaction()?;
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
            tx.commit()?;
            w.publish(ChangeEvent::Bot { id: id.into() });
            Ok(())
        })
    }

    /// Seed-only insert: never overwrites what the user changed. Also joins `ws`. No event.
    pub fn seed(&self, id: &str, main: bool, b: &NewBot, ws: &str) -> Result<()> {
        self.0.write(|w| seed(w.c, id, main, b, ws))
    }

    /// A new (non-main) bot in `ws`; audited as `bot.create`.
    pub fn create(&self, ws: &str, b: &NewBot, author: &str) -> Result<Option<Bot>> {
        self.0.write(|w| {
            let id = uid("bot");
            seed(w.c, &id, false, b, ws)?;
            w.c.run(
                "INSERT INTO audit (workspace_id, ts, actor, action, target, detail) VALUES (?, ?, ?, ?, ?, ?)",
                params![ws, now(), author, "bot.create", id, "{}"],
            )?;
            w.publish(ChangeEvent::Bot { id: id.clone() });
            one(w.c, BOT, [&id], Bot::from_row)
        })
    }

    /// Fails with `没有这个 Bot`. Bumps `revision` when something changed and always records
    /// a revision row `{before, after}` with the given keys.
    pub fn patch(&self, id: &str, p: &BotPatch, author: &str) -> Result<Option<Bot>> {
        self.0.write(|w| {
            let cur =
                one(w.c, BOT, [id], Bot::from_row)?.ok_or(StoreError::NotFound("没有这个 Bot"))?;
            let avatar = p.avatar.as_ref().map(to_json).transpose()?;
            let mut s = Sets::default();
            s.val("name", p.name.clone())
                .val("role", p.role.clone())
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
            if !s.is_empty() {
                w.c.run(
                    &format!(
                        "UPDATE bots SET {}, revision = revision + 1, updated = ? WHERE id = ?",
                        s.sql()
                    ),
                    rusqlite::params_from_iter(s.params([now().into(), id.to_string().into()])),
                )?;
            }
            let after = match serde_json::to_value(p)? {
                Value::Object(m) => m,
                _ => Map::new(),
            };
            let cur_json = serde_json::to_value(&cur)?;
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
            w.publish(ChangeEvent::Bot { id: id.into() });
            one(w.c, BOT, [id], Bot::from_row)
        })
    }

    /// Soft delete; the main bot cannot be removed.
    pub fn remove(&self, id: &str) -> Result<()> {
        self.0.write(|w| {
            w.c.run(
                "UPDATE bots SET deleted = 1, enabled = 0, updated = ? WHERE id = ? AND main = 0",
                params![now(), id],
            )?;
            w.publish(ChangeEvent::Bot { id: id.into() });
            Ok(())
        })
    }

    pub fn restore(&self, id: &str) -> Result<()> {
        self.0.write(|w| {
            w.c.run(
                "UPDATE bots SET deleted = 0, enabled = 1, updated = ? WHERE id = ?",
                params![now(), id],
            )?;
            w.publish(ChangeEvent::Bot { id: id.into() });
            Ok(())
        })
    }

    /// The raw SQL `seed()` in orchestrator.ts runs after seeding `steve` and `dev`: link `dev` to the
    /// Steve project if it has none, and give both their colours if their avatar is still `{}`. No event.
    pub fn seed_fixups(&self, steve_project: Option<&str>) -> Result<()> {
        self.0.write(|w| {
            if let Some(p) = steve_project {
                w.c.run("UPDATE bots SET default_project = ? WHERE id = 'dev' AND default_project IS NULL", [p])?;
            }
            w.c.run(r##"UPDATE bots SET avatar = '{"color":"#7aa7c7","round":70}' WHERE id = 'steve' AND avatar = '{}'"##, [])?;
            w.c.run(r##"UPDATE bots SET avatar = '{"color":"#8c7ae6","round":45}' WHERE id = 'dev' AND avatar = '{}'"##, [])?;
            Ok(())
        })
    }
}

fn seed(c: &rusqlite::Connection, id: &str, main: bool, b: &NewBot, ws: &str) -> Result<()> {
    let avatar = to_json(b.avatar.as_ref().unwrap_or(&Avatar::default()))?;
    c.prepare_cached(
        "INSERT INTO bots (id, workspace_id, name, role, main, harness, model, reasoning_effort, rules, persona, proactive, accept_mode, perm_mode, avatar, default_project, enabled, revision, updated)
      VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1, 1, ?) ON CONFLICT(id) DO NOTHING",
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
        now(),
    ])?;
    c.run(
        "INSERT OR IGNORE INTO bot_workspaces (bot_id, workspace_id, created) VALUES (?, ?, ?)",
        params![id, ws, now()],
    )?;
    Ok(())
}

pub struct Memories<'a>(&'a Store);

impl Memories<'_> {
    pub fn get(&self, id: &str) -> Result<Option<Memory>> {
        self.0.read(|c| {
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
        self.0.read(|c| {
            all(c, "SELECT * FROM bot_memories WHERE bot_id = ? AND workspace_id = ? ORDER BY created DESC", [bot, ws], Memory::from_row)
        })
    }

    pub fn count(&self, bot: &str, ws: &str) -> Result<i64> {
        self.0.read(|c| {
            Ok(c.prepare_cached(
                "SELECT count(*) n FROM bot_memories WHERE bot_id = ? AND workspace_id = ?",
            )?
            .query_row([bot, ws], |r| r.get(0))?)
        })
    }

    pub fn add(&self, bot: &str, ws: &str, text: &str, source: &str) -> Result<String> {
        self.0.write(|w| {
            let id = uid("mem");
            w.c.run(
                "INSERT INTO bot_memories (id, bot_id, workspace_id, text, source, created) VALUES (?, ?, ?, ?, ?, ?)",
                params![id, bot, ws, text, source, now()],
            )?;
            w.publish(ChangeEvent::Bot { id: bot.into() });
            Ok(id)
        })
    }

    pub fn edit(&self, id: &str, text: &str) -> Result<()> {
        self.0.write(|w| {
            let bot: Option<String> = one(
                w.c,
                "UPDATE bot_memories SET text = ? WHERE id = ? RETURNING bot_id",
                [text, id],
                |r| r.get(0),
            )?;
            if let Some(bot) = bot {
                w.publish(ChangeEvent::Bot { id: bot });
            }
            Ok(())
        })
    }

    pub fn remove(&self, id: &str) -> Result<()> {
        self.0.write(|w| {
            let bot: Option<String> = one(
                w.c,
                "DELETE FROM bot_memories WHERE id = ? RETURNING bot_id",
                [id],
                |r| r.get(0),
            )?;
            if let Some(bot) = bot {
                w.publish(ChangeEvent::Bot { id: bot });
            }
            Ok(())
        })
    }

    /// Forgets everything bots remember in `ws`; publishes `bot` with id `*`.
    pub fn clear(&self, ws: &str) -> Result<()> {
        self.0.write(|w| {
            w.c.run("DELETE FROM bot_memories WHERE workspace_id = ?", [ws])?;
            w.publish(ChangeEvent::Bot { id: "*".into() });
            Ok(())
        })
    }

    /// All memories of one bot in every workspace (`DELETE /api/bots/:id` with `memory: "delete"`). No event.
    pub fn remove_of_bot(&self, bot: &str) -> Result<()> {
        self.0.write(|w| {
            w.c.run("DELETE FROM bot_memories WHERE bot_id = ?", [bot])?;
            Ok(())
        })
    }
}
