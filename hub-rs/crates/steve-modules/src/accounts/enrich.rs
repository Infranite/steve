//! Completing an identity after it was identified (domain-model.md §2): what a provider tells later
//! about an account — a name, more handles, an email, an avatar, the organization (tenant) — is
//! written back here, and the account's person is settled without asking when the evidence is good:
//!
//! - a name fills the account's empty `display_name` and the person's name while that is still empty
//!   or a raw handle (`ou_…`); a name you set is never overwritten;
//! - an unconfirmed person who now has a name from the provider becomes a real contact (the directory
//!   or the chat member list is an exact source, contacts.md §2.5);
//! - a handle or email that already belongs to someone else is strong evidence: the two people are
//!   merged (the unconfirmed one into the other), never the other way round for two real people;
//! - the organization is linked with a `works_at` job when the person has none yet.
//!
//! Every change of a person goes through a revision whose `after` carries `"source": "sync"`, so the
//! change log tells it from your own edits. [`tenant_org`] maps a provider's organization key
//! (Feishu `tenant_key`, handle family `feishu_tenant`) to an organization party.

use std::sync::Arc;

use serde::Serialize;
use serde_json::{Map, Value, json};
use steve_store::Tx;
use steve_store::base::links::{self, LinkKey, NewLink};
use steve_store::base::{ObjRef, Viewer};

use super::identify::ExtraHandle;
use super::repo::{self, AccountPatch};
use super::service::{self as svc, Record, current_party, event, own_system, party_event};
use crate::contacts::PartyHooks;
use crate::contacts::repo::{self as people, NewParty, PARTY, Party, PartyPatch};
use crate::contacts::service::ensure_self_org;
use crate::{ApiError, ApiResult};

/// The accounts system of organization keys (Feishu `tenant_key`).
pub const TENANT_SYSTEM: &str = "feishu_tenant";
/// The name of an external organization nothing names yet, before the space it was seen in.
pub const EXTERNAL_ORG: &str = "外部组织";

/// What a provider told about one account.
#[derive(Debug, Clone, Default)]
pub struct Enrichment {
    pub name: Option<String>,
    /// More handles of the same identity (Feishu: the `union_id`, valid everywhere, and the `open_id`
    /// of one app, valid under its connection).
    pub handles: Vec<ExtraHandle>,
    pub email: Option<String>,
    pub avatar_url: Option<String>,
    /// The organization party the person belongs to ([`tenant_org`]).
    pub org_id: Option<String>,
    /// Department path at that organization (“基础架构 / 存储 / NoSQL”).
    pub dept: Option<String>,
    /// Job title at that organization (Feishu `job_title`).
    pub title: Option<String>,
    /// Found in the own organization's directory (`source = sync`); otherwise the name came from a
    /// chat (`source = chat`).
    pub directory: bool,
}

/// What [`enrich`] changed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Enriched {
    /// The account got its display name.
    pub account_named: bool,
    /// The person was renamed from a raw handle.
    pub renamed: bool,
    /// The person went from unconfirmed to real.
    pub promoted: bool,
    /// Merged by strong evidence: the person it now belongs to.
    pub merged_into: Option<String>,
    /// A `works_at` job was added.
    pub org_linked: bool,
    /// An empty title or department of their job was filled.
    pub job_filled: bool,
    /// The email became one of their mail accounts.
    pub email_added: bool,
    /// The current owner after all of it.
    pub party_id: String,
}

/// Is `name` what identification falls back to when it knows no name: empty, or a provider id
/// (`ou_…`, `on_…`, `cli_…`, `oc_…`) rather than something a person would call themselves?
pub fn looks_like_handle(name: &str) -> bool {
    let n = name.trim();
    if n.is_empty() {
        return true;
    }
    let Some((prefix, rest)) = n.split_once('_') else {
        return false;
    };
    matches!(prefix, "ou" | "on" | "cli" | "oc" | "u" | "uid")
        && rest.len() >= 6
        && rest.chars().all(|c| c.is_ascii_alphanumeric())
}

fn clip(s: &str, n: usize) -> String {
    s.trim().chars().take(n).collect()
}

/// A name equal to one of the account's handles is a raw handle too.
fn is_raw(c: &rusqlite::Connection, name: &str, account: &str) -> ApiResult<bool> {
    if looks_like_handle(name) {
        return Ok(true);
    }
    Ok(repo::handles_of(c, account)?
        .iter()
        .any(|(h, _)| h.eq_ignore_ascii_case(name.trim())))
}

/// Revises a person with `"source": "sync"` in the change log.
fn revise(
    tx: &Tx,
    p: &Party,
    patch: &PartyPatch,
    before: Map<String, Value>,
    after: Map<String, Value>,
    actor: &str,
) -> ApiResult<()> {
    let mut after = after;
    after.insert("source".into(), json!("sync"));
    crate::contacts::service::revise_as(
        tx,
        p,
        patch,
        Value::Object(before),
        Value::Object(after),
        actor,
    )
}

/// Applies `e` to account `account_id` of `ws` on behalf of `actor` (the connection's owner).
/// `hooks` are the contacts ports' party hooks (merging moves accounts through them).
pub fn enrich(
    tx: &Tx,
    hooks: &[Arc<dyn PartyHooks>],
    ws: &str,
    actor: &str,
    account_id: &str,
    e: &Enrichment,
) -> ApiResult<Enriched> {
    let acc = repo::get(tx, account_id)?.ok_or_else(|| ApiError::not_found("账号"))?;
    let system = own_system(tx, ws, &acc.system_id)?;
    let mut party =
        current_party(tx, &acc.party_id)?.ok_or_else(|| ApiError::not_found("联系人"))?;
    let mut out = Enriched {
        party_id: party.id.clone(),
        ..Enriched::default()
    };
    if party.kind != "contact" || party.status != "active" {
        return Ok(out);
    }
    let viewer = Viewer::load(tx, ws, actor)?;

    // 1. strong evidence: a handle or an email someone already has
    let mut owners: Vec<String> = Vec::new();
    for h in &e.handles {
        let handle = h.handle.trim();
        if handle.is_empty() {
            continue;
        }
        match repo::by_handle(tx, ws, &system.family, handle)? {
            Some(other) if other != acc.id => owners.push(other),
            Some(_) => {}
            None => {
                repo::add_handle(
                    tx,
                    ws,
                    &system.family,
                    handle,
                    &acc.id,
                    h.connection_id.as_deref(),
                )?;
            }
        }
    }
    if let Some(mail) = e
        .email
        .as_deref()
        .map(str::trim)
        .filter(|m| m.contains('@'))
    {
        let ms = own_system(tx, ws, "mail")?;
        if let Some(h) = svc::normalize(&ms, mail)
            && let Some(other) = repo::by_handle(tx, ws, &ms.family, &h.handle)?
        {
            owners.push(other);
        }
    }
    for other in owners {
        let Some(oa) = repo::get(tx, &other)? else {
            continue;
        };
        let Some(op) = current_party(tx, &oa.party_id)?
            .filter(|p| p.status == "active" && p.kind == "contact")
        else {
            continue;
        };
        if op.id == party.id {
            continue;
        }
        let mine_unconfirmed = party.contact_kind.as_deref() == Some("unconfirmed");
        let theirs_unconfirmed = op.contact_kind.as_deref() == Some("unconfirmed");
        // the unconfirmed one goes into the known one; two known people are for you to judge
        let (from, into) = if mine_unconfirmed && !party.is_me() {
            (party.id.clone(), op.id.clone())
        } else if theirs_unconfirmed && !op.is_me() {
            (op.id.clone(), party.id.clone())
        } else {
            continue;
        };
        if !viewer.sees(&op.visibility, &op.created_by) {
            continue;
        }
        crate::contacts::merge::merge_tx(tx, hooks, &viewer, actor, &from, &into)?;
        out.merged_into = Some(into.clone());
        party = people::get(tx, &into)?.ok_or_else(|| ApiError::not_found("联系人"))?;
    }
    out.party_id = party.id.clone();

    // 2. the name
    let name = e
        .name
        .as_deref()
        .map(|n| clip(n, 80))
        .filter(|n| !n.is_empty() && !looks_like_handle(n));
    if let Some(n) = &name
        && is_raw(tx, &acc.display_name, &acc.id)?
        && acc.display_name != *n
    {
        repo::update(
            tx,
            &acc.id,
            &AccountPatch {
                display_name: Some(n.clone()),
                ..AccountPatch::default()
            },
            None,
        )?;
        event(tx, ws, &acc.id);
        out.account_named = true;
    }
    let mut patch = PartyPatch::default();
    let (mut before, mut after) = (Map::new(), Map::new());
    if let Some(n) = &name
        && !party.is_me()
        && is_raw(tx, &party.name, &acc.id)?
        && !crate::contacts::profile::name_edited(tx, ws, &party.id)?
    {
        before.insert("name".into(), json!(party.name));
        after.insert("name".into(), json!(n));
        patch.name = Some(n.clone());
        out.renamed = true;
    }
    let named = name.is_some() || !looks_like_handle(&party.name);
    if party.contact_kind.as_deref() == Some("unconfirmed") && named && name.is_some() {
        before.insert("kind".into(), json!("unconfirmed"));
        after.insert("kind".into(), json!("real"));
        patch.contact_kind = Some("real".into());
        out.promoted = true;
    }
    if !after.is_empty() {
        revise(tx, &party, &patch, before, after, actor)?;
        if out.promoted {
            let src = if e.directory { "sync" } else { "chat" };
            people::set_source(tx, &party.id, src)?;
        }
    }

    // 3. avatar, when the person has none
    if let Some(url) = e
        .avatar_url
        .as_deref()
        .map(str::trim)
        .filter(|u| u.starts_with("https://"))
        && party.avatar.get("url").is_none()
    {
        let mut av = party.avatar.as_object().cloned().unwrap_or_default();
        av.insert("url".into(), json!(url));
        people::set_avatar(tx, &party.id, &Value::Object(av))?;
    }

    // 4. the organization, when the person works nowhere yet
    if let Some(org) = &e.org_id
        && *org != party.id
    {
        let open = links::from(tx, ws, &ObjRef::new(PARTY, &party.id), Some("works_at"))?
            .into_iter()
            .any(|l| l.props.get("to").is_none_or(Value::is_null));
        let key = LinkKey {
            subject: ObjRef::new(PARTY, &party.id),
            predicate: "works_at".into(),
            object: ObjRef::new(PARTY, org),
        };
        let title = e
            .title
            .as_deref()
            .map(|t| clip(t, 80))
            .filter(|t| !t.is_empty());
        let dept = e
            .dept
            .as_deref()
            .map(|t| clip(t, 80))
            .filter(|t| !t.is_empty());
        match links::get(tx, ws, &key)? {
            None if !open => {
                let mut l = NewLink::new(
                    key.subject,
                    "works_at",
                    key.object,
                    if e.directory { "sync" } else { "infer" },
                );
                l.props = json!({ "title": title, "dept": dept, "from": null, "to": null });
                out.org_linked = links::add(tx, ws, &l)?;
            }
            // a job Steve recorded there: fill what is empty, never what you set
            Some(l) if l.source != "manual" && l.props.get("to").is_none_or(Value::is_null) => {
                let manual =
                    crate::contacts::profile::job_manual_fields(tx, ws, &party.id, org, &l.props)?;
                let mut props = if l.props.is_object() {
                    l.props.clone()
                } else {
                    json!({})
                };
                for (k, v) in [("title", &title), ("dept", &dept)] {
                    let empty = props[k].as_str().is_none_or(|x| x.trim().is_empty());
                    if let Some(v) = v
                        && empty
                        && !manual.iter().any(|m| m == k)
                    {
                        props[k] = json!(v);
                    }
                }
                if props != l.props {
                    out.job_filled = links::set_props(tx, ws, &key, &props)?;
                }
            }
            _ => {}
        }
    }

    // 5. the email, as one of their mail accounts, when nobody has it yet
    if let Some(mail) = e
        .email
        .as_deref()
        .map(str::trim)
        .filter(|m| m.contains('@') && m.len() <= 254)
        && out.merged_into.is_none()
    {
        let ms = own_system(tx, ws, "mail")?;
        if let Some(h) = svc::normalize(&ms, mail)
            && repo::by_handle(tx, ws, &ms.family, &h.handle)?.is_none()
        {
            svc::record_tx(
                tx,
                &viewer,
                actor,
                &Record {
                    system_id: ms.id.clone(),
                    party_id: party.id.clone(),
                    kind: "work_email".into(),
                    handle: mail.to_string(),
                    // the person's name as of now (renamed just above, or as it was)
                    display_name: name
                        .clone()
                        .filter(|_| out.renamed)
                        .unwrap_or_else(|| party.name.clone()),
                    verified: e.directory,
                    confidence: if e.directory { 1.0 } else { 0.8 },
                    source: "sync".into(),
                    source_label: "飞书通讯录".into(),
                },
            )?;
            out.email_added = true;
        }
    }

    if out.renamed
        || out.promoted
        || out.org_linked
        || out.job_filled
        || out.email_added
        || out.merged_into.is_some()
        || out.account_named
    {
        party_event(tx, ws, &party.id);
    }
    Ok(out)
}

/// What is known about one organization key.
#[derive(Debug, Clone, Default)]
pub struct TenantHint {
    /// The provider's organization key (`tenant_key`).
    pub key: String,
    /// It is the workspace's own organization (the connection's tenant).
    pub own: bool,
    /// Its name, when the provider gives one.
    pub name: Option<String>,
    /// Where it was seen (a group's name), for the placeholder name “外部组织（<where> 里的成员）”.
    pub seen_in: Option<String>,
}

/// The organization party of a tenant, created when new: the own organization for the own tenant,
/// otherwise one organization per tenant named after what is known. A placeholder name is replaced
/// by a real one later, a name you gave is kept.
pub fn tenant_org(tx: &Tx, ws: &str, actor: &str, t: &TenantHint) -> ApiResult<String> {
    let key = t.key.trim();
    if key.is_empty() {
        return Err(ApiError::invalid("tenant_key"));
    }
    svc::ensure_seeds(tx, ws)?;
    let system = own_system(tx, ws, TENANT_SYSTEM)?;
    let name = t
        .name
        .as_deref()
        .map(|n| clip(n, 80))
        .filter(|n| !n.is_empty());
    if let Some(a) = repo::by_handle(tx, ws, &system.family, key)?
        && let Some(acc) = repo::get(tx, &a)?
        && let Some(p) = current_party(tx, &acc.party_id)?.filter(|p| p.status == "active")
    {
        if let Some(n) = &name
            && p.name != *n
            && placeholder(tx, ws, &p)?
        {
            let mut before = Map::new();
            let mut after = Map::new();
            before.insert("name".into(), json!(p.name));
            after.insert("name".into(), json!(n));
            revise(
                tx,
                &p,
                &PartyPatch {
                    name: Some(n.clone()),
                    ..PartyPatch::default()
                },
                before,
                after,
                actor,
            )?;
            party_event(tx, ws, &p.id);
        }
        return Ok(p.id);
    }
    let org = if t.own {
        let o = ensure_self_org(tx, ws, actor)?;
        if let Some(n) = &name
            && o.name != *n
            && placeholder(tx, ws, &o)?
        {
            let mut before = Map::new();
            let mut after = Map::new();
            before.insert("name".into(), json!(o.name));
            after.insert("name".into(), json!(n));
            revise(
                tx,
                &o,
                &PartyPatch {
                    name: Some(n.clone()),
                    ..PartyPatch::default()
                },
                before,
                after,
                actor,
            )?;
        }
        o
    } else {
        let n = name.clone().unwrap_or_else(|| match &t.seen_in {
            Some(w) if !w.trim().is_empty() => {
                format!("{EXTERNAL_ORG}（{} 里的成员）", clip(w, 40))
            }
            _ => EXTERNAL_ORG.to_string(),
        });
        let mut p = NewParty::new("org", &n, actor);
        p.source = "sync".into();
        p.source_ref = Some(key.to_string());
        let o = people::insert(tx, ws, &p)?;
        steve_store::base::audit(
            tx,
            ws,
            actor,
            "party.create",
            &o.id,
            &json!({ "type": "org", "tenant": key }),
        )?;
        o
    };
    let viewer = Viewer::load(tx, ws, actor)?;
    svc::record_tx(
        tx,
        &viewer,
        actor,
        &Record {
            system_id: system.id.clone(),
            party_id: org.id.clone(),
            kind: "other".into(),
            handle: key.to_string(),
            display_name: name.unwrap_or_default(),
            verified: true,
            confidence: 1.0,
            source: "sync".into(),
            source_label: if t.own {
                "本公司的飞书租户".into()
            } else {
                "飞书外部租户".into()
            },
        },
    )?;
    party_event(tx, ws, &org.id);
    Ok(org.id)
}

/// A name nobody chose: Steve's “外部组织…” placeholder or the own organization still named after the
/// workspace — and never one a person set, whatever it says ([`crate::contacts::profile::org_name`]).
fn placeholder(c: &rusqlite::Connection, ws: &str, p: &Party) -> ApiResult<bool> {
    crate::contacts::profile::org_name_replaceable(c, ws, p)
}

#[cfg(test)]
mod tests {
    use super::looks_like_handle;

    #[test]
    fn raw_handles() {
        assert!(looks_like_handle(""));
        assert!(looks_like_handle("ou_082e2586aa11"));
        assert!(looks_like_handle("on_57d81faa093cd4bd01372f0f4cb0c665"));
        assert!(looks_like_handle("cli_9a30915c503cd108"));
        assert!(!looks_like_handle("张伟"));
        assert!(!looks_like_handle("ou_bob")); // too short to be an id
        assert!(!looks_like_handle("Sarah Chen"));
    }
}
