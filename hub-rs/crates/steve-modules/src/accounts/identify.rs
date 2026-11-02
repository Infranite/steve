//! Who is behind a handle (accounts-vault.md §2.3). Called by channel messages, mail, calendar and
//! directory sync with whatever handle they saw; always ends with an account and a party:
//!
//! 1. an exact handle (`account_handles`, primary or extra) → its owner, following merges;
//! 2. an email whose local part matches exactly one owner's email under the same organization's domains
//!    (the `email_prefix` rule) → that owner;
//! 3. a display name equal to a visible contact's name or alias, in the same organization (by domain or
//!    by the mail signature) → a new unconfirmed contact plus a merge proposal, never a silent merge;
//! 4. otherwise a new unconfirmed contact, working at the domain's organization when there is one.

use serde::Serialize;
use serde_json::json;
use steve_store::base::links::{self, NewLink};
use steve_store::base::{self, DecisionKind, ObjRef, Viewer, Visibility};
use steve_store::{NewDecision, Tx, uid};

use super::repo::{self, Account, NewAccount, System};
use super::service::{self as svc, current_party, event, normalize, own_system, party_event};
use crate::contacts::repo::{self as people, NewParty, PARTY, Party};
use crate::contacts::service as contacts;
use crate::{ApiError, ApiResult};

/// Merge proposals from identification carry this confidence (§2.3 step 3).
pub const PROPOSAL_CONFIDENCE: f64 = 0.6;
/// Accounts attached by the `email_prefix` rule.
pub const PREFIX_CONFIDENCE: f64 = 0.9;

/// What the caller saw besides the handle.
#[derive(Debug, Clone, Default)]
pub struct Hints {
    /// The sender's display name (mail `From` name, chat nickname).
    pub display_name: Option<String>,
    /// The organization a mail signature names (“Sarah Chen · Acme” → `Acme`).
    pub signature_org: Option<String>,
    /// More handles of the same identity, each valid only under a connection (Feishu `open_id` per app).
    pub extra: Vec<ExtraHandle>,
    /// `source` of a new account (default `identify`) and its label (“邮件 · 10-29”, “Acme 客户群”).
    pub source: Option<String>,
    pub source_label: Option<String>,
    /// Account kind of a new account; default `work_email` for an email under a known organization,
    /// else `personal`. `bot` / `app_bot` (another bot seen in a chat) make a new identity a bot party
    /// (`contact_kind = bot`, ownership.md §1) with no 负责人 yet, never a merge proposal.
    pub kind: Option<String>,
    /// Visibility of a new contact (default `workspace`).
    pub visibility: Option<Visibility>,
}

/// A handle that is only valid under one connection (Feishu `open_id`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtraHandle {
    pub handle: String,
    pub connection_id: Option<String>,
}

/// The result of [`identify`].
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Identified {
    pub account_id: String,
    /// The current owner (after merges).
    pub party_id: String,
    /// The own organization's account (an app bot of ours): not a contact to talk to.
    pub is_ours: bool,
    /// A new account was created.
    pub created: bool,
    /// The merge proposal opened (step 3), if any.
    pub proposal: Option<String>,
}

/// Identifies `handle` of `system_id` in `ws` inside the caller's transaction. `actor` is the user on
/// whose behalf it runs (the connection's owner): new contacts are created by them, and only contacts
/// they can see are proposed as a match.
pub fn identify(
    tx: &Tx,
    ws: &str,
    actor: &str,
    system_id: &str,
    handle: &str,
    hints: &Hints,
) -> ApiResult<Identified> {
    svc::ensure_seeds(tx, ws)?;
    let system = own_system(tx, ws, system_id)?;
    let h = normalize(&system, handle).ok_or_else(|| ApiError::invalid("handle"))?;

    // 1. exact handle: the primary, then the extra ones
    let mut hit = repo::by_handle(tx, ws, &system.family, &h.handle)?;
    for x in &hints.extra {
        if hit.is_some() {
            break;
        }
        hit = repo::by_handle(tx, ws, &system.family, x.handle.trim())?;
    }
    if let Some(account_id) = hit {
        let a = repo::get(tx, &account_id)?.ok_or_else(|| ApiError::not_found("账号"))?;
        // remember the handles seen with it, so a message carrying only one of them still matches
        repo::add_handle(tx, ws, &system.family, &h.handle, &a.id, None)?;
        add_extra(tx, ws, &system, &a.id, hints)?;
        let p = current_party(tx, &a.party_id)?.ok_or_else(|| ApiError::not_found("联系人"))?;
        return Ok(Identified {
            account_id: a.id,
            is_ours: svc::is_ours(tx, &p)?,
            party_id: p.id,
            created: false,
            proposal: None,
        });
    }

    let org = match (system.family.as_str(), h.handle.split_once('@')) {
        ("email", Some((_, domain))) => people::domain_owner(tx, ws, domain)?,
        _ => None,
    };
    let kind = hints.kind.clone().unwrap_or_else(|| {
        if system.family == "email" && org.is_some() {
            "work_email".into()
        } else {
            "personal".into()
        }
    });
    let source = hints.source.clone().unwrap_or_else(|| "identify".into());
    let label = hints
        .source_label
        .clone()
        .unwrap_or_else(|| svc::default_label(&source).into());
    let display_name = hints
        .display_name
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .to_string();
    let new_account =
        |party: &str, verified: bool, confidence: f64, source: &str, label: &str| NewAccount {
            system_id: system.id.clone(),
            family: system.family.clone(),
            party_id: party.into(),
            kind: kind.clone(),
            handle: h.handle.clone(),
            handle_display: h.display.clone(),
            display_name: display_name.clone(),
            verified,
            confidence,
            source: source.into(),
            source_label: label.into(),
        };

    // 2. the email prefix rule
    if system.family == "email"
        && let Some(owner) = by_prefix(tx, ws, &h.handle, org.as_deref())?
    {
        let a = repo::insert(
            tx,
            ws,
            &new_account(
                &owner.id,
                true,
                PREFIX_CONFIDENCE,
                "rule:email_prefix",
                svc::default_label("rule:email_prefix"),
            ),
        )?;
        created(tx, ws, actor, &a, &system, hints)?;
        return Ok(Identified {
            account_id: a.id,
            is_ours: svc::is_ours(tx, &owner)?,
            party_id: owner.id,
            created: true,
            proposal: None,
        });
    }

    let name = if display_name.is_empty() {
        h.display.clone()
    } else {
        display_name.clone()
    };
    // a bot is not a person: its own bot party, nobody's yet (ownership.md §3)
    if matches!(kind.as_str(), "bot" | "app_bot") {
        let bot = crate::contacts::stewards::new_bot_party(tx, ws, actor, &name, "identify")?;
        let a = repo::insert(tx, ws, &new_account(&bot.id, true, 1.0, &source, &label))?;
        created(tx, ws, actor, &a, &system, hints)?;
        return Ok(Identified {
            account_id: a.id,
            party_id: bot.id,
            is_ours: false,
            created: true,
            proposal: None,
        });
    }

    // 3. a fuzzy clue: same name, same organization → proposal; 4. nothing → a new unconfirmed contact
    let viewer = Viewer::load(tx, ws, actor)?;
    let clue = same_name(tx, &viewer, &display_name, org.as_deref(), hints)?;
    let contact = new_contact(tx, ws, actor, &name, org.as_deref(), hints)?;
    let (verified, confidence) = if clue.is_some() {
        (false, PROPOSAL_CONFIDENCE)
    } else {
        (true, 1.0)
    };
    let a = repo::insert(
        tx,
        ws,
        &new_account(&contact.id, verified, confidence, &source, &label),
    )?;
    created(tx, ws, actor, &a, &system, hints)?;
    let proposal = match clue {
        Some((into, reason)) => propose(tx, ws, &contact, &into, &a, &reason)?,
        None => None,
    };
    Ok(Identified {
        account_id: a.id,
        party_id: contact.id,
        is_ours: false,
        created: true,
        proposal,
    })
}

fn add_extra(tx: &Tx, ws: &str, system: &System, account: &str, hints: &Hints) -> ApiResult<()> {
    for x in &hints.extra {
        let handle = x.handle.trim();
        if !handle.is_empty() {
            repo::add_handle(
                tx,
                ws,
                &system.family,
                handle,
                account,
                x.connection_id.as_deref(),
            )?;
        }
    }
    Ok(())
}

/// The bookkeeping after a new account: extra handles, audit, events.
fn created(
    tx: &Tx,
    ws: &str,
    actor: &str,
    a: &Account,
    system: &System,
    hints: &Hints,
) -> ApiResult<()> {
    add_extra(tx, ws, system, &a.id, hints)?;
    base::audit(
        tx,
        ws,
        actor,
        "account.create",
        &a.id,
        &json!({ "system": a.system_id, "party": a.party_id, "source": a.source }),
    )?;
    event(tx, ws, &a.id);
    party_event(tx, ws, &a.party_id);
    Ok(())
}

/// Step 2: the one owner of an email with the same local part (§2.3), if the rule allows it.
fn by_prefix(tx: &Tx, ws: &str, email: &str, org: Option<&str>) -> ApiResult<Option<Party>> {
    let Some(rule) = repo::prefix_rule(tx, ws)?.filter(|r| r.enabled) else {
        return Ok(None);
    };
    let Some((local, _)) = email.split_once('@') else {
        return Ok(None);
    };
    let cfg = &rule.config;
    if cfg.exclude.iter().any(|x| x.eq_ignore_ascii_case(local))
        || (local.chars().count() as i64) < cfg.min_len
        || (cfg.same_org_only && org.is_none())
    {
        return Ok(None);
    }
    let mut owners: Vec<Party> = Vec::new();
    for (handle, account) in repo::emails_with_local(tx, ws, local)? {
        if cfg.same_org_only {
            let domain = handle.split_once('@').map(|(_, d)| d).unwrap_or("");
            if people::domain_owner(tx, ws, domain)?.as_deref() != org {
                continue;
            }
        }
        let Some(a) = repo::get(tx, &account)? else {
            continue;
        };
        if let Some(p) = current_party(tx, &a.party_id)?.filter(|p| p.status == "active")
            && !owners.iter().any(|o| o.id == p.id)
        {
            owners.push(p);
        }
    }
    Ok(if owners.len() == 1 {
        owners.pop()
    } else {
        None
    })
}

/// Step 3: the one visible, confirmed contact with this name or alias (case-insensitive, ≥2 characters)
/// who works at the handle's organization or at the organization of the signature. Returns it and the
/// reason shown on the proposal. Ambiguous clues (two such contacts) propose nothing.
fn same_name(
    tx: &Tx,
    viewer: &Viewer,
    display_name: &str,
    org: Option<&str>,
    hints: &Hints,
) -> ApiResult<Option<(Party, String)>> {
    if display_name.chars().count() < 2 {
        return Ok(None);
    }
    let key = display_name.to_lowercase();
    let sig = hints
        .signature_org
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let named = people::contacts(
        tx,
        viewer,
        &people::ContactQuery {
            q: Some(display_name.to_string()),
            limit: 500,
            ..Default::default()
        },
    )?;
    let mut found: Vec<(Party, String)> = Vec::new();
    for p in named {
        let same =
            p.name.to_lowercase() == key || p.aliases.iter().any(|a| a.to_lowercase() == key);
        if !same {
            continue;
        }
        let mut why = None;
        for j in contacts::jobs(tx, viewer, &p.id)?
            .into_iter()
            .filter(|j| j.to.is_none())
        {
            if org == Some(j.org_id.as_str()) {
                why = Some(format!(
                    "{display_name} 的邮箱域名属于 {}，和 {} 同名同组织",
                    j.org_name, p.name
                ));
                break;
            }
            if let Some(s) = sig {
                let aliases = people::get(tx, &j.org_id)?
                    .map(|o| o.aliases)
                    .unwrap_or_default();
                if j.org_name.to_lowercase() == s.to_lowercase()
                    || aliases.iter().any(|a| a.to_lowercase() == s.to_lowercase())
                {
                    why = Some(format!(
                        "邮件签名写着 {display_name} · {s}，和 {} 同名同组织",
                        p.name
                    ));
                    break;
                }
            }
        }
        if let Some(w) = why {
            found.push((p, w));
        }
    }
    Ok(if found.len() == 1 { found.pop() } else { None })
}

/// A new `unconfirmed` contact (contacts.md §2.2), with a `works_at` link when the organization is known.
fn new_contact(
    tx: &Tx,
    ws: &str,
    actor: &str,
    name: &str,
    org: Option<&str>,
    hints: &Hints,
) -> ApiResult<Party> {
    let name: String = name.chars().take(80).collect();
    let mut p = NewParty::new("contact", &name, actor);
    p.contact_kind = Some("unconfirmed".into());
    p.source = "identify".into();
    p.visibility = hints.visibility.clone().unwrap_or(Visibility::Workspace);
    let p = people::insert(tx, ws, &p)?;
    if let Some(o) = org {
        let mut l = NewLink::new(
            ObjRef::new(PARTY, &p.id),
            "works_at",
            ObjRef::new(PARTY, o),
            "infer",
        );
        l.props = json!({ "title": null, "dept": null, "from": null, "to": null });
        links::add(tx, ws, &l)?;
    }
    base::audit(
        tx,
        ws,
        actor,
        "party.create",
        &p.id,
        &json!({ "type": "contact", "unconfirmed": true }),
    )?;
    party_event(tx, ws, &p.id);
    Ok(p)
}

/// Writes the merge proposal and opens its decision (contacts.md §2.7); the `party_merge` handler (P4)
/// acts on the answer. A pair rejected before is not proposed again.
fn propose(
    tx: &Tx,
    ws: &str,
    from: &Party,
    into: &Party,
    a: &Account,
    reason: &str,
) -> ApiResult<Option<String>> {
    if repo::pair_rejected(tx, ws, &from.id, &into.id)? {
        return Ok(None);
    }
    let id = uid("mp");
    if !repo::proposal_insert(
        tx,
        &id,
        ws,
        &from.id,
        &into.id,
        &a.id,
        reason,
        PROPOSAL_CONFIDENCE,
    )? {
        return Ok(None);
    }
    let d = base::decisions::open(
        tx,
        &NewDecision {
            workspace_id: ws.into(),
            kind: DecisionKind::Propose.code().into(),
            subject_type: "party_merge".into(),
            subject_id: id.clone(),
            item_id: None,
            session_id: None,
            question: format!("{} 可能是 {}", a.handle_display, into.name),
            draft: Some(reason.into()),
            options: Some(vec!["merge".into(), "not_same".into()]),
            requested_by: "steve".into(),
        },
    )?;
    repo::proposal_set_decision(tx, &id, &d.id)?;
    Ok(Some(id))
}
