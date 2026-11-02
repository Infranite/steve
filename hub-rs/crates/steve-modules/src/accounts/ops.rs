//! Conversational-config operations of the accounts module (accounts-vault.md §7, contacts.md §5):
//! `account.add {party, system, handle, kind?}` (the contact target plans it) and
//! `account.patch {id, display_name?, kind?, owner?}` (the account target of V6 plans it).
//! Neither ever carries a secret: a card is recorded by its last four digits only.

use rusqlite::Connection;
use serde::Deserialize;
use serde_json::{Value, json};
use steve_store::Tx;
use steve_store::base::{self, Viewer};

use super::repo::{self, ACCOUNT, AccountPatch};
use super::service::{
    self as svc, AccountPatchBody, KINDS, Record, default_label, own_account, own_owner, own_system,
};
use crate::changes::{ChangeOp, OpCx};
use crate::{ApiError, ApiResult, Check, parse};

fn args<T: for<'de> Deserialize<'de>>(op: &Value) -> ApiResult<T> {
    parse(op.to_string().as_bytes())
}

fn viewer(c: &Connection, cx: &OpCx) -> ApiResult<Viewer> {
    Ok(Viewer::load(c, cx.ws, cx.actor)?)
}

#[derive(Debug, Deserialize)]
struct Add {
    party: String,
    system: String,
    handle: String,
    kind: Option<String>,
    display_name: Option<String>,
}

/// `account.add`: records an account for a contact (`source=chat`). `"as"` binds the new account id.
pub struct AccountAdd;

impl AccountAdd {
    fn kind(a: &Add) -> String {
        a.kind.clone().unwrap_or_else(|| match a.system.as_str() {
            "phone" => "phone".into(),
            "bank" => "bank_card".into(),
            _ => "personal".into(),
        })
    }
}

impl ChangeOp for AccountAdd {
    fn validate(&self, c: &Connection, cx: &OpCx, op: &Value) -> ApiResult<()> {
        let a: Add = args(op)?;
        let v = viewer(c, cx)?;
        own_owner(c, &v, &a.party)?;
        let system = own_system(c, cx.ws, &a.system)?;
        Check::new()
            .one_of("kind", &Self::kind(&a), &KINDS)
            .len("handle", a.handle.trim(), 1, 200)
            .done()?;
        let h = svc::normalize(&system, &a.handle).ok_or_else(|| ApiError::invalid("handle"))?;
        if let Some(existing) = repo::by_handle(c, cx.ws, &system.family, &h.handle)? {
            return Err(svc::taken(c, &v, &existing)?);
        }
        Ok(())
    }

    fn apply(&self, tx: &Tx, cx: &OpCx, op: &Value) -> ApiResult<Value> {
        let a: Add = args(op)?;
        let v = viewer(tx, cx)?;
        let party = own_owner(tx, &v, &a.party)?;
        svc::ensure_seeds(tx, cx.ws)?;
        let acc = svc::record_tx(
            tx,
            &v,
            cx.actor,
            &Record {
                system_id: a.system.clone(),
                party_id: party.id,
                kind: Self::kind(&a),
                handle: a.handle.clone(),
                display_name: a.display_name.clone().unwrap_or_default(),
                verified: true,
                confidence: 1.0,
                source: "chat".into(),
                source_label: default_label("chat").into(),
            },
        )?;
        if let Some(name) = op.get("as").and_then(Value::as_str) {
            cx.bind(name, json!(acc.id));
        }
        Ok(json!({ "id": acc.id }))
    }

    fn undo(&self, tx: &Tx, cx: &OpCx, _: &Value, before: &Value) -> ApiResult<()> {
        let Some(id) = before.get("id").and_then(Value::as_str) else {
            return Ok(());
        };
        match repo::get(tx, id)? {
            Some(a) => svc::delete_tx(tx, cx.ws, cx.actor, &a),
            None => Ok(()),
        }
    }
}

#[derive(Debug, Deserialize)]
struct Patch {
    id: String,
    display_name: Option<String>,
    kind: Option<String>,
    owner: Option<String>,
}

impl Patch {
    fn body(&self) -> AccountPatchBody {
        AccountPatchBody {
            display_name: self.display_name.clone(),
            kind: self.kind.clone(),
            party_id: self.owner.clone(),
            verified: None,
        }
    }
}

/// `account.patch`: display name, kind, owner (a correction of identification).
pub struct AccountPatchOp;

impl ChangeOp for AccountPatchOp {
    fn validate(&self, c: &Connection, cx: &OpCx, op: &Value) -> ApiResult<()> {
        let p: Patch = args(op)?;
        let v = viewer(c, cx)?;
        own_account(c, &v, &p.id)?;
        if let Some(o) = &p.owner {
            own_owner(c, &v, o)?;
        }
        if let Some(k) = &p.kind {
            Check::new().one_of("kind", k, &KINDS).done()?;
        }
        Ok(())
    }

    fn apply(&self, tx: &Tx, cx: &OpCx, op: &Value) -> ApiResult<Value> {
        let p: Patch = args(op)?;
        let v = viewer(tx, cx)?;
        let a = own_account(tx, &v, &p.id)?;
        svc::patch_tx(tx, &v, cx.actor, &p.id, &p.body())?;
        Ok(json!({
            "display_name": a.display_name,
            "kind": a.kind,
            "party_id": a.party_id,
            "verified": a.verified,
            "confidence": a.confidence,
        }))
    }

    fn undo(&self, tx: &Tx, cx: &OpCx, op: &Value, before: &Value) -> ApiResult<()> {
        let p: Patch = args(op)?;
        let Some(a) = repo::get(tx, &p.id)?.filter(|a| a.workspace_id == cx.ws) else {
            return Ok(());
        };
        let s = |k: &str| before.get(k).and_then(Value::as_str).map(String::from);
        let patch = AccountPatch {
            display_name: s("display_name"),
            kind: s("kind"),
            party_id: s("party_id"),
            verified: before.get("verified").and_then(Value::as_bool),
            confidence: before.get("confidence").and_then(Value::as_f64),
        };
        let version = a.revision + 1;
        base::revision(
            tx,
            cx.ws,
            ACCOUNT,
            &a.id,
            version,
            &json!({ "display_name": a.display_name, "kind": a.kind, "party_id": a.party_id }),
            before,
            cx.actor,
        )?;
        repo::update(tx, &a.id, &patch, Some(version))?;
        if patch.party_id.as_deref().is_some_and(|p| p != a.party_id) {
            base::audit(
                tx,
                cx.ws,
                cx.actor,
                "account.move",
                &a.id,
                &json!({ "from": a.party_id, "to": patch.party_id, "undo": cx.changeset_id }),
            )?;
            svc::party_event(tx, cx.ws, &a.party_id);
            if let Some(p) = &patch.party_id {
                svc::party_event(tx, cx.ws, p);
            }
        }
        svc::event(tx, cx.ws, &a.id);
        Ok(())
    }
}
