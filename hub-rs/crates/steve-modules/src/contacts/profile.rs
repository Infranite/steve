//! What is known about a person or an organization, and why the rest is not:
//!
//! - [`org_name`]: whether an organization's name is yours, the provider's, Steve's placeholder or still
//!   the workspace's, and why a placeholder cannot be replaced by anything but you;
//! - [`contact_profile`]: per field (name, organization, title, email) known or missing, with the
//!   reason the last provider lookup gave (`account_profiles`, migration 025) — nothing guessed;
//! - [`name_edited`] / [`manual_fields`]: what a person set by hand, which no sync overwrites;
//! - [`record_lookup`] / [`due`]: the lookups' bookkeeping and retry schedule (1 h → 6 h → 24 h → 72 h).

use serde::Serialize;
use serde_json::{Map, Value, json};
use steve_store::base::links;
use steve_store::{
    Accounts, Attributes, Connections, Read, Revisions, Tx, UserRows, Workspaces,
    account_profiles::{AccountProfiles, Lookup, LookupUpdate},
};

use super::repo::{self, PARTY, Party};
use crate::ApiResult;

/// The accounts family of organization keys (Feishu `tenant_key`).
const TENANT_FAMILY: &str = "feishu_tenant";
/// The placeholder name prefix of an external organization ([`crate::accounts::EXTERNAL_ORG`]).
const EXTERNAL_ORG: &str = "外部组织";
/// The Feishu scope that names the own organization.
pub const TENANT_SCOPE: &str = "tenant:tenant:readonly";
/// Retry delays after a lookup that left gaps, by attempt (the last repeats).
pub const RETRY_MS: [i64; 4] = [3_600_000, 6 * 3_600_000, 24 * 3_600_000, 72 * 3_600_000];
/// Has a person (not a sync) ever set this party's name? A revision whose `after` names a `name`
/// without `"source": "sync"`.
pub fn name_edited(r: &mut impl Read, ws: &str, party: &str) -> ApiResult<bool> {
    Ok(Revisions(r).field_written(ws, PARTY, party, "$.after.name", false)?)
}

/// A person chose the contact's kind.
pub fn kind_edited(r: &mut impl Read, ws: &str, party: &str) -> ApiResult<bool> {
    Ok(Revisions(r).field_written(ws, PARTY, party, "$.after.kind", false)?)
}

/// The job fields a person edited (`props.manual`), which no sync overwrites.
pub fn manual_fields(props: &Value) -> Vec<String> {
    props["manual"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect()
}

/// The fields of `party`'s job at `org` a person edited: `props.manual`, and — for edits made before
/// that was recorded — every `title` / `dept` a job revision (`PATCH /contacts/:id/jobs/:org`, which
/// syncs never write) changed.
pub fn job_manual_fields(
    c: &mut impl Read,
    ws: &str,
    party: &str,
    org: &str,
    props: &Value,
) -> ApiResult<Vec<String>> {
    let mut out = manual_fields(props);
    for v in Revisions(c).job_patches(ws, party, org)? {
        for k in ["title", "dept"] {
            if v["before"]["job"]["props"][k] != v["after"]["job"]["props"][k]
                && !out.iter().any(|m| m == k)
            {
                out.push(k.to_string());
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------- organizations

/// Where an organization's name comes from.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OrgName {
    /// `manual` | `provider` | `placeholder` | `workspace`
    pub name_state: &'static str,
    /// `external_unavailable` | `permission`
    pub name_reason: Option<&'static str>,
    pub name_scope: Option<&'static str>,
    pub tenant: Option<Tenant>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Tenant {
    pub key: String,
    pub own: bool,
}

/// Where the organization's name comes from.
pub fn org_name(c: &mut impl Read, ws: &str, p: &Party) -> ApiResult<OrgName> {
    let key = Accounts(c)
        .of_party(ws, &p.id)?
        .into_iter()
        .find(|row| row.family == TENANT_FAMILY && row.status == "active")
        .map(|row| row.handle);
    let tenant = key.map(|k| Tenant {
        key: k,
        own: p.is_self_org(),
    });
    let edited = name_edited(c, ws, &p.id)?;
    let mut out = OrgName {
        name_state: "provider",
        name_reason: None,
        name_scope: None,
        tenant,
    };
    if edited {
        out.name_state = "manual";
    } else if p.is_self_org() {
        let wn = Workspaces(c).name(ws)?.unwrap_or_else(|| "本公司".into());
        let synced = Revisions(c).field_written(ws, PARTY, &p.id, "$.after.name", true)?;
        if !synced && (p.name == wn || p.name == "本公司") {
            out.name_state = "workspace";
            if out.tenant.is_some() && Connections(c).permission_missing(ws, TENANT_SCOPE)? {
                out.name_reason = Some("permission");
                out.name_scope = Some(TENANT_SCOPE);
            }
        }
    } else if p.source == "sync" && p.name.starts_with(EXTERNAL_ORG) {
        out.name_state = "placeholder";
        out.name_reason = Some("external_unavailable");
    } else if p.source != "sync" {
        // created by hand (`POST /orgs`, an import): its name was chosen
        out.name_state = "manual";
    }
    Ok(out)
}

/// May a provider rename this organization? Only Steve's placeholder or the untouched own-organization
/// name — never a name a person set (also one that happens to start with “外部组织”).
pub fn org_name_replaceable(c: &mut impl Read, ws: &str, p: &Party) -> ApiResult<bool> {
    Ok(matches!(
        org_name(c, ws, p)?.name_state,
        "placeholder" | "workspace"
    ))
}

// ---------------------------------------------------------------- people

/// One field of a contact's profile.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FieldState {
    /// `known` | `missing`
    pub state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manual: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
}

impl FieldState {
    fn known(manual: bool) -> Self {
        Self {
            state: "known",
            manual: manual.then_some(true),
            reason: None,
            scope: None,
        }
    }
    fn missing(reason: &str, scope: Option<String>) -> Self {
        Self {
            state: "missing",
            manual: None,
            reason: Some(reason.to_string()),
            scope,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProfileFields {
    pub name: FieldState,
    pub org: FieldState,
    pub title: FieldState,
    pub email: FieldState,
}

/// A contact's `profile`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ContactProfile {
    pub checked: Option<i64>,
    pub next_check: Option<i64>,
    /// `directory` | `chat` | `local`
    pub source: Option<String>,
    pub fields: ProfileFields,
}

/// The identities that can look people up: `(connections.adapter, systems.id)`, the adapter's
/// profile view and the system of the people it looks up. Registered when the channels are built
/// (each channel side with a profile view, by its declared handle system); shared by every
/// [`contact_profile`] of the process.
static LOOKUPS: std::sync::RwLock<Vec<(String, String)>> = std::sync::RwLock::new(Vec::new());

/// Makes the profile view of `adapter`, which looks up people of `system`, known to
/// [`contact_profile`] (“can this person be looked up?”).
pub fn register_lookup(adapter: &str, system: &str) {
    let mut l = LOOKUPS.write().unwrap_or_else(|e| e.into_inner());
    let x = (adapter.to_string(), system.to_string());
    if !l.contains(&x) {
        l.push(x);
    }
}

/// Can any identity of the workspace look this person up: they have an account in a system a
/// registered profile view looks up, and the workspace a live connection of that adapter?
fn reachable(r: &mut impl Read, ws: &str, party: &str) -> ApiResult<bool> {
    let lookups = LOOKUPS.read().unwrap_or_else(|e| e.into_inner()).clone();
    let accounts = Accounts(r).of_party(ws, party)?;
    for (adapter, system) in lookups {
        if accounts
            .iter()
            .any(|a| a.status == "active" && a.system_id == system)
            && Connections(r).usable_adapter(ws, &adapter)?
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// What is known about `p` and why the rest is not. `visible_orgs`: the organizations of the jobs
/// the viewer sees (the contact page); `None` counts every job (the lookups' own judgement).
pub fn contact_profile(
    c: &mut impl Read,
    ws: &str,
    p: &Party,
    visible_orgs: Option<&[String]>,
) -> ApiResult<ContactProfile> {
    let Lookup {
        checked,
        next_check,
        source,
        gaps,
    } = AccountProfiles(c).latest(ws, &p.id)?.unwrap_or(Lookup {
        checked: None,
        next_check: None,
        source: None,
        gaps: Value::Null,
    });
    let reachable = reachable(c, ws, &p.id)?;
    // a missing field: the reason the last lookup gave, else “not looked up yet” / “no source”
    let why = |field: &str| -> FieldState {
        let g = &gaps[field];
        if let Some(r) = g["reason"].as_str() {
            return FieldState::missing(r, g["scope"].as_str().map(str::to_string));
        }
        if reachable {
            FieldState::missing("pending", None)
        } else {
            FieldState::missing("no_source", None)
        }
    };
    let name = if crate::accounts::looks_like_handle(&p.name) {
        why("name")
    } else {
        FieldState::known(p.is_me() || name_edited(c, ws, &p.id)?)
    };
    let job = links::Links(c)
        .from(ws, &super::service::pref(&p.id), Some("works_at"))?
        .into_iter()
        .filter(|l| l.props.get("to").is_none_or(Value::is_null))
        .filter(|l| visible_orgs.is_none_or(|v| v.contains(&l.object_id)))
        .max_by_key(|l| l.created);
    let org = match &job {
        Some(l) => FieldState::known(l.source == "manual"),
        None => why("org"),
    };
    let title = match &job {
        Some(l)
            if l.props["title"]
                .as_str()
                .is_some_and(|t| !t.trim().is_empty()) =>
        {
            FieldState::known(
                l.source == "manual"
                    || job_manual_fields(c, ws, &p.id, &l.object_id, &l.props)?
                        .iter()
                        .any(|f| f == "title"),
            )
        }
        _ => why("title"),
    };
    let mail = Accounts(c)
        .of_party(ws, &p.id)?
        .into_iter()
        .find(|a| a.family == "email" && a.status == "active")
        .map(|a| a.source);
    let email = match mail {
        Some(src) => FieldState::known(!matches!(src.as_str(), "sync" | "chat" | "identify")),
        None => match Attributes(c)
            .values(ws, &p.id)?
            .into_iter()
            .find(|v| v.key == "email" && v.status == "confirmed")
        {
            Some(v) => FieldState::known(v.source == "me"),
            None => why("email"),
        },
    };
    Ok(ContactProfile {
        checked,
        next_check,
        source,
        fields: ProfileFields {
            name,
            org,
            title,
            email,
        },
    })
}

// ---------------------------------------------------------------- lookup bookkeeping

/// Why a field is missing after a lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gap {
    pub reason: String,
    pub scope: Option<String>,
}

impl Gap {
    pub fn new(reason: &str) -> Self {
        Self {
            reason: reason.into(),
            scope: None,
        }
    }
    pub fn scope(reason: &str, scope: &str) -> Self {
        Self {
            reason: reason.into(),
            scope: Some(scope.into()),
        }
    }
}

/// Records one lookup of `account` at `now`: where the answer came from and the gaps it left. No gaps
/// clears the retry; gaps schedule the next one (1 h, 6 h, 24 h, then 72 h).
pub fn record_lookup(
    tx: &Tx,
    ws: &str,
    account: &str,
    now: i64,
    source: Option<&str>,
    gaps: &[(&str, Gap)],
) -> ApiResult<()> {
    tx.write(|w| {
        let account_row = Accounts(w)
            .get(account)?
            .filter(|row| row.workspace_id == ws);
        if account_row.is_none() {
            return Err(crate::ApiError::not_found("账号"));
        }
        let attempts = AccountProfiles(w).attempts(account)?;
        let mut g = Map::new();
        for (f, gap) in gaps {
            let mut v = json!({"reason": gap.reason});
            if let Some(s) = &gap.scope {
                v["scope"] = json!(s);
            }
            g.insert((*f).to_string(), v);
        }
        // only what a retry could change is retried: an external person's title never comes
        let retry = gaps
            .iter()
            .any(|(_, x)| !matches!(x.reason.as_str(), "external"));
        let (attempts, next) = if retry {
            (attempts + 1, Some(now + retry_after_ms(attempts)))
        } else {
            (0, None)
        };
        AccountProfiles(w).upsert(&LookupUpdate {
            account,
            workspace: ws,
            at: now,
            next,
            attempts,
            source,
            gaps: &Value::Object(g),
        })?;
        Ok(())
    })
}

/// Is `account` due for a lookup at `now` (never looked up, or its retry time has come)?
pub fn due(r: &mut impl Read, account: &str, now: i64) -> ApiResult<bool> {
    Ok(AccountProfiles(r)
        .retry(account)?
        .is_none_or(|row| row.is_some_and(|at| at <= now)))
}

/// A recorded retry is due even when the provider cache still considers its answer fresh.
pub fn retry_due(r: &mut impl Read, account: &str, now: i64) -> ApiResult<bool> {
    Ok(AccountProfiles(r)
        .retry(account)?
        .flatten()
        .is_some_and(|at| at <= now))
}

/// Everyone of `ws` with gaps is due now (a connection changed: a permission may have been granted).
pub fn all_due(tx: &Tx, ws: &str, now: i64) -> ApiResult<usize> {
    Ok(tx.write(|w| AccountProfiles(w).mark_due(ws, now))?)
}

/// Fill only a missing email from the Hub's own user record. Connected account profiles and
/// jobs are already synchronized by their modules; machine-local git/locale are not Hub data.
/// Any existing value (including a rejected/stale one) suppresses automatic re-creation.
pub fn supplement_me(
    w: &mut steve_store::W<'_>,
    ws: &str,
    user: &str,
    party: &str,
) -> ApiResult<()> {
    if Attributes(w)
        .values(ws, party)?
        .iter()
        .any(|a| a.key == "email")
    {
        return Ok(());
    }
    if Revisions(w).removed_attribute(ws, party, "email")? {
        return Ok(());
    }
    let email = UserRows(w).email(user)?;
    if let Some(email) = email.filter(|e| !e.trim().is_empty() && e.len() <= 500) {
        let v = Attributes(w).insert_value(
            ws,
            &repo::NewValue {
                party_id: party.into(),
                key: "email".into(),
                label: "邮箱".into(),
                value: email.trim().into(),
                source: "profile".into(),
                source_label: "Hub 用户资料".into(),
                source_ref: Some(user.into()),
                level: steve_store::base::Level::Conclusion,
                status: "confirmed".into(),
                decision_id: None,
            },
        )?;
        if let Some(p) = w.contacts().party(party)? {
            super::service::revise_in(
                w,
                &p,
                &repo::PartyPatch::default(),
                json!({"attr": null}),
                json!({"source": "sync", "attr": {"id": v.id, "key": v.key, "source": "profile"}}),
                user,
            )?;
        }
        w.emit(crate::accounts::service::party_changed(ws, party));
    }
    Ok(())
}

/// A confirmed, usable way to address this user. `call` is exclusively internal;
/// `call_external` is exclusively for mentioning them to other people. Scoped overrides
/// are JSON `call_ext_scope` facts: {"kind":"persona"|"space", "id":"…", "name":"…"}.
/// A conversation override wins over a persona override. Private/internal nicknames never
/// become the external fallback; the contact name does.
pub fn address(
    c: &mut impl steve_store::Read,
    ws: &str,
    user: &str,
    external: bool,
    persona: Option<&str>,
    space: Option<&str>,
) -> ApiResult<Option<String>> {
    let Some(me) = repo::me_of(c, ws, user)? else {
        return Ok(None);
    };
    let mut values = repo::values(c, ws, &me.id)?;
    values.sort_by_key(|a| a.updated);
    let usable = |a: &&repo::AttrValue| {
        a.status == "confirmed"
            && a.level != steve_store::base::Level::Never
            && (!external || a.level != steve_store::base::Level::Reasoning)
            && !a.value.trim().is_empty()
    };
    if external {
        for (kind, id) in [("space", space), ("persona", persona)] {
            if let Some(id) = id {
                for a in values
                    .iter()
                    .rev()
                    .filter(usable)
                    .filter(|a| a.key == "call_ext_scope")
                {
                    if let Ok(v) = serde_json::from_str::<Value>(&a.value) {
                        if v["kind"] == kind && v["id"] == id {
                            if let Some(name) = v["name"].as_str().filter(|n| !n.trim().is_empty())
                            {
                                return Ok(Some(name.trim().into()));
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(Some(
        values
            .iter()
            .rev()
            .filter(usable)
            .find(|a| a.key == if external { "call_external" } else { "call" })
            .map(|a| a.value.clone())
            .unwrap_or(me.name),
    ))
}

/// Scope facts have a small explicit schema; arbitrary prose is never interpreted as a scope.
pub fn valid_address_fact(key: &str, value: &str) -> bool {
    if key != "call_ext_scope" {
        return true;
    }
    let Ok(v) = serde_json::from_str::<Value>(value) else {
        return false;
    };
    matches!(v["kind"].as_str(), Some("persona" | "space"))
        && v["id"]
            .as_str()
            .is_some_and(|s| !s.trim().is_empty() && s.len() <= 120)
        && v["name"]
            .as_str()
            .is_some_and(|s| !s.trim().is_empty() && s.chars().count() <= 80)
}

/// Profile lookups keep their explicit third wait; other attempts use a capped backoff.
fn retry_after_ms(attempts: i64) -> i64 {
    // This domain's irregular sequence is clearer as a table than as an exponential plus override.
    RETRY_MS[usize::try_from(attempts)
        .unwrap_or(usize::MAX)
        .min(RETRY_MS.len() - 1)]
}

#[cfg(test)]
mod runtime_policy_sequence {
    #[test]
    fn r7_profile_backoff_sequence() {
        for (attempt, hours) in [(0, 1), (1, 6), (2, 24), (3, 72), (4, 72), (100, 72)] {
            assert_eq!(super::retry_after_ms(attempt), hours * 3_600_000);
        }
    }
}
