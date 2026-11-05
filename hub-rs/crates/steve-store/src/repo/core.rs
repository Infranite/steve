//! Workspaces, users, auth sessions, settings, counters, audit.

use super::Cached;
use rusqlite::params;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::{Sets, all, exists, must, one, revision, to_json};
use crate::types::json_or;
use crate::{ChangeEvent, MemberWorkspace, Result, Store, User, WS, Workspace, now, sha, uid};

impl Store {
    /// `audit(actor, action, target, detail, ws)`; TS defaults are `detail = {}`, `ws = WS`.
    pub fn audit(
        &self,
        actor: &str,
        action: &str,
        target: &str,
        detail: &Value,
        ws: &str,
    ) -> Result<()> {
        self.write(|w| {
            w.c.prepare_cached("INSERT INTO audit (workspace_id, ts, actor, action, target, detail) VALUES (?, ?, ?, ?, ?, ?)")?
                .execute(params![ws, now(), actor, action, target, to_json(detail)?])?;
            Ok(())
        })
    }

    /// `peekCounter`: the last number handed out for `name` (e.g. `item:ST`), 0 if none.
    pub fn peek_counter(&self, name: &str) -> Result<i64> {
        self.read(|c| {
            Ok(one(
                c,
                "SELECT value FROM counters WHERE workspace_id = '*' AND name = ?",
                [name],
                |r| r.get(0),
            )?
            .unwrap_or(0))
        })
    }

    /// `ensureWorkspace`: creates `default` (个人), renames the old default name, and carries the old
    /// per-workspace item counter over to the global `item:ST` once.
    pub fn ensure_workspace(&self) -> Result<()> {
        self.write(|w| {
            let c = w.c;
            c.run("INSERT OR IGNORE INTO workspaces (id, name, created) VALUES (?, ?, ?)", params![WS, "个人", now()])?;
            c.run("UPDATE workspaces SET name = '个人' WHERE id = ? AND name = '默认工作空间'", [WS])?;
            let old: Option<i64> =
                one(c, "SELECT value FROM counters WHERE workspace_id = ? AND name = 'item'", [WS], |r| r.get(0))?;
            if let Some(old) = old {
                c.run(
                    "INSERT INTO counters (workspace_id, name, value) VALUES ('*', 'item:ST', ?) ON CONFLICT(workspace_id, name) DO UPDATE SET value = max(value, excluded.value)",
                    [old],
                )?;
                c.run("DELETE FROM counters WHERE workspace_id = ? AND name = 'item'", [WS])?;
            }
            super::kinds::ensure(w, WS)?;
            Ok(())
        })
    }

    pub fn workspaces(&self) -> Workspaces<'_> {
        Workspaces(self)
    }
    pub fn users(&self) -> Users<'_> {
        Users(self)
    }
    pub fn auth_sessions(&self) -> AuthSessions<'_> {
        AuthSessions(self)
    }
    pub fn settings(&self) -> Settings<'_> {
        Settings(self)
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct NewWorkspace {
    pub name: String,
    pub description: Option<String>,
    pub item_prefix: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct WorkspacePatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_prefix: Option<String>,
}

pub struct Workspaces<'a>(&'a Store);

impl Workspaces<'_> {
    pub fn get(&self, id: &str) -> Result<Option<Workspace>> {
        self.0.read(|c| {
            one(
                c,
                "SELECT * FROM workspaces WHERE id = ?",
                [id],
                Workspace::from_row,
            )
        })
    }

    /// The user's workspaces with their member role, ordered by sort then creation.
    pub fn of_user(&self, user_id: &str) -> Result<Vec<MemberWorkspace>> {
        self.0.read(|c| {
            all(
                c,
                "SELECT w.*, m.role FROM workspaces w JOIN workspace_members m ON m.workspace_id = w.id WHERE m.user_id = ? ORDER BY w.sort, w.created",
                [user_id],
                |r| Ok(MemberWorkspace { workspace: Workspace::from_row(r)?, role: r.get("role")? }),
            )
        })
    }

    pub fn all(&self) -> Result<Vec<Workspace>> {
        self.0.read(|c| {
            all(
                c,
                "SELECT * FROM workspaces ORDER BY sort, created",
                [],
                Workspace::from_row,
            )
        })
    }

    /// All workspace ids, unordered (`SELECT id FROM workspaces` in housekeeping and recover).
    pub fn ids(&self) -> Result<Vec<String>> {
        self.0
            .read(|c| all(c, "SELECT id FROM workspaces", [], |r| r.get(0)))
    }

    /// New workspace at the end of the sort order, `owner_id` as its owner.
    pub fn create(&self, p: &NewWorkspace, owner_id: &str) -> Result<Workspace> {
        self.0.write(|w| {
            let (id, t) = (uid("ws"), now());
            w.c.run(
                "INSERT INTO workspaces (id, name, description, item_prefix, sort, created) VALUES (?, ?, ?, ?, (SELECT coalesce(max(sort), 0) + 1 FROM workspaces), ?)",
                params![id, p.name, p.description.as_deref().unwrap_or(""), p.item_prefix.as_deref().unwrap_or("ST"), t],
            )?;
            w.c.run(
                "INSERT INTO workspace_members (workspace_id, user_id, role, created) VALUES (?, ?, 'owner', ?)",
                params![id, owner_id, t],
            )?;
            super::kinds::ensure(w, &id)?;
            w.publish(ChangeEvent::Workspace { id: id.clone() });
            must(one(w.c, "SELECT * FROM workspaces WHERE id = ?", [&id], Workspace::from_row)?)
        })
    }

    pub fn patch(&self, id: &str, p: &WorkspacePatch) -> Result<Option<Workspace>> {
        self.0.write(|w| {
            let mut s = Sets::default();
            s.val("name", p.name.clone())
                .val("description", p.description.clone())
                .val("item_prefix", p.item_prefix.clone());
            if !s.is_empty() {
                w.c.run(
                    &format!("UPDATE workspaces SET {} WHERE id = ?", s.sql()),
                    rusqlite::params_from_iter(s.params([id.to_string().into()])),
                )?;
            }
            w.publish(ChangeEvent::Workspace { id: id.into() });
            one(
                w.c,
                "SELECT * FROM workspaces WHERE id = ?",
                [id],
                Workspace::from_row,
            )
        })
    }

    pub fn is_member(&self, id: &str, user_id: &str) -> Result<bool> {
        self.0.read(|c| {
            exists(
                c,
                "SELECT 1 FROM workspace_members WHERE workspace_id = ? AND user_id = ?",
                [id, user_id],
            )
        })
    }
}

pub struct Users<'a>(&'a Store);

impl Users<'_> {
    pub fn count(&self) -> Result<i64> {
        self.0
            .read(|c| Ok(c.row("SELECT count(*) n FROM users", [], |r| r.get(0))?))
    }

    pub fn by_subject(&self, sub: &str) -> Result<Option<User>> {
        self.0.read(|c| {
            one(
                c,
                "SELECT * FROM users WHERE subject = ?",
                [sub],
                User::from_row,
            )
        })
    }

    pub fn get(&self, id: &str) -> Result<Option<User>> {
        self.0
            .read(|c| one(c, "SELECT * FROM users WHERE id = ?", [id], User::from_row))
    }

    /// Creates the user in workspace `default` with a membership of the same role; returns the id. No event.
    pub fn create(&self, sub: &str, name: &str, email: Option<&str>, role: &str) -> Result<String> {
        self.0.write(|w| {
            let (id, t) = (uid("usr"), now());
            w.c.run(
                "INSERT INTO users (id, workspace_id, subject, name, email, role, created) VALUES (?, ?, ?, ?, ?, ?, ?)",
                params![id, WS, sub, name, email, role, t],
            )?;
            w.c.run(
                "INSERT INTO workspace_members (workspace_id, user_id, role, created) VALUES (?, ?, ?, ?)",
                params![WS, id, role, t],
            )?;
            Ok(id)
        })
    }
}

/// Browser login sessions; only `sha(token)` is stored.
pub struct AuthSessions<'a>(&'a Store);

impl AuthSessions<'_> {
    pub fn create(&self, user_id: &str, token: &str, ttl_ms: i64) -> Result<()> {
        self.0.write(|w| {
            w.c.run(
                "INSERT INTO auth_sessions (token_hash, user_id, created, expires) VALUES (?, ?, ?, ?)",
                params![sha(token), user_id, now(), now() + ttl_ms],
            )?;
            Ok(())
        })
    }

    /// The user behind an unexpired token.
    pub fn user(&self, token: &str) -> Result<Option<User>> {
        self.0.read(|c| {
            one(
                c,
                "SELECT u.* FROM auth_sessions s JOIN users u ON u.id = s.user_id WHERE s.token_hash = ? AND s.expires > ?",
                params![sha(token), now()],
                User::from_row,
            )
        })
    }

    pub fn drop(&self, token: &str) -> Result<()> {
        self.0.write(|w| {
            w.c.run(
                "DELETE FROM auth_sessions WHERE token_hash = ?",
                [sha(token)],
            )?;
            Ok(())
        })
    }
}

/// One JSON value per key and workspace.
pub struct Settings<'a>(&'a Store);

impl Settings<'_> {
    /// Every stored key, values parsed (unparsable → null).
    pub fn all(&self, ws: &str) -> Result<Map<String, Value>> {
        self.0.read(|c| {
            let rows: Vec<(String, Option<String>)> = all(
                c,
                "SELECT key, value FROM settings WHERE workspace_id = ?",
                [ws],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            Ok(rows
                .into_iter()
                .map(|(k, v)| (k, json_or(v, Value::Null)))
                .collect())
        })
    }

    /// The stored value decoded as `T`, or `d` if missing or not decodable.
    pub fn get<T: DeserializeOwned>(&self, ws: &str, key: &str, d: T) -> Result<T> {
        self.0.read(|c| {
            let v: Option<Option<String>> = one(
                c,
                "SELECT value FROM settings WHERE workspace_id = ? AND key = ?",
                [ws, key],
                |r| r.get(0),
            )?;
            Ok(match v {
                Some(v) => json_or(v, d),
                None => d,
            })
        })
    }

    /// Upserts every key in one transaction, records a revision and publishes `settings`.
    pub fn set(&self, ws: &str, p: &Map<String, Value>, author: &str) -> Result<()> {
        self.0.write(|w| {
            let tx = w.tx()?;
            {
                let mut st = tx.prepare_cached(
                    "INSERT INTO settings (workspace_id, key, value, updated) VALUES (?, ?, ?, ?) ON CONFLICT(workspace_id, key) DO UPDATE SET value = excluded.value, updated = excluded.updated",
                )?;
                for (k, v) in p {
                    st.execute(params![ws, k, to_json(v)?, now()])?;
                }
            }
            tx.commit()?;
            revision(w.c, ws, "settings", ws, 0, p, author)?;
            w.publish(ChangeEvent::Settings { id: ws.into() });
            Ok(())
        })
    }
}
