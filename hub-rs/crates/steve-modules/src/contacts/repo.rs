//! SQL of the contacts module (contacts.md §1.2): parties, organization domains, attribute definitions
//! and values, and the reads the API shapes need. Every list of parties goes through a [`Viewer`]
//! (§8: visibility is filtered here, never in a handler). Writes take a [`Tx`]; events are published by
//! the service, once per request.

use rusqlite::types::Value as Sql;
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use steve_store::base::{Level, Viewer, Visibility, bind};
use steve_store::{Cached, Result, Tx, now, uid};

use crate::Owned;

/// `object_type` of parties in `links`, `mem_about` and `entity_tags`.
pub const PARTY: &str = "party";

fn json_or<T: DeserializeOwned>(s: Option<String>, d: T) -> T {
    match s.as_deref() {
        None | Some("") => d,
        Some(s) => serde_json::from_str(s).unwrap_or(d),
    }
}

fn to_json(v: &impl Serialize) -> Result<String> {
    Ok(serde_json::to_string(v)?)
}

/// A `parties` row: a contact, an organization or a team.
#[derive(Debug, Clone, PartialEq)]
pub struct Party {
    pub id: String,
    pub workspace_id: String,
    /// `contact` | `org` | `team`
    pub kind: String,
    pub name: String,
    pub aliases: Vec<String>,
    pub avatar: Value,
    pub rels: Vec<String>,
    pub important: bool,
    pub note: String,
    /// contacts: `real` | `virtual` | `unconfirmed`
    pub contact_kind: Option<String>,
    /// organizations: `self` | `customer` | `supplier` | `partner` | `other`
    pub org_kind: Option<String>,
    /// Set only on “我”.
    pub user_id: Option<String>,
    pub visibility: Visibility,
    /// `active` | `merged` | `deleted` | `dissolved`
    pub status: String,
    pub merged_into: Option<String>,
    pub source: String,
    pub source_ref: Option<String>,
    pub revision: i64,
    pub created_by: String,
    pub created: i64,
    pub updated: i64,
}

impl Party {
    pub(crate) fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get("id")?,
            workspace_id: r.get("workspace_id")?,
            kind: r.get("type")?,
            name: r.get("name")?,
            aliases: json_or(r.get("aliases")?, Vec::new()),
            avatar: json_or(r.get("avatar")?, Value::Object(Default::default())),
            rels: json_or(r.get("rels")?, Vec::new()),
            important: r.get::<_, i64>("important")? != 0,
            note: r.get("note")?,
            contact_kind: r.get("contact_kind")?,
            org_kind: r.get("org_kind")?,
            user_id: r.get("user_id")?,
            visibility: Visibility::from_db(&r.get::<_, String>("visibility")?),
            status: r.get("status")?,
            merged_into: r.get("merged_into")?,
            source: r.get("source")?,
            source_ref: r.get("source_ref")?,
            revision: r.get("revision")?,
            created_by: r.get("created_by")?,
            created: r.get("created")?,
            updated: r.get("updated")?,
        })
    }

    pub fn is_me(&self) -> bool {
        self.user_id.is_some()
    }

    pub fn is_self_org(&self) -> bool {
        self.org_kind.as_deref() == Some("self")
    }
}

impl Owned for Party {
    fn workspace_id(&self) -> &str {
        &self.workspace_id
    }
    fn visibility(&self) -> Option<&Visibility> {
        Some(&self.visibility)
    }
    fn created_by(&self) -> &str {
        &self.created_by
    }
}

/// A party to insert.
#[derive(Debug, Clone)]
pub struct NewParty {
    pub kind: String,
    pub name: String,
    pub aliases: Vec<String>,
    pub rels: Vec<String>,
    pub important: bool,
    pub note: String,
    pub contact_kind: Option<String>,
    pub org_kind: Option<String>,
    pub user_id: Option<String>,
    pub visibility: Visibility,
    pub source: String,
    pub source_ref: Option<String>,
    pub created_by: String,
}

impl NewParty {
    pub fn new(kind: &str, name: &str, created_by: &str) -> Self {
        Self {
            kind: kind.into(),
            name: name.into(),
            aliases: Vec::new(),
            rels: Vec::new(),
            important: false,
            note: String::new(),
            contact_kind: None,
            org_kind: None,
            user_id: None,
            visibility: Visibility::Workspace,
            source: "manual".into(),
            source_ref: None,
            created_by: created_by.into(),
        }
    }
}

/// Id prefix per party type (§0.3).
fn prefix(kind: &str) -> &'static str {
    match kind {
        "org" => "org",
        "team" => "tm",
        _ => "ct",
    }
}

pub fn get(c: &Connection, id: &str) -> Result<Option<Party>> {
    Ok(c.prepare_cached("SELECT * FROM parties WHERE id = ?")?
        .query_row([id], Party::from_row)
        .optional()?)
}

/// “我” of a user in a workspace.
pub fn me_of(c: &Connection, ws: &str, user: &str) -> Result<Option<Party>> {
    Ok(
        c.prepare_cached("SELECT * FROM parties WHERE workspace_id = ? AND user_id = ?")?
            .query_row(params![ws, user], Party::from_row)
            .optional()?,
    )
}

/// The workspace's own organization (`org_kind = self`), if created.
pub fn self_org(c: &Connection, ws: &str) -> Result<Option<Party>> {
    Ok(c
        .prepare_cached(
            "SELECT * FROM parties WHERE workspace_id = ? AND type = 'org' AND org_kind = 'self' AND status = 'active' ORDER BY created LIMIT 1",
        )?
        .query_row([ws], Party::from_row)
        .optional()?)
}

pub fn insert(tx: &Tx, ws: &str, p: &NewParty) -> Result<Party> {
    let id = uid(prefix(&p.kind));
    let t = now();
    tx.run(
        "INSERT INTO parties (id, workspace_id, type, name, aliases, rels, important, note, contact_kind, org_kind, user_id, visibility, source, source_ref, created_by, created, updated)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            id,
            ws,
            p.kind,
            p.name,
            to_json(&p.aliases)?,
            to_json(&p.rels)?,
            p.important as i64,
            p.note,
            p.contact_kind,
            p.org_kind,
            p.user_id,
            p.visibility.code(),
            p.source,
            p.source_ref,
            p.created_by,
            t,
            t
        ],
    )?;
    get(tx, &id)?.ok_or(steve_store::StoreError::Sqlite(
        rusqlite::Error::QueryReturnedNoRows,
    ))
}

/// Columns a party edit may set; `None` = unchanged.
#[derive(Debug, Clone, Default)]
pub struct PartyPatch {
    pub name: Option<String>,
    pub aliases: Option<Vec<String>>,
    pub rels: Option<Vec<String>>,
    pub important: Option<bool>,
    pub note: Option<String>,
    pub contact_kind: Option<String>,
    pub org_kind: Option<String>,
    pub visibility: Option<Visibility>,
    pub status: Option<String>,
}

/// Applies a patch; bumps `updated` and, when `revision` is given, sets it. Returns whether a column changed.
pub fn update(tx: &Tx, id: &str, p: &PartyPatch, revision: Option<i64>) -> Result<bool> {
    let mut cols: Vec<&str> = Vec::new();
    let mut vals: Vec<Sql> = Vec::new();
    let mut set = |c: &'static str, v: Option<Sql>| {
        if let Some(v) = v {
            cols.push(c);
            vals.push(v);
        }
    };
    set("name", p.name.clone().map(Sql::Text));
    set(
        "aliases",
        p.aliases.as_ref().map(to_json).transpose()?.map(Sql::Text),
    );
    set(
        "rels",
        p.rels.as_ref().map(to_json).transpose()?.map(Sql::Text),
    );
    set("important", p.important.map(|b| Sql::Integer(b as i64)));
    set("note", p.note.clone().map(Sql::Text));
    set("contact_kind", p.contact_kind.clone().map(Sql::Text));
    set("org_kind", p.org_kind.clone().map(Sql::Text));
    set(
        "visibility",
        p.visibility.as_ref().map(|v| Sql::Text(v.code())),
    );
    set("status", p.status.clone().map(Sql::Text));
    set("revision", revision.map(Sql::Integer));
    if cols.is_empty() {
        return Ok(false);
    }
    cols.push("updated");
    vals.push(Sql::Integer(now()));
    vals.push(Sql::Text(id.into()));
    let sql = format!(
        "UPDATE parties SET {} WHERE id = ?",
        cols.iter()
            .map(|c| format!("{c} = ?"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    tx.execute(&sql, bind(vals))?;
    Ok(true)
}

/// `source` of a party (an unconfirmed contact settled by a sync or a chat).
pub fn set_source(tx: &Tx, id: &str, source: &str) -> Result<()> {
    tx.run(
        "UPDATE parties SET source = ?, updated = ? WHERE id = ?",
        params![source, now(), id],
    )?;
    Ok(())
}

/// `avatar` of a party (`{"url": …}` from a provider's profile).
pub fn set_avatar(tx: &Tx, id: &str, avatar: &Value) -> Result<()> {
    tx.run(
        "UPDATE parties SET avatar = ?, updated = ? WHERE id = ?",
        params![avatar.to_string(), now(), id],
    )?;
    Ok(())
}

/// Only `updated` (attributes and jobs changed the party's page).
pub fn touch(tx: &Tx, id: &str) -> Result<()> {
    tx.run(
        "UPDATE parties SET updated = ? WHERE id = ?",
        params![now(), id],
    )?;
    Ok(())
}

/// Filters of `GET /contacts`.
#[derive(Debug, Clone, Default)]
pub struct ContactQuery {
    pub q: Option<String>,
    pub rel: Option<String>,
    /// `None` = everything but `unconfirmed`.
    pub kind: Option<String>,
    pub org: Option<String>,
    pub tag: Option<String>,
    /// Party ids whose account handles match `q` (from the accounts module).
    pub handle_ids: Vec<String>,
    pub limit: i64,
}

/// `LIKE` pattern for a substring, with `\` as the escape character.
pub fn like(q: &str) -> String {
    let mut s = String::from("%");
    for ch in q.chars() {
        if matches!(ch, '%' | '_' | '\\') {
            s.push('\\');
        }
        s.push(ch);
    }
    s.push('%');
    s
}

/// The condition “`p` has a current `works_at` link to `o`” over aliases `p` (contact) and `l` (link).
const CURRENT_JOB: &str = "l.workspace_id = p.workspace_id AND l.subject_type = 'party' AND l.subject_id = p.id AND l.predicate = 'works_at' AND l.object_type = 'party' AND json_extract(l.props, '$.to') IS NULL";

/// Active contacts visible to `viewer`, `important DESC, name`.
pub fn contacts(c: &Connection, viewer: &Viewer, f: &ContactQuery) -> Result<Vec<Party>> {
    let scope = viewer.sql("p");
    let mut sql = format!(
        "SELECT p.* FROM parties p WHERE p.workspace_id = ? AND p.type = 'contact' AND p.status = 'active' AND {}",
        scope.sql
    );
    let mut ps: Vec<Sql> = vec![viewer.ws.clone().into()];
    ps.extend(scope.params);
    match &f.kind {
        Some(k) => {
            sql.push_str(" AND p.contact_kind = ?");
            ps.push(k.clone().into());
        }
        None => sql.push_str(" AND COALESCE(p.contact_kind, '') != 'unconfirmed'"),
    }
    if let Some(r) = &f.rel {
        sql.push_str(" AND EXISTS (SELECT 1 FROM json_each(p.rels) WHERE value = ?)");
        ps.push(r.clone().into());
    }
    if let Some(o) = &f.org {
        sql.push_str(&format!(
            " AND EXISTS (SELECT 1 FROM links l WHERE {CURRENT_JOB} AND l.object_id = ?)"
        ));
        ps.push(o.clone().into());
    }
    if let Some(t) = &f.tag {
        sql.push_str(" AND EXISTS (SELECT 1 FROM entity_tags t WHERE t.workspace_id = p.workspace_id AND t.object_type = 'party' AND t.object_id = p.id AND t.tag_id = ?)");
        ps.push(t.clone().into());
    }
    if let Some(q) = f.q.as_deref().filter(|q| !q.is_empty()) {
        let pat = like(q);
        sql.push_str(&format!(
            " AND (p.name LIKE ? ESCAPE '\\'
                 OR EXISTS (SELECT 1 FROM json_each(p.aliases) WHERE value LIKE ? ESCAPE '\\')
                 OR EXISTS (SELECT 1 FROM links l JOIN parties o ON o.id = l.object_id WHERE {CURRENT_JOB}
                              AND (o.name LIKE ? ESCAPE '\\' OR json_extract(l.props, '$.title') LIKE ? ESCAPE '\\'))"
        ));
        for _ in 0..4 {
            ps.push(pat.clone().into());
        }
        if !f.handle_ids.is_empty() {
            sql.push_str(&format!(
                " OR p.id IN ({})",
                vec!["?"; f.handle_ids.len()].join(", ")
            ));
            ps.extend(f.handle_ids.iter().cloned().map(Sql::Text));
        }
        sql.push(')');
    }
    sql.push_str(" ORDER BY p.important DESC, p.name, p.id LIMIT ?");
    ps.push(Sql::Integer(f.limit));
    let mut st = c.prepare(&sql)?;
    let rows = st.query_map(bind(ps), Party::from_row)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Active organizations visible to `viewer`, by name (the own organization first).
pub fn orgs(c: &Connection, viewer: &Viewer) -> Result<Vec<Party>> {
    let scope = viewer.sql("p");
    let sql = format!(
        "SELECT p.* FROM parties p WHERE p.workspace_id = ? AND p.type = 'org' AND p.status = 'active' AND {}
         ORDER BY (p.org_kind = 'self') DESC, p.name, p.id",
        scope.sql
    );
    let mut ps: Vec<Sql> = vec![viewer.ws.clone().into()];
    ps.extend(scope.params);
    let mut st = c.prepare(&sql)?;
    let rows = st.query_map(bind(ps), Party::from_row)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Active contacts visible to `viewer` with a current job at `org`: `(id, name, title)`, by name.
pub fn org_people(
    c: &Connection,
    viewer: &Viewer,
    org: &str,
) -> Result<Vec<(String, String, Option<String>)>> {
    let scope = viewer.sql("p");
    let sql = format!(
        "SELECT p.id, p.name, json_extract(l.props, '$.title') FROM parties p JOIN links l ON {CURRENT_JOB} AND l.object_id = ?
         WHERE p.workspace_id = ? AND p.type = 'contact' AND p.status = 'active' AND {}
         ORDER BY p.name, p.id",
        scope.sql
    );
    let mut ps: Vec<Sql> = vec![org.to_string().into(), viewer.ws.clone().into()];
    ps.extend(scope.params);
    let mut st = c.prepare(&sql)?;
    let rows = st.query_map(bind(ps), |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn user_name(c: &Connection, user: &str) -> Result<Option<String>> {
    Ok(c.prepare_cached("SELECT name FROM users WHERE id = ?")?
        .query_row([user], |r| r.get(0))
        .optional()?)
}

pub fn workspace_name(c: &Connection, ws: &str) -> Result<Option<String>> {
    Ok(
        c.prepare_cached("SELECT name FROM workspaces WHERE id = ?")?
            .query_row([ws], |r| r.get(0))
            .optional()?,
    )
}

// ---------------------------------------------------------------- domains

pub fn domains(c: &Connection, ws: &str, org: &str) -> Result<Vec<String>> {
    let mut st = c.prepare_cached(
        "SELECT domain FROM org_domains WHERE workspace_id = ? AND org_id = ? ORDER BY created, domain",
    )?;
    let rows = st.query_map(params![ws, org], |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// The organization a domain belongs to.
pub fn domain_owner(c: &Connection, ws: &str, domain: &str) -> Result<Option<String>> {
    Ok(
        c.prepare_cached("SELECT org_id FROM org_domains WHERE workspace_id = ? AND domain = ?")?
            .query_row(params![ws, domain], |r| r.get(0))
            .optional()?,
    )
}

/// Replaces the domains of an organization (the caller checked conflicts).
pub fn set_domains(tx: &Tx, ws: &str, org: &str, ds: &[String]) -> Result<()> {
    tx.run(
        "DELETE FROM org_domains WHERE workspace_id = ? AND org_id = ?",
        params![ws, org],
    )?;
    let t = now();
    for (i, d) in ds.iter().enumerate() {
        tx.run(
            "INSERT INTO org_domains (workspace_id, domain, org_id, created) VALUES (?, ?, ?, ?)",
            params![ws, d, org, t + i as i64],
        )?;
    }
    Ok(())
}

// ---------------------------------------------------------------- attribute definitions

/// An `attr_defs` row; `workspace_id = '*'` for the built-in ones.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AttrDef {
    pub id: String,
    #[serde(skip)]
    pub workspace_id: String,
    /// `contact` | `org` | `both`
    pub applies: String,
    pub key: String,
    pub label: String,
    /// `text` | `phone` | `date` | `address` | `url` | `enum`
    #[serde(rename = "type")]
    pub kind: String,
    pub options: Vec<String>,
    pub default_level: Level,
    /// `builtin` | `me` | `steve` | `plugin:<id>`
    pub source: String,
    /// `active` | `proposed`
    pub status: String,
    pub builtin: bool,
    pub created: i64,
}

impl AttrDef {
    fn from_row(r: &Row) -> rusqlite::Result<Self> {
        let ws: String = r.get("workspace_id")?;
        Ok(Self {
            id: r.get("id")?,
            builtin: ws == "*",
            workspace_id: ws,
            applies: r.get("applies")?,
            key: r.get("key")?,
            label: r.get("label")?,
            kind: r.get("type")?,
            options: json_or(r.get("options")?, Vec::new()),
            default_level: Level::from_db(&r.get::<_, String>("default_level")?),
            source: r.get("source")?,
            status: r.get("status")?,
            created: r.get("created")?,
        })
    }

    /// Does this definition apply to a party of `kind` (`contact` / `org`)?
    pub fn applies_to(&self, kind: &str) -> bool {
        self.applies == "both" || self.applies == kind
    }
}

/// Built-in definitions, then the workspace's, each in creation order.
pub fn defs(c: &Connection, ws: &str) -> Result<Vec<AttrDef>> {
    let mut st = c.prepare_cached(
        "SELECT * FROM attr_defs WHERE workspace_id IN ('*', ?) ORDER BY workspace_id != '*', created, rowid",
    )?;
    let rows = st.query_map([ws], AttrDef::from_row)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// The definition of `key` visible in `ws` (built-in or the workspace's).
pub fn def_by_key(c: &Connection, ws: &str, key: &str) -> Result<Option<AttrDef>> {
    Ok(c
        .prepare_cached(
            "SELECT * FROM attr_defs WHERE workspace_id IN ('*', ?) AND key = ? ORDER BY workspace_id = '*' DESC LIMIT 1",
        )?
        .query_row(params![ws, key], AttrDef::from_row)
        .optional()?)
}

pub fn def_get(c: &Connection, id: &str) -> Result<Option<AttrDef>> {
    Ok(c.prepare_cached("SELECT * FROM attr_defs WHERE id = ?")?
        .query_row([id], AttrDef::from_row)
        .optional()?)
}

/// A definition to insert.
#[derive(Debug, Clone)]
pub struct NewDef {
    pub applies: String,
    pub key: String,
    pub label: String,
    pub kind: String,
    pub options: Vec<String>,
    pub default_level: Level,
    pub source: String,
    pub status: String,
}

pub fn def_insert(tx: &Tx, ws: &str, d: &NewDef) -> Result<AttrDef> {
    let id = uid("ad");
    tx.run(
        "INSERT INTO attr_defs (id, workspace_id, applies, key, label, type, options, default_level, source, status, created) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            id,
            ws,
            d.applies,
            d.key,
            d.label,
            d.kind,
            to_json(&d.options)?,
            d.default_level.code(),
            d.source,
            d.status,
            now()
        ],
    )?;
    def_get(tx, &id)?.ok_or(steve_store::StoreError::Sqlite(
        rusqlite::Error::QueryReturnedNoRows,
    ))
}

pub fn def_update(
    tx: &Tx,
    id: &str,
    label: Option<&str>,
    options: Option<&[String]>,
    default_level: Option<Level>,
) -> Result<()> {
    if let Some(l) = label {
        tx.run(
            "UPDATE attr_defs SET label = ? WHERE id = ?",
            params![l, id],
        )?;
    }
    if let Some(o) = options {
        tx.run(
            "UPDATE attr_defs SET options = ? WHERE id = ?",
            params![to_json(&o)?, id],
        )?;
    }
    if let Some(l) = default_level {
        tx.run(
            "UPDATE attr_defs SET default_level = ? WHERE id = ?",
            params![l.code(), id],
        )?;
    }
    Ok(())
}

pub fn def_delete(tx: &Tx, id: &str) -> Result<()> {
    tx.run("DELETE FROM attr_defs WHERE id = ?", [id])?;
    Ok(())
}

/// How many active parties hold a value of `key` (deleting a definition still in use is refused).
pub fn def_usage(c: &Connection, ws: &str, key: &str) -> Result<i64> {
    Ok(c.row(
        "SELECT COUNT(DISTINCT v.party_id) FROM attr_values v JOIN parties p ON p.id = v.party_id
         WHERE v.workspace_id = ? AND v.key = ? AND p.status = 'active'",
        params![ws, key],
        |r| r.get(0),
    )?)
}

// ---------------------------------------------------------------- attribute values

/// An `attr_values` row.
#[derive(Debug, Clone, PartialEq)]
pub struct AttrValue {
    pub id: String,
    pub workspace_id: String,
    pub party_id: String,
    pub key: String,
    pub label: String,
    pub value: String,
    pub source: String,
    pub source_label: String,
    pub source_ref: Option<String>,
    pub level: Level,
    /// `candidate` | `confirmed` | `stale`
    pub status: String,
    pub decision_id: Option<String>,
    pub created: i64,
    pub updated: i64,
}

impl AttrValue {
    fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get("id")?,
            workspace_id: r.get("workspace_id")?,
            party_id: r.get("party_id")?,
            key: r.get("key")?,
            label: r.get("label")?,
            value: r.get("value")?,
            source: r.get("source")?,
            source_label: r.get("source_label")?,
            source_ref: r.get("source_ref")?,
            level: Level::from_db(&r.get::<_, String>("level")?),
            status: r.get("status")?,
            decision_id: r.get("decision_id")?,
            created: r.get("created")?,
            updated: r.get("updated")?,
        })
    }
}

/// Values of one party, by key then age.
pub fn values(c: &Connection, ws: &str, party: &str) -> Result<Vec<AttrValue>> {
    let mut st = c.prepare_cached(
        "SELECT * FROM attr_values WHERE workspace_id = ? AND party_id = ? ORDER BY created, rowid",
    )?;
    let rows = st.query_map(params![ws, party], AttrValue::from_row)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn value_get(c: &Connection, id: &str) -> Result<Option<AttrValue>> {
    Ok(c.prepare_cached("SELECT * FROM attr_values WHERE id = ?")?
        .query_row([id], AttrValue::from_row)
        .optional()?)
}

/// A value to insert.
#[derive(Debug, Clone)]
pub struct NewValue {
    pub party_id: String,
    pub key: String,
    pub label: String,
    pub value: String,
    pub source: String,
    pub source_label: String,
    pub source_ref: Option<String>,
    pub level: Level,
    pub status: String,
    pub decision_id: Option<String>,
}

pub fn value_insert(tx: &Tx, ws: &str, v: &NewValue) -> Result<AttrValue> {
    let id = uid("av");
    let t = now();
    tx.run(
        "INSERT INTO attr_values (id, workspace_id, party_id, key, label, value, source, source_label, source_ref, level, status, decision_id, created, updated)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            id,
            ws,
            v.party_id,
            v.key,
            v.label,
            v.value,
            v.source,
            v.source_label,
            v.source_ref,
            v.level.code(),
            v.status,
            v.decision_id,
            t,
            t
        ],
    )?;
    value_get(tx, &id)?.ok_or(steve_store::StoreError::Sqlite(
        rusqlite::Error::QueryReturnedNoRows,
    ))
}

pub fn value_update(
    tx: &Tx,
    id: &str,
    value: Option<&str>,
    level: Option<Level>,
    status: Option<&str>,
) -> Result<()> {
    let t = now();
    if let Some(v) = value {
        tx.run(
            "UPDATE attr_values SET value = ?, updated = ? WHERE id = ?",
            params![v, t, id],
        )?;
    }
    if let Some(l) = level {
        tx.run(
            "UPDATE attr_values SET level = ?, updated = ? WHERE id = ?",
            params![l.code(), t, id],
        )?;
    }
    if let Some(s) = status {
        tx.run(
            "UPDATE attr_values SET status = ?, updated = ? WHERE id = ?",
            params![s, t, id],
        )?;
    }
    Ok(())
}

/// Marks the other confirmed values with the same `key + label` stale (a newer confirmed value replaces
/// them; a second phone with another label stays). Returns how many.
pub fn stale_others(tx: &Tx, v: &AttrValue) -> Result<usize> {
    Ok(tx.run(
        "UPDATE attr_values SET status = 'stale', updated = ? WHERE workspace_id = ? AND party_id = ? AND key = ? AND label = ? AND status = 'confirmed' AND id != ?",
        params![now(), v.workspace_id, v.party_id, v.key, v.label, v.id],
    )?)
}

pub fn value_delete(tx: &Tx, id: &str) -> Result<bool> {
    Ok(tx.run("DELETE FROM attr_values WHERE id = ?", [id])? > 0)
}

/// Physically deletes every value of a party (deleting a contact, §8). Returns how many.
pub fn values_delete_of(tx: &Tx, ws: &str, party: &str) -> Result<usize> {
    Ok(tx.run(
        "DELETE FROM attr_values WHERE workspace_id = ? AND party_id = ?",
        params![ws, party],
    )?)
}

// ---------------------------------------------------------------- teams, items, merges (reads for the contact page)

/// A team a contact belongs to.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TeamRef {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub role: String,
}

/// Active teams visible to `viewer` that `contact` is a direct member of.
pub fn teams_of(c: &Connection, viewer: &Viewer, contact: &str) -> Result<Vec<TeamRef>> {
    let scope = viewer.sql("p");
    let sql = format!(
        "SELECT p.id, p.name, COALESCE(d.kind, 'virtual'), m.role FROM team_members m
           JOIN parties p ON p.id = m.team_id LEFT JOIN team_details d ON d.party_id = p.id
         WHERE m.workspace_id = ? AND m.contact_id = ? AND p.workspace_id = ? AND p.status = 'active' AND {}
         ORDER BY p.name, p.id",
        scope.sql
    );
    let mut ps: Vec<Sql> = vec![
        viewer.ws.clone().into(),
        contact.to_string().into(),
        viewer.ws.clone().into(),
    ];
    ps.extend(scope.params);
    let mut st = c.prepare(&sql)?;
    let rows = st.query_map(bind(ps), |r| {
        Ok(TeamRef {
            id: r.get(0)?,
            name: r.get(1)?,
            kind: r.get(2)?,
            role: r.get(3)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Removes a contact from every team. Returns how many rows.
pub fn memberships_delete_of(tx: &Tx, ws: &str, contact: &str) -> Result<usize> {
    Ok(tx.run(
        "DELETE FROM team_members WHERE workspace_id = ? AND contact_id = ?",
        params![ws, contact],
    )?)
}

/// Is `id` an active team of `ws` (for `visibility: team:<id>`)?
pub fn team_exists(c: &Connection, ws: &str, id: &str) -> Result<bool> {
    Ok(c.prepare_cached(
        "SELECT 1 FROM parties WHERE id = ? AND workspace_id = ? AND type = 'team' AND status = 'active'",
    )?
    .exists(params![id, ws])?)
}

/// Unarchived items of `ws` linked to the party in any role.
pub fn items_count(c: &Connection, ws: &str, party: &str) -> Result<i64> {
    Ok(c.row(
        "SELECT COUNT(DISTINCT l.subject_id) FROM links l JOIN items i ON i.id = l.subject_id
         WHERE l.workspace_id = ? AND l.subject_type = 'item' AND l.object_type = 'party' AND l.object_id = ?
           AND i.workspace_id = l.workspace_id AND i.archived = 0",
        params![ws, party],
        |r| r.get(0),
    )?)
}

/// `(merge id, from name, created, split)`.
pub type MergeRow = (String, String, i64, Option<i64>);

/// A merge into a contact, for its page: `(id, from_name, created, split)`, newest first.
pub fn merges_into(c: &Connection, ws: &str, into: &str) -> Result<Vec<MergeRow>> {
    let mut st = c.prepare_cached(
        "SELECT m.id, COALESCE(p.name, ''), m.created, m.split FROM merges m LEFT JOIN parties p ON p.id = m.from_id
         WHERE m.workspace_id = ? AND m.into_id = ? ORDER BY m.created DESC, m.rowid DESC",
    )?;
    let rows = st.query_map(params![ws, into], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}
