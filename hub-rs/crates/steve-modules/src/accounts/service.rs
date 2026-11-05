//! Domain rules of accounts: handle normalization, recording, editing and
//! removing accounts, the systems list and the match rules. The `*_tx` functions run inside a caller's
//! transaction, so the REST handlers, the conversational-config operations and identification share them.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use steve_store::base::{self, Viewer};
use steve_store::{ChangeEvent, Tx};

use super::repo::{
    self, ACCOUNT, Account, AccountPatch, AccountQuery, ConnectionBrief, ItemRef, MatchRule,
    NewAccount, System,
};
use crate::contacts::repo::{self as people, Party};
use crate::contacts::service as contacts;
use crate::{ApiError, ApiResult, Caller, Check, Modules, own};

/// Account kinds.
pub const KINDS: [&str; 11] = [
    "personal",
    "work_email",
    "app_bot",
    "shared_mailbox",
    "bank_card",
    "service",
    "phone",
    "bot",
    "calendar",
    "root",
    "other",
];

/// `source_label` per `source` when the caller gives none (the prototype's wording).
pub fn default_label(source: &str) -> &'static str {
    match source {
        "me" => "你填的",
        "connect" => "你接入",
        "rule:email_prefix" => "邮箱前缀规则",
        "sync" => "通讯录同步",
        "chat" => "你说的",
        _ => "",
    }
}

pub(crate) fn event(tx: &Tx, ws: &str, id: &str) {
    tx.publish(ChangeEvent::Account {
        id: id.into(),
        ws: ws.into(),
    });
}

/// The party's page shows its accounts and counts them.
pub(crate) fn party_event(tx: &Tx, ws: &str, id: &str) {
    tx.publish(ChangeEvent::Party {
        id: id.into(),
        ws: ws.into(),
    });
}

// ---------------------------------------------------------------- handles

/// A handle after the system's `handle_rule`: what is stored and looked up, and what is shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Handle {
    pub handle: String,
    pub display: String,
}

/// Normalizes a raw handle for `system`. `email`: lower case, no white space, `local@domain`;
/// `phone`: digits and a leading `+`; `lower`; `none`: trimmed. Cards keep only the last four digits:
/// `card:<last4>:<system>`, shown as `•••• 4821`; the full number belongs in the vault. `None` when
/// nothing usable is left.
pub fn normalize(system: &System, raw: &str) -> Option<Handle> {
    if system.family == "card" {
        let digits: Vec<char> = raw.chars().filter(char::is_ascii_digit).collect();
        if digits.len() < 4 {
            return None;
        }
        let last4: String = digits[digits.len() - 4..].iter().collect();
        return Some(Handle {
            handle: format!("card:{last4}:{}", system.id),
            display: format!("•••• {last4}"),
        });
    }
    let h = match system.normalize() {
        "email" => {
            let h: String = raw
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect::<String>()
                .to_lowercase();
            let (local, domain) = h.split_once('@')?;
            if local.is_empty() || domain.is_empty() || domain.contains('@') {
                return None;
            }
            h
        }
        "phone" => {
            let t = raw.trim();
            let digits: String = t.chars().filter(char::is_ascii_digit).collect();
            if digits.is_empty() {
                return None;
            }
            if t.starts_with('+') {
                format!("+{digits}")
            } else {
                digits
            }
        }
        "lower" => raw.trim().to_lowercase(),
        _ => raw.trim().to_string(),
    };
    if h.is_empty() {
        return None;
    }
    let display = if system.normalize() == "email" || system.normalize() == "lower" {
        h.clone()
    } else {
        raw.trim().to_string()
    };
    Some(Handle { handle: h, display })
}

// ---------------------------------------------------------------- seeds, start

/// Seeds the built-in systems and the workspace's default match rule when missing.
pub fn ensure_seeds(tx: &Tx, ws: &str) -> ApiResult<()> {
    if !repo::builtins_seeded(tx)? {
        repo::seed_builtins(tx)?;
    }
    if repo::prefix_rule(tx, ws)?.is_none() {
        repo::seed_rule(tx, ws)?;
    }
    Ok(())
}

/// Like contacts' `start`: “我” and the own organization exist, the seeds are there. Returns the
/// caller's viewer and “我”.
pub fn start(m: &Modules, who: &Caller) -> ApiResult<(Viewer, Party)> {
    let seeded = m.store.reader(|c| {
        Ok::<_, ApiError>(repo::builtins_seeded(c)? && repo::prefix_rule(c, &who.ws)?.is_some())
    })?;
    if !seeded {
        m.store.transaction(|tx| ensure_seeds(tx, &who.ws))?;
    }
    contacts::start(m, who)
}

/// The system `id` of `ws`, or `404 没有这个系统`.
pub fn own_system(c: &rusqlite::Connection, ws: &str, id: &str) -> ApiResult<System> {
    repo::system(c, ws, id)?.ok_or_else(|| ApiError::not_found("系统"))
}

/// The noun of a party type in `404 没有这个<noun>`.
fn noun(kind: &str) -> &'static str {
    match kind {
        "org" => "组织",
        "team" => "团队",
        _ => "联系人",
    }
}

/// An active contact, organization or team the viewer can see: an account's owner.
pub fn own_owner(c: &rusqlite::Connection, viewer: &Viewer, id: &str) -> ApiResult<Party> {
    let p = people::get(c, id)?;
    let n = p.as_ref().map(|p| noun(&p.kind)).unwrap_or("联系人");
    let p =
        p.filter(|p| p.status == "active" && matches!(p.kind.as_str(), "contact" | "org" | "team"));
    own(p, viewer, n)
}

/// An account of the viewer's workspace whose owner the viewer can see; `404 没有这个账号` otherwise.
pub fn own_account(c: &rusqlite::Connection, viewer: &Viewer, id: &str) -> ApiResult<Account> {
    let a = repo::get(c, id)?.filter(|a| a.workspace_id == viewer.ws);
    let Some(a) = a else {
        return Err(ApiError::not_found("账号"));
    };
    let visible = people::get(c, &a.party_id)?
        .is_some_and(|p| p.status != "deleted" && viewer.sees(&p.visibility, &p.created_by));
    if visible {
        Ok(a)
    } else {
        Err(ApiError::not_found("账号"))
    }
}

/// Follows `merged_into` to the party that holds a merged contact's data now (at most 32 hops).
pub fn current_party(c: &rusqlite::Connection, id: &str) -> ApiResult<Option<Party>> {
    let mut p = people::get(c, id)?;
    for _ in 0..32 {
        match &p {
            Some(x) if x.status == "merged" => match &x.merged_into {
                Some(next) => p = people::get(c, next)?,
                None => break,
            },
            _ => break,
        }
    }
    Ok(p)
}

/// Can an account of this party have a connection (“我”, the own organization, a team)?
pub fn may_connect(p: &Party) -> bool {
    p.is_me() || p.is_self_org() || p.kind == "team" || p.contact_kind.as_deref() == Some("bot")
}

/// Is this party one of ours — the own organization, or a bot party one of whose accounts is
/// connected (an app bot of ours)?
pub fn is_ours(c: &rusqlite::Connection, p: &Party) -> ApiResult<bool> {
    if p.is_self_org() {
        return Ok(true);
    }
    if p.contact_kind.as_deref() != Some("bot") {
        return Ok(false);
    }
    Ok(c.prepare_cached(
        "SELECT 1 FROM connections c JOIN accounts a ON a.id = c.account_id WHERE a.party_id = ? AND c.status != 'revoked'",
    )?
    .exists([&p.id])?)
}

// ---------------------------------------------------------------- shapes

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SystemRef {
    pub id: String,
    pub name: String,
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PartyRef {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub is_me: bool,
}

/// `AccountOut`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AccountOut {
    pub id: String,
    pub system: SystemRef,
    pub party: PartyRef,
    pub kind: String,
    /// The display form (`•••• 4821` for a card).
    pub handle: String,
    pub display_name: String,
    pub verified: bool,
    pub confidence: f64,
    pub source: String,
    pub source_label: String,
    pub connection: Option<ConnectionBrief>,
    pub vault_items: Vec<ItemRef>,
    pub revision: i64,
}

pub fn out(c: &rusqlite::Connection, viewer: &Viewer, a: &Account) -> ApiResult<AccountOut> {
    let system = match repo::system(c, &a.workspace_id, &a.system_id)? {
        Some(s) => SystemRef {
            name: crate::shown::system_name(&s.id, s.builtin, &s.name),
            id: s.id,
            kind: s.kind,
        },
        None => SystemRef {
            id: a.system_id.clone(),
            name: a.system_id.clone(),
            kind: "other".into(),
        },
    };
    let party = match people::get(c, &a.party_id)? {
        Some(p) => PartyRef {
            is_me: p.is_me() && p.user_id.as_deref() == Some(viewer.user.as_str()),
            id: p.id,
            name: p.name,
            kind: p.kind,
        },
        None => PartyRef {
            id: a.party_id.clone(),
            name: String::new(),
            kind: "contact".into(),
            is_me: false,
        },
    };
    Ok(AccountOut {
        id: a.id.clone(),
        system,
        party,
        kind: a.kind.clone(),
        handle: a.handle_display.clone(),
        display_name: if a.display_name.is_empty() {
            a.handle_display.clone()
        } else {
            a.display_name.clone()
        },
        verified: a.verified,
        confidence: a.confidence,
        source: a.source.clone(),
        source_label: crate::shown::source_label(&a.source_label),
        connection: repo::connection_of(c, &a.id)?,
        vault_items: repo::vault_items_of(c, &a.id, &viewer.user)?,
        revision: a.revision,
    })
}

// ---------------------------------------------------------------- reads

#[derive(Debug, Clone, Default, Deserialize)]
pub struct AccountsParams {
    pub party: Option<String>,
    pub scope: Option<String>,
    pub system: Option<String>,
}

/// `GET /accounts?party=&scope=mine|company|others&system=`.
pub fn list(m: &Modules, who: &Caller, p: AccountsParams) -> ApiResult<Vec<AccountOut>> {
    let (viewer, me) = start(m, who)?;
    let some = |s: Option<String>| s.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let scope = some(p.scope);
    if let Some(s) = &scope {
        Check::new()
            .one_of("scope", s, &["mine", "company", "others"])
            .done()?;
    }
    m.store.reader(|c| {
        let mut f = AccountQuery {
            system: some(p.system),
            ..Default::default()
        };
        if let Some(party) = some(p.party) {
            let id = if party == "me" { me.id.clone() } else { party };
            let p = people::get(c, &id)?.filter(|p| p.status != "deleted");
            let n = p.as_ref().map(|p| noun(&p.kind)).unwrap_or("联系人");
            f.party = Some(own(p, &viewer, n)?.id);
        }
        let self_org = people::self_org(c, &who.ws)?;
        match scope.as_deref() {
            Some("mine") => f.party = Some(me.id.clone()),
            Some("company") => match &self_org {
                Some(o) => f.party = Some(o.id.clone()),
                None => return Ok(Vec::new()),
            },
            Some("others") => {
                f.not_parties.push(me.id.clone());
                f.not_parties.extend(self_org.map(|o| o.id));
            }
            _ => {}
        }
        repo::list(c, &viewer, &f)?
            .iter()
            .map(|a| out(c, &viewer, a))
            .collect()
    })
}

/// `GET /accounts/:id`.
pub fn get(m: &Modules, who: &Caller, id: &str) -> ApiResult<AccountOut> {
    let (viewer, _) = start(m, who)?;
    m.store.reader(|c| {
        let a = own_account(c, &viewer, id)?;
        out(c, &viewer, &a)
    })
}

/// `GET /systems`.
pub fn systems(m: &Modules, who: &Caller) -> ApiResult<Vec<System>> {
    start(m, who)?;
    let mut xs = m.store.reader(|c| repo::systems(c, &who.ws))?;
    for s in &mut xs {
        s.name = crate::shown::system_name(&s.id, s.builtin, &s.name);
    }
    Ok(xs)
}

// ---------------------------------------------------------------- writes

/// What recording an account needs, before normalization.
#[derive(Debug, Clone)]
pub struct Record {
    pub system_id: String,
    pub party_id: String,
    pub kind: String,
    pub handle: String,
    pub display_name: String,
    pub verified: bool,
    pub confidence: f64,
    pub source: String,
    pub source_label: String,
}

/// `409 这个账号已经记在「<主人>」名下`. The owner's name is shown only when the viewer may see it.
pub(crate) fn taken(
    c: &rusqlite::Connection,
    viewer: &Viewer,
    account: &str,
) -> ApiResult<ApiError> {
    let owner = match repo::get(c, account)? {
        Some(a) => current_party(c, &a.party_id)?,
        None => None,
    };
    let name = owner
        .filter(|p| viewer.sees(&p.visibility, &p.created_by))
        .map(|p| p.name)
        .unwrap_or_else(|| steve_i18n::t!("modules.accounts.someone_else"));
    Ok(ApiError::conflict(steve_i18n::t!(
        "modules.accounts.taken",
        name = name
    )))
}

/// Records an account for an owner the caller already checked. Handle conflicts are `409`.
pub fn record_tx(tx: &Tx, viewer: &Viewer, actor: &str, r: &Record) -> ApiResult<Account> {
    let system = own_system(tx, &viewer.ws, &r.system_id)?;
    let h = normalize(&system, &r.handle).ok_or_else(|| ApiError::invalid("handle"))?;
    if let Some(existing) = repo::by_handle(tx, &viewer.ws, &system.family, &h.handle)? {
        return Err(taken(tx, viewer, &existing)?);
    }
    let a = repo::insert(
        tx,
        &viewer.ws,
        &NewAccount {
            system_id: system.id.clone(),
            family: system.family.clone(),
            party_id: r.party_id.clone(),
            kind: r.kind.clone(),
            handle: h.handle,
            handle_display: h.display,
            display_name: r.display_name.clone(),
            verified: r.verified,
            confidence: r.confidence,
            source: r.source.clone(),
            source_label: r.source_label.clone(),
        },
    )?;
    base::audit(
        tx,
        &viewer.ws,
        actor,
        "account.create",
        &a.id,
        &json!({ "system": a.system_id, "party": a.party_id, "source": a.source }),
    )?;
    event(tx, &viewer.ws, &a.id);
    party_event(tx, &viewer.ws, &a.party_id);
    Ok(a)
}

#[derive(Debug, Clone, Deserialize)]
pub struct NewAccountBody {
    pub system_id: String,
    pub party_id: Option<String>,
    pub kind: String,
    pub handle: String,
    pub display_name: Option<String>,
}

/// `POST /accounts`: only recorded, not connected (`source=me`).
pub fn create(m: &Modules, who: &Caller, b: NewAccountBody) -> ApiResult<AccountOut> {
    let (viewer, me) = start(m, who)?;
    Check::new()
        .one_of("kind", &b.kind, &KINDS)
        .len("handle", b.handle.trim(), 1, 200)
        .opt_len("display_name", b.display_name.as_deref(), 0, 200)
        .done()?;
    let id = m.store.transaction(|tx| {
        let party = match b.party_id.as_deref() {
            None | Some("me") => me.clone(),
            Some(id) => own_owner(tx, &viewer, id)?,
        };
        let a = record_tx(
            tx,
            &viewer,
            &who.me,
            &Record {
                system_id: b.system_id.clone(),
                party_id: party.id,
                kind: b.kind.clone(),
                handle: b.handle.clone(),
                display_name: b
                    .display_name
                    .clone()
                    .unwrap_or_default()
                    .trim()
                    .to_string(),
                verified: true,
                confidence: 1.0,
                source: "me".into(),
                source_label: default_label("me").into(),
            },
        )?;
        Ok::<_, ApiError>(a.id)
    })?;
    get(m, who, &id)
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct AccountPatchBody {
    pub display_name: Option<String>,
    pub kind: Option<String>,
    pub party_id: Option<String>,
    pub verified: Option<bool>,
}

/// Edits an account inside a transaction. Changing the owner is a manual correction of
/// identification: the account becomes verified, audit `account.move`. Writes a revision.
pub fn patch_tx(
    tx: &Tx,
    viewer: &Viewer,
    actor: &str,
    id: &str,
    b: &AccountPatchBody,
) -> ApiResult<Account> {
    let a = own_account(tx, viewer, id)?;
    let mut ck = Check::new();
    if let Some(k) = &b.kind {
        ck.one_of("kind", k, &KINDS);
    }
    ck.opt_len("display_name", b.display_name.as_deref(), 0, 200)
        .that("verified", b.verified != Some(false))
        .done()?;
    let owner = match &b.party_id {
        Some(p) if *p != a.party_id => {
            let p = own_owner(tx, viewer, p)?;
            if repo::connection_of(tx, &a.id)?.is_some() && !may_connect(&p) {
                return Err(ApiError::rule(steve_i18n::t!(
                    "modules.accounts.connected_owner"
                )));
            }
            Some(p)
        }
        _ => None,
    };
    let mut patch = AccountPatch::default();
    let (mut before, mut after) = (Map::new(), Map::new());
    if let Some(d) = b.display_name.as_ref().map(|d| d.trim().to_string())
        && d != a.display_name
    {
        before.insert("display_name".into(), json!(a.display_name));
        after.insert("display_name".into(), json!(d));
        patch.display_name = Some(d);
    }
    if let Some(k) = b.kind.as_ref().filter(|k| **k != a.kind) {
        before.insert("kind".into(), json!(a.kind));
        after.insert("kind".into(), json!(k));
        patch.kind = Some(k.clone());
    }
    let verify = b.verified == Some(true) || owner.is_some();
    if verify && (!a.verified || a.confidence < 1.0) {
        before.insert("verified".into(), json!(a.verified));
        before.insert("confidence".into(), json!(a.confidence));
        after.insert("verified".into(), json!(true));
        after.insert("confidence".into(), json!(1.0));
        patch.verified = Some(true);
        patch.confidence = Some(1.0);
    }
    if let Some(p) = &owner {
        before.insert("party_id".into(), json!(a.party_id));
        after.insert("party_id".into(), json!(p.id));
        patch.party_id = Some(p.id.clone());
    }
    if patch == AccountPatch::default() {
        return Ok(a);
    }
    let version = a.revision + 1;
    base::revision(
        tx,
        &viewer.ws,
        ACCOUNT,
        &a.id,
        version,
        &Value::Object(before),
        &Value::Object(after.clone()),
        actor,
    )?;
    repo::update(tx, &a.id, &patch, Some(version))?;
    match &owner {
        Some(p) => {
            base::audit(
                tx,
                &viewer.ws,
                actor,
                "account.move",
                &a.id,
                &json!({ "from": a.party_id, "to": p.id }),
            )?;
            party_event(tx, &viewer.ws, &a.party_id);
            party_event(tx, &viewer.ws, &p.id);
        }
        None => base::audit(
            tx,
            &viewer.ws,
            actor,
            "account.update",
            &a.id,
            &json!({ "fields": after.keys().collect::<Vec<_>>() }),
        )?,
    }
    event(tx, &viewer.ws, &a.id);
    repo::get(tx, &a.id)?.ok_or_else(|| ApiError::not_found("账号"))
}

/// `PATCH /accounts/:id`.
pub fn patch(m: &Modules, who: &Caller, id: &str, b: AccountPatchBody) -> ApiResult<AccountOut> {
    let (viewer, _) = start(m, who)?;
    m.store
        .transaction(|tx| patch_tx(tx, &viewer, &who.me, id, &b))?;
    get(m, who, id)
}

/// Removes an account inside a transaction: refused while connected, its vault items stay unlinked.
pub fn delete_tx(tx: &Tx, ws: &str, actor: &str, a: &Account) -> ApiResult<()> {
    if repo::connection_of(tx, &a.id)?.is_some() {
        return Err(ApiError::conflict(steve_i18n::t!(
            "modules.accounts.disconnect_first"
        )));
    }
    repo::delete(tx, &a.id)?;
    base::audit(
        tx,
        ws,
        actor,
        "account.delete",
        &a.id,
        &json!({ "system": a.system_id, "party": a.party_id }),
    )?;
    event(tx, ws, &a.id);
    party_event(tx, ws, &a.party_id);
    Ok(())
}

/// `DELETE /accounts/:id`.
pub fn delete(m: &Modules, who: &Caller, id: &str) -> ApiResult<()> {
    let (viewer, _) = start(m, who)?;
    m.store.transaction(|tx| {
        let a = own_account(tx, &viewer, id)?;
        delete_tx(tx, &who.ws, &who.me, &a)
    })
}

// ---------------------------------------------------------------- match rules

/// `GET /match-rules`.
pub fn rules(m: &Modules, who: &Caller) -> ApiResult<Vec<MatchRule>> {
    start(m, who)?;
    Ok(m.store.reader(|c| repo::rules(c, &who.ws))?)
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct RuleConfigPatch {
    pub same_org_only: Option<bool>,
    pub exclude: Option<Vec<String>>,
    pub min_len: Option<i64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct RulePatch {
    pub enabled: Option<bool>,
    pub config: Option<RuleConfigPatch>,
}

/// `PATCH /match-rules/:id`: `exclude` ≤50 prefixes (stored lower case, unique), `min_len` 1..10.
pub fn patch_rule(m: &Modules, who: &Caller, id: &str, b: RulePatch) -> ApiResult<MatchRule> {
    start(m, who)?;
    let exclude = b.config.as_ref().and_then(|c| c.exclude.clone()).map(|xs| {
        let mut out: Vec<String> = Vec::new();
        for x in xs.into_iter().map(|x| x.trim().to_lowercase()) {
            if !out.contains(&x) {
                out.push(x);
            }
        }
        out
    });
    let mut ck = Check::new();
    if let Some(xs) = &exclude {
        ck.list("config.exclude", xs, 50, 64);
    }
    if let Some(n) = b.config.as_ref().and_then(|c| c.min_len) {
        ck.that("config.min_len", (1..=10).contains(&n));
    }
    ck.done()?;
    m.store.transaction(|tx| {
        let r = repo::rule(tx, id)?
            .filter(|r| r.workspace_id == who.ws)
            .ok_or_else(|| ApiError::not_found("规则"))?;
        let mut config = r.config.clone();
        if let Some(c) = &b.config {
            if let Some(v) = c.same_org_only {
                config.same_org_only = v;
            }
            if let Some(v) = c.min_len {
                config.min_len = v;
            }
        }
        if let Some(xs) = &exclude {
            config.exclude = xs.clone();
        }
        let enabled = b.enabled.unwrap_or(r.enabled);
        repo::rule_update(tx, &r.id, enabled, &config)?;
        base::audit(
            tx,
            &who.ws,
            &who.me,
            "match_rule.update",
            &r.id,
            &json!({ "enabled": enabled, "config": config }),
        )?;
        repo::rule(tx, &r.id)?.ok_or_else(|| ApiError::not_found("规则"))
    })
}
