//! What is known about a person or an organization, and why the rest is not:
//!
//! - [`org_name`]: whether an organization's name is yours, the provider's, Steve's placeholder or still
//!   the workspace's, and why a placeholder cannot be replaced by anything but you;
//! - [`contact_profile`]: per field (name, organization, title, email) known or missing, with the
//!   reason the last provider lookup gave (`account_profiles`, migration 025) — nothing guessed;
//! - [`name_edited`] / [`manual_fields`]: what a person set by hand, which no sync overwrites;
//! - [`record_lookup`] / [`due`]: the lookups' bookkeeping and retry schedule (1 h → 6 h → 24 h → 72 h).

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde_json::{Map, Value, json};
use steve_store::Tx;
use steve_store::base::links;

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
pub fn name_edited(c: &Connection, ws: &str, party: &str) -> ApiResult<bool> {
    Ok(c.query_row(
        "SELECT EXISTS (SELECT 1 FROM revisions WHERE workspace_id = ?1 AND object_type = ?2 AND object_id = ?3
            AND json_valid(patch) AND json_type(patch, '$.after.name') IS NOT NULL
            AND coalesce(json_extract(patch, '$.after.source'), '') <> 'sync')",
        params![ws, PARTY, party],
        |r| r.get(0),
    )?)
}

/// A person set the party's kind (confirmed it as real, virtual …): a revision of `kind` that no
/// sync or enrichment wrote. An unconfirmed contact the names run promoted on a provider's name was
/// confirmed by nobody.
pub fn kind_edited(c: &Connection, ws: &str, party: &str) -> ApiResult<bool> {
    Ok(c.query_row(
        "SELECT EXISTS (SELECT 1 FROM revisions WHERE workspace_id = ?1 AND object_type = ?2 AND object_id = ?3
            AND json_valid(patch) AND json_type(patch, '$.after.kind') IS NOT NULL
            AND coalesce(json_extract(patch, '$.after.source'), '') <> 'sync')",
        params![ws, PARTY, party],
        |r| r.get(0),
    )?)
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
    c: &Connection,
    ws: &str,
    party: &str,
    org: &str,
    props: &Value,
) -> ApiResult<Vec<String>> {
    let mut out = manual_fields(props);
    let mut st = c.prepare_cached(
        "SELECT patch FROM revisions WHERE workspace_id = ?1 AND object_type = ?2 AND object_id = ?3
            AND json_valid(patch) AND json_extract(patch, '$.after.job.org_id') = ?4",
    )?;
    let rows = st
        .query_map(params![ws, PARTY, party, org], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for patch in rows {
        let v: Value = serde_json::from_str(&patch).unwrap_or(Value::Null);
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

/// The tenant key an organization stands for (its `feishu_tenant` account).
fn tenant_of(c: &Connection, ws: &str, org: &str) -> ApiResult<Option<String>> {
    Ok(c.query_row(
        "SELECT handle FROM accounts WHERE workspace_id = ? AND party_id = ? AND family = ? AND status = 'active'
          ORDER BY created LIMIT 1",
        params![ws, org, TENANT_FAMILY],
        |r| r.get(0),
    )
    .optional()?)
}

/// Is the own organization's name readable but refused (`tenant:tenant:readonly` noted missing on a
/// connection of the workspace)?
fn tenant_scope_missing(c: &Connection, ws: &str) -> ApiResult<bool> {
    Ok(c.query_row(
        "SELECT EXISTS (SELECT 1 FROM connections cn, json_each(CASE WHEN json_valid(cn.config)
                  THEN coalesce(json_extract(cn.config, '$.permissions'), '[]') ELSE '[]' END) j
          WHERE cn.workspace_id = ? AND json_extract(j.value, '$.scope') = ?
            AND json_extract(j.value, '$.status') = 'missing')",
        params![ws, TENANT_SCOPE],
        |r| r.get(0),
    )?)
}

/// Where the organization's name comes from.
pub fn org_name(c: &Connection, ws: &str, p: &Party) -> ApiResult<OrgName> {
    let key = tenant_of(c, ws, &p.id)?;
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
        let wn = repo::workspace_name(c, ws)?.unwrap_or_else(|| "本公司".into());
        let synced: bool = c.query_row(
            "SELECT EXISTS (SELECT 1 FROM revisions WHERE workspace_id = ?1 AND object_type = ?2 AND object_id = ?3
                AND json_valid(patch) AND json_type(patch, '$.after.name') IS NOT NULL
                AND json_extract(patch, '$.after.source') = 'sync')",
            params![ws, PARTY, p.id],
            |r| r.get(0),
        )?;
        if !synced && (p.name == wn || p.name == "本公司") {
            out.name_state = "workspace";
            if out.tenant.is_some() && tenant_scope_missing(c, ws)? {
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
pub fn org_name_replaceable(c: &Connection, ws: &str, p: &Party) -> ApiResult<bool> {
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

/// `(checked, next_check, source, gaps)` of a lookup row.
type Lookup = (Option<i64>, Option<i64>, Option<String>, Value);

/// The lookup rows of a person's accounts: `(checked, next_check, source, gaps)` of the most recent.
fn lookup_of(c: &Connection, ws: &str, party: &str) -> ApiResult<Option<Lookup>> {
    Ok(c.query_row(
        "SELECT ap.checked, ap.next_check, ap.source, ap.gaps FROM account_profiles ap
           JOIN accounts a ON a.id = ap.account_id AND a.workspace_id = ap.workspace_id
          WHERE ap.workspace_id = ? AND a.party_id = ? AND a.status = 'active'
          ORDER BY ap.checked IS NULL, ap.checked DESC LIMIT 1",
        params![ws, party],
        |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get::<_, String>(3)
                    .ok()
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or(Value::Null),
            ))
        },
    )
    .optional()?)
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
fn reachable(c: &Connection, ws: &str, party: &str) -> ApiResult<bool> {
    let lookups = LOOKUPS.read().unwrap_or_else(|e| e.into_inner()).clone();
    for (adapter, system) in lookups {
        let found: bool = c.query_row(
            "SELECT EXISTS (SELECT 1 FROM accounts WHERE workspace_id = ?1 AND party_id = ?2 AND status = 'active'
                              AND system_id = ?3)
                AND EXISTS (SELECT 1 FROM connections WHERE workspace_id = ?1 AND status IN ('ok', 'error')
                              AND adapter = ?4)",
            params![ws, party, system, adapter],
            |r| r.get(0),
        )?;
        if found {
            return Ok(true);
        }
    }
    Ok(false)
}

/// What is known about `p` and why the rest is not. `visible_orgs`: the organizations of the jobs
/// the viewer sees (the contact page); `None` counts every job (the lookups' own judgement).
pub fn contact_profile(
    c: &Connection,
    ws: &str,
    p: &Party,
    visible_orgs: Option<&[String]>,
) -> ApiResult<ContactProfile> {
    let lookup = lookup_of(c, ws, &p.id)?;
    let (checked, next_check, source, gaps) = lookup.unwrap_or((None, None, None, Value::Null));
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
    let job = links::from(c, ws, &super::service::pref(&p.id), Some("works_at"))?
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
    let mail: Option<String> = c
        .query_row(
            "SELECT source FROM accounts WHERE workspace_id = ? AND party_id = ? AND family = 'email' AND status = 'active'
              ORDER BY created LIMIT 1",
            params![ws, p.id],
            |r| r.get(0),
        )
        .optional()?;
    let email = match mail {
        Some(src) => FieldState::known(!matches!(src.as_str(), "sync" | "chat" | "identify")),
        None => match repo::values(c, ws, &p.id)?
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
/// clears the retry; gaps schedule the next one ([`RETRY_MS`], by attempt).
pub fn record_lookup(
    tx: &Tx,
    ws: &str,
    account: &str,
    now: i64,
    source: Option<&str>,
    gaps: &[(&str, Gap)],
) -> ApiResult<()> {
    // only an account of this workspace
    let mine: bool = tx.query_row(
        "SELECT EXISTS (SELECT 1 FROM accounts WHERE id = ? AND workspace_id = ?)",
        params![account, ws],
        |r| r.get(0),
    )?;
    if !mine {
        return Err(crate::ApiError::not_found("账号"));
    }
    let attempts: i64 = tx
        .query_row(
            "SELECT attempts FROM account_profiles WHERE account_id = ?",
            [account],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0);
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
        let i = (attempts as usize).min(RETRY_MS.len() - 1);
        (attempts + 1, Some(now + RETRY_MS[i]))
    } else {
        (0, None)
    };
    tx.execute(
        "INSERT INTO account_profiles (account_id, workspace_id, checked, next_check, attempts, source, gaps)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(account_id) DO UPDATE SET checked = ?3, next_check = ?4, attempts = ?5,
           source = coalesce(?6, account_profiles.source), gaps = ?7",
        params![account, ws, now, next, attempts, source, Value::Object(g).to_string()],
    )?;
    Ok(())
}

/// Is `account` due for a lookup at `now` (never looked up, or its retry time has come)?
pub fn due(c: &Connection, account: &str, now: i64) -> ApiResult<bool> {
    let row: Option<Option<i64>> = c
        .query_row(
            "SELECT next_check FROM account_profiles WHERE account_id = ?",
            [account],
            |r| r.get(0),
        )
        .optional()?;
    Ok(match row {
        None => true,
        Some(Some(n)) => n <= now,
        Some(None) => false,
    })
}

/// Is a recorded retry of `account` due at `now` (a lookup left gaps and its `next_check` has come)?
/// Such a retry is what the contact page promised: it is made even when a lookup cache says the
/// last answer is still fresh. A person never looked up has no promise to keep.
pub fn retry_due(c: &Connection, account: &str, now: i64) -> ApiResult<bool> {
    Ok(c.query_row(
        "SELECT EXISTS (SELECT 1 FROM account_profiles WHERE account_id = ? AND next_check IS NOT NULL AND next_check <= ?)",
        params![account, now],
        |r| r.get(0),
    )?)
}

/// Everyone of `ws` with gaps is due now (a connection changed: a permission may have been granted).
pub fn all_due(tx: &Tx, ws: &str, now: i64) -> ApiResult<usize> {
    Ok(tx.execute(
        "UPDATE account_profiles SET next_check = ?2, attempts = 0 WHERE workspace_id = ?1 AND gaps <> '{}'",
        params![ws, now],
    )?)
}

/// Fill only a missing email from the Hub's own user record. Connected account profiles and
/// jobs are already synchronized by their modules; machine-local git/locale are not Hub data.
/// Any existing value (including a rejected/stale one) suppresses automatic re-creation.
pub fn supplement_me(tx: &Tx, ws: &str, user: &str, party: &str) -> ApiResult<()> {
    if repo::values(tx, ws, party)?
        .iter()
        .any(|a| a.key == "email")
    {
        return Ok(());
    }
    let removed: bool = tx.query_row(
        "SELECT EXISTS (SELECT 1 FROM revisions WHERE workspace_id = ?1 AND object_type = 'party' AND object_id = ?2
         AND json_valid(patch) AND json_extract(patch, '$.before.attr.key') = 'email'
         AND json_type(patch, '$.after.attr') = 'null')", params![ws, party], |r| r.get(0),
    )?;
    if removed {
        return Ok(());
    }
    let email: Option<String> = tx
        .query_row("SELECT email FROM users WHERE id = ?", [user], |r| r.get(0))
        .optional()?
        .flatten();
    if let Some(email) = email.filter(|e| !e.trim().is_empty() && e.len() <= 500) {
        let v = repo::value_insert(
            tx,
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
        if let Some(p) = repo::get(tx, party)? {
            super::service::revise(
                tx,
                &p,
                &repo::PartyPatch::default(),
                json!({"attr": null}),
                json!({"source": "sync", "attr": {"id": v.id, "key": v.key, "source": "profile"}}),
                user,
            )?;
        }
        super::service::event(tx, ws, party);
    }
    Ok(())
}

/// A confirmed, usable way to address this user. `call` is exclusively internal;
/// `call_external` is exclusively for mentioning them to other people. Scoped overrides
/// are JSON `call_ext_scope` facts: {"kind":"persona"|"space", "id":"…", "name":"…"}.
/// A conversation override wins over a persona override. Private/internal nicknames never
/// become the external fallback; the contact name does.
pub fn address(
    c: &Connection,
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
