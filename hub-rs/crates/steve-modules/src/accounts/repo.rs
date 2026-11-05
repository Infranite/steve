//! SQL of the accounts slice (tables of migration 5): systems, accounts, their
//! handles, match rules, plus the few reads of `connections`, `vault_items` and `merge_proposals` the
//! account pages and identification need. Writes take a [`Tx`]; the service publishes events.

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use steve_store::{Cached, Result, Tx, now, uid};

/// `object_type` of accounts in `revisions`.
pub const ACCOUNT: &str = "account";

fn json_or<T: serde::de::DeserializeOwned>(s: Option<String>, d: T) -> T {
    match s.as_deref() {
        None | Some("") => d,
        Some(s) => serde_json::from_str(s).unwrap_or(d),
    }
}

// ---------------------------------------------------------------- systems

/// A `systems` row; `workspace_id = '*'` for the built-in ones.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct System {
    pub id: String,
    #[serde(skip)]
    pub workspace_id: String,
    pub name: String,
    /// `comm` | `calendar` | `code` | `cloud` | `finance` | `steve`
    pub kind: String,
    /// The handle family: `email` | `phone` | `feishu_user` | … | `card` | `other`.
    pub family: String,
    pub plugin: Option<String>,
    pub handle_rule: Value,
    pub builtin: bool,
}

impl System {
    fn from_row(r: &Row) -> rusqlite::Result<Self> {
        let ws: String = r.get("workspace_id")?;
        Ok(Self {
            id: r.get("id")?,
            builtin: ws == "*",
            workspace_id: ws,
            name: r.get("name")?,
            kind: r.get("kind")?,
            family: r.get("family")?,
            plugin: r.get("plugin")?,
            handle_rule: json_or(r.get("handle_rule")?, json!({})),
        })
    }

    /// `handle_rule.normalize`: `email` | `phone` | `lower` | `none` (default).
    pub fn normalize(&self) -> &str {
        self.handle_rule
            .get("normalize")
            .and_then(Value::as_str)
            .unwrap_or("none")
    }
}

/// The built-in systems (§1 seeds): `(id, name, kind, family, normalize)`.
pub const BUILTIN_SYSTEMS: [(&str, &str, &str, &str, &str); 14] = [
    ("steve", "Steve", "steve", "other", "none"),
    ("mail", "邮件", "comm", "email", "email"),
    ("gmail", "Gmail", "comm", "email", "email"),
    ("outlook", "Outlook", "comm", "email", "email"),
    ("icloud", "iCloud", "comm", "email", "email"),
    ("feishu", "飞书", "comm", "feishu_user", "none"),
    ("feishu_app", "飞书应用", "comm", "feishu_app", "none"),
    ("slack", "Slack", "comm", "slack_user", "none"),
    ("gcal", "Google 日历", "calendar", "email", "email"),
    ("github", "GitHub", "code", "github_user", "lower"),
    ("aws", "AWS", "cloud", "aws_account", "none"),
    ("phone", "电话", "comm", "phone", "phone"),
    ("bank", "银行卡", "finance", "card", "none"),
    // an organization in Feishu (`tenant_key`): the handle of an organization party
    ("feishu_tenant", "飞书租户", "org", "feishu_tenant", "none"),
];

/// Are the built-in systems there (all of them)?
pub fn builtins_seeded(c: &Connection) -> Result<bool> {
    let n: i64 = c.row(
        "SELECT COUNT(*) FROM systems WHERE workspace_id = '*'",
        [],
        |r| r.get(0),
    )?;
    Ok(n >= BUILTIN_SYSTEMS.len() as i64)
}

/// `INSERT OR IGNORE` of the built-in systems.
pub fn seed_builtins(tx: &Tx) -> Result<()> {
    let t = now();
    for (id, name, kind, family, norm) in BUILTIN_SYSTEMS {
        tx.run(
            "INSERT OR IGNORE INTO systems (id, workspace_id, name, kind, family, plugin, handle_rule, created) VALUES (?, '*', ?, ?, ?, ?, ?, ?)",
            params![
                id,
                name,
                kind,
                family,
                format!("builtin:{id}"),
                json!({ "normalize": norm }).to_string(),
                t
            ],
        )?;
    }
    Ok(())
}

/// Built-in systems, then the workspace's, each by creation and id.
pub fn systems(c: &Connection, ws: &str) -> Result<Vec<System>> {
    let mut st = c.prepare_cached(
        "SELECT * FROM systems WHERE workspace_id IN ('*', ?) ORDER BY workspace_id != '*', created, rowid",
    )?;
    let rows = st.query_map([ws], System::from_row)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// The system `id` as seen from `ws` (the workspace's own definition wins over a built-in one).
pub fn system(c: &Connection, ws: &str, id: &str) -> Result<Option<System>> {
    Ok(c
        .prepare_cached(
            "SELECT * FROM systems WHERE workspace_id IN ('*', ?) AND id = ? ORDER BY workspace_id = '*' LIMIT 1",
        )?
        .query_row(params![ws, id], System::from_row)
        .optional()?
        .or_else(|| builtin(id)))
}

/// A built-in system before `seed_builtins` ran (a fresh workspace validating its first change):
/// the same row the seed will insert, so validate and apply agree.
fn builtin(id: &str) -> Option<System> {
    BUILTIN_SYSTEMS
        .iter()
        .find(|b| b.0 == id)
        .map(|&(id, name, kind, family, norm)| System {
            id: id.into(),
            workspace_id: "*".into(),
            name: name.into(),
            kind: kind.into(),
            family: family.into(),
            plugin: Some(format!("builtin:{id}")),
            handle_rule: json!({ "normalize": norm }),
            builtin: true,
        })
}

// ---------------------------------------------------------------- accounts

/// An `accounts` row. Never holds secrets: a card is only its last four digits.
#[derive(Debug, Clone, PartialEq)]
pub struct Account {
    pub id: String,
    pub workspace_id: String,
    pub system_id: String,
    pub family: String,
    pub party_id: String,
    pub kind: String,
    pub handle: String,
    pub handle_display: String,
    pub display_name: String,
    pub verified: bool,
    pub confidence: f64,
    pub source: String,
    pub source_label: String,
    pub status: String,
    pub revision: i64,
    pub created: i64,
    pub updated: i64,
}

impl Account {
    fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get("id")?,
            workspace_id: r.get("workspace_id")?,
            system_id: r.get("system_id")?,
            family: r.get("family")?,
            party_id: r.get("party_id")?,
            kind: r.get("kind")?,
            handle: r.get("handle")?,
            handle_display: r.get("handle_display")?,
            display_name: r.get("display_name")?,
            verified: r.get::<_, i64>("verified")? != 0,
            confidence: r.get("confidence")?,
            source: r.get("source")?,
            source_label: r.get("source_label")?,
            status: r.get("status")?,
            revision: r.get("revision")?,
            created: r.get("created")?,
            updated: r.get("updated")?,
        })
    }
}

/// An account to insert, with its normalized primary handle.
#[derive(Debug, Clone)]
pub struct NewAccount {
    pub system_id: String,
    pub family: String,
    pub party_id: String,
    pub kind: String,
    pub handle: String,
    pub handle_display: String,
    pub display_name: String,
    pub verified: bool,
    pub confidence: f64,
    pub source: String,
    pub source_label: String,
}

pub fn get(c: &Connection, id: &str) -> Result<Option<Account>> {
    Ok(c.prepare_cached("SELECT * FROM accounts WHERE id = ?")?
        .query_row([id], Account::from_row)
        .optional()?)
}

/// Inserts the account and its primary handle row. The caller checked the handle is free.
pub fn insert(tx: &Tx, ws: &str, a: &NewAccount) -> Result<Account> {
    let id = uid("acc");
    let t = now();
    tx.run(
        "INSERT INTO accounts (id, workspace_id, system_id, family, party_id, kind, handle, handle_display, display_name, verified, confidence, source, source_label, created, updated)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            id,
            ws,
            a.system_id,
            a.family,
            a.party_id,
            a.kind,
            a.handle,
            a.handle_display,
            a.display_name,
            a.verified as i64,
            a.confidence,
            a.source,
            a.source_label,
            t,
            t
        ],
    )?;
    add_handle(tx, ws, &a.family, &a.handle, &id, None)?;
    get(tx, &id)?.ok_or(steve_store::StoreError::Sqlite(
        rusqlite::Error::QueryReturnedNoRows,
    ))
}

/// Columns an account edit may set; `None` = unchanged.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AccountPatch {
    pub display_name: Option<String>,
    pub kind: Option<String>,
    pub party_id: Option<String>,
    pub verified: Option<bool>,
    pub confidence: Option<f64>,
}

/// Applies a patch, bumps `updated` and, when given, sets `revision`.
pub fn update(tx: &Tx, id: &str, p: &AccountPatch, revision: Option<i64>) -> Result<()> {
    let t = now();
    if let Some(v) = &p.display_name {
        tx.run(
            "UPDATE accounts SET display_name = ?, updated = ? WHERE id = ?",
            params![v, t, id],
        )?;
    }
    if let Some(v) = &p.kind {
        tx.run(
            "UPDATE accounts SET kind = ?, updated = ? WHERE id = ?",
            params![v, t, id],
        )?;
    }
    if let Some(v) = &p.party_id {
        tx.run(
            "UPDATE accounts SET party_id = ?, updated = ? WHERE id = ?",
            params![v, t, id],
        )?;
    }
    if let Some(v) = p.verified {
        tx.run(
            "UPDATE accounts SET verified = ?, updated = ? WHERE id = ?",
            params![v as i64, t, id],
        )?;
    }
    if let Some(v) = p.confidence {
        tx.run(
            "UPDATE accounts SET confidence = ?, updated = ? WHERE id = ?",
            params![v, t, id],
        )?;
    }
    if let Some(v) = revision {
        tx.run(
            "UPDATE accounts SET revision = ?, updated = ? WHERE id = ?",
            params![v, t, id],
        )?;
    }
    Ok(())
}

/// Deletes an account and its handle rows; vault items linked to it keep existing, unlinked.
pub fn delete(tx: &Tx, id: &str) -> Result<()> {
    tx.run(
        "UPDATE vault_items SET account_id = NULL WHERE account_id = ?",
        [id],
    )?;
    tx.run("DELETE FROM account_handles WHERE account_id = ?", [id])?;
    tx.run("DELETE FROM accounts WHERE id = ?", [id])?;
    Ok(())
}

/// Accounts of one party, oldest first.
pub fn of_party(c: &Connection, ws: &str, party: &str) -> Result<Vec<Account>> {
    let mut st = c.prepare_cached(
        "SELECT * FROM accounts WHERE workspace_id = ? AND party_id = ? ORDER BY created, rowid",
    )?;
    let rows = st.query_map(params![ws, party], Account::from_row)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Moves accounts to another party (merge, split). Only rows still on `from` move; returns those ids.
pub fn move_party(tx: &Tx, ws: &str, ids: &[String], from: &str, to: &str) -> Result<Vec<String>> {
    let mut moved = Vec::new();
    let t = now();
    for id in ids {
        let n = tx.run(
            "UPDATE accounts SET party_id = ?, updated = ? WHERE id = ? AND workspace_id = ? AND party_id = ?",
            params![to, t, id, ws, from],
        )?;
        if n > 0 {
            moved.push(id.clone());
        }
    }
    Ok(moved)
}

/// Number of accounts of a party (`ContactBrief.accounts`).
pub fn count_of(c: &Connection, ws: &str, party: &str) -> Result<i64> {
    Ok(c.row(
        "SELECT COUNT(*) FROM accounts WHERE workspace_id = ? AND party_id = ? AND status = 'active'",
        params![ws, party],
        |r| r.get(0),
    )?)
}

/// Party ids owning an account with a handle (any of its handles, or the display form) containing `q`.
pub fn parties_by_handle(c: &Connection, ws: &str, q: &str) -> Result<Vec<String>> {
    let pat = crate::contacts::repo::like(&q.to_lowercase());
    let mut st = c.prepare_cached(
        "SELECT DISTINCT a.party_id FROM accounts a
         WHERE a.workspace_id = ?1 AND a.status = 'active'
           AND (lower(a.handle_display) LIKE ?2 ESCAPE '\\'
                OR EXISTS (SELECT 1 FROM account_handles h WHERE h.account_id = a.id AND lower(h.handle) LIKE ?2 ESCAPE '\\'))
         ORDER BY a.party_id",
    )?;
    let rows = st.query_map(params![ws, pat], |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Filters of `GET /accounts` once the scope is resolved to party conditions.
#[derive(Debug, Clone, Default)]
pub struct AccountQuery {
    /// Only this party.
    pub party: Option<String>,
    /// Exclude these parties (`scope=others`).
    pub not_parties: Vec<String>,
    pub system: Option<String>,
}

/// Accounts of `ws` whose party `viewer` can see, oldest first.
pub fn list(
    c: &Connection,
    viewer: &steve_store::base::Viewer,
    f: &AccountQuery,
) -> Result<Vec<Account>> {
    use rusqlite::types::Value as Sql;
    let scope = viewer.sql("p");
    let mut sql = format!(
        "SELECT a.* FROM accounts a JOIN parties p ON p.id = a.party_id
         WHERE a.workspace_id = ? AND p.workspace_id = a.workspace_id AND p.status != 'deleted' AND {}",
        scope.sql
    );
    let mut ps: Vec<Sql> = vec![viewer.ws.clone().into()];
    ps.extend(scope.params);
    if let Some(p) = &f.party {
        sql.push_str(" AND a.party_id = ?");
        ps.push(p.clone().into());
    }
    for p in &f.not_parties {
        sql.push_str(" AND a.party_id != ?");
        ps.push(p.clone().into());
    }
    if let Some(s) = &f.system {
        sql.push_str(" AND a.system_id = ?");
        ps.push(s.clone().into());
    }
    sql.push_str(" ORDER BY a.created, a.rowid");
    let mut st = c.prepare(&sql)?;
    let rows = st.query_map(steve_store::base::bind(ps), Account::from_row)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

// ---------------------------------------------------------------- handles

/// The account owning `(family, handle)` in `ws`.
pub fn by_handle(c: &Connection, ws: &str, family: &str, handle: &str) -> Result<Option<String>> {
    Ok(c
        .prepare_cached(
            "SELECT account_id FROM account_handles WHERE workspace_id = ? AND family = ? AND handle = ?",
        )?
        .query_row(params![ws, family, handle], |r| r.get(0))
        .optional()?)
}

/// Adds a handle row; `false` if the handle is already taken (by any account).
pub fn add_handle(
    tx: &Tx,
    ws: &str,
    family: &str,
    handle: &str,
    account: &str,
    connection: Option<&str>,
) -> Result<bool> {
    Ok(tx.run(
        "INSERT OR IGNORE INTO account_handles (workspace_id, family, handle, account_id, connection_id, created) VALUES (?, ?, ?, ?, ?, ?)",
        params![ws, family, handle, account, connection, now()],
    )? > 0)
}

/// Every handle of an account: `(handle, connection_id)`, primary first.
pub fn handles_of(c: &Connection, account: &str) -> Result<Vec<(String, Option<String>)>> {
    let mut st = c.prepare_cached(
        "SELECT h.handle, h.connection_id FROM account_handles h JOIN accounts a ON a.id = h.account_id
         WHERE h.account_id = ? ORDER BY h.handle != a.handle, h.created, h.rowid",
    )?;
    let rows = st.query_map([account], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Email handles of `ws` with this local part: `(handle, account_id)`.
pub fn emails_with_local(c: &Connection, ws: &str, local: &str) -> Result<Vec<(String, String)>> {
    let mut pat = String::new();
    for ch in local.chars() {
        if matches!(ch, '%' | '_' | '\\') {
            pat.push('\\');
        }
        pat.push(ch);
    }
    pat.push_str("@%");
    let mut st = c.prepare_cached(
        "SELECT handle, account_id FROM account_handles WHERE workspace_id = ? AND family = 'email' AND handle LIKE ? ESCAPE '\\' ORDER BY created, rowid",
    )?;
    let rows = st.query_map(params![ws, pat], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

// ---------------------------------------------------------------- connections, vault items (reads)

/// The part of a connection an `AccountOut` shows.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ConnectionBrief {
    pub id: String,
    pub status: String,
    pub caps: Vec<String>,
    pub last_sync: Option<i64>,
}

pub fn connection_of(c: &Connection, account: &str) -> Result<Option<ConnectionBrief>> {
    Ok(c.prepare_cached(
        "SELECT id, status, caps, last_sync FROM connections WHERE account_id = ?",
    )?
    .query_row([account], |r| {
        Ok(ConnectionBrief {
            id: r.get(0)?,
            status: r.get(1)?,
            caps: json_or(r.get(2)?, Vec::new()),
            last_sync: r.get(3)?,
        })
    })
    .optional()?)
}

/// `{id, title}` of an item linked to an account.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ItemRef {
    pub id: String,
    pub title: String,
}

/// Active vault items linked to an account that `user` may see: the shared vault and their own
/// personal vault (the system vault never shows up).
pub fn vault_items_of(c: &Connection, account: &str, user: &str) -> Result<Vec<ItemRef>> {
    let mut st = c.prepare_cached(
        "SELECT i.id, i.title FROM vault_items i JOIN vaults v ON v.id = i.vault_id
         WHERE i.account_id = ? AND i.status = 'active' AND i.workspace_id = v.workspace_id
           AND (v.scope = 'shared' OR (v.scope = 'personal' AND v.owner_user = ?))
         ORDER BY i.created, i.rowid",
    )?;
    let rows = st.query_map(params![account, user], |r| {
        Ok(ItemRef {
            id: r.get(0)?,
            title: r.get(1)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

// ---------------------------------------------------------------- match rules

/// `match_rules.config` of `email_prefix`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrefixConfig {
    pub same_org_only: bool,
    pub exclude: Vec<String>,
    pub min_len: i64,
}

impl Default for PrefixConfig {
    fn default() -> Self {
        Self {
            same_org_only: true,
            exclude: ["admin", "info", "noreply", "support", "hello"]
                .map(String::from)
                .to_vec(),
            min_len: 3,
        }
    }
}

/// A `match_rules` row.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MatchRule {
    pub id: String,
    #[serde(skip)]
    pub workspace_id: String,
    pub family: String,
    pub kind: String,
    pub config: PrefixConfig,
    pub enabled: bool,
    pub updated: i64,
}

impl MatchRule {
    fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get("id")?,
            workspace_id: r.get("workspace_id")?,
            family: r.get("family")?,
            kind: r.get("kind")?,
            config: json_or(r.get("config")?, PrefixConfig::default()),
            enabled: r.get::<_, i64>("enabled")? != 0,
            updated: r.get("updated")?,
        })
    }
}

pub fn rules(c: &Connection, ws: &str) -> Result<Vec<MatchRule>> {
    let mut st = c.prepare_cached(
        "SELECT * FROM match_rules WHERE workspace_id = ? ORDER BY created, rowid",
    )?;
    let rows = st.query_map([ws], MatchRule::from_row)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn rule(c: &Connection, id: &str) -> Result<Option<MatchRule>> {
    Ok(c.prepare_cached("SELECT * FROM match_rules WHERE id = ?")?
        .query_row([id], MatchRule::from_row)
        .optional()?)
}

/// The workspace's `email_prefix` rule, if seeded.
pub fn prefix_rule(c: &Connection, ws: &str) -> Result<Option<MatchRule>> {
    Ok(c
        .prepare_cached(
            "SELECT * FROM match_rules WHERE workspace_id = ? AND family = 'email' AND kind = 'email_prefix' ORDER BY created LIMIT 1",
        )?
        .query_row([ws], MatchRule::from_row)
        .optional()?)
}

/// Seeds the default `email_prefix` rule of a workspace.
pub fn seed_rule(tx: &Tx, ws: &str) -> Result<()> {
    let t = now();
    tx.run(
        "INSERT INTO match_rules (id, workspace_id, family, kind, config, enabled, created, updated) VALUES (?, ?, 'email', 'email_prefix', ?, 1, ?, ?)",
        params![
            uid("mr"),
            ws,
            serde_json::to_string(&PrefixConfig::default())?,
            t,
            t
        ],
    )?;
    Ok(())
}

pub fn rule_update(tx: &Tx, id: &str, enabled: bool, config: &PrefixConfig) -> Result<()> {
    tx.run(
        "UPDATE match_rules SET enabled = ?, config = ?, updated = ? WHERE id = ?",
        params![enabled as i64, serde_json::to_string(config)?, now(), id],
    )?;
    Ok(())
}

// ---------------------------------------------------------------- merge proposals

/// Writes an open merge proposal; `false` if the same pair already has one open.
#[allow(clippy::too_many_arguments)]
pub fn proposal_insert(
    tx: &Tx,
    id: &str,
    ws: &str,
    from: &str,
    into: &str,
    account: &str,
    reason: &str,
    confidence: f64,
) -> Result<bool> {
    Ok(tx.run(
        "INSERT OR IGNORE INTO merge_proposals (id, workspace_id, from_id, into_id, account_id, reason, confidence, status, created) VALUES (?, ?, ?, ?, ?, ?, ?, 'open', ?)",
        params![id, ws, from, into, account, reason, confidence, now()],
    )? > 0)
}

pub fn proposal_set_decision(tx: &Tx, id: &str, decision: &str) -> Result<()> {
    tx.run(
        "UPDATE merge_proposals SET decision_id = ? WHERE id = ?",
        params![decision, id],
    )?;
    Ok(())
}

/// Was this pair rejected before (“不是同一个人”)? Then it is never proposed again.
pub fn pair_rejected(c: &Connection, ws: &str, from: &str, into: &str) -> Result<bool> {
    Ok(c.prepare_cached(
        "SELECT 1 FROM merge_proposals WHERE workspace_id = ? AND from_id = ? AND into_id = ? AND status = 'rejected'",
    )?
    .exists(params![ws, from, into])?)
}
