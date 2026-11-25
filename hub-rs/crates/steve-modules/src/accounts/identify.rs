//! Who is behind a handle. Called by channel messages, mail, calendar and
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
use steve_store::base::{DecisionKind, ObjRef, Viewer, Visibility};
use steve_store::{
    Accounts, Connections, DecisionRows, MatchRules, MergeProposals, NewDecision, NewMergeProposal,
    Systems, W, uid,
};

use super::repo::{Account, NewAccount, System};
use super::service::{self as svc, normalize, own_system};
use crate::contacts::repo::{self as people, NewParty, PARTY, Party};
use crate::contacts::service as contacts;
use crate::{ApiError, ApiResult};

/// Merge proposals from identification carry this confidence.
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
    /// (`contact_kind = bot`) with no 负责人 yet, never a merge proposal.
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

fn exact_account(
    w: &mut W<'_>,
    ws: &str,
    system: &System,
    h: &svc::Handle,
    hints: &Hints,
) -> ApiResult<Option<Identified>> {
    // 1. exact handle: the primary, then the extra ones
    let mut hit = Accounts(w).by_handle(ws, &system.family, &h.handle)?;
    for x in &hints.extra {
        if hit.is_some() {
            break;
        }
        hit = Accounts(w).by_handle(ws, &system.family, x.handle.trim())?;
    }
    if let Some(account_id) = hit {
        let a = Accounts(w)
            .get(&account_id)?
            .ok_or_else(|| ApiError::not_found("账号"))?;
        // remember the handles seen with it, so a message carrying only one of them still matches
        Accounts(w).add_handle(ws, &system.family, &h.handle, &a.id, None)?;
        add_extra(w, ws, system, &a.id, hints)?;
        let p = w
            .contacts()
            .current_party(&a.party_id)?
            .ok_or_else(|| ApiError::not_found("联系人"))?;
        return Ok(Some(Identified {
            account_id: a.id,
            is_ours: is_ours(w, &p)?,
            party_id: p.id,
            created: false,
            proposal: None,
        }));
    }
    Ok(None)
}

fn is_ours(w: &mut W<'_>, party: &Party) -> ApiResult<bool> {
    if party.is_self_org() {
        return Ok(true);
    }
    if party.contact_kind.as_deref() != Some("bot") {
        return Ok(false);
    }
    for account in Accounts(w).of_party(&party.workspace_id, &party.id)? {
        if Connections(w)
            .brief(&account.id)?
            .is_some_and(|connection| connection.status != "revoked")
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn identified_kind(system: &System, org: Option<&str>, hints: &Hints) -> String {
    hints.kind.clone().unwrap_or_else(|| {
        if system.family == "email" && org.is_some() {
            "work_email".into()
        } else {
            "personal".into()
        }
    })
}

struct AccountTemplate<'a> {
    system: &'a System,
    handle: &'a svc::Handle,
    kind: &'a str,
    display_name: &'a str,
}

impl AccountTemplate<'_> {
    fn for_party(
        &self,
        party: &str,
        verified: bool,
        confidence: f64,
        source: &str,
        label: &str,
    ) -> NewAccount {
        NewAccount {
            system_id: self.system.id.clone(),
            family: self.system.family.clone(),
            party_id: party.into(),
            kind: self.kind.into(),
            handle: self.handle.handle.clone(),
            handle_display: self.handle.display.clone(),
            display_name: self.display_name.into(),
            verified,
            confidence,
            source: source.into(),
            source_label: label.into(),
        }
    }
}

/// Identifies `handle` of `system_id` in `ws` inside the caller's transaction. `actor` is the user on
/// whose behalf it runs (the connection's owner): new contacts are created by them, and only contacts
/// they can see are proposed as a match.
pub fn identify(
    w: &mut W<'_>,
    ws: &str,
    actor: &str,
    system_id: &str,
    handle: &str,
    hints: &Hints,
) -> ApiResult<Identified> {
    svc::ensure_seeds(w, ws)?;
    let system = own_system(w, ws, system_id)?;
    let h = normalize(&system, handle).ok_or_else(|| ApiError::invalid("handle"))?;

    if let Some(known) = exact_account(w, ws, &system, &h, hints)? {
        return Ok(known);
    }

    let org = match (system.family.as_str(), h.handle.split_once('@')) {
        ("email", Some((_, domain))) => w.contacts().domain_owner(ws, domain)?,
        _ => None,
    };
    let kind = identified_kind(&system, org.as_deref(), hints);
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
    let new_account = AccountTemplate {
        system: &system,
        handle: &h,
        kind: &kind,
        display_name: &display_name,
    };

    // 2. the email prefix rule
    if system.family == "email"
        && let Some(owner) = by_prefix(w, ws, &h.handle, org.as_deref())?
    {
        let a = Accounts(w).insert(
            ws,
            &new_account.for_party(
                &owner.id,
                true,
                PREFIX_CONFIDENCE,
                "rule:email_prefix",
                svc::default_label("rule:email_prefix"),
            ),
        )?;
        created(w, ws, actor, &a, &system, hints)?;
        return Ok(Identified {
            account_id: a.id,
            is_ours: is_ours(w, &owner)?,
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
    // a bot is not a person: its own bot party, nobody's yet
    if matches!(kind.as_str(), "bot" | "app_bot") {
        let bot = crate::contacts::stewards::new_bot_party(w, ws, actor, &name, "identify")?;
        let a = Accounts(w).insert(
            ws,
            &new_account.for_party(&bot.id, true, 1.0, &source, &label),
        )?;
        created(w, ws, actor, &a, &system, hints)?;
        return Ok(Identified {
            account_id: a.id,
            party_id: bot.id,
            is_ours: false,
            created: true,
            proposal: None,
        });
    }

    // 3. a fuzzy clue: same name, same organization → proposal; 4. nothing → a new unconfirmed contact
    let viewer = Viewer::bare(ws, actor);
    let clue = same_name(w, &viewer, &display_name, org.as_deref(), hints)?;
    let contact = new_contact(w, ws, actor, &name, org.as_deref(), hints)?;
    let (verified, confidence) = if clue.is_some() {
        (false, PROPOSAL_CONFIDENCE)
    } else {
        (true, 1.0)
    };
    let a = Accounts(w).insert(
        ws,
        &new_account.for_party(&contact.id, verified, confidence, &source, &label),
    )?;
    created(w, ws, actor, &a, &system, hints)?;
    let proposal = match clue {
        Some((into, reason)) => propose(w, ws, &contact, &into, &a, &reason)?,
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

/// One more handle of an identity, in any system: what an adapter knows of a sender besides its
/// primary handle (an email next to an internal user id), with its handles valid only under one
/// connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnyHandle {
    /// A `systems.id`.
    pub system: String,
    pub handle: String,
    pub extra: Vec<ExtraHandle>,
}

/// The account holding `handle` of `system` or one of `extra` (normalized), if any.
fn held(
    w: &mut W<'_>,
    ws: &str,
    system: &System,
    handle: &str,
    extra: &[ExtraHandle],
) -> ApiResult<Option<String>> {
    if let Some(h) = normalize(system, handle)
        && let Some(a) = Accounts(w).by_handle(ws, &system.family, &h.handle)?
    {
        return Ok(Some(a));
    }
    for x in extra {
        let x = x.handle.trim();
        if !x.is_empty()
            && let Some(a) = Accounts(w).by_handle(ws, &system.family, x)?
        {
            return Ok(Some(a));
        }
    }
    Ok(None)
}

/// What a handle attached to a known party gets.
pub(crate) struct Attach<'a> {
    pub kind: &'a str,
    pub display_name: &'a str,
    pub source: &'a str,
    pub source_label: &'a str,
}

/// Attaches `handle` of `system_id` (and its `extra` handles) to `party` as a new account, when
/// nobody holds it yet. A handle held already only gets the extra handles, and only when its
/// account is that party's. Unknown systems and handles that do not normalize are skipped (`None`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn attach_handle(
    w: &mut W<'_>,
    ws: &str,
    actor: &str,
    party: &str,
    system_id: &str,
    handle: &str,
    extra: &[ExtraHandle],
    what: &Attach<'_>,
) -> ApiResult<Option<String>> {
    let Some(system) = Systems(w).get(ws, system_id)? else {
        return Ok(None);
    };
    let Some(h) = normalize(&system, handle) else {
        return Ok(None);
    };
    let hints = Hints {
        extra: extra.to_vec(),
        ..Hints::default()
    };
    if let Some(id) = held(w, ws, &system, handle, extra)? {
        let a = Accounts(w)
            .get(&id)?
            .ok_or_else(|| ApiError::not_found("账号"))?;
        if w.contacts()
            .current_party(&a.party_id)?
            .is_some_and(|p| p.id == party)
        {
            Accounts(w).add_handle(ws, &system.family, &h.handle, &a.id, None)?;
            add_extra(w, ws, &system, &a.id, &hints)?;
        }
        return Ok(Some(id));
    }
    let a = Accounts(w).insert(
        ws,
        &NewAccount {
            system_id: system.id.clone(),
            family: system.family.clone(),
            party_id: party.into(),
            kind: what.kind.into(),
            handle: h.handle,
            handle_display: h.display,
            display_name: what.display_name.trim().into(),
            verified: true,
            confidence: 1.0,
            source: what.source.into(),
            source_label: what.source_label.into(),
        },
    )?;
    created(w, ws, actor, &a, &system, &hints)?;
    Ok(Some(a.id))
}

/// [`identify`] for an identity known by several handles, in any systems: `handle` of `system_id`
/// (with `hints.extra`) and `aliases`. The party already holding one of them (the primary first,
/// then each alias in order) is the one: the primary is attached to it when it is new there, and so
/// is every alias nobody holds yet. With no hit, [`identify`] of the primary decides (a match by
/// rule, a proposal, a new unconfirmed contact) and the aliases are attached to what it found.
/// Without aliases this is [`identify`].
pub fn identify_any(
    w: &mut W<'_>,
    ws: &str,
    actor: &str,
    system_id: &str,
    handle: &str,
    aliases: &[AnyHandle],
    hints: &Hints,
) -> ApiResult<Identified> {
    if aliases.is_empty() {
        return identify(w, ws, actor, system_id, handle, hints);
    }
    svc::ensure_seeds(w, ws)?;
    let system = own_system(w, ws, system_id)?;
    normalize(&system, handle).ok_or_else(|| ApiError::invalid("handle"))?;
    let source = hints.source.clone().unwrap_or_else(|| "identify".into());
    let label = hints
        .source_label
        .clone()
        .unwrap_or_else(|| svc::default_label(&source).into());
    let kind = hints.kind.clone().unwrap_or_else(|| "personal".into());
    let display_name = hints.display_name.clone().unwrap_or_default();
    let what = Attach {
        kind: &kind,
        display_name: &display_name,
        source: &source,
        source_label: &label,
    };
    if held(w, ws, &system, handle, &hints.extra)?.is_none() {
        let mut found = None;
        for a in aliases {
            let Some(sys) = Systems(w).get(ws, &a.system)? else {
                continue;
            };
            let Some(id) = held(w, ws, &sys, &a.handle, &a.extra)? else {
                continue;
            };
            let Some(acc) = Accounts(w).get(&id)? else {
                continue;
            };
            if let Some(p) = w
                .contacts()
                .current_party(&acc.party_id)?
                .filter(|p| p.status == "active")
            {
                found = Some(p.id);
                break;
            }
        }
        if let Some(p) = found {
            attach_handle(w, ws, actor, &p, system_id, handle, &hints.extra, &what)?;
        }
    }
    let id = identify(w, ws, actor, system_id, handle, hints)?;
    for a in aliases {
        attach_handle(
            w,
            ws,
            actor,
            &id.party_id,
            &a.system,
            &a.handle,
            &a.extra,
            &what,
        )?;
    }
    Ok(id)
}

fn add_extra(
    w: &mut W<'_>,
    ws: &str,
    system: &System,
    account: &str,
    hints: &Hints,
) -> ApiResult<()> {
    for x in &hints.extra {
        let handle = x.handle.trim();
        if !handle.is_empty() {
            Accounts(w).add_handle(
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
    w: &mut W<'_>,
    ws: &str,
    actor: &str,
    a: &Account,
    system: &System,
    hints: &Hints,
) -> ApiResult<()> {
    add_extra(w, ws, system, &a.id, hints)?;
    w.audit(
        ws,
        actor,
        "account.create",
        &a.id,
        &json!({ "system": a.system_id, "party": a.party_id, "source": a.source }),
    )?;
    w.emit(svc::account_event(ws, &a.id));
    w.emit(svc::party_changed(ws, &a.party_id));
    Ok(())
}

/// Step 2: the one owner of an email with the same local part, if the rule allows it.
fn by_prefix(w: &mut W<'_>, ws: &str, email: &str, org: Option<&str>) -> ApiResult<Option<Party>> {
    let Some(rule) = MatchRules(w).prefix(ws)?.filter(|r| r.enabled) else {
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
    for (handle, account) in Accounts(w).emails_with_local(ws, local)? {
        if cfg.same_org_only {
            let domain = handle.split_once('@').map(|(_, d)| d).unwrap_or("");
            if w.contacts().domain_owner(ws, domain)?.as_deref() != org {
                continue;
            }
        }
        let Some(a) = Accounts(w).get(&account)? else {
            continue;
        };
        if let Some(p) = w
            .contacts()
            .current_party(&a.party_id)?
            .filter(|p| p.status == "active")
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
    w: &mut W<'_>,
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
    let named = w.contacts().contacts(
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
        for j in contacts::jobs_in(w, viewer, &p.id)?
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
                let aliases = w
                    .contacts()
                    .party(&j.org_id)?
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

/// A new `unconfirmed` contact, with a `works_at` link when the organization is known.
fn new_contact(
    w: &mut W<'_>,
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
    let p = w.contacts().insert(ws, &p)?;
    if let Some(o) = org {
        let mut l = NewLink::new(
            ObjRef::new(PARTY, &p.id),
            "works_at",
            ObjRef::new(PARTY, o),
            "infer",
        );
        l.props = json!({ "title": null, "dept": null, "from": null, "to": null });
        links::Links(w).add(ws, &l)?;
    }
    w.audit(
        ws,
        actor,
        "party.create",
        &p.id,
        &json!({ "type": "contact", "unconfirmed": true }),
    )?;
    w.emit(svc::party_changed(ws, &p.id));
    Ok(p)
}

/// A contact seen for the first time that looks like you, `me` (a user's “我”): opens a merge
/// proposal of `party` into `me` (the `party_merge` decision, which folds out of `DecisionTier::Decide`
/// as low priority), never merges by itself. Merging an unconfirmed inbound contact into “我” without
/// a proven OAuth self-claim (`claim_self`) would escalate an external handle into owner identity.
/// It looks like you when one of `handles` is an email that is an alias of `me` or the user's own
/// email, or when its `name` is your name or one of your aliases and one of its emails is in a
/// domain one of your email accounts uses. Only an unconfirmed, active contact is proposed; a pair
/// rejected before is not proposed again. Returns the proposal id.
pub fn propose_if_me(
    w: &mut W<'_>,
    ws: &str,
    me: &str,
    party: &str,
    account: &str,
    name: Option<&str>,
    handles: &[&str],
) -> ApiResult<Option<String>> {
    let (Some(me), Some(from)) = (w.contacts().party(me)?, w.contacts().current_party(party)?)
    else {
        return Ok(None);
    };
    if !me.is_me()
        || from.id == me.id
        || from.status != "active"
        || from.contact_kind.as_deref() != Some("unconfirmed")
    {
        return Ok(None);
    }
    let Some(a) = Accounts(w).get(account)? else {
        return Ok(None);
    };
    let emails: Vec<String> = handles
        .iter()
        .map(|h| h.trim().to_lowercase())
        .filter(|h| {
            h.split_once('@')
                .is_some_and(|(l, d)| !l.is_empty() && d.contains('.'))
        })
        .collect();
    if emails.is_empty() {
        return Ok(None);
    }
    let user_email = steve_store::UserRows(w)
        .email(me.user_id.as_deref().unwrap_or_default())?
        .map(|email| email.trim().to_lowercase());
    let aliases: Vec<String> = me.aliases.iter().map(|x| x.trim().to_lowercase()).collect();
    let reason = if let Some(e) = emails
        .iter()
        .find(|e| aliases.contains(e) || user_email.as_ref() == Some(*e))
    {
        Some(format!("邮箱 {e} 是「{}」用过的邮箱", me.name))
    } else {
        let key = name.map(|n| n.trim().to_lowercase()).unwrap_or_default();
        let same_name = key.chars().count() >= 2
            && (me.name.trim().to_lowercase() == key || aliases.contains(&key));
        let mut domains: Vec<String> = Vec::new();
        if same_name {
            let owners = w.contacts().merged_party_ids(&me.id)?;
            for handle in Accounts(w).primary_emails(ws, &owners)? {
                if let Some((_, domain)) = handle.trim().to_lowercase().split_once('@') {
                    domains.push(domain.to_owned());
                }
            }
            if let Some(d) = user_email.as_ref().and_then(|e| e.split_once('@')) {
                domains.push(d.1.to_string());
            }
        }
        emails
            .iter()
            .filter_map(|e| e.split_once('@'))
            .find(|(_, d)| domains.iter().any(|x| x == d))
            .map(|(_, d)| {
                format!(
                    "{} 和「{}」同名，邮箱域名 {d} 也是「{}」在用的",
                    name.unwrap_or("").trim(),
                    me.name,
                    me.name
                )
            })
    };
    match reason {
        Some(r) => propose(w, ws, &from, &me, &a, &r),
        None => Ok(None),
    }
}

/// Writes the merge proposal and opens its `party_merge` decision; the `party_merge` handler (P4)
/// acts on `merge` / `not_same` (and undo). `party_merge` falls outside the `DecisionTier::Decide`
/// whitelist, so it folds into passive review with zero blocking cards in "待我决定". A pair
/// rejected before is not proposed again.
fn propose(
    w: &mut W<'_>,
    ws: &str,
    from: &Party,
    into: &Party,
    a: &Account,
    reason: &str,
) -> ApiResult<Option<String>> {
    let mut proposals = MergeProposals(w);
    if proposals.rejected(ws, &from.id, &into.id)? {
        return Ok(None);
    }
    let id = uid("mp");
    if !proposals.insert(&NewMergeProposal {
        id: &id,
        workspace: ws,
        from: &from.id,
        into: &into.id,
        account: &a.id,
        reason,
        confidence: PROPOSAL_CONFIDENCE,
    })? {
        return Ok(None);
    }
    let d = DecisionRows(w).open(&NewDecision {
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
        question_template: None,
        source: Some(steve_store::DecisionSource::Record),
        wait: None,
    })?;
    MergeProposals(w).set_decision(&id, &d.id)?;
    Ok(Some(id))
}
