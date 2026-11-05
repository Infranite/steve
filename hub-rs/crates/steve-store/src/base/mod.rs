//! The shared base of the new modules (migration 4): visibility scopes, disclosure
//! levels, memories, entity tags, generic links, persisted changesets, decisions opened inside a
//! transaction, audit and revisions.
//!
//! Everything here is a free function over `&Connection` (reads, usually from [`Store::reader`]) or
//! [`Tx`] (writes, from [`Store::transaction`]), so a module service can combine its own SQL and these
//! in one transaction. Writes queue their SSE event on the `Tx`; it goes out after the commit.
//!
//! [`Store::reader`]: crate::Store::reader
//! [`Store::transaction`]: crate::Store::transaction

pub mod changesets;
pub mod decisions;
pub mod links;
pub mod mems;
pub mod tags;

use std::fmt;

use rusqlite::types::Value as Sql;
use rusqlite::{Connection, params};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Value, json};

use crate::repo::to_json;
use crate::{Result, Tx, now};

/// Who can see an object. Stored and sent as its code: `workspace`, `private`, `team:<id>`, `space:<id>`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Visibility {
    /// Every member of the workspace.
    Workspace,
    /// Only `created_by`.
    Private,
    /// Users whose contact is a member of the team or one of its sub-teams, plus the creator.
    Team(String),
    /// Owners of the space's member accounts, plus the creator (channels module).
    Space(String),
}

impl Visibility {
    /// Parses a code; `None` for anything else (callers answer `400 参数不对：visibility`).
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "workspace" => Some(Self::Workspace),
            "private" => Some(Self::Private),
            _ => {
                let (k, id) = s.split_once(':')?;
                if id.is_empty() || id.len() > 64 {
                    return None;
                }
                match k {
                    "team" => Some(Self::Team(id.into())),
                    "space" => Some(Self::Space(id.into())),
                    _ => None,
                }
            }
        }
    }

    /// For rows: an unreadable code is treated as `private`, the narrowest scope.
    pub fn from_db(s: &str) -> Self {
        Self::parse(s).unwrap_or(Self::Private)
    }

    pub fn code(&self) -> String {
        match self {
            Self::Workspace => "workspace".into(),
            Self::Private => "private".into(),
            Self::Team(id) => format!("team:{id}"),
            Self::Space(id) => format!("space:{id}"),
        }
    }
}

impl fmt::Display for Visibility {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.code())
    }
}

impl Serialize for Visibility {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&self.code())
    }
}

impl<'de> Deserialize<'de> for Visibility {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::parse(&s).ok_or_else(|| serde::de::Error::custom("unknown visibility"))
    }
}

/// Disclosure level of a fact. `Never` content never leaves the database: not in search results,
/// MCP answers or prompts. The other three travel with their level so the channel module can check
/// what an outgoing message reveals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    /// 公开: can be quoted.
    Public,
    /// 可转述结论: state conclusions only.
    Conclusion,
    /// 仅用于推理: use for reasoning, never mention.
    Reasoning,
    /// 不可使用.
    Never,
}

impl Level {
    pub const ALL: [Level; 4] = [
        Level::Public,
        Level::Conclusion,
        Level::Reasoning,
        Level::Never,
    ];

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|l| l.code() == s)
    }

    /// For rows: an unreadable code is treated as `never`, so it is withheld rather than leaked.
    pub fn from_db(s: &str) -> Self {
        Self::parse(s).unwrap_or(Self::Never)
    }

    pub fn code(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Conclusion => "conclusion",
            Self::Reasoning => "reasoning",
            Self::Never => "never",
        }
    }
}

/// The line `steve_contacts_find` and other prompt builders end with (verbatim).
pub const LEVELS_NOTE: &str = "Levels: public = can be quoted; conclusion = state conclusions only; reasoning = use for reasoning, never mention.";

/// Decision kinds. The modules add exactly one, `confirm` (确认候选, options `yes`/`no`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DecisionKind {
    Accept,
    Ask,
    Approve,
    Fail,
    Propose,
    Confirm,
}

impl DecisionKind {
    pub fn code(self) -> &'static str {
        match self {
            Self::Accept => "accept",
            Self::Ask => "ask",
            Self::Approve => "approve",
            Self::Fail => "fail",
            Self::Propose => "propose",
            Self::Confirm => "confirm",
        }
    }
}

/// The options of every `confirm` decision.
pub const CONFIRM_OPTIONS: [&str; 2] = ["yes", "no"];

/// The user an object is filtered for, with the team/space scopes that make `team:`/`space:` objects
/// visible to them. Build it once per request with [`Viewer::load`]; spaces are added by the channels
/// module through [`Viewer::with_scopes`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Viewer {
    pub ws: String,
    pub user: String,
    /// `team:<id>` / `space:<id>` codes this user belongs to.
    pub scopes: Vec<String>,
}

impl Viewer {
    /// A viewer without team or space scopes (sees `workspace` objects and their own).
    pub fn bare(ws: &str, user: &str) -> Self {
        Self {
            ws: ws.into(),
            user: user.into(),
            scopes: Vec::new(),
        }
    }

    /// Loads the team scopes: every team the user's contact (“我”, `parties.user_id`) is a direct member
    /// of, plus all their ancestors, because a member of a sub-team counts as a member of the parent.
    pub fn load(c: &Connection, ws: &str, user: &str) -> Result<Self> {
        let mut st = c.prepare_cached(
            "WITH RECURSIVE mine(id) AS (
               SELECT tm.team_id FROM team_members tm JOIN parties p ON p.id = tm.contact_id
                WHERE tm.workspace_id = ?1 AND p.workspace_id = ?1 AND p.user_id = ?2 AND p.status = 'active'
               UNION
               SELECT td.parent_id FROM team_details td JOIN mine ON td.party_id = mine.id
                WHERE td.parent_id IS NOT NULL)
             SELECT id FROM mine ORDER BY id",
        )?;
        let scopes = st
            .query_map(params![ws, user], |r| r.get::<_, String>(0))?
            .map(|r| r.map(|id| format!("team:{id}")))
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(Self {
            ws: ws.into(),
            user: user.into(),
            scopes,
        })
    }

    /// Adds scopes from other modules (e.g. `space:<id>`), keeping them unique.
    pub fn with_scopes(mut self, extra: impl IntoIterator<Item = String>) -> Self {
        for s in extra {
            if !self.scopes.contains(&s) {
                self.scopes.push(s);
            }
        }
        self
    }

    /// Can this viewer see an object with this visibility, created by `created_by`?
    pub fn sees(&self, visibility: &Visibility, created_by: &str) -> bool {
        match visibility {
            Visibility::Workspace => true,
            _ if created_by == self.user => true,
            Visibility::Private => false,
            v => self.scopes.contains(&v.code()),
        }
    }

    /// The visibility condition for rows of table alias `alias` (columns `visibility`, `created_by`).
    /// Workspace scoping is separate: the caller still filters `workspace_id`.
    pub fn sql(&self, alias: &str) -> Scope {
        let mut sql = format!("({alias}.visibility = 'workspace' OR {alias}.created_by = ?");
        let mut params = vec![Sql::Text(self.user.clone())];
        if !self.scopes.is_empty() {
            sql.push_str(&format!(
                " OR {alias}.visibility IN ({})",
                vec!["?"; self.scopes.len()].join(", ")
            ));
            params.extend(self.scopes.iter().cloned().map(Sql::Text));
        }
        sql.push(')');
        Scope { sql, params }
    }
}

/// `visible_sql(alias, me)`: an SQL condition and its positional parameters, to splice into a
/// query at the place where `sql` appears.
#[derive(Debug, Clone, PartialEq)]
pub struct Scope {
    pub sql: String,
    pub params: Vec<Sql>,
}

/// Shorthand for [`Viewer::sql`].
pub fn visible_sql(alias: &str, viewer: &Viewer) -> Scope {
    viewer.sql(alias)
}

/// A typed object reference (`{type, id}` in the API).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ObjRef {
    #[serde(rename = "type")]
    pub kind: String,
    pub id: String,
}

impl ObjRef {
    pub fn new(kind: &str, id: &str) -> Self {
        Self {
            kind: kind.into(),
            id: id.into(),
        }
    }
}

/// One line of a draft preview, the same shape as the in-memory drafts (`{label, from, to}`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    pub label: String,
    pub from: String,
    pub to: String,
}

impl Change {
    pub fn new(label: impl Into<String>, from: impl Into<String>, to: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            from: from.into(),
            to: to.into(),
        }
    }
}

/// `show(v)` from changes.ts: empty → “（空）”, longer than 40 characters → cut with “…”.
pub fn show(v: &str) -> String {
    if v.is_empty() {
        return "（空）".into();
    }
    // JS counts UTF-16 units; for the CJK and Latin text shown here that is the char count.
    if v.encode_utf16().count() > 40 {
        let mut out = String::new();
        let mut n = 0;
        for ch in v.chars() {
            n += ch.len_utf16();
            if n > 40 {
                break;
            }
            out.push(ch);
        }
        out.push('…');
        return out;
    }
    v.into()
}

/// `audit(ws, actor, action, target, detail)` inside a transaction. `action` is `<object>.<verb>`;
/// `detail` must not contain secret values or message text.
pub fn audit(
    tx: &Tx,
    ws: &str,
    actor: &str,
    action: &str,
    target: &str,
    detail: &Value,
) -> Result<()> {
    tx.prepare_cached(
        "INSERT INTO audit (workspace_id, ts, actor, action, target, detail) VALUES (?, ?, ?, ?, ?, ?)",
    )?
    .execute(params![ws, now(), actor, action, target, to_json(detail)?])?;
    Ok(())
}

/// Writes `revisions(object_type, object_id, version, patch={before,after}, author)`. The caller
/// bumps the object's own `revision` column to `version` in the same transaction.
#[allow(clippy::too_many_arguments)]
pub fn revision(
    tx: &Tx,
    ws: &str,
    object_type: &str,
    object_id: &str,
    version: i64,
    before: &Value,
    after: &Value,
    author: &str,
) -> Result<()> {
    crate::repo::revision(
        tx,
        ws,
        object_type,
        object_id,
        version,
        &json!({ "before": before, "after": after }),
        author,
    )
}

/// Revisions of one object, newest first: `(version, patch, author, created)`.
pub fn revisions(
    c: &Connection,
    ws: &str,
    object_type: &str,
    object_id: &str,
) -> Result<Vec<(i64, Value, String, i64)>> {
    let mut st = c.prepare_cached(
        "SELECT version, patch, author, created FROM revisions WHERE workspace_id = ? AND object_type = ? AND object_id = ? ORDER BY id DESC",
    )?;
    let rows = st.query_map(params![ws, object_type, object_id], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            crate::types::json_or(r.get(1)?, Value::Null),
            r.get::<_, String>(2)?,
            r.get::<_, i64>(3)?,
        ))
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// `rusqlite` parameters from a list of SQL values (for queries built around a [`Scope`]).
pub fn bind(vals: Vec<Sql>) -> rusqlite::ParamsFromIter<Vec<Sql>> {
    rusqlite::params_from_iter(vals)
}
