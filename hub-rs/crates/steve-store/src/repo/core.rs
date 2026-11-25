//! Workspaces, users, auth sessions, settings, counters, audit.

use super::Cached;
use rusqlite::params;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

use super::{Sets, all, exists, must, one, revision, to_json};
use crate::types::json_or;
use crate::{MemberWorkspace, Result, Store, User, WS, Workspace, now, sha, uid};

/// One entry of the audit trail: who did what to what, and the detail the writer kept.
#[derive(Debug, Clone, Serialize)]
pub struct AuditEntry {
    pub id: i64,
    pub ts: i64,
    pub actor: String,
    pub action: String,
    pub target: String,
    pub detail: Value,
}

/// A page of [`Store::audit_page`].
#[derive(Debug, Clone, Default, Serialize)]
pub struct AuditPage {
    pub entries: Vec<AuditEntry>,
    pub names: BTreeMap<String, String>,
    pub more: bool,
}

/// What an id of an audit entry is called for the reader `?2`: the first of the things an entry can
/// point at that has it. A memory goes by the start of its text. A contact or a memory that is not
/// the whole workspace's is named only for whoever made it: the trail says that something was
/// written, not what another member keeps to themselves.
const NAME_OF: &str = "SELECT COALESCE(
    (SELECT NULLIF(name, '') FROM parties WHERE user_id = ?1 AND workspace_id = ?3 AND status = 'active' LIMIT 1),
    (SELECT NULLIF(name, '') FROM users WHERE id = ?1),
    (SELECT NULLIF(name, '') FROM bots WHERE id = ?1),
    (SELECT NULLIF(name, '') FROM parties WHERE id = ?1 AND (visibility = 'workspace' OR created_by = ?2)),
    (SELECT NULLIF(name, '') FROM projects WHERE id = ?1),
    (SELECT NULLIF(title, '') FROM items WHERE id = ?1),
    (SELECT NULLIF(name, '') FROM access_tokens WHERE id = ?1),
    (SELECT NULLIF(name, '') FROM clients WHERE id = ?1),
    (SELECT NULLIF(substr(text, 1, 80), '') FROM mems WHERE id = ?1 AND (visibility = 'workspace' OR created_by = ?2)),
    (SELECT NULLIF(name, '') FROM flows WHERE id = ?1),
    (SELECT NULLIF(name, '') FROM devices WHERE id = ?1),
    (SELECT NULLIF(name, '') FROM nodes WHERE node_id = ?1),
    (SELECT NULLIF(name, '') FROM workspaces WHERE id = ?1))";

/// A value of an entry's detail that may be the id of something: `prefix_` and a generated tail.
fn id_like(v: &str) -> bool {
    v.split_once('_').is_some_and(|(kind, tail)| {
        !kind.is_empty()
            && kind.bytes().all(|b| b.is_ascii_lowercase())
            && tail.len() >= 8
            && tail.bytes().all(|b| b.is_ascii_hexdigit())
    })
}

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
        self.write_conn(|w| {
            w.c.prepare_cached("INSERT INTO audit (workspace_id, ts, actor, action, target, detail) VALUES (?, ?, ?, ?, ?, ?)")?
                .execute(params![ws, now(), actor, action, target, to_json(detail)?])?;
            Ok(())
        })
    }

    /// One page of a workspace's audit trail, newest first: the entries before `before` (an entry's
    /// id; `None` starts at the newest), at most `limit`, and whether older ones remain. `names` says
    /// what the ids in the page are called now, for `reader` (a user id) who is shown it: the actor
    /// and target of every entry and the ids its detail refers to. An id nothing is called by any
    /// more, or whose name is not the reader's to see, is not in it.
    pub fn audit_page(
        &self,
        ws: &str,
        reader: &str,
        before: Option<i64>,
        limit: i64,
    ) -> Result<AuditPage> {
        let limit = limit.clamp(1, 200);
        self.read_conn(|c| {
            let mut entries = all(
                c,
                "SELECT id, ts, actor, action, target, detail FROM audit WHERE workspace_id = ? AND id < ? ORDER BY id DESC LIMIT ?",
                params![ws, before.unwrap_or(i64::MAX), limit + 1],
                |r| {
                    Ok(AuditEntry {
                        id: r.get(0)?,
                        ts: r.get(1)?,
                        actor: r.get(2)?,
                        action: r.get(3)?,
                        target: r.get(4)?,
                        detail: json_or(r.get(5)?, Value::Null),
                    })
                },
            )?;
            let more = entries.len() as i64 > limit;
            entries.truncate(limit as usize);
            let mut names = BTreeMap::new();
            for e in &entries {
                let told = e.detail.as_object().into_iter().flatten();
                let ids = told.filter_map(|(_, v)| v.as_str()).filter(|v| id_like(v));
                for id in [e.actor.as_str(), e.target.as_str()].into_iter().chain(ids) {
                    if id.is_empty() || names.contains_key(id) {
                        continue;
                    }
                    if let Some(name) = one(c, NAME_OF, [id, reader, ws], |r| r.get::<_, Option<String>>(0))?.flatten() {
                        names.insert(id.to_string(), name);
                    }
                }
            }
            Ok(AuditPage { entries, names, more })
        })
    }

    /// `peekCounter`: the last number handed out for `name` (e.g. `item:ST`), 0 if none.
    pub fn peek_counter(&self, name: &str) -> Result<i64> {
        self.read_conn(|c| {
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
        self.write(|_| self.write_conn(|w| {
            let c = w.c;
            let name = crate::seeded::text(crate::seeded::WORKSPACE_NAME);
            let created = c.execute("INSERT OR IGNORE INTO workspaces (id, name, created) VALUES (?, ?, ?)", params![WS, name, now()])? == 1;
            // T47: move this cleanup into the next new-layout migration, then remove it.
            c.run("DELETE FROM settings WHERE key = '__delivery_tags_backfilled_v1'", [])?;
            c.run("UPDATE workspaces SET name = ? WHERE id = ? AND name = '默认工作空间'", params![name, WS])?;
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
            if created {
                initialize_notice_cursor(c, WS)?;
                super::kinds::seed_deliveries(w, WS)?;
            }
            Ok(())
        }))
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

pub const NOTIFY_SEEN: &str = "notifySeen";

fn initialize_notice_cursor(c: &rusqlite::Connection, ws: &str) -> Result<()> {
    let at = now();
    c.execute(
        "INSERT INTO settings (workspace_id, key, value, updated) VALUES (?, ?, ?, ?)",
        params![ws, NOTIFY_SEEN, at.to_string(), at],
    )?;
    Ok(())
}

pub struct Workspaces<'a>(&'a Store);

impl Workspaces<'_> {
    pub fn get(&self, id: &str) -> Result<Option<Workspace>> {
        self.0.read_conn(|c| {
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
        self.0.read_conn(|c| {
            all(
                c,
                "SELECT w.*, m.role FROM workspaces w JOIN workspace_members m ON m.workspace_id = w.id WHERE m.user_id = ? ORDER BY w.sort, w.created",
                [user_id],
                |r| Ok(MemberWorkspace { workspace: Workspace::from_row(r)?, role: r.get("role")? }),
            )
        })
    }

    pub fn all(&self) -> Result<Vec<Workspace>> {
        self.0.read_conn(|c| {
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
            .read_conn(|c| all(c, "SELECT id FROM workspaces", [], |r| r.get(0)))
    }

    /// New workspace at the end of the sort order, `owner_id` as its owner.
    pub fn create(&self, p: &NewWorkspace, owner_id: &str) -> Result<Workspace> {
        self.0.write(|_| self.0.write_conn(|w| {
            let (id, t) = (uid("ws"), now());
            w.c.run(
                "INSERT INTO workspaces (id, name, description, item_prefix, sort, created) VALUES (?, ?, ?, ?, (SELECT coalesce(max(sort), 0) + 1 FROM workspaces), ?)",
                params![id, p.name, p.description.as_deref().unwrap_or(""), p.item_prefix.as_deref().unwrap_or("ST"), t],
            )?;
            w.c.run(
                "INSERT INTO workspace_members (workspace_id, user_id, role, created) VALUES (?, ?, 'owner', ?)",
                params![id, owner_id, t],
            )?;
            ensure_member_party(w.c, &id, owner_id, None, t)?;
            initialize_notice_cursor(w.c, &id)?;
            super::kinds::ensure(w, &id)?;
            super::kinds::seed_deliveries(w, &id)?;
            w.publish(steve_types::DomainEvent::from(
                steve_types::ConfigEvent::Workspace {
                    ws: id.clone().into(),
                    id: id.clone().into(),
                },
            ));
            must(one(w.c, "SELECT * FROM workspaces WHERE id = ?", [&id], Workspace::from_row)?)
        }))
    }

    pub fn patch(&self, id: &str, p: &WorkspacePatch) -> Result<Option<Workspace>> {
        self.0.write_conn(|w| {
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
            w.publish(steve_types::DomainEvent::from(
                steve_types::ConfigEvent::Workspace {
                    ws: id.into(),
                    id: id.into(),
                },
            ));
            one(
                w.c,
                "SELECT * FROM workspaces WHERE id = ?",
                [id],
                Workspace::from_row,
            )
        })
    }

    pub fn is_member(&self, id: &str, user_id: &str) -> Result<bool> {
        self.0.read_conn(|c| {
            exists(
                c,
                "SELECT 1 FROM workspace_members WHERE workspace_id = ? AND user_id = ?",
                [id, user_id],
            )
        })
    }
}

pub struct Users<'a>(&'a Store);

/// Ensures a workspace member has a linked contact row in `parties` (idempotent).
fn ensure_member_party(
    c: &rusqlite::Connection,
    ws: &str,
    user_id: &str,
    fallback_name: Option<&str>,
    created: i64,
) -> Result<()> {
    let exists: Option<String> = one(
        c,
        "SELECT id FROM parties WHERE workspace_id = ? AND user_id = ? LIMIT 1",
        params![ws, user_id],
        |r| r.get(0),
    )?;
    if exists.is_some() {
        return Ok(());
    }
    let name = fallback_name
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            one(
                c,
                "SELECT NULLIF(trim(name), '') FROM users WHERE id = ?",
                [user_id],
                |r| r.get::<_, Option<String>>(0),
            )
            .ok()
            .flatten()
            .flatten()
        })
        .unwrap_or_else(|| "我".into());
    let pid = uid("ct");
    c.execute(
        "INSERT INTO parties (
            id, workspace_id, type, name, aliases, avatar, rels, important, note,
            contact_kind, org_kind, user_id, visibility, status, merged_into,
            source, source_ref, revision, created_by, created, updated
        ) VALUES (?, ?, 'contact', ?, '[]', '{}', '[\"me\"]', 0, '', 'real', NULL, ?, 'workspace', 'active', NULL, 'manual', NULL, 1, ?, ?, ?)",
        params![pid, ws, name, user_id, user_id, created, created],
    )?;
    Ok(())
}

impl Users<'_> {
    pub fn count(&self) -> Result<i64> {
        self.0
            .read_conn(|c| Ok(c.row("SELECT count(*) n FROM users", [], |r| r.get(0))?))
    }

    pub fn by_subject(&self, sub: &str) -> Result<Option<User>> {
        self.0.read_conn(|c| {
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
            .read_conn(|c| one(c, "SELECT * FROM users WHERE id = ?", [id], User::from_row))
    }

    /// Person display name for a user in a workspace (linked contact name, fallback to login name).
    pub fn display_name(&self, ws: &str, id: &str) -> Result<Option<String>> {
        self.0.read(|r| crate::UserRows(r).display_name(ws, id))
    }

    /// All workspace members with their unified person identities.
    pub fn member_identities(&self, ws: &str) -> Result<Vec<steve_types::MemberIdentity>> {
        self.0.read(|r| crate::UserRows(r).member_identities(ws))
    }

    /// One workspace member's unified person identity.
    pub fn member_identity(
        &self,
        ws: &str,
        id: &str,
    ) -> Result<Option<steve_types::MemberIdentity>> {
        self.0.read(|r| crate::UserRows(r).member_identity(ws, id))
    }

    /// Creates the user in workspace `default` with a membership of the same role and a linked contact; returns the id.
    pub fn create(&self, sub: &str, name: &str, email: Option<&str>, role: &str) -> Result<String> {
        self.0.write_conn(|w| {
            let (id, t) = (uid("usr"), now());
            w.c.run(
                "INSERT INTO users (id, workspace_id, subject, name, email, role, created) VALUES (?, ?, ?, ?, ?, ?, ?)",
                params![id, WS, sub, name, email, role, t],
            )?;
            w.c.run(
                "INSERT INTO workspace_members (workspace_id, user_id, role, created) VALUES (?, ?, ?, ?)",
                params![WS, id, role, t],
            )?;
            ensure_member_party(w.c, WS, &id, Some(name), t)?;
            if role == "owner" {
                w.publish(steve_types::DomainEvent::from(steve_types::ConfigEvent::Workspace {
                    id: WS.into(),
                    ws: WS.into(),
                }));
            }
            Ok(id)
        })
    }
}

/// Browser login sessions; only `sha(token)` is stored.
pub struct AuthSessions<'a>(&'a Store);

impl AuthSessions<'_> {
    pub fn create(&self, user_id: &str, token: &str, ttl_ms: i64) -> Result<()> {
        self.0.write_conn(|w| {
            w.c.run(
                "INSERT INTO auth_sessions (token_hash, user_id, created, expires) VALUES (?, ?, ?, ?)",
                params![sha(token), user_id, now(), now() + ttl_ms],
            )?;
            Ok(())
        })
    }

    /// The user behind an unexpired token.
    pub fn user(&self, token: &str) -> Result<Option<User>> {
        self.0.read_conn(|c| {
            one(
                c,
                "SELECT u.* FROM auth_sessions s JOIN users u ON u.id = s.user_id WHERE s.token_hash = ? AND s.expires > ?",
                params![sha(token), now()],
                User::from_row,
            )
        })
    }

    pub fn drop(&self, token: &str) -> Result<()> {
        self.0.write_conn(|w| {
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
    /// Operator settings, values parsed (unparsable → null).
    pub fn all(&self, ws: &str) -> Result<Map<String, Value>> {
        self.0.read_conn(|c| {
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
        self.0.read_conn(|c| {
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
        self.0.write_conn(|w| {
            let transaction = w.tx()?;
            let tx = transaction.connection();
            {
                let mut st = tx.prepare_cached(
                    "INSERT INTO settings (workspace_id, key, value, updated) VALUES (?, ?, ?, ?) ON CONFLICT(workspace_id, key) DO UPDATE SET value = excluded.value, updated = excluded.updated",
                )?;
                for (k, v) in p {
                    st.execute(params![ws, k, to_json(v)?, now()])?;
                }
            }
            transaction.commit()?;
            revision(w.c, ws, "settings", ws, 0, p, author)?;
            w.publish(steve_types::DomainEvent::from(
                steve_types::ConfigEvent::Settings {
                    ws: ws.into(),
                    id: ws.into(),
                },
            ));
            Ok(())
        })
    }

    /// Whether a one-time installation maintenance task (`workspace_id = '*'`) has already finished.
    pub fn maintenance_done(&self, key: &str) -> Result<bool> {
        self.0.read_conn(|c| {
            exists(
                c,
                "SELECT 1 FROM settings WHERE workspace_id = '*' AND key = ?",
                [key],
            )
        })
    }

    /// Record completion of a one-time installation maintenance task (`workspace_id = '*'`).
    pub fn mark_maintenance_done(&self, key: &str, detail: &Value) -> Result<()> {
        self.0.write_conn(|w| {
            w.c.run(
                "INSERT INTO settings (workspace_id, key, value, updated) VALUES ('*', ?, ?, ?) ON CONFLICT(workspace_id, key) DO UPDATE SET value = excluded.value, updated = excluded.updated",
                params![key, to_json(detail)?, now()],
            )?;
            Ok(())
        })
    }
}
