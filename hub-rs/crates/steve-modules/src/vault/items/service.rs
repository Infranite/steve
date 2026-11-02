//! Item rules (accounts-vault.md §4.1): the three vaults, items by template, fields, masked output,
//! whole-value replacement of secrets, and delete = destroy the data key. Methods on [`Vault`], so
//! they share its KEK cache. Every write is audited and publishes `{type:"vault", id: <item>, ws}`.
//!
//! Plaintext of a secret field exists only as [`Zeroizing`] input on its way into [`seal_field`];
//! decryption is in `reveal.rs` and `credential.rs` (§10 rule 2), not here.

use serde::Serialize;
use serde_json::{Value, json};
use steve_store::{ChangeEvent, Tx, base, now, uid};
use zeroize::Zeroizing;

use super::repo::{self, FieldRow, ItemFilter, ItemRow, VaultRow};
use super::templates::{
    BANK_CARD, CONNECTION_CREDENTIAL, FIELD_TYPES, SENSITIVITIES, Template, builtin,
};
use crate::vault::crypto::seal_field;
use crate::vault::secret::Dek;
use crate::vault::{Vault, VaultError, VaultResult, ensure_active};
use crate::{ApiError, Check};

/// Limits of §5.2.
const TITLE_MAX: usize = 80;
const SITE_MAX: usize = 200;
const LABEL_MAX: usize = 40;
const VALUE_MAX: usize = 20_000;
const FIELDS_MAX: usize = 50;

/// One field of a new item or `POST /vault/items/:id/fields`. No `Debug`: `value` may be a secret.
#[derive(serde::Deserialize)]
pub struct NewField {
    pub label: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub secret: bool,
    #[serde(default)]
    pub value: Zeroizing<String>,
}

/// `POST /vault/items`. No `Debug` (field values).
#[derive(serde::Deserialize)]
pub struct NewItem {
    pub vault_id: String,
    pub template: String,
    pub title: String,
    #[serde(default)]
    pub site: Option<String>,
    #[serde(default)]
    pub account_id: Option<String>,
    #[serde(default)]
    pub sensitivity: Option<String>,
    #[serde(default)]
    pub fields: Vec<NewField>,
}

/// `PATCH /vault/items/:id`. `account_id: null` unlinks the account.
#[derive(Debug, Default, serde::Deserialize)]
pub struct ItemPatch {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub site: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    pub account_id: Option<Option<String>>,
    #[serde(default)]
    pub sensitivity: Option<String>,
}

/// Absent → `None`, `null` → `Some(None)`.
fn nullable<'de, D: serde::Deserializer<'de>, T: serde::Deserialize<'de>>(
    d: D,
) -> Result<Option<Option<T>>, D::Error> {
    serde::Deserialize::deserialize(d).map(Some)
}

/// `GET /vaults`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VaultOut {
    pub id: String,
    pub name: String,
    pub scope: String,
    pub backend: String,
    pub items: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VaultRef {
    pub id: String,
    pub name: String,
    pub scope: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AccountRef {
    pub id: String,
    pub display_name: String,
}

/// A field as every response shows it: a secret field's `value` is always `null` (the web draws the
/// mask), card numbers carry their last four digits in `hint`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FieldOut {
    pub id: String,
    pub label: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub secret: bool,
    pub hint: String,
    pub value: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GrantOut {
    pub id: String,
    pub bot_id: String,
    pub scope: Value,
    pub scope_label: String,
    pub until: i64,
}

/// `VaultItemOut` (§5.1). Never contains a secret value; the list leaves `fields` out entirely.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ItemOut {
    pub id: String,
    pub vault: VaultRef,
    pub template: String,
    pub title: String,
    pub site: String,
    pub account: Option<AccountRef>,
    pub sensitivity: String,
    pub backend: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fields: Option<Vec<FieldOut>>,
    pub grants: Vec<GrantOut>,
    pub created: i64,
    pub updated: i64,
}

fn event(tx: &Tx, ws: &str, item: &str) {
    tx.publish(ChangeEvent::Vault {
        id: item.into(),
        ws: ws.into(),
    });
}

/// `•••• 4821` for a card number (§4.1); other secrets show no hint.
fn hint(kind: &str, value: &str) -> String {
    if kind != "card_number" {
        return String::new();
    }
    let digits: Vec<char> = value.chars().filter(char::is_ascii_digit).collect();
    if digits.len() < 4 {
        return String::new();
    }
    let last: String = digits[digits.len() - 4..].iter().collect();
    format!("•••• {last}")
}

fn check_field(c: &mut Check, at: &str, f: &NewField) {
    c.len(&format!("{at}.label"), f.label.trim(), 1, LABEL_MAX)
        .one_of(&format!("{at}.type"), &f.kind, FIELD_TYPES)
        .that(
            &format!("{at}.value"),
            f.value.encode_utf16().count() <= VALUE_MAX,
        );
}

/// A field row ready to insert: the secret flag decided by the template, the value sealed.
fn sealed_field(
    ws: &str,
    item_id: &str,
    template: &Template,
    dek: Option<&Dek>,
    f: &NewField,
    sort: i64,
    at: i64,
) -> VaultResult<FieldRow> {
    let label = f.label.trim().to_string();
    let secret = f.secret
        || template.id == CONNECTION_CREDENTIAL
        || template.field(&label).is_some_and(|d| d.secret);
    let id = uid("vf");
    let (plain, ct, hint) = if secret {
        let dek = dek.ok_or_else(|| VaultError::internal("vault item without a data key"))?;
        let ct = seal_field(dek, ws, item_id, &id, f.value.as_bytes())?;
        (None, Some(ct), hint(&f.kind, &f.value))
    } else {
        (Some(f.value.to_string()), None, String::new())
    };
    Ok(FieldRow {
        id,
        item_id: item_id.into(),
        label,
        kind: f.kind.clone(),
        secret,
        value_plain: plain,
        value_ct: ct,
        hint,
        sort,
        updated: at,
    })
}

/// The scope of a grant in words: `项目 steve`, `网站 github.com`, `没有项目的事项`.
fn scope_label(c: &rusqlite::Connection, ws: &str, scope: &Value) -> steve_store::Result<String> {
    if let Some(p) = scope.get("project_id").and_then(Value::as_str) {
        let name = repo::project_name(c, ws, p)?.unwrap_or_else(|| p.to_string());
        return Ok(format!("项目 {name}"));
    }
    if let Some(s) = scope.get("site").and_then(Value::as_str) {
        return Ok(format!("网站 {s}"));
    }
    if scope.get("scratch").and_then(Value::as_bool) == Some(true) {
        return Ok("没有项目的事项".into());
    }
    Ok(String::new())
}

/// The output of an item, read on `c`.
pub(crate) fn item_out(
    c: &rusqlite::Connection,
    item: &ItemRow,
    vault: &VaultRow,
    with_fields: bool,
) -> steve_store::Result<ItemOut> {
    let ws = &item.workspace_id;
    let account = match &item.account_id {
        Some(a) => {
            repo::account_ref(c, ws, a)?.map(|(id, display_name)| AccountRef { id, display_name })
        }
        None => None,
    };
    let mut grants = Vec::new();
    for g in repo::active_grants(c, ws, &item.id, now())? {
        grants.push(GrantOut {
            scope_label: scope_label(c, ws, &g.scope)?,
            id: g.id,
            bot_id: g.subject_id,
            scope: g.scope,
            until: g.until,
        });
    }
    let fields = if with_fields {
        Some(
            repo::fields(c, &item.id)?
                .into_iter()
                .map(|f| FieldOut {
                    value: if f.secret { None } else { f.value_plain },
                    id: f.id,
                    label: f.label,
                    kind: f.kind,
                    secret: f.secret,
                    hint: f.hint,
                })
                .collect(),
        )
    } else {
        None
    };
    Ok(ItemOut {
        id: item.id.clone(),
        vault: VaultRef {
            id: vault.id.clone(),
            name: vault.name.clone(),
            scope: vault.scope.clone(),
        },
        template: item.template.clone(),
        title: item.title.clone(),
        site: item.site.clone(),
        account,
        sensitivity: item.sensitivity.clone(),
        backend: vault.backend.clone(),
        fields,
        grants,
        created: item.created,
        updated: item.updated,
    })
}

/// `own_vault_item(ws, me, id)`: active, in `ws`, in a vault `user` may see; else `404 没有这个条目`.
pub(crate) fn own_item(
    c: &rusqlite::Connection,
    ws: &str,
    user: &str,
    id: &str,
) -> VaultResult<(ItemRow, VaultRow)> {
    let item = repo::item(c, id)?.filter(|i| i.workspace_id == ws && i.status == "active");
    let pair = match item {
        Some(i) => repo::vault(c, &i.vault_id)?.map(|v| (i, v)),
        None => None,
    };
    match pair {
        Some((i, v)) if v.visible_to(ws, user) => Ok((i, v)),
        _ => Err(ApiError::not_found("条目").into()),
    }
}

fn own_field(c: &rusqlite::Connection, item: &str, fid: &str) -> VaultResult<FieldRow> {
    repo::fields(c, item)?
        .into_iter()
        .find(|f| f.id == fid)
        .ok_or_else(|| ApiError::not_found("字段").into())
}

fn check_account(c: &rusqlite::Connection, ws: &str, id: Option<&str>) -> VaultResult<()> {
    match id {
        Some(a) if repo::account_ref(c, ws, a)?.is_none() => {
            Err(ApiError::not_found("账号").into())
        }
        _ => Ok(()),
    }
}

impl Vault {
    /// The three vaults of §1 (personal for `user`, shared, system), created on first use, and the
    /// built-in templates. Cheap when they exist (one read).
    pub fn ensure_vaults(&self, ws: &str, user: &str) -> VaultResult<()> {
        let missing = |vs: &[VaultRow]| {
            let has = |scope: &str, owner: Option<&str>| {
                vs.iter().any(|v| {
                    v.scope == scope && (owner.is_none() || v.owner_user.as_deref() == owner)
                })
            };
            !(has("shared", None) && has("system", None) && has("personal", Some(user)))
        };
        let (vs, seeded) = self.store().reader(|c| {
            Ok::<_, VaultError>((
                repo::vaults(c, ws)?,
                repo::templates(c, ws)?.len() >= builtin().len(),
            ))
        })?;
        if !missing(&vs) && seeded {
            return Ok(());
        }
        self.store().transaction(|tx| {
            repo::seed_templates(tx)?;
            let vs = repo::vaults(tx, ws)?;
            if !missing(&vs) {
                return Ok::<_, VaultError>(());
            }
            let at = now();
            let has = |scope: &str| vs.iter().any(|v| v.scope == scope);
            let mk = |name: &str, scope: &str, owner: Option<&str>| VaultRow {
                id: uid("vlt"),
                workspace_id: ws.into(),
                name: name.into(),
                scope: scope.into(),
                owner_user: owner.map(Into::into),
                backend: "builtin".into(),
                created: at,
            };
            if !vs
                .iter()
                .any(|v| v.scope == "personal" && v.owner_user.as_deref() == Some(user))
            {
                repo::insert_vault(tx, &mk("个人", "personal", Some(user)))?;
            }
            if !has("shared") {
                repo::insert_vault(tx, &mk("共享", "shared", None))?;
            }
            if !has("system") {
                repo::insert_vault(tx, &mk("系统", "system", None))?;
            }
            Ok(())
        })
    }

    /// `GET /vaults`: the caller's personal vault and the shared one.
    pub fn list_vaults(&self, ws: &str, user: &str) -> VaultResult<Vec<VaultOut>> {
        self.ensure_vaults(ws, user)?;
        self.store().reader(|c| {
            let counts = repo::item_counts(c, ws)?;
            let mut out: Vec<VaultOut> = repo::vaults(c, ws)?
                .into_iter()
                .filter(|v| v.visible_to(ws, user))
                .map(|v| VaultOut {
                    items: counts.iter().find(|x| x.0 == v.id).map_or(0, |x| x.1),
                    id: v.id,
                    name: v.name,
                    scope: v.scope,
                    backend: v.backend,
                })
                .collect();
            out.sort_by_key(|v| v.scope != "personal");
            Ok(out)
        })
    }

    /// `GET /vault/templates` (without `connection_credential`).
    pub fn templates(&self, ws: &str, user: &str) -> VaultResult<Vec<Template>> {
        self.ensure_vaults(ws, user)?;
        Ok(self
            .store()
            .reader(|c| repo::templates(c, ws))?
            .into_iter()
            .filter(|t| t.id != CONNECTION_CREDENTIAL)
            .collect())
    }

    /// `GET /vault/items`: no `fields` at all in the list.
    pub fn list_items(&self, ws: &str, user: &str, f: &ItemFilter) -> VaultResult<Vec<ItemOut>> {
        self.ensure_vaults(ws, user)?;
        self.store().reader(|c| {
            if let Some(v) = &f.vault
                && !repo::vault(c, v)?.is_some_and(|v| v.visible_to(ws, user))
            {
                return Err(ApiError::not_found("保险库").into());
            }
            let vaults = repo::vaults(c, ws)?;
            let mut out = Vec::new();
            for i in repo::list_items(c, ws, user, f)? {
                if let Some(v) = vaults.iter().find(|v| v.id == i.vault_id) {
                    out.push(item_out(c, &i, v, false)?);
                }
            }
            Ok::<_, VaultError>(out)
        })
    }

    /// `GET /vault/items/:id`.
    pub fn get_item(&self, ws: &str, user: &str, id: &str) -> VaultResult<ItemOut> {
        self.store().reader(|c| {
            let (i, v) = own_item(c, ws, user, id)?;
            Ok::<_, VaultError>(item_out(c, &i, &v, true)?)
        })
    }

    /// `POST /vault/items`.
    pub fn create_item(&self, ws: &str, user: &str, n: NewItem) -> VaultResult<ItemOut> {
        self.ensure_vaults(ws, user)?;
        let (vault, template) = self.store().reader(|c| {
            let v = repo::vault(c, &n.vault_id)?
                .filter(|v| v.visible_to(ws, user))
                .ok_or_else(|| ApiError::not_found("保险库"))?;
            let t = repo::template(c, ws, &n.template)?
                .filter(|t| t.id != CONNECTION_CREDENTIAL)
                .ok_or_else(|| ApiError::not_found("模板"))?;
            check_account(c, ws, n.account_id.as_deref())?;
            Ok::<_, VaultError>((v, t))
        })?;
        let id = self.insert_item(ws, user, &vault, &template, n)?;
        self.get_item(ws, user, &id)
    }

    /// Validates and writes a new item; returns its id. Shared by the API and system items.
    fn insert_item(
        &self,
        ws: &str,
        actor: &str,
        vault: &VaultRow,
        template: &Template,
        n: NewItem,
    ) -> VaultResult<String> {
        let p = self.prepare_item(ws, actor, vault, template, n)?;
        self.store().transaction(|tx| write_prepared(tx, &p))?;
        tracing::info!(ws, item = %p.item.id, template = %p.template, "vault item created");
        Ok(p.item.id.clone())
    }

    /// Validates and seals a new item (the data key and every field), outside any transaction: the
    /// key store may be another process. Nothing is written until [`write_prepared`].
    fn prepare_item(
        &self,
        ws: &str,
        actor: &str,
        vault: &VaultRow,
        template: &Template,
        n: NewItem,
    ) -> VaultResult<PreparedItem> {
        let mut c = Check::new();
        c.len("title", n.title.trim(), 1, TITLE_MAX)
            .opt_len("site", n.site.as_deref(), 0, SITE_MAX)
            .that("fields", n.fields.len() <= FIELDS_MAX);
        if let Some(s) = &n.sensitivity {
            c.one_of("sensitivity", s, SENSITIVITIES);
        }
        for (k, f) in n.fields.iter().enumerate() {
            check_field(&mut c, &format!("fields.{k}"), f);
        }
        c.done()?;
        for (k, f) in n.fields.iter().enumerate() {
            if n.fields[..k]
                .iter()
                .any(|g| g.label.trim() == f.label.trim())
            {
                return Err(
                    ApiError::rule(format!("已经有叫「{}」的字段了", f.label.trim())).into(),
                );
            }
        }
        let sensitivity = n
            .sensitivity
            .clone()
            .unwrap_or_else(|| template.default_sensitivity.clone());
        if template.id == BANK_CARD && sensitivity != template.default_sensitivity {
            return Err(ApiError::rule("银行卡条目的敏感度不能修改").into());
        }
        let id = uid("vi");
        let key = self.new_item_key(ws, &id)?;
        let at = now();
        let mut rows = Vec::with_capacity(n.fields.len());
        for (k, f) in n.fields.iter().enumerate() {
            rows.push(sealed_field(
                ws,
                &id,
                template,
                Some(&key.dek),
                f,
                k as i64,
                at,
            )?);
        }
        let item = ItemRow {
            id: id.clone(),
            workspace_id: ws.into(),
            vault_id: vault.id.clone(),
            template: template.id.clone(),
            title: n.title.trim().into(),
            site: n.site.as_deref().unwrap_or("").trim().into(),
            account_id: n.account_id.clone(),
            sensitivity,
            dek_wrapped: Some(key.wrapped.clone()),
            kek_version: Some(key.kek_version),
            external_ref: None,
            status: "active".into(),
            created_by: actor.into(),
            created: at,
            updated: at,
        };
        Ok(PreparedItem {
            item,
            rows,
            kek_version: key.kek_version,
            actor: actor.into(),
            vault: vault.id.clone(),
            template: template.id.clone(),
        })
    }

    /// `PATCH /vault/items/:id` (§4.1): raising to `high` revokes every grant; a bank card's
    /// sensitivity never changes (§4.3).
    pub fn patch_item(&self, ws: &str, user: &str, id: &str, p: ItemPatch) -> VaultResult<ItemOut> {
        let mut c = Check::new();
        c.opt_len("title", p.title.as_deref().map(str::trim), 1, TITLE_MAX)
            .opt_len("site", p.site.as_deref(), 0, SITE_MAX);
        if let Some(s) = &p.sensitivity {
            c.one_of("sensitivity", s, SENSITIVITIES);
        }
        c.done()?;
        self.store().transaction(|tx| {
            let (before, _) = own_item(tx, ws, user, id)?;
            let mut after = before.clone();
            if let Some(t) = &p.title {
                after.title = t.trim().into();
            }
            if let Some(s) = &p.site {
                after.site = s.trim().into();
            }
            if let Some(a) = &p.account_id {
                check_account(tx, ws, a.as_deref())?;
                after.account_id = a.clone();
            }
            if let Some(s) = &p.sensitivity {
                if before.template == BANK_CARD && *s != before.sensitivity {
                    return Err(ApiError::rule("银行卡条目的敏感度不能修改").into());
                }
                after.sensitivity = s.clone();
            }
            let at = now();
            repo::update_item_meta(tx, &after, at)?;
            let revoked = if after.sensitivity == "high" && before.sensitivity != "high" {
                repo::revoke_grants(tx, ws, id, at)?
            } else {
                0
            };
            let meta = |i: &ItemRow| {
                json!({"title": i.title, "site": i.site, "account_id": i.account_id, "sensitivity": i.sensitivity})
            };
            base::audit(
                tx,
                ws,
                user,
                "vault_item.patch",
                id,
                &json!({"before": meta(&before), "after": meta(&after), "revoked_grants": revoked}),
            )?;
            event(tx, ws, id);
            Ok::<_, VaultError>(())
        })?;
        self.get_item(ws, user, id)
    }

    /// `POST /vault/items/:id/fields`.
    pub fn add_field(&self, ws: &str, user: &str, id: &str, f: NewField) -> VaultResult<ItemOut> {
        let mut c = Check::new();
        check_field(&mut c, "", &f);
        // the extractor's dotted paths start at the body: drop the leading dot
        if let Err(ApiError::Invalid(xs)) = c.done() {
            return Err(ApiError::Invalid(
                xs.into_iter()
                    .map(|x| x.trim_start_matches('.').to_string())
                    .collect(),
            )
            .into());
        }
        let (item, template, existing) = self.store().reader(|c| {
            let (i, _) = own_item(c, ws, user, id)?;
            let t = repo::template(c, ws, &i.template)?
                .ok_or_else(|| VaultError::internal("vault item with an unknown template"))?;
            let fs = repo::fields(c, id)?;
            Ok::<_, VaultError>((i, t, fs))
        })?;
        if existing.len() >= FIELDS_MAX {
            return Err(ApiError::invalid("fields").into());
        }
        if existing.iter().any(|x| x.label == f.label.trim()) {
            return Err(ApiError::rule(format!("已经有叫「{}」的字段了", f.label.trim())).into());
        }
        let dek = self.dek_of(&item)?;
        let sort = existing.iter().map(|x| x.sort).max().map_or(0, |s| s + 1);
        let row = sealed_field(ws, id, &template, dek.as_ref(), &f, sort, now())?;
        self.store().transaction(|tx| {
            own_item(tx, ws, user, id)?;
            repo::insert_field(tx, ws, &row)?;
            repo::touch_item(tx, id, row.updated)?;
            base::audit(
                tx,
                ws,
                user,
                "vault_field.add",
                id,
                &json!({"field": row.id, "label": row.label, "secret": row.secret}),
            )?;
            event(tx, ws, id);
            Ok::<_, VaultError>(())
        })?;
        self.get_item(ws, user, id)
    }

    /// `PUT /vault/items/:id/fields/:fid {value}`: the whole value is replaced (new nonce).
    pub fn put_field(
        &self,
        ws: &str,
        user: &str,
        id: &str,
        fid: &str,
        value: Zeroizing<String>,
    ) -> VaultResult<ItemOut> {
        Check::new()
            .that("value", value.encode_utf16().count() <= VALUE_MAX)
            .done()?;
        let (item, field) = self.store().reader(|c| {
            let (i, _) = own_item(c, ws, user, id)?;
            let f = own_field(c, id, fid)?;
            Ok::<_, VaultError>((i, f))
        })?;
        let at = now();
        let (plain, ct, h) = if field.secret {
            let dek = self
                .dek_of(&item)?
                .ok_or_else(|| VaultError::internal("vault item without a data key"))?;
            let ct = seal_field(&dek, ws, id, fid, value.as_bytes())?;
            (None, Some(ct), hint(&field.kind, &value))
        } else {
            (Some(value.to_string()), None, String::new())
        };
        self.store().transaction(|tx| {
            own_item(tx, ws, user, id)?;
            own_field(tx, id, fid)?;
            repo::set_field_value(tx, fid, plain.as_deref(), ct.as_deref(), &h, at)?;
            repo::touch_item(tx, id, at)?;
            base::audit(
                tx,
                ws,
                user,
                "vault_field.update",
                id,
                &json!({"field": fid, "label": field.label}),
            )?;
            event(tx, ws, id);
            Ok::<_, VaultError>(())
        })?;
        self.get_item(ws, user, id)
    }

    /// `DELETE /vault/items/:id/fields/:fid`.
    pub fn delete_field(&self, ws: &str, user: &str, id: &str, fid: &str) -> VaultResult<ItemOut> {
        self.store().transaction(|tx| {
            own_item(tx, ws, user, id)?;
            let f = own_field(tx, id, fid)?;
            repo::delete_field(tx, fid)?;
            repo::touch_item(tx, id, now())?;
            base::audit(
                tx,
                ws,
                user,
                "vault_field.delete",
                id,
                &json!({"field": fid, "label": f.label}),
            )?;
            event(tx, ws, id);
            Ok::<_, VaultError>(())
        })?;
        self.get_item(ws, user, id)
    }

    /// `DELETE /vault/items/:id` (§4.1): data key and field values destroyed, grants revoked, title
    /// kept for the use log. Returns how many grants were revoked.
    pub fn delete_item(&self, ws: &str, user: &str, id: &str) -> VaultResult<usize> {
        self.store().transaction(|tx| {
            own_item(tx, ws, user, id)?;
            destroy(tx, ws, user, id)
        })
    }

    /// The data key of an item, if it has one (built-in backend).
    pub(crate) fn dek_of(&self, item: &ItemRow) -> VaultResult<Option<Dek>> {
        match (&item.dek_wrapped, item.kek_version) {
            (Some(w), Some(v)) => Ok(Some(self.item_key(&item.workspace_id, &item.id, w, v)?)),
            _ => Ok(None),
        }
    }

    // ------------------------------------------------- system vault (connection credentials, A2)

    /// A `connection_credential` item in the system vault (§2.2): every field is secret. Returns the
    /// item id for `connections.credential_item`.
    pub fn create_credential(
        &self,
        ws: &str,
        actor: &str,
        title: &str,
        account_id: Option<&str>,
        fields: Vec<(String, Zeroizing<String>)>,
    ) -> VaultResult<String> {
        let p = self.prepare_credential(ws, actor, title, account_id, fields)?;
        self.store().transaction(|tx| write_prepared(tx, &p))?;
        tracing::info!(ws, item = %p.item.id, "vault credential created");
        Ok(p.item.id.clone())
    }

    /// [`Self::create_credential`] sealed now and written later in the caller's transaction
    /// ([`Self::insert_prepared`]), so the item and the caller's own rows commit together or not at
    /// all.
    pub fn prepare_credential(
        &self,
        ws: &str,
        actor: &str,
        title: &str,
        account_id: Option<&str>,
        fields: Vec<(String, Zeroizing<String>)>,
    ) -> VaultResult<PreparedItem> {
        self.ensure_vaults(ws, actor)?;
        let (vault, template) = self.store().reader(|c| {
            let v = repo::vaults(c, ws)?
                .into_iter()
                .find(|v| v.scope == "system")
                .ok_or_else(|| VaultError::internal("no system vault"))?;
            let t = repo::template(c, ws, CONNECTION_CREDENTIAL)?
                .ok_or_else(|| VaultError::internal("no connection_credential template"))?;
            check_account(c, ws, account_id)?;
            Ok::<_, VaultError>((v, t))
        })?;
        let n = NewItem {
            vault_id: vault.id.clone(),
            template: template.id.clone(),
            title: title.into(),
            site: None,
            account_id: account_id.map(Into::into),
            sensitivity: None,
            fields: fields
                .into_iter()
                .map(|(label, value)| NewField {
                    label,
                    kind: "password".into(),
                    secret: true,
                    value,
                })
                .collect(),
        };
        self.prepare_item(ws, actor, &vault, &template, n)
    }

    /// Writes a [`Self::prepare_credential`] item in `tx`; returns its id.
    pub fn insert_prepared(&self, tx: &Tx, p: &PreparedItem) -> VaultResult<String> {
        write_prepared(tx, p)?;
        Ok(p.item.id.clone())
    }

    /// `PUT /connections/:id/credential`: replaces all fields of a credential item (new nonces, same
    /// data key).
    pub fn replace_credential(
        &self,
        ws: &str,
        actor: &str,
        item_id: &str,
        fields: Vec<(String, Zeroizing<String>)>,
    ) -> VaultResult<()> {
        let item = self
            .store()
            .reader(|c| repo::item(c, item_id))?
            .filter(|i| {
                i.workspace_id == ws && i.status == "active" && i.template == CONNECTION_CREDENTIAL
            })
            .ok_or_else(|| ApiError::not_found("条目"))?;
        let template = self
            .store()
            .reader(|c| repo::template(c, ws, CONNECTION_CREDENTIAL))?
            .ok_or_else(|| VaultError::internal("no connection_credential template"))?;
        let dek = self.dek_of(&item)?;
        let at = now();
        let mut rows = Vec::new();
        for (k, (label, value)) in fields.into_iter().enumerate() {
            let f = NewField {
                label,
                kind: "password".into(),
                secret: true,
                value,
            };
            let mut c = Check::new();
            check_field(&mut c, &format!("fields.{k}"), &f);
            c.done()?;
            rows.push(sealed_field(
                ws,
                item_id,
                &template,
                dek.as_ref(),
                &f,
                k as i64,
                at,
            )?);
        }
        self.store().transaction(|tx| {
            repo::delete_fields(tx, item_id)?;
            for f in &rows {
                repo::insert_field(tx, ws, f)?;
            }
            repo::touch_item(tx, item_id, at)?;
            base::audit(
                tx,
                ws,
                actor,
                "vault_item.credential",
                item_id,
                &json!({"labels": rows.iter().map(|f| &f.label).collect::<Vec<_>>()}),
            )?;
            event(tx, ws, item_id);
            Ok::<_, VaultError>(())
        })
    }

    /// Disconnecting (§2.2): destroys a credential item like `DELETE /vault/items/:id`, without the
    /// visibility check (system items are never visible). Missing or already deleted is fine.
    pub fn destroy_item(
        &self,
        tx: &Tx,
        ws: &str,
        actor: &str,
        item_id: &str,
    ) -> VaultResult<usize> {
        match repo::item(tx, item_id)? {
            Some(i) if i.workspace_id == ws && i.status == "active" => {
                destroy(tx, ws, actor, item_id)
            }
            _ => Ok(0),
        }
    }
}

/// A sealed item not yet written ([`Vault::prepare_credential`]). Holds only ciphertext and the
/// wrapped data key; no `Debug`.
pub struct PreparedItem {
    item: ItemRow,
    rows: Vec<repo::FieldRow>,
    kek_version: i64,
    actor: String,
    vault: String,
    template: String,
}

impl PreparedItem {
    /// The id the item gets once written.
    pub fn id(&self) -> &str {
        &self.item.id
    }
}

/// Writes a prepared item in `tx` (with its audit row and event); refuses if the key moved on since.
fn write_prepared(tx: &Tx, p: &PreparedItem) -> VaultResult<()> {
    let ws = p.item.workspace_id.as_str();
    ensure_active(tx, ws, p.kek_version)?;
    repo::insert_item(tx, &p.item)?;
    for f in &p.rows {
        repo::insert_field(tx, ws, f)?;
    }
    base::audit(
        tx,
        ws,
        &p.actor,
        "vault_item.create",
        &p.item.id,
        &json!({
            "vault": p.vault, "template": p.template, "title": p.item.title,
            "sensitivity": p.item.sensitivity,
            "fields": p.rows.iter().map(|f| json!({"label": f.label, "secret": f.secret})).collect::<Vec<_>>(),
        }),
    )?;
    event(tx, ws, &p.item.id);
    Ok(())
}

fn destroy(tx: &Tx, ws: &str, actor: &str, id: &str) -> VaultResult<usize> {
    let at = now();
    let revoked = repo::revoke_grants(tx, ws, id, at)?;
    repo::destroy_item(tx, id, at)?;
    base::audit(
        tx,
        ws,
        actor,
        "vault_item.delete",
        id,
        &json!({"revoked_grants": revoked}),
    )?;
    event(tx, ws, id);
    Ok(revoked)
}
