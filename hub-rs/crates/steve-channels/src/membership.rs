//! Spaces and members following the provider's own events:
//! the bot added to a group creates the space and opens the “新群” decision, removed or
//! disbanded sets `status=left|archived`, people joining or leaving update `space_members` and
//! `has_external`, a renamed group gets its new name. [`Channels::sync_spaces`] creates the spaces
//! of the groups a bot is already in when it is connected (`configured=0`, no decision).
//!
//! Adapter calls (`space_info`, `members`) are best effort, as in the inbound pipeline: a failure
//! falls back to what the event says.

use steve_store::channel_spaces::{members as member_repo, registration};
use steve_store::{Read, W};

use crate::channel::{HandleRef, MemberInfo, SpaceEvent, SpaceInfo, SpaceKind};
use crate::inbound::{ConnFacts, facts_system, identify_with_aliases};
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

fn mode_of(
    w: &mut W<'_>,
    ws: &str,
    conn: &str,
    facts: &ConnFacts,
    now: i64,
) -> ChannelResult<Mode> {
    let mode = match spaces::channel_settings(w, ws, conn)? {
        Some(s) => s.mode,
        None if facts.owner_user.is_some() => Mode::Own,
        None => Mode::Bot,
    };
    spaces::ensure_channel_settings(w, ws, conn, mode, now)?;
    Ok(mode)
}

/// The space's row after an update.
fn reload(w: &mut W<'_>, id: &str) -> ChannelResult<SpaceRow> {
    steve_store::channel_spaces::rows::get(w, id)?
        .ok_or_else(|| steve_modules::ApiError::not_found("空间"))
}

/// Name, count and external flag from a fresh `space_info` (empty names are not written).
fn apply_info(w: &mut W<'_>, id: &str, info: &SpaceInfo, now: i64) -> ChannelResult<()> {
    registration::describe(
        w,
        id,
        info.name.trim(),
        info.member_count,
        info.has_external,
        now,
    )?;
    Ok(())
}

/// Adds identified members; returns how many rows were new.
#[allow(clippy::too_many_arguments)]
fn add_members(
    w: &mut W<'_>,
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
        if let Some(id) = identify_with_aliases(
            w,
            ws,
            conn,
            actor,
            &m.handle,
            &m.aliases,
            Some(&m.name),
            &label,
            m.role == "bot",
        ) && id.account_id != facts.account_id
            && member_repo::add(w, ws, &space.id, &id.account_id, &m.role, now)?
        {
            n += 1;
        }
    }
    Ok(n)
}

/// The account of a handle already known in `ws` (people leaving are not identified anew).
fn known_account(r: &mut impl Read, ws: &str, h: &HandleRef) -> ChannelResult<Option<String>> {
    for x in std::iter::once(&h.handle).chain(h.extra.iter()) {
        let x = x.trim();
        if x.is_empty() {
            continue;
        }
        if let Some(account) = steve_store::accounts::by_any_handle(r, ws, x)? {
            return Ok(Some(account));
        }
    }
    Ok(None)
}

struct SpaceEventCx<'a> {
    store: &'a steve_store::Store,
    ws: &'a str,
    conn: &'a str,
    facts: &'a ConnFacts,
    ext: &'a str,
    existing: Option<&'a SpaceRow>,
    now: i64,
}

async fn described_members(
    adapter: &dyn crate::channel::ChannelAdapter,
    cx: &steve_modules::accounts::connections::ConnCx,
    conn: &str,
    ext_id: &str,
) -> Option<Vec<MemberInfo>> {
    match adapter.members(cx, ext_id).await {
        Ok(m) => Some(m),
        Err(e) => {
            tracing::warn!(conn, "channel space event: members failed: {e}");
            None
        }
    }
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
            described_members(a.as_ref(), &cx, conn, ext_id).await
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
        let facts = self.facts(ws, conn)?;
        let existing =
            store.read(|r| spaces::find_as(r, ws, conn, &ext, facts.platform.as_deref()))?;
        if existing.is_none() && !matches!(ev, SpaceEvent::Joined { .. }) {
            tracing::debug!(
                conn,
                space = ext,
                "channel space event for an unknown space"
            );
            return Ok(None);
        }
        let now = self.now();
        let cx = SpaceEventCx {
            store: &store,
            ws,
            conn,
            facts: &facts,
            ext: &ext,
            existing: existing.as_ref(),
            now,
        };
        match ev {
            SpaceEvent::Joined {
                name, has_external, ..
            } => self.space_joined(&cx, name, has_external).await,
            SpaceEvent::Left { archived, .. } => self.space_left(&cx, archived),
            SpaceEvent::MembersAdded {
                members,
                has_external,
                ..
            } => self.space_members_added(&cx, members, has_external).await,
            SpaceEvent::MembersRemoved {
                members,
                has_external,
                ..
            } => self.space_members_removed(&cx, members, has_external).await,
            SpaceEvent::Updated {
                name, has_external, ..
            } => self.space_updated(&cx, name, has_external).await,
        }
    }

    async fn space_joined(
        &self,
        cx: &SpaceEventCx<'_>,
        name: Option<String>,
        has_external: Option<bool>,
    ) -> ChannelResult<Option<SpaceChange>> {
        let SpaceEventCx {
            store,
            ws,
            conn,
            facts,
            ext,
            now,
            ..
        } = *cx;

        let (info, members) = self.describe(&facts.adapter, ws, conn, ext, true).await;
        let info = info.unwrap_or_else(|| SpaceInfo {
            name: name.clone().unwrap_or_default(),
            kind: SpaceKind::Group,
            member_count: 0,
            has_external: has_external.unwrap_or(false),
        });
        let members = members.unwrap_or_default();
        store
            .write(|w| self.joined(w, ws, conn, facts, ext, &info, &members, now))
            .map(Some)
    }

    fn space_left(
        &self,
        cx: &SpaceEventCx<'_>,
        archived: bool,
    ) -> ChannelResult<Option<SpaceChange>> {
        let SpaceEventCx {
            store,
            ws,
            existing,
            now,
            ..
        } = *cx;
        store
            .write(|w| {
                let s = existing.ok_or_else(|| steve_modules::ApiError::not_found("空间"))?;
                let status = if archived {
                    registration::State::Archived
                } else {
                    registration::State::Left
                };
                if registration::set_state(w, &s.id, status, now)? {
                    w.emit(steve_types::ChannelsEvent::Space {
                        id: s.id.clone().into(),
                        ws: ws.into(),
                    });
                }
                // nothing left to set up there: an open “新群” card goes away
                crate::space_decisions::expire_for_space(
                    w,
                    ws,
                    &s.id,
                    if archived { "disbanded" } else { "left" },
                )?;
                Ok(SpaceChange::of(
                    &steve_store::channel_spaces::rows::get(w, &s.id)?
                        .ok_or_else(|| steve_modules::ApiError::not_found("空间"))?,
                ))
            })
            .map(Some)
    }

    async fn space_members_added(
        &self,
        cx: &SpaceEventCx<'_>,
        members: Vec<MemberInfo>,
        has_external: Option<bool>,
    ) -> ChannelResult<Option<SpaceChange>> {
        let SpaceEventCx {
            store,
            ws,
            conn,
            facts,
            ext,
            existing,
            now,
        } = *cx;

        let (info, _) = self.describe(&facts.adapter, ws, conn, ext, false).await;
        store
            .write(|w| {
                let s = existing
                    .as_ref()
                    .ok_or_else(|| steve_modules::ApiError::not_found("空间"))?;
                let n = add_members(w, ws, conn, facts, s, &members, now)?;
                match &info {
                    Some(i) => {
                        // a newcomer of another organization makes it external whatever
                        // the (possibly older) read says
                        let mut i = i.clone();
                        i.has_external |=
                            members.iter().any(|m| m.external) || has_external == Some(true);
                        apply_info(w, &s.id, &i, now)?
                    }
                    None => {
                        // no fresh count: the provider's flag, else any external newcomer
                        let ext_now = has_external.unwrap_or(s.has_external)
                            || members.iter().any(|m| m.external);
                        registration::count_members(
                            w,
                            &s.id,
                            registration::MembershipChange::Add(n as i64),
                            ext_now,
                            now,
                        )?;
                    }
                }
                registration::emit(w, ws, &s.id);
                let mut out = SpaceChange::of(&reload(w, &s.id)?);
                out.members_changed = n;
                Ok(out)
            })
            .map(Some)
    }

    async fn space_members_removed(
        &self,
        cx: &SpaceEventCx<'_>,
        members: Vec<HandleRef>,
        has_external: Option<bool>,
    ) -> ChannelResult<Option<SpaceChange>> {
        let SpaceEventCx {
            store,
            ws,
            conn,
            facts,
            ext,
            existing,
            now,
        } = *cx;

        let (info, _) = self.describe(&facts.adapter, ws, conn, ext, false).await;
        store
            .write(|w| {
                let s = existing
                    .as_ref()
                    .ok_or_else(|| steve_modules::ApiError::not_found("空间"))?;
                let mut n = 0;
                for h in &members {
                    if let Some(account) = known_account(w, ws, h)?
                        && account != facts.account_id
                    {
                        n += usize::from(member_repo::remove(w, &s.id, &account)?);
                    }
                }
                match &info {
                    Some(i) => apply_info(w, &s.id, i, now)?,
                    None => {
                        registration::count_members(
                            w,
                            &s.id,
                            registration::MembershipChange::Remove(n as i64),
                            has_external.unwrap_or(s.has_external),
                            now,
                        )?;
                    }
                }
                registration::emit(w, ws, &s.id);
                let mut out = SpaceChange::of(&reload(w, &s.id)?);
                out.members_changed = n;
                Ok(out)
            })
            .map(Some)
    }

    async fn space_updated(
        &self,
        cx: &SpaceEventCx<'_>,
        name: Option<String>,
        has_external: Option<bool>,
    ) -> ChannelResult<Option<SpaceChange>> {
        let SpaceEventCx {
            store,
            ws,
            conn,
            facts,
            ext,
            existing,
            now,
        } = *cx;

        let (info, _) = self.describe(&facts.adapter, ws, conn, ext, false).await;
        let mut info = info.unwrap_or_else(|| {
            let s = existing;
            SpaceInfo {
                name: String::new(),
                kind: SpaceKind::Group,
                member_count: 0,
                has_external: has_external.unwrap_or_else(|| s.is_some_and(|s| s.has_external)),
            }
        });
        // the event's new name wins over a read that may predate the change
        if let Some(n) = name.filter(|n| !n.trim().is_empty()) {
            info.name = n;
        }
        store
            .write(|w| {
                let s = existing
                    .as_ref()
                    .ok_or_else(|| steve_modules::ApiError::not_found("空间"))?;
                apply_info(w, &s.id, &info, now)?;
                registration::emit(w, ws, &s.id);
                Ok(SpaceChange::of(&reload(w, &s.id)?))
            })
            .map(Some)
    }

    #[allow(clippy::too_many_arguments)]
    fn joined(
        &self,
        w: &mut W<'_>,
        ws: &str,
        conn: &str,
        facts: &ConnFacts,
        ext: &str,
        info: &SpaceInfo,
        members: &[MemberInfo],
        now: i64,
    ) -> ChannelResult<SpaceChange> {
        let mode = mode_of(w, ws, conn, facts, now)?;
        let system = facts_system(w, &facts.account_id)?;
        let member_count = if info.member_count == 0 {
            members.len() as i64
        } else {
            info.member_count
        };
        let has_external = info.has_external || members.iter().any(|m| m.external);
        let (space, created) = spaces::locate_or_create(
            w,
            &NewSpace {
                ws,
                system_id: &system,
                connection_id: conn,
                kind: info.kind,
                external_id: ext,
                name: &info.name,
                member_count,
                has_external,
                muted: spaces::default_new_space_muted(mode, info.kind),
                now,
                platform: facts.platform.as_deref(),
            },
        )?;
        if !created {
            // added back after leaving (or a space first seen through a message): refresh it
            registration::set_state(w, &space.id, registration::State::Active, now)?;
            apply_info(
                w,
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
        member_repo::add(w, ws, &space.id, &facts.account_id, "bot", now)?;
        let n = add_members(w, ws, conn, facts, &space, members, now)?;
        let space = reload(w, &space.id)?;
        // New bot groups default to silent record (`muted = true`, `configured = false`) and never
        // push a 4-choice ask card into Inbox (#213); users can change the rule with one click on
        // the space page.
        registration::emit(w, ws, &space.id);
        Ok(SpaceChange {
            created,
            decision_id: None,
            members_changed: n,
            ..SpaceChange::of(&space)
        })
    }

    /// Creates the spaces of the groups connection `conn` is already in:
    /// `configured=0`, default silent record for bot groups, no Inbox ask decision each.
    /// Known spaces are left as they are. Returns how many were created.
    pub async fn sync_spaces(
        &self,
        ws: &str,
        conn: &str,
        chats: &[(String, SpaceInfo)],
    ) -> ChannelResult<usize> {
        let store = self.store().clone();
        let facts = self.facts(ws, conn)?;
        let now = self.now();
        store.write(|w| {
            let mode = mode_of(w, ws, conn, &facts, now)?;
            let system = facts_system(w, &facts.account_id)?;
            let mut n = 0;
            for (ext, info) in chats {
                if ext.trim().is_empty() {
                    continue;
                }
                let (s, created) = spaces::locate_or_create(
                    w,
                    &NewSpace {
                        ws,
                        system_id: &system,
                        connection_id: conn,
                        kind: info.kind,
                        external_id: ext,
                        name: &info.name,
                        member_count: info.member_count,
                        has_external: info.has_external,
                        muted: spaces::default_new_space_muted(mode, info.kind),
                        now,
                        platform: facts.platform.as_deref(),
                    },
                )?;
                if created {
                    member_repo::add(w, ws, &s.id, &facts.account_id, "bot", now)?;
                    n += 1;
                }
            }
            Ok(n)
        })
    }
}
