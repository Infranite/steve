//! Spaces and members following the provider's own events:
//! the bot added to a group creates the space and opens the “新群” decision, removed or
//! disbanded sets `status=left|archived`, people joining or leaving update `space_members` and
//! `has_external`, a renamed group gets its new name. [`Channels::sync_spaces`] creates the spaces
//! of the groups a bot is already in when it is connected (`configured=0`, no decision).
//!
//! Adapter calls (`space_info`, `members`) are best effort, as in the inbound pipeline: a failure
//! falls back to what the event says.

use rusqlite::{OptionalExtension, params};
use steve_store::Tx;

use crate::channel::{HandleRef, MemberInfo, SpaceEvent, SpaceInfo, SpaceKind};
use crate::inbound::{ConnFacts, conn_facts, facts_system, identify_handle_as};
use crate::spaces::{self, Mode, NewSpace, SpaceRow};
use crate::{ChannelResult, Channels};

/// What [`Channels::on_space_event`] did. `None` from it: the space is not known and the event
/// does not create it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpaceChange {
    /// `spc_…`
    pub space_id: String,
    /// Created by this event.
    pub created: bool,
    /// The “新群” decision opened (or already open) for it.
    pub decision_id: Option<String>,
    /// `spaces.status` afterwards.
    pub status: String,
    pub has_external: bool,
    /// Members added (`MembersAdded`, `Joined`) or removed (`MembersRemoved`).
    pub members_changed: usize,
}

impl SpaceChange {
    fn of(s: &SpaceRow) -> Self {
        Self {
            space_id: s.id.clone(),
            created: false,
            decision_id: None,
            status: s.status.clone(),
            has_external: s.has_external,
            members_changed: 0,
        }
    }
}

fn mode_of(tx: &Tx, ws: &str, conn: &str, facts: &ConnFacts, now: i64) -> ChannelResult<Mode> {
    let mode = match spaces::channel_settings(tx, ws, conn)? {
        Some(s) => s.mode,
        None if facts.owner_user.is_some() => Mode::Own,
        None => Mode::Bot,
    };
    spaces::ensure_channel_settings(tx, ws, conn, mode, now)?;
    Ok(mode)
}

/// The space's row after an update.
fn reload(tx: &Tx, id: &str) -> ChannelResult<SpaceRow> {
    spaces::get(tx, id)?.ok_or_else(|| steve_modules::ApiError::not_found("空间"))
}

/// Name, count and external flag from a fresh `space_info` (empty names are not written).
fn apply_info(tx: &Tx, id: &str, info: &SpaceInfo, now: i64) -> ChannelResult<()> {
    tx.execute(
        "UPDATE spaces SET name = CASE WHEN ?1 <> '' THEN ?1 ELSE name END,
            member_count = CASE WHEN ?2 > 0 THEN ?2 ELSE member_count END, has_external = ?3, updated = ?4
          WHERE id = ?5",
        params![
            info.name.trim(),
            info.member_count,
            info.has_external as i64,
            now,
            id
        ],
    )?;
    Ok(())
}

/// Adds identified members; returns how many rows were new.
#[allow(clippy::too_many_arguments)]
fn add_members(
    tx: &Tx,
    ws: &str,
    conn: &str,
    facts: &ConnFacts,
    space: &SpaceRow,
    members: &[MemberInfo],
    now: i64,
) -> ChannelResult<usize> {
    let actor = facts.owner_user.as_deref().unwrap_or(&facts.created_by);
    let label = if space.name.is_empty() {
        "渠道成员".to_string()
    } else {
        space.name.clone()
    };
    let mut n = 0;
    for m in members {
        if let Some(id) = identify_handle_as(
            tx,
            ws,
            conn,
            actor,
            &m.handle,
            Some(&m.name),
            &label,
            m.role == "bot",
        ) && id.account_id != facts.account_id
            && spaces::add_member(tx, ws, &space.id, &id.account_id, &m.role, now)?
        {
            n += 1;
        }
    }
    Ok(n)
}

/// The account of a handle already known in `ws` (people leaving are not identified anew).
fn known_account(
    c: &rusqlite::Connection,
    ws: &str,
    h: &HandleRef,
) -> ChannelResult<Option<String>> {
    for x in std::iter::once(&h.handle).chain(h.extra.iter()) {
        let x = x.trim();
        if x.is_empty() {
            continue;
        }
        let a: Option<String> = c
            .query_row(
                "SELECT account_id FROM account_handles WHERE workspace_id = ? AND handle = ?",
                params![ws, x],
                |r| r.get(0),
            )
            .optional()?;
        if a.is_some() {
            return Ok(a);
        }
    }
    Ok(None)
}

impl Channels {
    /// What the adapter of `conn` says about a space and its members (best effort).
    async fn describe(
        &self,
        adapter: &str,
        ws: &str,
        conn: &str,
        ext_id: &str,
        with_members: bool,
    ) -> (Option<SpaceInfo>, Option<Vec<MemberInfo>>) {
        let Some(a) = self.adapter(adapter) else {
            return (None, None);
        };
        let cx = match self.connections().cx(ws, conn) {
            Ok(cx) => cx,
            Err(e) => {
                tracing::warn!(conn, "channel space event: no connection context: {e}");
                return (None, None);
            }
        };
        let info = match a.space_info(&cx, ext_id).await {
            Ok(i) => Some(i),
            Err(e) => {
                tracing::warn!(conn, "channel space event: space_info failed: {e}");
                None
            }
        };
        let members = if with_members && a.features().members {
            match a.members(&cx, ext_id).await {
                Ok(m) => Some(m),
                Err(e) => {
                    tracing::warn!(conn, "channel space event: members failed: {e}");
                    None
                }
            }
        } else {
            None
        };
        (info, members)
    }

    /// Applies one space event received through connection `conn` of `ws`. `Ok(None)` when the
    /// space is unknown and the event is not one that creates it (only `Joined` does).
    pub async fn on_space_event(
        &self,
        ws: &str,
        conn: &str,
        ev: SpaceEvent,
    ) -> ChannelResult<Option<SpaceChange>> {
        let ext = ev.space_ext_id().to_string();
        let mut c = steve_modules::Check::new();
        c.len("space_ext_id", &ext, 1, 500);
        c.done()?;
        let store = self.store().clone();
        let facts = store.reader(|c| conn_facts(c, ws, conn))?;
        let existing = store.reader(|c| spaces::find(c, ws, conn, &ext))?;
        if existing.is_none() && !matches!(ev, SpaceEvent::Joined { .. }) {
            tracing::debug!(
                conn,
                space = ext,
                "channel space event for an unknown space"
            );
            return Ok(None);
        }
        let now = self.now();
        match ev {
            SpaceEvent::Joined {
                name, has_external, ..
            } => {
                let (info, members) = self.describe(&facts.adapter, ws, conn, &ext, true).await;
                let info = info.unwrap_or_else(|| SpaceInfo {
                    name: name.clone().unwrap_or_default(),
                    kind: SpaceKind::Group,
                    member_count: 0,
                    has_external: has_external.unwrap_or(false),
                });
                let members = members.unwrap_or_default();
                store
                    .transaction(|tx| self.joined(tx, ws, conn, &facts, &ext, &info, &members, now))
                    .map(Some)
            }
            SpaceEvent::Left { archived, .. } => store
                .transaction(|tx| {
                    let s = existing
                        .as_ref()
                        .ok_or_else(|| steve_modules::ApiError::not_found("空间"))?;
                    let status = if archived { "archived" } else { "left" };
                    if tx.execute(
                        "UPDATE spaces SET status = ?, updated = ? WHERE id = ? AND status <> ?",
                        params![status, now, s.id, status],
                    )? > 0
                    {
                        spaces::event(tx, ws, &s.id);
                    }
                    // nothing left to set up there: an open “新群” card goes away
                    crate::space_decisions::expire_for_space(
                        tx,
                        ws,
                        &s.id,
                        if archived { "disbanded" } else { "left" },
                    )?;
                    Ok(SpaceChange::of(&reload(tx, &s.id)?))
                })
                .map(Some),
            SpaceEvent::MembersAdded {
                members,
                has_external,
                ..
            } => {
                let (info, _) = self.describe(&facts.adapter, ws, conn, &ext, false).await;
                store
                    .transaction(|tx| {
                        let s = existing
                            .as_ref()
                            .ok_or_else(|| steve_modules::ApiError::not_found("空间"))?;
                        let n = add_members(tx, ws, conn, &facts, s, &members, now)?;
                        match &info {
                            Some(i) => {
                                // a newcomer of another organization makes it external whatever
                                // the (possibly older) read says
                                let mut i = i.clone();
                                i.has_external |= members.iter().any(|m| m.external)
                                    || has_external == Some(true);
                                apply_info(tx, &s.id, &i, now)?
                            }
                            None => {
                                // no fresh count: the provider's flag, else any external newcomer
                                let ext_now = has_external.unwrap_or(s.has_external)
                                    || members.iter().any(|m| m.external);
                                tx.execute(
                                    "UPDATE spaces SET member_count = member_count + ?, has_external = ?, updated = ? WHERE id = ?",
                                    params![n as i64, ext_now as i64, now, s.id],
                                )?;
                            }
                        }
                        spaces::event(tx, ws, &s.id);
                        let mut out = SpaceChange::of(&reload(tx, &s.id)?);
                        out.members_changed = n;
                        Ok(out)
                    })
                    .map(Some)
            }
            SpaceEvent::MembersRemoved {
                members,
                has_external,
                ..
            } => {
                let (info, _) = self.describe(&facts.adapter, ws, conn, &ext, false).await;
                store
                    .transaction(|tx| {
                        let s = existing
                            .as_ref()
                            .ok_or_else(|| steve_modules::ApiError::not_found("空间"))?;
                        let mut n = 0;
                        for h in &members {
                            if let Some(a) = known_account(tx, ws, h)?
                                && a != facts.account_id
                            {
                                n += tx.execute(
                                    "DELETE FROM space_members WHERE space_id = ? AND account_id = ?",
                                    params![s.id, a],
                                )?;
                            }
                        }
                        match &info {
                            Some(i) => apply_info(tx, &s.id, i, now)?,
                            None => {
                                tx.execute(
                                    "UPDATE spaces SET member_count = max(member_count - ?, 0), has_external = ?, updated = ? WHERE id = ?",
                                    params![
                                        n as i64,
                                        has_external.unwrap_or(s.has_external) as i64,
                                        now,
                                        s.id
                                    ],
                                )?;
                            }
                        }
                        spaces::event(tx, ws, &s.id);
                        let mut out = SpaceChange::of(&reload(tx, &s.id)?);
                        out.members_changed = n;
                        Ok(out)
                    })
                    .map(Some)
            }
            SpaceEvent::Updated {
                name, has_external, ..
            } => {
                let (info, _) = self.describe(&facts.adapter, ws, conn, &ext, false).await;
                let mut info = info.unwrap_or_else(|| {
                    let s = existing.as_ref();
                    SpaceInfo {
                        name: String::new(),
                        kind: SpaceKind::Group,
                        member_count: 0,
                        has_external: has_external
                            .unwrap_or_else(|| s.is_some_and(|s| s.has_external)),
                    }
                });
                // the event's new name wins over a read that may predate the change
                if let Some(n) = name.filter(|n| !n.trim().is_empty()) {
                    info.name = n;
                }
                store
                    .transaction(|tx| {
                        let s = existing
                            .as_ref()
                            .ok_or_else(|| steve_modules::ApiError::not_found("空间"))?;
                        apply_info(tx, &s.id, &info, now)?;
                        spaces::event(tx, ws, &s.id);
                        Ok(SpaceChange::of(&reload(tx, &s.id)?))
                    })
                    .map(Some)
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn joined(
        &self,
        tx: &Tx,
        ws: &str,
        conn: &str,
        facts: &ConnFacts,
        ext: &str,
        info: &SpaceInfo,
        members: &[MemberInfo],
        now: i64,
    ) -> ChannelResult<SpaceChange> {
        let mode = mode_of(tx, ws, conn, facts, now)?;
        let system = facts_system(tx, &facts.account_id)?;
        let member_count = if info.member_count == 0 {
            members.len() as i64
        } else {
            info.member_count
        };
        let has_external = info.has_external || members.iter().any(|m| m.external);
        let (space, created) = spaces::locate_or_create(
            tx,
            &NewSpace {
                ws,
                system_id: &system,
                connection_id: conn,
                kind: info.kind,
                external_id: ext,
                name: &info.name,
                member_count,
                has_external,
                now,
            },
        )?;
        let mut rejoined = false;
        if !created {
            // added back after leaving (or a space first seen through a message): refresh it
            rejoined = space.status != "active";
            tx.execute(
                "UPDATE spaces SET status = 'active', updated = ? WHERE id = ?",
                params![now, space.id],
            )?;
            apply_info(
                tx,
                &space.id,
                &SpaceInfo {
                    name: info.name.clone(),
                    kind: info.kind,
                    member_count,
                    has_external,
                },
                now,
            )?;
        }
        spaces::add_member(tx, ws, &space.id, &facts.account_id, "bot", now)?;
        let n = add_members(tx, ws, conn, facts, &space, members, now)?;
        let space = reload(tx, &space.id)?;
        let mut decision_id = None;
        let kind = SpaceKind::parse(&space.kind).unwrap_or(info.kind);
        if mode == Mode::Bot
            && kind.is_group()
            && (created || (rejoined && !spaces::settings(tx, &space.id)?.configured))
        {
            decision_id = Some(spaces::open_new_space_decision(
                tx,
                ws,
                &space,
                &facts.account_name,
            )?);
        }
        spaces::event(tx, ws, &space.id);
        Ok(SpaceChange {
            created,
            decision_id,
            members_changed: n,
            ..SpaceChange::of(&space)
        })
    }

    /// Creates the spaces of the groups connection `conn` is already in:
    /// `configured=0`, no “新群” decision each (the spaces page points them out).
    /// Known spaces are left as they are. Returns how many were created.
    pub async fn sync_spaces(
        &self,
        ws: &str,
        conn: &str,
        chats: &[(String, SpaceInfo)],
    ) -> ChannelResult<usize> {
        let store = self.store().clone();
        let facts = store.reader(|c| conn_facts(c, ws, conn))?;
        let now = self.now();
        store.transaction(|tx| {
            mode_of(tx, ws, conn, &facts, now)?;
            let system = facts_system(tx, &facts.account_id)?;
            let mut n = 0;
            for (ext, info) in chats {
                if ext.trim().is_empty() {
                    continue;
                }
                let (s, created) = spaces::locate_or_create(
                    tx,
                    &NewSpace {
                        ws,
                        system_id: &system,
                        connection_id: conn,
                        kind: info.kind,
                        external_id: ext,
                        name: &info.name,
                        member_count: info.member_count,
                        has_external: info.has_external,
                        now,
                    },
                )?;
                if created {
                    spaces::add_member(tx, ws, &s.id, &facts.account_id, "bot", now)?;
                    n += 1;
                }
            }
            Ok(n)
        })
    }
}
