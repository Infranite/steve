//! Names for the people the channels meet (domain-model.md §2, “识别与补全”): an account identified
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

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Mutex;
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use steve_modules::accounts::connections::repo as conn_repo;
use steve_modules::accounts::{self, Enrichment, ExtraHandle, TenantHint, looks_like_handle};

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
    /// `ws` → last run.
    last: HashMap<String, i64>,
    running: bool,
}

/// The name jobs' memory; part of [`Channels`].
#[derive(Default)]
pub struct Names {
    mem: Mutex<Memory>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Names {
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
            if !out.contains(w) && m.last.get(w).is_none_or(|t| now - t >= PERIOD_MS) {
                out.push(w.clone());
            }
        }
        for w in &out {
            m.last.insert(w.clone(), now);
        }
        out
    }
}

/// `SELECT … WHERE` the account or person still needs something.
fn candidates(c: &Connection, ws: &str) -> ChannelResult<Vec<Candidate>> {
    let mut st = c.prepare(
        "SELECT a.id, a.party_id, p.created_by, a.system_id, a.handle, a.display_name, p.name, p.contact_kind
           FROM accounts a JOIN parties p ON p.id = a.party_id
          WHERE a.workspace_id = ? AND a.status = 'active' AND p.type = 'contact' AND p.status = 'active'
            AND p.user_id IS NULL AND coalesce(p.contact_kind, '') <> 'bot'
            AND a.kind NOT IN ('bot', 'app_bot')
          ORDER BY a.created, a.id",
    )?;
    let rows = st
        .query_map(params![ws], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, Option<String>>(7)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut out = Vec::new();
    for (acc, _party, actor, system, handle, display, pname, kind) in rows {
        let raw = |n: &str| looks_like_handle(n) || n.trim() == handle;
        let account_named = !raw(&display);
        let party_named = !raw(&pname);
        let unconfirmed = kind.as_deref() == Some("unconfirmed");
        if account_named && party_named && !unconfirmed {
            continue;
        }
        let mut h = HandleRef::new(system, handle);
        for (x, _) in accounts::repo::handles_of(c, &acc)? {
            h = h.with_extra(x);
        }
        out.push(Candidate {
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
/// `account → (tenant_key, space name)`. Bots are left out.
fn local_tenants(c: &Connection, ws: &str) -> ChannelResult<HashMap<String, (String, String)>> {
    let mut out: HashMap<String, (String, String)> = HashMap::new();
    let mut st = c.prepare(
        "SELECT m.sender_account_id,
                coalesce(json_extract(e.payload, '$.raw.event.sender.tenant_key'), json_extract(e.payload, '$.raw.sender.tenant_key')),
                coalesce(json_extract(e.payload, '$.raw.event.sender.sender_type'), json_extract(e.payload, '$.raw.sender.sender_type')),
                s.name
           FROM chan_messages m JOIN ext_events e ON e.id = m.event_id JOIN spaces s ON s.id = m.space_id
           JOIN accounts a ON a.id = m.sender_account_id JOIN parties p ON p.id = a.party_id
          WHERE m.workspace_id = ? AND m.direction = 'in' AND e.payload IS NOT NULL
            AND a.kind NOT IN ('bot', 'app_bot') AND coalesce(p.contact_kind, '') <> 'bot'
          ORDER BY m.created DESC",
    )?;
    let rows = st.query_map(params![ws], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, Option<String>>(2)?,
            r.get::<_, String>(3)?,
        ))
    })?;
    for row in rows {
        let (acc, tk, kind, space) = row?;
        if kind.as_deref() == Some("app") {
            continue;
        }
        if let Some(tk) = tk.filter(|t| !t.trim().is_empty()) {
            out.entry(acc).or_insert((tk, space));
        }
    }
    // the people @-mentioned: their handle, through the account that has it
    let mut st = c.prepare(
        "SELECT h.account_id, json_extract(j.value, '$.tenant_key'), s.name
           FROM chan_messages m JOIN ext_events e ON e.id = m.event_id JOIN spaces s ON s.id = m.space_id,
                json_each(coalesce(json_extract(e.payload, '$.raw.event.message.mentions'), json_extract(e.payload, '$.raw.mentions'), '[]')) j
           JOIN account_handles h ON h.workspace_id = m.workspace_id
            AND h.handle = coalesce(json_extract(j.value, '$.id.open_id'), json_extract(j.value, '$.id'))
           JOIN accounts a ON a.id = h.account_id JOIN parties p ON p.id = a.party_id
          WHERE m.workspace_id = ? AND e.payload IS NOT NULL AND json_type(j.value, '$.id') IS NOT NULL
            AND a.kind NOT IN ('bot', 'app_bot') AND coalesce(p.contact_kind, '') <> 'bot'
          ORDER BY m.created DESC",
    )?;
    let rows = st.query_map(params![ws], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    for row in rows {
        let (acc, tk, space) = row?;
        if let Some(tk) = tk.filter(|t| !t.trim().is_empty()) {
            out.entry(acc).or_insert((tk, space));
        }
    }
    Ok(out)
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
    let hits = profiles.iter().filter(|p| {
        p.handle.as_ref().is_some_and(|x| {
            x.system == h.system && (x.is(&h.handle) || h.extra.iter().any(|e| x.is(e)))
        })
    });
    let mut best: Option<&Profile> = None;
    for p in hits {
        best = match best {
            None => Some(p),
            Some(b) if !b.directory && p.directory && !p.name.is_empty() => Some(p),
            Some(b) if b.name.is_empty() && !p.name.is_empty() => Some(p),
            Some(b) => Some(b),
        };
    }
    best
}

impl Channels {
    /// The name jobs' state.
    pub fn names(&self) -> &Names {
        self.names_state()
    }

    /// Kicks a run of `ws` soon (see [`Names::kick`]).
    pub fn kick_names(&self, ws: &str, fresh: bool) {
        self.names().kick(ws, self.now(), fresh);
    }

    /// Starts the name job (once): kicked runs after their debounce, and every workspace with
    /// profile identities every [`PERIOD_MS`]. Returns `None` when it already runs.
    pub fn spawn_names(&self) -> Option<tokio::task::JoinHandle<()>> {
        {
            let mut m = lock(&self.names().mem);
            if m.running {
                return None;
            }
            m.running = true;
        }
        let me = self.downgrade();
        Some(tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(5)).await;
                let Some(ch) = me.upgrade() else { return };
                let adapters = ch.profile_adapter_ids();
                if adapters.is_empty() {
                    continue;
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
        }))
    }

    /// One run over `ws` (module doc). Safe to call any time; the provider lookups are cached.
    pub async fn resolve_names(&self, ws: &str) -> ChannelResult<NameReport> {
        let mut rep = NameReport::default();
        let store = self.store().clone();
        let cands = store.reader(|c| candidates(c, ws))?;
        rep.candidates = cands.len();

        // 2. what Steve already has
        let (mentions, dms) = store
            .reader(|c| -> ChannelResult<_> { Ok((mention_names(c, ws)?, dm_names(c, ws)?)) })?;
        let mut open: Vec<Candidate> = Vec::new();
        for cand in cands {
            let local = mentions
                .get(&cand.account_id)
                .or_else(|| dms.get(&cand.account_id));
            match local {
                Some(n) if !cand.named => {
                    let e = Enrichment {
                        name: Some(n.clone()),
                        ..Enrichment::default()
                    };
                    if let Some(r) = self.apply(ws, &cand, &e, &mut rep)
                        && (r.account_named || r.renamed)
                    {
                        rep.named_local += 1;
                    }
                    // still ask the provider for the organization of an unconfirmed person
                    open.push(Candidate {
                        named: true,
                        ..cand
                    });
                }
                _ => open.push(cand),
            }
        }

        // the identities of each installation, and their organizations
        let adapters = self.profile_adapter_ids();
        let ids = if adapters.is_empty() {
            Vec::new()
        } else {
            store.reader(|c| identities(c, ws, &adapters))?
        };
        let mut tenants: HashMap<String, Vec<TenantInfo>> = HashMap::new(); // installation → own tenants
        let now = self.now();
        // the organizations of the identities
        for i in &ids {
            let known = lock(&self.names().mem)
                .tenants
                .get(&i.id)
                .filter(|(_, at)| now - at < MEMBERS_TTL_MS)
                .map(|(t, _)| t.clone());
            let t = match known {
                Some(t) => t,
                None => {
                    let ans = self
                        .ask(
                            i,
                            ProfileQuery {
                                space_ext_id: None,
                                handles: Vec::new(),
                                want_tenant: true,
                            },
                            &mut rep,
                        )
                        .await;
                    let t = ans.and_then(|a| a.tenant);
                    lock(&self.names().mem)
                        .tenants
                        .insert(i.id.clone(), (t.clone(), now));
                    t
                }
            };
            if let Some(t) = t {
                tenants.entry(i.installation.clone()).or_default().push(t);
            }
        }

        // 2c. organizations from the stored payloads (anyone without a job yet, named or not)
        let (tenants_seen, mut own_keys) = store
            .reader(|c| -> ChannelResult<_> { Ok((local_tenants(c, ws)?, own_tenants(c, ws)?)) })?;
        let own_names: HashMap<String, String> = tenants
            .values()
            .flatten()
            .filter_map(|t| t.name.clone().map(|n| (t.key.clone(), n)))
            .collect();
        for t in tenants.values().flatten() {
            if !own_keys.contains(&t.key) {
                own_keys.push(t.key.clone());
            }
        }
        if !own_keys.is_empty() {
            for (acc, (tk, space)) in &tenants_seen {
                let hint = TenantHint {
                    key: tk.clone(),
                    own: own_keys.contains(tk),
                    name: own_names.get(tk).cloned(),
                    seen_in: Some(space.clone()),
                };
                let actor = store.reader(|c| -> ChannelResult<Option<String>> {
                    Ok(c.query_row(
                        "SELECT p.created_by FROM accounts a JOIN parties p ON p.id = a.party_id WHERE a.id = ?",
                        params![acc],
                        |r| r.get(0),
                    )
                    .optional()?)
                })?;
                let Some(actor) = actor else { continue };
                let has_job = store.reader(|c| -> ChannelResult<bool> {
                    Ok(c.query_row(
                        "SELECT EXISTS (SELECT 1 FROM accounts a JOIN links l ON l.subject_id = a.party_id
                           WHERE a.id = ? AND l.predicate = 'works_at' AND l.workspace_id = ?)",
                        params![acc, ws],
                        |r| r.get(0),
                    )?)
                })?;
                if has_job {
                    continue;
                }
                let ws2 = ws.to_string();
                let org =
                    match store.transaction(|tx| accounts::tenant_org(tx, &ws2, &actor, &hint)) {
                        Ok(o) => o,
                        Err(e) => {
                            tracing::warn!(ws, "names: organization not recorded: {e}");
                            continue;
                        }
                    };
                let cand = Candidate {
                    account_id: acc.clone(),
                    actor,
                    handle: HandleRef::new("", ""),
                    named: true,
                };
                let e = Enrichment {
                    org_id: Some(org),
                    ..Enrichment::default()
                };
                self.apply(ws, &cand, &e, &mut rep);
            }
        }

        // 3. lookups through the identities
        if ids.is_empty() || open.is_empty() {
            rep.nameless = self.count_nameless(ws)?;
            return Ok(rep);
        }
        let spaces = store.reader(|c| seen_spaces(c, ws, &open))?;
        let mut profiles: Vec<(Profile, Option<String>, String)> = Vec::new(); // (profile, seen in, installation)

        // members of the spaces the candidates were seen in
        let named_by =
            |profiles: &[(Profile, Option<String>, String)], acc: &str, cands: &[Candidate]| {
                cands.iter().find(|c| c.account_id == acc).is_some_and(|c| {
                    let ps: Vec<Profile> = profiles.iter().map(|(p, _, _)| p.clone()).collect();
                    match_profile(&ps, &c.handle).is_some_and(|p| !p.name.is_empty())
                })
            };
        for sp in spaces.iter().take(MAX_SPACES) {
            if sp.accounts.iter().all(|a| named_by(&profiles, a, &open)) {
                continue;
            }
            let inst = store.reader(|c| installation_of(&ids, c, &sp.connection))?;
            let handles: Vec<HandleRef> = open
                .iter()
                .filter(|c| sp.accounts.contains(&c.account_id))
                .map(|c| c.handle.clone())
                .collect();
            for i in of_installation(&ids, &inst) {
                let key = (i.id.clone(), sp.ext_id.clone());
                let fresh = lock(&self.names().mem)
                    .members
                    .get(&key)
                    .is_some_and(|at| now - at < MEMBERS_TTL_MS);
                if fresh {
                    continue;
                }
                let Some(ans) = self
                    .ask(
                        i,
                        ProfileQuery {
                            space_ext_id: Some(sp.ext_id.clone()),
                            handles: handles.clone(),
                            want_tenant: false,
                        },
                        &mut rep,
                    )
                    .await
                else {
                    continue;
                };
                lock(&self.names().mem).members.insert(key, now);
                let got = !ans.profiles.is_empty();
                for p in ans.profiles {
                    profiles.push((p, Some(sp.name.clone()), inst.clone()));
                }
                if got && sp.accounts.iter().all(|a| named_by(&profiles, a, &open)) {
                    break;
                }
            }
        }

        // the directory for whoever is still unnamed, per installation
        let mut by_inst: BTreeMap<String, Vec<HandleRef>> = BTreeMap::new();
        for cand in &open {
            let ps: Vec<Profile> = profiles.iter().map(|(p, _, _)| p.clone()).collect();
            if match_profile(&ps, &cand.handle).is_some_and(|p| !p.name.is_empty()) {
                continue;
            }
            // the installation of where they were seen (or every one)
            let insts: BTreeSet<String> = {
                let mut s = BTreeSet::new();
                for sp in spaces
                    .iter()
                    .filter(|s| s.accounts.contains(&cand.account_id))
                {
                    s.insert(store.reader(|c| installation_of(&ids, c, &sp.connection))?);
                }
                if s.is_empty() {
                    s.extend(ids.iter().map(|i| i.installation.clone()));
                }
                s
            };
            for inst in insts {
                by_inst.entry(inst).or_default().push(cand.handle.clone());
            }
        }
        for (inst, handles) in by_inst {
            let mut left = handles;
            for i in of_installation(&ids, &inst) {
                left.retain(|h| {
                    let m = lock(&self.names().mem);
                    m.handles
                        .get(&(i.id.clone(), h.handle.clone()))
                        .is_none_or(|at| now - at >= HANDLE_TTL_MS)
                });
                if left.is_empty() {
                    break;
                }
                let Some(ans) = self
                    .ask(
                        i,
                        ProfileQuery {
                            space_ext_id: None,
                            handles: left.clone(),
                            want_tenant: false,
                        },
                        &mut rep,
                    )
                    .await
                else {
                    continue;
                };
                {
                    let mut m = lock(&self.names().mem);
                    for h in &left {
                        m.handles.insert((i.id.clone(), h.handle.clone()), now);
                    }
                }
                let found: Vec<Profile> = ans.profiles;
                left.retain(|h| match_profile(&found, h).is_none_or(|p| p.name.is_empty()));
                for p in found {
                    profiles.push((p, None, inst.clone()));
                }
            }
        }

        // 4. apply
        let ps: Vec<Profile> = profiles.iter().map(|(p, _, _)| p.clone()).collect();
        for cand in &open {
            let Some(p) = match_profile(&ps, &cand.handle) else {
                continue;
            };
            let (seen_in, inst) = profiles
                .iter()
                .find(|(x, _, _)| std::ptr::eq(x as *const Profile, p as *const Profile) || x == p)
                .map(|(_, s, i)| (s.clone(), i.clone()))
                .unwrap_or((None, String::new()));
            let own_keys: Vec<TenantInfo> = tenants.get(&inst).cloned().unwrap_or_default();
            // a directory profile is of the identity's own organization
            let tenant_key = p.tenant_key.clone().or_else(|| {
                p.directory
                    .then(|| own_keys.first().map(|t| t.key.clone()))
                    .flatten()
            });
            let org_id = match tenant_key {
                Some(k) => {
                    let own = own_keys
                        .iter()
                        .find(|t| t.key == k && t.name.is_some())
                        .or_else(|| own_keys.iter().find(|t| t.key == k));
                    let hint = TenantHint {
                        key: k.clone(),
                        own: own.is_some(),
                        name: own.and_then(|t| t.name.clone()),
                        seen_in: seen_in.clone(),
                    };
                    let actor = cand.actor.clone();
                    let ws2 = ws.to_string();
                    match store.transaction(|tx| accounts::tenant_org(tx, &ws2, &actor, &hint)) {
                        Ok(o) => Some(o),
                        Err(e) => {
                            tracing::warn!(ws, "names: organization not recorded: {e}");
                            None
                        }
                    }
                }
                None => None,
            };
            let mut handles: Vec<ExtraHandle> = Vec::new();
            if let Some(h) = &p.handle {
                for x in std::iter::once(&h.handle).chain(h.extra.iter()) {
                    // `union_id`s hold everywhere; `open_id`s only under one app
                    let conn = x
                        .starts_with("ou_")
                        .then(|| inst.clone())
                        .filter(|c| !c.is_empty());
                    handles.push(ExtraHandle {
                        handle: x.clone(),
                        connection_id: conn,
                    });
                }
            }
            let e = Enrichment {
                name: Some(p.name.clone()).filter(|n| !n.trim().is_empty()),
                handles,
                email: p.email.clone(),
                avatar_url: p.avatar_url.clone(),
                org_id,
                dept: None,
                directory: p.directory,
            };
            if let Some(r) = self.apply(ws, cand, &e, &mut rep)
                && (r.account_named || r.renamed)
            {
                rep.named_remote += 1;
            }
        }
        rep.nameless = self.count_nameless(ws)?;
        Ok(rep)
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
                    if let Err(e) = self.store().transaction(|tx| {
                        conn_repo::note_permissions(tx, &id, &missing, &granted, now)
                    }) {
                        tracing::warn!(conn = i.id, "names: permissions not noted: {e}");
                    }
                }
                Some(ans)
            }
            Err(e) => {
                tracing::debug!(conn = i.id, "names: lookup failed: {e}");
                None
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
            .transaction(|tx| accounts::enrich(tx, hooks, ws, &cand.actor, &cand.account_id, e));
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
        let cands = self.store().reader(|c| candidates(c, ws))?;
        Ok(cands.iter().filter(|c| !c.named).count())
    }
}
