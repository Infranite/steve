//! Names for the people the channels meet (“识别与补全”): an account identified
//! from a message often has only a provider id (`ou_…`). This completes it — without asking — and
//! leaves only what really needs you in 待确认.
//!
//! One run ([`Channels::resolve_names`]) over a workspace:
//! 1. **candidates**: active accounts of people whose account or person name is still empty or a raw
//!    handle, and unconfirmed people (they may be settled now);
//! 2. **what Steve already has** (no network): the name a message's @mention gave the account, and
//!    the name of a private chat whose only other party is the account;
//! 3. **the identities of the installation** ([`ProfileAdapter`], yours first, then the bot): the
//!    members of each space the candidates were seen in, then the provider's directory by id;
//! 4. each answer is applied by [`steve_modules::accounts::enrich`]: names fill empty or raw names
//!    (never one you set), unconfirmed people with a provider name become real, a handle or email
//!    someone else has merges them, and the organization key (`tenant_key`) becomes a `works_at`
//!    job at the own organization or at one organization per external tenant.
//!
//! Missing permissions are written on the identity's connection (`config.permissions`) so the 接入
//! page shows them; once granted, the next run — after the connection changes, or the periodic one —
//! uses them. Lookups are remembered for a while ([`MEMBERS_TTL_MS`], [`HANDLE_TTL_MS`]) so the
//! periodic run costs nothing when nothing is new.

mod resolve;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusqlite::{Connection, params};
use serde::Serialize;
use steve_modules::accounts::connections::repo as conn_repo;
use steve_modules::accounts::{self, Enrichment, ExtraHandle, TenantHint, looks_like_handle};
use steve_runtime::policy::{Limiter, RateLimit};

use crate::channel::{HandleRef, Profile, ProfileAnswer, ProfileQuery, TenantInfo};
use crate::{ChannelResult, Channels};

/// A space's member list is read again after this long.
pub const MEMBERS_TTL_MS: i64 = 6 * 3_600_000;
/// A handle the directory didn't know is asked again after this long.
pub const HANDLE_TTL_MS: i64 = 24 * 3_600_000;
/// A kicked run waits this long for more kicks (a burst of new senders is one run).
pub const DEBOUNCE_MS: i64 = 20_000;
/// Every workspace with profile identities runs this often anyway.
pub const PERIOD_MS: i64 = 30 * 60_000;
/// Spaces whose members are read in one run at most.
pub const MAX_SPACES: usize = 40;

/// What one run did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct NameReport {
    /// Accounts that needed something.
    pub candidates: usize,
    /// Named from what Steve already had (mentions, private chats).
    pub named_local: usize,
    /// Named by a provider lookup.
    pub named_remote: usize,
    /// People renamed from a raw handle.
    pub renamed: usize,
    /// Unconfirmed people who became real.
    pub promoted: usize,
    /// Merged into someone by a shared handle or email.
    pub merged: usize,
    /// `works_at` jobs added from the organization key (stored payloads or a lookup).
    pub orgs_linked: usize,
    /// Accounts still without a name.
    pub nameless: usize,
    /// Lookups made (member lists + directory calls), for the log.
    pub lookups: usize,
    /// Lookups that failed (network, token, provider error): their people are retried, not judged.
    pub failed: usize,
    /// The connections whose lookups failed.
    #[serde(skip)]
    pub failed_conns: Vec<String>,
    /// `(connection, scope)` found missing.
    pub missing: Vec<(String, String)>,
}

/// One account that needs something.
#[derive(Debug, Clone)]
struct Candidate {
    account_id: String,
    actor: String,
    handle: HandleRef,
    /// Has a usable name already (only the person or the org may be open).
    named: bool,
    /// A recorded retry is due: asked again even where a lookup cache is still fresh.
    retry: bool,
}

/// What one call that actually ran with a candidate's handle showed about them (a call skipped as
/// fresh in the cache is no evidence).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// The call failed (network, token, provider error).
    Failed,
    /// That identity was refused the address book for the missing permission.
    Refused,
    /// That identity searched its address book.
    Searched,
    /// Only a chat's member list answered (no address-book search).
    Members,
}

impl Outcome {
    fn of(ans: Option<&ProfileAnswer>) -> Self {
        let contact = crate::adapters::feishu::profiles::CONTACT_SCOPE;
        match ans {
            None => Self::Failed,
            Some(a) if a.missing_scopes.iter().any(|(s, _)| s == contact) => Self::Refused,
            Some(a) if a.granted_scopes.iter().any(|s| s == contact) => Self::Searched,
            Some(_) => Self::Members,
        }
    }
}

/// The own organization keys of the installation whose identity answered for a candidate (or of
/// every one), for [`Channels::note_lookup`].
struct Evidence {
    /// `(connection, outcome)` of every call that actually asked about them this run.
    calls: Vec<(String, Outcome)>,
    own_keys: Vec<String>,
}

/// An identity that can look people up.
#[derive(Debug, Clone)]
struct Identity {
    id: String,
    ws: String,
    adapter: String,
    /// Grouping key of one app: the bot connection's id (`config.app_connection` of yours).
    installation: String,
    /// You, not a bot: asked first.
    own: bool,
}

/// A space a candidate was seen in.
#[derive(Debug, Clone)]
struct SeenSpace {
    ext_id: String,
    name: String,
    connection: String,
    accounts: Vec<String>,
}

#[derive(Default)]
struct Memory {
    /// `(connection, space ext id)` → when its members were read.
    members: HashMap<(String, String), i64>,
    /// `(connection, handle)` → when the directory was asked.
    handles: HashMap<(String, String), i64>,
    /// `connection` → its organization, and when learned.
    tenants: HashMap<String, (Option<TenantInfo>, i64)>,
    /// `ws` → when a kicked run is due.
    due: BTreeMap<String, i64>,
    running: bool,
}

/// The name jobs' memory; part of [`Channels`].
pub struct Names {
    mem: Mutex<Memory>,
    scans: Limiter<String>,
}

impl Default for Names {
    fn default() -> Self {
        Self::with_clock(Arc::new(steve_runtime::SystemClock))
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Names {
    pub(crate) fn with_clock(clock: Arc<dyn steve_runtime::Clock>) -> Self {
        Self {
            mem: Mutex::new(Memory::default()),
            scans: Limiter::with_clock(
                RateLimit::Window {
                    len: Duration::from_millis(PERIOD_MS as u64),
                    max: 1,
                    sliding: true,
                },
                clock,
            ),
        }
    }

    /// Something new to name in `ws` (a sender without a name): one run after [`DEBOUNCE_MS`].
    /// `fresh`: a connection changed (verified, a permission granted) — forget what was looked up.
    pub fn kick(&self, ws: &str, now: i64, fresh: bool) {
        let mut m = lock(&self.mem);
        if fresh {
            m.members.clear();
            m.handles.clear();
            m.tenants.clear();
        }
        let due = now + DEBOUNCE_MS;
        let e = m.due.entry(ws.to_string()).or_insert(due);
        if *e > due {
            *e = due;
        }
    }

    fn take_due(&self, now: i64, all: &[String]) -> Vec<String> {
        let mut m = lock(&self.mem);
        self.scans.prune();
        let mut out: Vec<String> = m
            .due
            .iter()
            .filter(|(_, at)| **at <= now)
            .map(|(w, _)| w.clone())
            .collect();
        for w in &out {
            m.due.remove(w);
        }
        for w in all {
            if !out.contains(w) && self.scans.peek(w).is_ok() {
                out.push(w.clone());
            }
        }
        // Kicks bypass the ordinary cooldown and start a fresh sliding interval too.
        // Record dispatch, even if resolution fails; direct resolve_names calls do not hit it.
        // The policy clock keeps its high-water mark across wall-clock rewinds.
        for w in &out {
            self.scans.hit(w);
        }
        out
    }
}

/// Named people looked up per run for their missing organization, title or email at most.
pub const MAX_PROFILE_CHECKS: usize = 200;

/// Does the person still miss an organization, a job title or an email (contacts `profile`)?
fn incomplete(r: &mut impl steve_store::Read, ws: &str, party: &str) -> ChannelResult<bool> {
    let Some(p) = steve_store::Contacts::new(r).party(party)? else {
        return Ok(false);
    };
    let f = steve_modules::contacts::profile::contact_profile(r, ws, &p, None)?.fields;
    Ok([f.org, f.title, f.email]
        .iter()
        .any(|x| x.state == "missing"))
}

/// `SELECT … WHERE` the account or person still needs something: no name, unconfirmed, or — for
/// people a registered profile view looks up, due for a check
/// ([`steve_modules::contacts::profile::due`]) — a missing
/// organization, title or email (at most [`MAX_PROFILE_CHECKS`]).
fn candidates(
    r: &mut impl steve_store::Read,
    ws: &str,
    now: i64,
    lookups: &[String],
) -> ChannelResult<Vec<Candidate>> {
    let accounts = steve_store::Accounts(r).identification_candidates(ws)?;
    let parties: Vec<_> = accounts.iter().map(|a| a.party_id.as_str()).collect();
    let parties = steve_store::Contacts::new(r).by_ids(&parties)?;
    let mut out = Vec::new();
    let mut checks = 0;
    for account in accounts {
        let Some(party) = parties.get(&account.party_id).filter(|p| {
            p.kind == "contact"
                && p.status == "active"
                && p.user_id.is_none()
                && p.contact_kind.as_deref() != Some("bot")
        }) else {
            continue;
        };
        let (acc, actor, system, handle, display, pname, kind) = (
            account.id,
            party.created_by.clone(),
            account.system_id,
            account.handle,
            account.display_name,
            &party.name,
            &party.contact_kind,
        );
        let raw = |n: &str| looks_like_handle(n) || n.trim() == handle;
        let account_named = !raw(&display);
        let party_named = !raw(pname);
        let unconfirmed = kind.as_deref() == Some("unconfirmed");
        if account_named && party_named && !unconfirmed {
            if !lookups.contains(&system)
                || checks >= MAX_PROFILE_CHECKS
                || !steve_modules::contacts::profile::due(r, &acc, now)?
                || !incomplete(r, ws, &party.id)?
            {
                continue;
            }
            checks += 1;
        }
        let mut h = HandleRef::new(system, handle);
        for (x, _) in steve_store::Accounts(r).handles_of(&acc)? {
            h = h.with_extra(x);
        }
        out.push(Candidate {
            retry: steve_modules::contacts::profile::retry_due(r, &acc, now)?,
            account_id: acc,
            actor,
            handle: h,
            named: account_named || party_named,
        });
    }
    Ok(out)
}

/// Step 2a: the newest non-raw name each account was @-mentioned with.
fn mention_names(c: &Connection, ws: &str) -> ChannelResult<HashMap<String, String>> {
    let mut st = c.prepare(
        "SELECT json_extract(j.value, '$.account_id'), json_extract(j.value, '$.name')
           FROM chan_messages m, json_each(CASE WHEN json_valid(m.mentions) THEN m.mentions ELSE '[]' END) j
          WHERE m.workspace_id = ? AND json_extract(j.value, '$.account_id') IS NOT NULL
          ORDER BY m.created DESC",
    )?;
    let mut out: HashMap<String, String> = HashMap::new();
    let rows = st.query_map(params![ws], |r| {
        Ok((
            r.get::<_, Option<String>>(0)?,
            r.get::<_, Option<String>>(1)?,
        ))
    })?;
    for row in rows {
        let (Some(a), Some(n)) = row? else { continue };
        let n = n.trim().trim_start_matches('@').trim().to_string();
        if !n.is_empty() && !looks_like_handle(&n) && n != "所有人" {
            out.entry(a).or_insert(n);
        }
    }
    Ok(out)
}

/// Step 2b: a private chat named after the other party, whose inbound messages all came from one
/// account that is not the connection's own: that account has the chat's name.
fn dm_names(c: &Connection, ws: &str) -> ChannelResult<HashMap<String, String>> {
    let mut st = c.prepare(
        "SELECT s.name, min(m.sender_account_id), count(DISTINCT m.sender_account_id)
           FROM spaces s JOIN chan_messages m ON m.space_id = s.id AND m.direction = 'in'
          WHERE s.workspace_id = ? AND s.kind = 'dm' AND m.sender_account_id IS NOT NULL
            AND m.sender_account_id NOT IN (SELECT cn.account_id FROM space_identities si
                  JOIN connections cn ON cn.id = si.connection_id WHERE si.space_id = s.id
                 UNION SELECT cn.account_id FROM connections cn WHERE cn.id = s.connection_id)
          GROUP BY s.id",
    )?;
    let mut out = HashMap::new();
    let rows = st.query_map(params![ws], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, i64>(2)?,
        ))
    })?;
    for row in rows {
        let (name, acc, n) = row?;
        let name = name.trim().to_string();
        if n == 1 && !name.is_empty() && name != "私聊" && !looks_like_handle(&name) {
            out.insert(acc, name);
        }
    }
    Ok(out)
}

/// Step 2c: the organization key each person carried in the stored payloads — as a sender
/// (`sender.tenant_key`; the long connection keeps the whole envelope, the poller the message) or
/// as someone @-mentioned (`mentions[].tenant_key`) — with the space it was last seen in:
/// `account → (tenant_key, space name, party, who created the party)`. Bots and people who
/// already have a job are left out.
fn local_tenants(c: &Connection, ws: &str) -> ChannelResult<HashMap<String, SeenTenant>> {
    let mut out: HashMap<String, SeenTenant> = HashMap::new();
    let mut st = c.prepare(
        "SELECT m.sender_account_id,
                coalesce(json_extract(e.payload, '$.raw.event.sender.tenant_key'), json_extract(e.payload, '$.raw.sender.tenant_key')),
                coalesce(json_extract(e.payload, '$.raw.event.sender.sender_type'), json_extract(e.payload, '$.raw.sender.sender_type')),
                s.name, p.id, p.created_by
           FROM chan_messages m JOIN ext_events e ON e.id = m.event_id JOIN spaces s ON s.id = m.space_id
           JOIN accounts a ON a.id = m.sender_account_id JOIN parties p ON p.id = a.party_id
          WHERE m.workspace_id = ?1 AND m.direction = 'in' AND e.payload IS NOT NULL
            AND a.kind NOT IN ('bot', 'app_bot') AND coalesce(p.contact_kind, '') <> 'bot'
            AND NOT EXISTS (SELECT 1 FROM links l WHERE l.subject_id = a.party_id
                             AND l.predicate = 'works_at' AND l.workspace_id = ?1)
          ORDER BY m.created DESC",
    )?;
    let rows = st.query_map(params![ws], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, Option<String>>(2)?,
            SeenTenant {
                tenant: String::new(),
                space: r.get(3)?,
                party: r.get(4)?,
                actor: r.get(5)?,
            },
        ))
    })?;
    for row in rows {
        let (acc, tk, kind, seen) = row?;
        if kind.as_deref() == Some("app") {
            continue;
        }
        if let Some(tk) = tk.filter(|t| !t.trim().is_empty()) {
            out.entry(acc).or_insert(SeenTenant { tenant: tk, ..seen });
        }
    }
    // the people @-mentioned: their handle, through the account that has it
    let mut st = c.prepare(
        "SELECT h.account_id, json_extract(j.value, '$.tenant_key'), s.name, p.id, p.created_by
           FROM chan_messages m JOIN ext_events e ON e.id = m.event_id JOIN spaces s ON s.id = m.space_id,
                json_each(coalesce(json_extract(e.payload, '$.raw.event.message.mentions'), json_extract(e.payload, '$.raw.mentions'), '[]')) j
           JOIN account_handles h ON h.workspace_id = m.workspace_id
            AND h.handle = coalesce(json_extract(j.value, '$.id.open_id'), json_extract(j.value, '$.id'))
           JOIN accounts a ON a.id = h.account_id JOIN parties p ON p.id = a.party_id
          WHERE m.workspace_id = ?1 AND e.payload IS NOT NULL AND json_type(j.value, '$.id') IS NOT NULL
            AND a.kind NOT IN ('bot', 'app_bot') AND coalesce(p.contact_kind, '') <> 'bot'
            AND NOT EXISTS (SELECT 1 FROM links l WHERE l.subject_id = a.party_id
                             AND l.predicate = 'works_at' AND l.workspace_id = ?1)
          ORDER BY m.created DESC",
    )?;
    let rows = st.query_map(params![ws], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, Option<String>>(1)?,
            SeenTenant {
                tenant: String::new(),
                space: r.get(2)?,
                party: r.get(3)?,
                actor: r.get(4)?,
            },
        ))
    })?;
    for row in rows {
        let (acc, tk, seen) = row?;
        if let Some(tk) = tk.filter(|t| !t.trim().is_empty()) {
            out.entry(acc).or_insert(SeenTenant { tenant: tk, ..seen });
        }
    }
    Ok(out)
}

/// What [`local_tenants`] found about one account.
struct SeenTenant {
    tenant: String,
    /// The space it was last seen in.
    space: String,
    party: String,
    /// Who created the party: the actor its organization is recorded for.
    actor: Option<String>,
}

/// Whether the account's party works somewhere in `ws`.
fn has_job(store: &steve_store::Store, ws: &str, acc: &str) -> ChannelResult<bool> {
    store.reader(|c| -> ChannelResult<bool> {
        Ok(c.query_row(
            "SELECT EXISTS (SELECT 1 FROM accounts a JOIN links l ON l.subject_id = a.party_id
               WHERE a.id = ? AND l.predicate = 'works_at' AND l.workspace_id = ?)",
            params![acc, ws],
            |r| r.get(0),
        )?)
    })
}

/// The organization keys of the workspace's own connections (`config.tenant_key`, written when you
/// connected yourself).
fn own_tenants(c: &Connection, ws: &str) -> ChannelResult<Vec<String>> {
    let mut st = c.prepare(
        "SELECT DISTINCT json_extract(config, '$.tenant_key') FROM connections
          WHERE workspace_id = ? AND json_extract(config, '$.tenant_key') IS NOT NULL",
    )?;
    let rows = st
        .query_map(params![ws], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows.into_iter().filter(|t| !t.trim().is_empty()).collect())
}

/// The spaces the candidates were seen in (member or sender), most candidates first.
fn seen_spaces(c: &Connection, ws: &str, cands: &[Candidate]) -> ChannelResult<Vec<SeenSpace>> {
    let ids: BTreeSet<&str> = cands.iter().map(|x| x.account_id.as_str()).collect();
    let mut st = c.prepare(
        "SELECT x.account_id, s.external_id, s.name, coalesce(s.connection_id,
                  (SELECT si.connection_id FROM space_identities si WHERE si.space_id = s.id
                    ORDER BY si.left_at IS NOT NULL, si.joined, si.connection_id LIMIT 1), '')
           FROM (SELECT account_id, space_id FROM space_members WHERE workspace_id = ?1
                 UNION SELECT sender_account_id, space_id FROM chan_messages
                        WHERE workspace_id = ?1 AND direction = 'in' AND sender_account_id IS NOT NULL) x
           JOIN spaces s ON s.id = x.space_id
          WHERE s.status = 'active'
          ORDER BY s.last_message DESC",
    )?;
    let mut by: BTreeMap<(String, String), SeenSpace> = BTreeMap::new();
    let mut order: Vec<(String, String)> = Vec::new();
    let rows = st.query_map(params![ws], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
        ))
    })?;
    for row in rows {
        let (acc, ext, name, conn) = row?;
        if !ids.contains(acc.as_str()) || conn.is_empty() {
            continue;
        }
        let k = (conn.clone(), ext.clone());
        let e = by.entry(k.clone()).or_insert_with(|| {
            order.push(k.clone());
            SeenSpace {
                ext_id: ext,
                name,
                connection: conn,
                accounts: Vec::new(),
            }
        });
        if !e.accounts.contains(&acc) {
            e.accounts.push(acc);
        }
    }
    let mut out: Vec<SeenSpace> = order.into_iter().filter_map(|k| by.remove(&k)).collect();
    out.sort_by_key(|s| std::cmp::Reverse(s.accounts.len()));
    Ok(out)
}

/// The identities of `ws` that have a profile adapter, live.
fn identities(c: &Connection, ws: &str, adapters: &[String]) -> ChannelResult<Vec<Identity>> {
    let mut st = c.prepare(
        "SELECT cn.id, cn.adapter, cn.config, p.user_id IS NOT NULL
           FROM connections cn JOIN accounts a ON a.id = cn.account_id LEFT JOIN parties p ON p.id = a.party_id
          WHERE cn.workspace_id = ? AND cn.status IN ('ok', 'error')
          ORDER BY cn.created, cn.id",
    )?;
    let rows = st
        .query_map(params![ws], |r| {
            let id: String = r.get(0)?;
            let config: serde_json::Value = r
                .get::<_, Option<String>>(2)?
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default();
            Ok(Identity {
                ws: ws.to_string(),
                installation: steve_modules::accounts::connections::installation_key(&id, &config),
                id,
                adapter: r.get(1)?,
                own: r.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows
        .into_iter()
        .filter(|i| adapters.contains(&i.adapter))
        .collect())
}

/// The installation key of a connection (one of `ids`, else [`crate::spaces::installation_of`]).
fn installation_of(ids: &[Identity], c: &Connection, conn: &str) -> ChannelResult<String> {
    if let Some(i) = ids.iter().find(|i| i.id == conn) {
        return Ok(i.installation.clone());
    }
    crate::spaces::installation_of(c, conn)
}

/// The identities of one installation: yours first, then the bot.
fn of_installation<'a>(ids: &'a [Identity], inst: &str) -> Vec<&'a Identity> {
    let mut v: Vec<&Identity> = ids.iter().filter(|i| i.installation == inst).collect();
    v.sort_by_key(|i| !i.own);
    v
}

/// The profile that names a candidate, if any (a named one; the directory's wins).
fn match_profile<'a>(profiles: &'a [Profile], h: &HandleRef) -> Option<&'a Profile> {
    match_at(profiles, h).map(|(_, p)| p)
}

/// [`match_profile`] over any list of profiles, with the position of the one picked.
fn match_at<'a>(
    profiles: impl IntoIterator<Item = &'a Profile>,
    h: &HandleRef,
) -> Option<(usize, &'a Profile)> {
    let hits = profiles.into_iter().enumerate().filter(|(_, p)| {
        p.handle.as_ref().is_some_and(|x| {
            x.system == h.system && (x.is(&h.handle) || h.extra.iter().any(|e| x.is(e)))
        })
    });
    let mut best: Option<(usize, &Profile)> = None;
    for (i, p) in hits {
        best = match best {
            None => Some((i, p)),
            Some((_, b)) if !b.directory && p.directory && !p.name.is_empty() => Some((i, p)),
            Some((_, b)) if b.name.is_empty() && !p.name.is_empty() => Some((i, p)),
            Some(b) => Some(b),
        };
    }
    best
}

/// The profiles found so far, without what they were found with.
fn just_profiles(profiles: &[(Profile, Option<String>, String)]) -> impl Iterator<Item = &Profile> {
    profiles.iter().map(|(p, _, _)| p)
}

impl Channels {
    /// The name jobs' state.
    pub fn names(&self) -> &Names {
        self.names_state()
    }

    /// Kicks a run of `ws` soon (see [`Names::kick`]).
    pub fn kick_names(&self, ws: &str, fresh: bool) {
        if fresh {
            let now = self.now();
            if let Err(e) = self
                .store()
                .transaction(|tx| steve_modules::contacts::profile::all_due(tx, ws, now))
            {
                tracing::warn!(ws, "names: retries not reset: {e}");
            }
        }
        self.names().kick(ws, self.now(), fresh);
    }

    /// Starts the name job (once): kicked runs after their debounce, and every workspace with
    /// profile identities every [`PERIOD_MS`]. Returns `None` when it already runs.
    pub fn spawn_names(&self) -> Option<steve_runtime::Task> {
        {
            let mut m = lock(&self.names().mem);
            if m.running {
                return None;
            }
            m.running = true;
        }
        struct Running(crate::WeakChannels);
        impl Drop for Running {
            fn drop(&mut self) {
                if let Some(ch) = self.0.upgrade() {
                    lock(&ch.names().mem).running = false;
                }
            }
        }
        let running = std::sync::Arc::new(Running(self.downgrade()));
        let me = self.downgrade();
        let ended = me.clone().closed();
        Some(steve_runtime::every_after(
            "channels.names",
            self.modules().clock.clone(),
            Duration::from_secs(5),
            steve_runtime::FirstRun::AfterPeriod,
            move || {
                let (me, running) = (me.clone(), running.clone());
                async move {
                    let _running = running;
                    let Some(ch) = me.upgrade() else { return };
                    let adapters = ch.profile_adapter_ids();
                    if adapters.is_empty() {
                        return;
                    }
                    let all = ch
                    .store()
                    .reader(|c| -> ChannelResult<Vec<String>> {
                        let mut st = c.prepare(
                            "SELECT DISTINCT workspace_id FROM connections WHERE status IN ('ok', 'error')",
                        )?;
                        let rows = st
                            .query_map([], |r| r.get::<_, String>(0))?
                            .collect::<rusqlite::Result<Vec<_>>>()?;
                        Ok(rows)
                    })
                    .unwrap_or_default();
                    for ws in ch.names().take_due(ch.now(), &all) {
                        match ch.resolve_names(&ws).await {
                            Ok(r) if r.candidates > 0 || r.orgs_linked > 0 => tracing::info!(
                                ws,
                                candidates = r.candidates,
                                named_local = r.named_local,
                                named_remote = r.named_remote,
                                promoted = r.promoted,
                                merged = r.merged,
                                orgs_linked = r.orgs_linked,
                                nameless = r.nameless,
                                "names: run"
                            ),
                            Ok(_) => {}
                            Err(e) => tracing::warn!(ws, "names: run failed: {e}"),
                        }
                    }
                }
            },
        ).until(ended))
    }

    /// One run over `ws` (module doc). Safe to call any time; the provider lookups are cached.
    pub async fn resolve_names(&self, ws: &str) -> ChannelResult<NameReport> {
        resolve::run(self, ws).await
    }

    /// Records the lookup of `cand` ([`steve_modules::contacts::profile::record_lookup`]): the
    /// fields still missing, each with the reason this run can tell.
    fn note_lookup(
        &self,
        ws: &str,
        cand: &Candidate,
        p: Option<&Profile>,
        ev: &Evidence,
        now: i64,
    ) -> ChannelResult<()> {
        use steve_modules::contacts::profile::{self as prof, Gap};
        let contact = crate::adapters::feishu::profiles::CONTACT_SCOPE;
        self.store().transaction(|tx| -> ChannelResult<()> {
            let Some(party) = tx.write(|w| {
                let row = steve_store::Accounts(w).get(&cand.account_id)?;
                row.map(|row| w.contacts().party(&row.party_id))
                    .transpose()
                    .map(Option::flatten)
            })?
            else {
                return Ok(());
            };
            let f = tx
                .write(|w| prof::contact_profile(w, ws, &party, None))?
                .fields;
            let external = p
                .and_then(|p| p.tenant_key.as_ref())
                .is_some_and(|k| !ev.own_keys.is_empty() && !ev.own_keys.contains(k));
            // no answer named the field: what the calls about them show — a failure (retry), an
            // address book searched without them, an address book refused for the permission (every
            // identity asked), or no search at all (retry)
            let has = |o: Outcome| ev.calls.iter().any(|(_, x)| *x == o);
            let unanswered = || {
                if has(Outcome::Failed) {
                    Gap::new("pending")
                } else if has(Outcome::Searched) {
                    Gap::new("not_visible")
                } else if has(Outcome::Refused) {
                    Gap::scope("permission", contact)
                } else {
                    Gap::new("pending")
                }
            };
            let mut gaps: Vec<(&str, Gap)> = Vec::new();
            for (field, st) in [("name", &f.name), ("org", &f.org)] {
                if st.state == "missing" {
                    gaps.push((
                        field,
                        if p.is_some() {
                            Gap::new("not_provided")
                        } else {
                            unanswered()
                        },
                    ));
                }
            }
            for (field, st) in [("title", &f.title), ("email", &f.email)] {
                if st.state != "missing" {
                    continue;
                }
                // a field absent from an answer proves no missing permission: it is “not provided”
                let g = match p {
                    _ if external => Gap::new("external"),
                    Some(p) if p.directory => Gap::new("not_provided"),
                    _ => unanswered(),
                };
                gaps.push((field, g));
            }
            let source = match p {
                Some(p) if p.directory => Some("directory"),
                Some(_) => Some("chat"),
                None if f.name.state == "known" => Some("local"),
                None => None,
            };
            prof::record_lookup(tx, ws, &cand.account_id, now, source, &gaps)?;
            Ok(())
        })
    }

    /// One lookup through identity `i`; missing / granted permissions are written on it. `None`
    /// when the identity failed.
    async fn ask(
        &self,
        i: &Identity,
        q: ProfileQuery,
        rep: &mut NameReport,
    ) -> Option<ProfileAnswer> {
        let a = self.profile_adapter(&i.adapter)?;
        let cx = match self.connections().cx(&i.ws, &i.id) {
            Ok(cx) => cx,
            Err(e) => {
                tracing::debug!(conn = i.id, "names: no connection context: {e}");
                return None;
            }
        };
        rep.lookups += 1;
        match a.resolve_profiles(&cx, &q).await {
            Ok(ans) => {
                self.note_profile_permissions(i, &ans, rep);
                Some(ans)
            }
            Err(e) => {
                tracing::debug!(conn = i.id, "names: lookup failed: {e}");
                rep.failed += 1;
                if !rep.failed_conns.contains(&i.id) {
                    rep.failed_conns.push(i.id.clone());
                }
                None
            }
        }
    }

    fn note_profile_permissions(&self, i: &Identity, ans: &ProfileAnswer, rep: &mut NameReport) {
        let missing: Vec<(&str, &str)> = ans
            .missing_scopes
            .iter()
            .map(|(s, p)| (s.as_str(), p.as_str()))
            .collect();
        let granted: Vec<&str> = ans.granted_scopes.iter().map(String::as_str).collect();
        for (s, _) in &missing {
            let k = (i.id.clone(), s.to_string());
            if !rep.missing.contains(&k) {
                rep.missing.push(k);
            }
        }
        if !missing.is_empty() || !granted.is_empty() {
            let (id, now) = (i.id.clone(), self.now());
            if let Err(e) = self
                .store()
                .transaction(|tx| conn_repo::note_permissions(tx, &id, &missing, &granted, now))
            {
                tracing::warn!(conn = i.id, "names: permissions not noted: {e}");
            }
        }
    }

    /// Applies one enrichment in its own transaction; a failure is logged, not fatal.
    fn apply(
        &self,
        ws: &str,
        cand: &Candidate,
        e: &Enrichment,
        rep: &mut NameReport,
    ) -> Option<accounts::Enriched> {
        let hooks = self.party_hooks();
        let r = self
            .store()
            .write(|tx| accounts::enrich(tx, hooks, ws, &cand.actor, &cand.account_id, e));
        match r {
            Ok(r) => {
                rep.renamed += r.renamed as usize;
                rep.promoted += r.promoted as usize;
                rep.merged += r.merged_into.is_some() as usize;
                rep.orgs_linked += r.org_linked as usize;
                Some(r)
            }
            Err(e) => {
                tracing::warn!(ws, account = cand.account_id, "names: not applied: {e}");
                None
            }
        }
    }

    /// Accounts of people that still have no name.
    fn count_nameless(&self, ws: &str) -> ChannelResult<usize> {
        let now = self.now();
        let lookups = self.profile_systems();
        let cands = self
            .store()
            .read(|r| Ok(candidates(r, ws, now, &lookups)))??;
        Ok(cands.iter().filter(|c| !c.named).count())
    }
}

#[cfg(test)]
mod scheduling_tests {
    use super::*;
    use steve_runtime::{Clock, FakeClock};

    fn names(clock: &Arc<FakeClock>) -> Names {
        Names::with_clock(clock.clone())
    }

    fn due(names: &Names, clock: &FakeClock, at: i64, all: &[&str]) -> Vec<String> {
        clock.set(at);
        let mut out = names.take_due(
            clock.now(),
            &all.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        );
        out.sort();
        out
    }

    #[test]
    fn periodic_scans_are_independent_per_workspace_and_kicks_reset_the_interval() {
        let clock = Arc::new(FakeClock::new(0));
        let names = names(&clock);
        assert_eq!(due(&names, &clock, 0, &["a", "b"]), ["a", "b"]);
        names.kick("a", 5_000, false);
        names.kick("a", 10_000, false); // a later kick must not postpone the first one
        for (at, expected) in [
            (20_000, vec![]),
            (25_000, vec!["a"]),
            (PERIOD_MS - 1, vec![]),
            (PERIOD_MS, vec!["b"]),
            (PERIOD_MS + 24_999, vec![]),
            (PERIOD_MS + 25_000, vec!["a"]),
        ] {
            assert_eq!(due(&names, &clock, at, &["a", "b"]), expected, "at {at}");
        }
    }

    #[test]
    fn a_periodic_scan_does_not_consume_a_future_kick() {
        let clock = Arc::new(FakeClock::new(0));
        let names = names(&clock);
        assert_eq!(due(&names, &clock, 0, &["a"]), ["a"]);
        names.kick("a", PERIOD_MS - 10_000, false);
        assert_eq!(due(&names, &clock, PERIOD_MS, &["a"]), ["a"]);
        assert!(due(&names, &clock, PERIOD_MS + 9_999, &["a"]).is_empty());
        assert_eq!(due(&names, &clock, PERIOD_MS + 10_000, &["a"]), ["a"]);
    }

    #[test]
    fn fresh_kicks_clear_lookup_memory_without_refunding_the_scan_interval() {
        let clock = Arc::new(FakeClock::new(0));
        let names = names(&clock);
        assert_eq!(due(&names, &clock, 0, &["a"]), ["a"]);
        {
            let mut m = lock(&names.mem);
            m.members.insert(("c".into(), "space".into()), 0);
            m.handles.insert(("c".into(), "handle".into()), 0);
            m.tenants.insert("c".into(), (None, 0));
            m.running = true;
        }
        names.kick("a", 10_000, true);
        {
            let m = lock(&names.mem);
            assert!(m.members.is_empty() && m.handles.is_empty() && m.tenants.is_empty());
            assert!(m.running);
        }
        assert!(due(&names, &clock, 20_000, &["a"]).is_empty());
        assert_eq!(due(&names, &clock, 30_000, &["a"]), ["a"]);
    }

    #[test]
    fn dispatch_consumes_the_interval_without_a_success_acknowledgement() {
        let clock = Arc::new(FakeClock::new(0));
        let names = names(&clock);
        // The dispatched resolver may fail: there is no success acknowledgement to refund it.
        assert_eq!(due(&names, &clock, 0, &["a"]), ["a"]);
        assert!(due(&names, &clock, PERIOD_MS - 1, &["a"]).is_empty());
        assert_eq!(due(&names, &clock, PERIOD_MS, &["a"]), ["a"]);
    }

    #[test]
    fn a_kick_after_a_wall_clock_rewind_uses_the_policy_high_water_mark() {
        let clock = Arc::new(FakeClock::new(0));
        let names = names(&clock);
        assert_eq!(due(&names, &clock, 0, &["a"]), ["a"]);
        assert_eq!(due(&names, &clock, PERIOD_MS, &["a"]), ["a"]);
        clock.set(0);
        names.kick("a", 0, false);
        assert_eq!(due(&names, &clock, DEBOUNCE_MS, &["a"]), ["a"]);
        // The old last-run map would admit at PERIOD_MS + DEBOUNCE_MS. The kick
        // hits the limiter's prior high-water mark instead of moving its clock back.
        assert!(due(&names, &clock, PERIOD_MS + DEBOUNCE_MS, &["a"]).is_empty());
        assert_eq!(due(&names, &clock, 2 * PERIOD_MS, &["a"]), ["a"]);
    }

    #[test]
    fn expired_workspace_intervals_are_pruned_even_when_no_workspace_is_live() {
        let clock = Arc::new(FakeClock::new(0));
        let names = names(&clock);
        assert_eq!(due(&names, &clock, 0, &["a"]), ["a"]);
        assert!(due(&names, &clock, PERIOD_MS, &[]).is_empty());
        // Once expired and pruned, a wall-clock rewind must not resurrect the key.
        assert_eq!(due(&names, &clock, 0, &["a"]), ["a"]);
    }
}
