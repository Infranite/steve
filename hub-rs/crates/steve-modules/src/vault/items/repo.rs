//! SQL of slice V2: vaults, templates, items, fields, plus the grant and use rows items show or revoke.
//! Parameterized queries only. Field rows carry ciphertext, never plaintext of a secret field.

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde_json::Value;
use steve_store::Result;

use super::templates::{FieldDef, Template, builtin};

// ------------------------------------------------------------------------------------------ vaults

/// One row of `vaults`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultRow {
    pub id: String,
    pub workspace_id: String,
    pub name: String,
    /// `personal` | `shared` | `system`
    pub scope: String,
    pub owner_user: Option<String>,
    pub backend: String,
    pub created: i64,
}

impl VaultRow {
    /// §4.1: personal only for its owner, shared for every member, system never (Hub-internal).
    pub fn visible_to(&self, ws: &str, user: &str) -> bool {
        self.workspace_id == ws
            && match self.scope.as_str() {
                "shared" => true,
                "personal" => self.owner_user.as_deref() == Some(user),
                _ => false,
            }
    }
}

fn vault_row(r: &Row) -> rusqlite::Result<VaultRow> {
    Ok(VaultRow {
        id: r.get(0)?,
        workspace_id: r.get(1)?,
        name: r.get(2)?,
        scope: r.get(3)?,
        owner_user: r.get(4)?,
        backend: r.get(5)?,
        created: r.get(6)?,
    })
}

const VAULT_COLS: &str = "id, workspace_id, name, scope, owner_user, backend, created";

pub fn vaults(c: &Connection, ws: &str) -> Result<Vec<VaultRow>> {
    let mut st = c.prepare_cached(&format!(
        "SELECT {VAULT_COLS} FROM vaults WHERE workspace_id = ? ORDER BY created, id"
    ))?;
    let rows = st.query_map([ws], vault_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn vault(c: &Connection, id: &str) -> Result<Option<VaultRow>> {
    Ok(
        c.prepare_cached(&format!("SELECT {VAULT_COLS} FROM vaults WHERE id = ?"))?
            .query_row([id], vault_row)
            .optional()?,
    )
}

pub fn insert_vault(c: &Connection, v: &VaultRow) -> Result<()> {
    c.prepare_cached(
        "INSERT INTO vaults (id, workspace_id, name, scope, owner_user, backend, created) VALUES (?, ?, ?, ?, ?, ?, ?)",
    )?
    .execute(params![
        v.id,
        v.workspace_id,
        v.name,
        v.scope,
        v.owner_user,
        v.backend,
        v.created
    ])?;
    Ok(())
}

/// Active items per vault of a workspace.
pub fn item_counts(c: &Connection, ws: &str) -> Result<Vec<(String, i64)>> {
    let mut st = c.prepare_cached(
        "SELECT vault_id, COUNT(*) FROM vault_items WHERE workspace_id = ? AND status = 'active' GROUP BY vault_id",
    )?;
    let rows = st.query_map([ws], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

// --------------------------------------------------------------------------------------- templates

/// `INSERT OR IGNORE` of the built-in templates under workspace `*`.
pub fn seed_templates(c: &Connection) -> Result<()> {
    let mut st = c.prepare_cached(
        "INSERT OR IGNORE INTO vault_templates (id, workspace_id, name, fields, default_sensitivity, source)
         VALUES (?, '*', ?, ?, ?, ?)",
    )?;
    for t in builtin() {
        let fields = serde_json::to_string(&t.fields).unwrap_or_else(|_| "[]".into());
        st.execute(params![
            t.id,
            t.name,
            fields,
            t.default_sensitivity,
            t.source
        ])?;
    }
    Ok(())
}

fn template_row(r: &Row) -> rusqlite::Result<Template> {
    let fields: String = r.get(2)?;
    Ok(Template {
        id: r.get(0)?,
        name: r.get(1)?,
        fields: serde_json::from_str::<Vec<FieldDef>>(&fields).unwrap_or_default(),
        default_sensitivity: r.get(3)?,
        source: r.get(4)?,
    })
}

/// Built-in templates plus the workspace's own (a workspace row overrides a built-in with the same id),
/// built-ins first in their seed order.
pub fn templates(c: &Connection, ws: &str) -> Result<Vec<Template>> {
    let mut st = c.prepare_cached(
        "SELECT id, name, fields, default_sensitivity, source, workspace_id FROM vault_templates
         WHERE workspace_id IN ('*', ?) ORDER BY workspace_id = '*' DESC, id",
    )?;
    let rows = st.query_map([ws], |r| Ok((template_row(r)?, r.get::<_, String>(5)?)))?;
    let mut out: Vec<Template> = Vec::new();
    for row in rows {
        let (t, _) = row?;
        match out.iter_mut().find(|x| x.id == t.id) {
            Some(x) => *x = t,
            None => out.push(t),
        }
    }
    let order: Vec<String> = builtin().into_iter().map(|t| t.id).collect();
    out.sort_by_key(|t| order.iter().position(|x| *x == t.id).unwrap_or(order.len()));
    Ok(out)
}

pub fn template(c: &Connection, ws: &str, id: &str) -> Result<Option<Template>> {
    Ok(templates(c, ws)?.into_iter().find(|t| t.id == id))
}

// ------------------------------------------------------------------------------------------- items

/// One row of `vault_items`. `Debug` leaves out the wrapped key.
#[derive(Clone)]
pub struct ItemRow {
    pub id: String,
    pub workspace_id: String,
    pub vault_id: String,
    pub template: String,
    pub title: String,
    pub site: String,
    pub account_id: Option<String>,
    pub sensitivity: String,
    pub dek_wrapped: Option<Vec<u8>>,
    pub kek_version: Option<i64>,
    pub external_ref: Option<String>,
    pub status: String,
    pub created_by: String,
    pub created: i64,
    pub updated: i64,
}

impl std::fmt::Debug for ItemRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ItemRow")
            .field("id", &self.id)
            .field("vault_id", &self.vault_id)
            .field("template", &self.template)
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}

const ITEM_COLS: &str =
    "i.id, i.workspace_id, i.vault_id, i.template, i.title, i.site, i.account_id, i.sensitivity,
    i.dek_wrapped, i.kek_version, i.external_ref, i.status, i.created_by, i.created, i.updated";

fn item_row(r: &Row) -> rusqlite::Result<ItemRow> {
    Ok(ItemRow {
        id: r.get(0)?,
        workspace_id: r.get(1)?,
        vault_id: r.get(2)?,
        template: r.get(3)?,
        title: r.get(4)?,
        site: r.get(5)?,
        account_id: r.get(6)?,
        sensitivity: r.get(7)?,
        dek_wrapped: r.get(8)?,
        kek_version: r.get(9)?,
        external_ref: r.get(10)?,
        status: r.get(11)?,
        created_by: r.get(12)?,
        created: r.get(13)?,
        updated: r.get(14)?,
    })
}

pub fn item(c: &Connection, id: &str) -> Result<Option<ItemRow>> {
    Ok(c.prepare_cached(&format!(
        "SELECT {ITEM_COLS} FROM vault_items i WHERE i.id = ?"
    ))?
    .query_row([id], item_row)
    .optional()?)
}

pub fn insert_item(c: &Connection, i: &ItemRow) -> Result<()> {
    c.prepare_cached(
        "INSERT INTO vault_items (id, workspace_id, vault_id, template, title, site, account_id, sensitivity,
           dek_wrapped, kek_version, external_ref, status, created_by, created, updated)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )?
    .execute(params![
        i.id,
        i.workspace_id,
        i.vault_id,
        i.template,
        i.title,
        i.site,
        i.account_id,
        i.sensitivity,
        i.dek_wrapped,
        i.kek_version,
        i.external_ref,
        i.status,
        i.created_by,
        i.created,
        i.updated
    ])?;
    Ok(())
}

/// Filters of `GET /vault/items`.
#[derive(Debug, Clone, Default)]
pub struct ItemFilter {
    pub vault: Option<String>,
    pub template: Option<String>,
    pub account: Option<String>,
    /// Matched against title, site, field labels, non-secret field values and the account's handles.
    /// Never against secret fields (their values are only ciphertext anyway).
    pub q: Option<String>,
}

/// Active items of `ws` in vaults `user` may see (shared, their own personal), newest first.
pub fn list_items(c: &Connection, ws: &str, user: &str, f: &ItemFilter) -> Result<Vec<ItemRow>> {
    let q = f.q.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let mut st = c.prepare_cached(&format!(
        "SELECT {ITEM_COLS} FROM vault_items i JOIN vaults v ON v.id = i.vault_id
         WHERE i.workspace_id = ?1 AND i.status = 'active'
           AND (v.scope = 'shared' OR (v.scope = 'personal' AND v.owner_user = ?2))
           AND (?3 IS NULL OR i.vault_id = ?3)
           AND (?4 IS NULL OR i.template = ?4)
           AND (?5 IS NULL OR i.account_id = ?5)
           AND (?6 IS NULL
                OR instr(lower(i.title), lower(?6)) > 0
                OR instr(lower(i.site), lower(?6)) > 0
                OR EXISTS (SELECT 1 FROM vault_fields f WHERE f.item_id = i.id
                           AND (instr(lower(f.label), lower(?6)) > 0
                                OR (f.secret = 0 AND instr(lower(coalesce(f.value_plain, '')), lower(?6)) > 0)))
                OR EXISTS (SELECT 1 FROM accounts a WHERE a.id = i.account_id
                           AND (instr(lower(a.display_name), lower(?6)) > 0 OR instr(lower(a.handle_display), lower(?6)) > 0)))
         ORDER BY i.updated DESC, i.id"
    ))?;
    let rows = st.query_map(
        params![ws, user, f.vault, f.template, f.account, q],
        item_row,
    )?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Metadata columns `PATCH /vault/items/:id` may change.
pub fn update_item_meta(c: &Connection, i: &ItemRow, now: i64) -> Result<()> {
    c.prepare_cached(
        "UPDATE vault_items SET title = ?, site = ?, account_id = ?, sensitivity = ?, updated = ? WHERE id = ?",
    )?
    .execute(params![i.title, i.site, i.account_id, i.sensitivity, now, i.id])?;
    Ok(())
}

pub fn touch_item(c: &Connection, id: &str, now: i64) -> Result<()> {
    c.prepare_cached("UPDATE vault_items SET updated = ? WHERE id = ?")?
        .execute(params![now, id])?;
    Ok(())
}

/// §4.1 delete: destroys the data key and every field value, keeps the title for the use log. The
/// blobs are first overwritten with zeros of the same size so the freed bytes in the page don't keep
/// the old wrapped key or ciphertext.
pub fn destroy_item(c: &Connection, id: &str, now: i64) -> Result<()> {
    c.prepare_cached(
        "UPDATE vault_items SET dek_wrapped = zeroblob(length(dek_wrapped)) WHERE id = ? AND dek_wrapped IS NOT NULL",
    )?
    .execute([id])?;
    c.prepare_cached(
        "UPDATE vault_fields SET value_ct = zeroblob(length(value_ct)) WHERE item_id = ? AND value_ct IS NOT NULL",
    )?
    .execute([id])?;
    c.prepare_cached(
        "UPDATE vault_fields SET value_ct = NULL, value_plain = NULL, hint = '', updated = ? WHERE item_id = ?",
    )?
    .execute(params![now, id])?;
    c.prepare_cached(
        "UPDATE vault_items SET dek_wrapped = NULL, kek_version = NULL, external_ref = NULL, status = 'deleted', updated = ? WHERE id = ?",
    )?
    .execute(params![now, id])?;
    Ok(())
}

// ------------------------------------------------------------------------------------------ fields

/// One row of `vault_fields`. `Debug` leaves out the values.
#[derive(Clone)]
pub struct FieldRow {
    pub id: String,
    pub item_id: String,
    pub label: String,
    pub kind: String,
    pub secret: bool,
    pub value_plain: Option<String>,
    pub value_ct: Option<Vec<u8>>,
    pub hint: String,
    pub sort: i64,
    pub updated: i64,
}

impl std::fmt::Debug for FieldRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FieldRow")
            .field("id", &self.id)
            .field("label", &self.label)
            .field("secret", &self.secret)
            .finish_non_exhaustive()
    }
}

const FIELD_COLS: &str =
    "id, item_id, label, type, secret, value_plain, value_ct, hint, sort, updated";

fn field_row(r: &Row) -> rusqlite::Result<FieldRow> {
    Ok(FieldRow {
        id: r.get(0)?,
        item_id: r.get(1)?,
        label: r.get(2)?,
        kind: r.get(3)?,
        secret: r.get::<_, i64>(4)? != 0,
        value_plain: r.get(5)?,
        value_ct: r.get(6)?,
        hint: r.get(7)?,
        sort: r.get(8)?,
        updated: r.get(9)?,
    })
}

pub fn fields(c: &Connection, item_id: &str) -> Result<Vec<FieldRow>> {
    let mut st = c.prepare_cached(&format!(
        "SELECT {FIELD_COLS} FROM vault_fields WHERE item_id = ? ORDER BY sort, id"
    ))?;
    let rows = st.query_map([item_id], field_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn insert_field(c: &Connection, ws: &str, f: &FieldRow) -> Result<()> {
    c.prepare_cached(
        "INSERT INTO vault_fields (id, workspace_id, item_id, label, type, secret, value_plain, value_ct, hint, sort, updated)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )?
    .execute(params![
        f.id,
        ws,
        f.item_id,
        f.label,
        f.kind,
        f.secret as i64,
        f.value_plain,
        f.value_ct,
        f.hint,
        f.sort,
        f.updated
    ])?;
    Ok(())
}

/// Whole-value replacement (§4.1: no read-modify-write of a secret).
pub fn set_field_value(
    c: &Connection,
    id: &str,
    plain: Option<&str>,
    ct: Option<&[u8]>,
    hint: &str,
    now: i64,
) -> Result<()> {
    c.prepare_cached(
        "UPDATE vault_fields SET value_ct = zeroblob(length(value_ct)) WHERE id = ? AND value_ct IS NOT NULL",
    )?
    .execute([id])?;
    c.prepare_cached(
        "UPDATE vault_fields SET value_plain = ?, value_ct = ?, hint = ?, updated = ? WHERE id = ?",
    )?
    .execute(params![plain, ct, hint, now, id])?;
    Ok(())
}

pub fn delete_field(c: &Connection, id: &str) -> Result<()> {
    c.prepare_cached(
        "UPDATE vault_fields SET value_ct = zeroblob(length(value_ct)) WHERE id = ? AND value_ct IS NOT NULL",
    )?
    .execute([id])?;
    c.prepare_cached("DELETE FROM vault_fields WHERE id = ?")?
        .execute([id])?;
    Ok(())
}

pub fn delete_fields(c: &Connection, item_id: &str) -> Result<()> {
    c.prepare_cached(
        "UPDATE vault_fields SET value_ct = zeroblob(length(value_ct)) WHERE item_id = ? AND value_ct IS NOT NULL",
    )?
    .execute([item_id])?;
    c.prepare_cached("DELETE FROM vault_fields WHERE item_id = ?")?
        .execute([item_id])?;
    Ok(())
}

// ------------------------------------------------------------------------ accounts, grants, uses

/// `(id, display_name)` of an account of `ws` (A1's table; the display name falls back to the handle).
pub fn account_ref(c: &Connection, ws: &str, id: &str) -> Result<Option<(String, String)>> {
    Ok(c.prepare_cached(
        "SELECT id, CASE WHEN display_name = '' THEN handle_display ELSE display_name END
         FROM accounts WHERE id = ? AND workspace_id = ?",
    )?
    .query_row([id, ws], |r| Ok((r.get(0)?, r.get(1)?)))
    .optional()?)
}

/// An unexpired, unrevoked `secret.use` grant on an item.
#[derive(Debug, Clone, PartialEq)]
pub struct GrantRow {
    pub id: String,
    pub subject_id: String,
    pub scope: Value,
    pub until: i64,
}

pub fn active_grants(c: &Connection, ws: &str, item_id: &str, now: i64) -> Result<Vec<GrantRow>> {
    let mut st = c.prepare_cached(
        "SELECT id, subject_id, scope, until FROM grants
         WHERE workspace_id = ? AND capability = 'secret.use' AND resource_type = 'vault_item' AND resource_id = ?
           AND subject_type = 'bot' AND revoked IS NULL AND until > ?
         ORDER BY created, id",
    )?;
    let rows = st.query_map(params![ws, item_id, now], |r| {
        let scope: String = r.get(2)?;
        Ok(GrantRow {
            id: r.get(0)?,
            subject_id: r.get(1)?,
            scope: serde_json::from_str(&scope).unwrap_or(Value::Object(Default::default())),
            until: r.get(3)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Revokes every open grant on an item (sensitivity raised to `high`, item deleted). Returns how many.
pub fn revoke_grants(c: &Connection, ws: &str, item_id: &str, now: i64) -> Result<usize> {
    Ok(c.prepare_cached(
        "UPDATE grants SET revoked = ? WHERE workspace_id = ? AND resource_type = 'vault_item' AND resource_id = ?
           AND revoked IS NULL AND until > ?",
    )?
    .execute(params![now, ws, item_id, now])?)
}

pub fn project_name(c: &Connection, ws: &str, id: &str) -> Result<Option<String>> {
    Ok(
        c.prepare_cached("SELECT name FROM projects WHERE id = ? AND workspace_id = ?")?
            .query_row([id, ws], |r| r.get(0))
            .optional()?,
    )
}

/// A `secret_uses` row (§4.5). Holds no value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UseRow {
    pub id: String,
    pub item_id: String,
    pub actor_type: String,
    pub actor_id: String,
    pub purpose: String,
    pub mode: String,
    pub fields: Vec<String>,
    pub status: String,
    pub created: i64,
}

pub fn insert_use(c: &Connection, ws: &str, u: &UseRow) -> Result<()> {
    c.prepare_cached(
        "INSERT INTO secret_uses (id, workspace_id, item_id, actor_type, actor_id, purpose, mode, fields, status, created, finished)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )?
    .execute(params![
        u.id,
        ws,
        u.item_id,
        u.actor_type,
        u.actor_id,
        u.purpose,
        u.mode,
        serde_json::to_string(&u.fields).unwrap_or_else(|_| "[]".into()),
        u.status,
        u.created,
        u.created
    ])?;
    Ok(())
}

/// Whether an `internal` use with this status was already recorded for the item and actor since
/// `since` (§4.4: internal uses are merged into one row per connection and day; failures each get one).
pub fn has_internal_use(
    c: &Connection,
    ws: &str,
    item_id: &str,
    actor_id: &str,
    since: i64,
) -> Result<bool> {
    Ok(c.prepare_cached(
        "SELECT 1 FROM secret_uses WHERE workspace_id = ? AND item_id = ? AND mode = 'internal' AND actor_id = ?
           AND status = 'ok' AND created >= ? LIMIT 1",
    )?
    .query_row(params![ws, item_id, actor_id, since], |_| Ok(()))
    .optional()?
    .is_some())
}
