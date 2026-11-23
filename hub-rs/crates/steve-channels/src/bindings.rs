//! Persona bindings: which persona speaks for a bot account
//! (`space_id = ''`, the account default) and, optionally, in one space. The effective persona of a
//! space = space binding ?? account default ?? the workspace's default persona “Steve”.
//!
//! Accounts of your own (`mode=self`) never use a persona: you speak as yourself, so they have no
//! rows here and setting one is `400`.

use std::collections::{HashMap, HashSet};

use serde::Serialize;
use steve_modules::ApiError;
use steve_store::account_rows::AccountQuery;
use steve_store::base::Viewer;
use steve_store::channel_personas::bindings as repo;
use steve_store::channel_spaces::{account_settings, rows};
use steve_store::connections::{self, lifecycle};
use steve_store::{Accounts, Connections, Read, W};

use crate::ChannelResult;
use crate::personas::{self, PersonaRow, UsedAccount, UsedBy, UsedSpace};
use crate::spaces::{Mode, SpaceRow};

/// `400` for a persona on an account of your own (PATCH /channels and PATCH /spaces).
pub const SELF_NO_PERSONA: &str = "以你本人接入的账号用你自己的身份说话，不能设对外身份";

/// The account behind a connection and the connection's mode: `channel_settings.mode`, or — before
/// the settings exist — `self` when the account belongs to a user's own contact (like the inbound
/// pipeline decides).
pub fn connection_account(
    r: &mut impl Read,
    ws: &str,
    conn: &str,
) -> ChannelResult<(String, Mode)> {
    let source = connections::Connections::new(r)
        .source(ws, conn)?
        .ok_or_else(|| ApiError::not_found("连接"))?;
    let mode = match account_settings::channel_mode(r, ws, conn)?.as_deref() {
        Some("self") => Mode::Own,
        Some(_) => Mode::Bot,
        None if lifecycle::personal_user(r, ws, conn)?.is_some() => Mode::Own,
        None => Mode::Bot,
    };
    Ok((source.account_id, mode))
}

/// The live persona bound at `(account, space)` (`space = ""` for the account default).
fn bound(
    r: &mut impl Read,
    ws: &str,
    account: &str,
    space: &str,
) -> ChannelResult<Option<PersonaRow>> {
    let Some(id) = repo::bound_id(r, ws, account, space)? else {
        return Ok(None);
    };
    Ok(personas::get(r, &id)?.filter(|p| !p.deleted))
}

/// Where an effective persona comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// Bound to this space on its own.
    Space,
    /// The account's default.
    Account,
    /// Nothing bound: the workspace's default persona.
    Default,
}

/// The persona that speaks in a space.
#[derive(Debug, Clone, PartialEq)]
pub struct Effective {
    pub persona: PersonaRow,
    pub source: Source,
}

impl Effective {
    /// `Space.persona.inherited`: not bound to the space itself.
    pub fn inherited(&self) -> bool {
        self.source != Source::Space
    }
}

/// The effective persona of a space. `None` for spaces of your own accounts, or when the
/// workspace has no default persona yet (no Bot to seed it with).
pub fn effective(r: &mut impl Read, space: &SpaceRow) -> ChannelResult<Option<Effective>> {
    // disconnected: nobody speaks there any more
    if space.connection_id.is_empty() {
        return Ok(None);
    }
    let ws = &space.workspace_id;
    let (account, mode) = connection_account(r, ws, &space.connection_id)?;
    if mode == Mode::Own {
        return Ok(None);
    }
    if let Some(p) = bound(r, ws, &account, &space.id)? {
        return Ok(Some(Effective {
            persona: p,
            source: Source::Space,
        }));
    }
    account_effective(r, ws, &account)
}

fn account_effective(
    r: &mut impl Read,
    ws: &str,
    account: &str,
) -> ChannelResult<Option<Effective>> {
    if let Some(p) = bound(r, ws, account, "")? {
        return Ok(Some(Effective {
            persona: p,
            source: Source::Account,
        }));
    }
    Ok(personas::default_persona(r, ws)?.map(|p| Effective {
        persona: p,
        source: Source::Default,
    }))
}

/// `{id, name}` of a persona as shown on accounts; `inherited` only on spaces.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PersonaRef {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inherited: Option<bool>,
}

/// `Space.persona`: `{id, name, inherited}`, `null` for your own accounts.
pub fn space_persona(r: &mut impl Read, space: &SpaceRow) -> ChannelResult<Option<PersonaRef>> {
    Ok(effective(r, space)?.map(|e| PersonaRef {
        inherited: Some(e.inherited()),
        id: e.persona.id,
        name: e.persona.name,
    }))
}

/// `ChannelAccount.persona`: the account's default persona, `null` for your own accounts.
pub fn account_persona(
    r: &mut impl Read,
    ws: &str,
    conn: &str,
) -> ChannelResult<Option<PersonaRef>> {
    let (account, mode) = connection_account(r, ws, conn)?;
    if mode == Mode::Own {
        return Ok(None);
    }
    Ok(account_effective(r, ws, &account)?.map(|e| PersonaRef {
        id: e.persona.id,
        name: e.persona.name,
        inherited: None,
    }))
}

/// The persona bound to the space itself (not inherited), for revisions.
pub fn space_binding(r: &mut impl Read, space: &SpaceRow) -> ChannelResult<Option<String>> {
    if space.connection_id.is_empty() {
        return Ok(None);
    }
    let (account, _) = connection_account(r, &space.workspace_id, &space.connection_id)?;
    Ok(bound(r, &space.workspace_id, &account, &space.id)?.map(|p| p.id))
}

/// Sets (`Some`) or clears (`None` = follow the account) the space's own persona. Your own
/// accounts' spaces → `400`; the persona must be a live one of this workspace. Publishes `space`.
/// The caller writes the space's revision.
pub fn set_space(
    w: &mut W<'_>,
    space: &SpaceRow,
    persona: Option<&str>,
    now: i64,
) -> ChannelResult<()> {
    let ws = &space.workspace_id;
    let (account, mode) = connection_account(w, ws, &space.connection_id)?;
    if let Some(persona) = persona {
        if mode == Mode::Own {
            return Err(ApiError::rule(steve_i18n::t!(
                "channels.error.self_no_persona"
            )));
        }
        personas::own(w, ws, persona)?;
    }
    match persona {
        Some(persona) => repo::bind(w, ws, &account, &space.id, persona, now)?,
        None => repo::clear(w, ws, &account, &space.id)?,
    }
    w.emit(steve_types::ChannelsEvent::Space {
        id: space.id.clone().into(),
        ws: ws.clone().into(),
    });
    Ok(())
}

/// Sets a bot account's default persona (`POST /channels`, `PATCH /channels/:id` with
/// `persona_id`). Your own accounts → `400`. Seeds the default persona if needed. Publishes
/// `channel`; the caller writes the channel's revision.
pub fn set_account_default(
    w: &mut W<'_>,
    viewer: &Viewer,
    conn: &str,
    persona: &str,
    now: i64,
) -> ChannelResult<()> {
    let ws = &viewer.ws;
    let (account, mode) = connection_account(w, ws, conn)?;
    if mode == Mode::Own {
        return Err(ApiError::rule(steve_i18n::t!(
            "channels.error.self_no_persona"
        )));
    }
    personas::own(w, ws, persona)?;
    personas::ensure_default(w, ws, now)?;
    repo::bind(w, ws, &account, "", persona, now)?;
    w.emit(steve_types::ChannelsEvent::Channel {
        id: conn.into(),
        ws: ws.clone().into(),
    });
    Ok(())
}

/// `Persona.used_by`: bot accounts whose default it is (for the default persona also those with
/// no default bound), and the active spaces the viewer sees where it speaks.
pub fn used_by(r: &mut impl Read, viewer: &Viewer, p: &PersonaRow) -> ChannelResult<UsedBy> {
    let ws = &p.workspace_id;
    let bot_connections: HashSet<_> = account_settings::bot_connections(r, ws)?
        .into_iter()
        .collect();
    let accounts: HashMap<_, _> = Accounts(r)
        .list(viewer, &AccountQuery::default())?
        .into_iter()
        .map(|account| (account.id.clone(), account))
        .collect();
    let mut connections: Vec<_> = Connections(r)
        .list(ws)?
        .into_iter()
        .filter(|conn| conn.status != "revoked" && bot_connections.contains(&conn.id))
        .filter_map(|conn| accounts.get(&conn.account_id))
        .collect();
    connections.sort_by(|a, b| {
        a.display_name
            .cmp(&b.display_name)
            .then_with(|| a.id.cmp(&b.id))
    });
    let mut out = UsedBy::default();
    for account in connections {
        if account_effective(r, ws, &account.id)?.is_some_and(|e| e.persona.id == p.id) {
            out.accounts.push(UsedAccount {
                account_id: account.id.clone(),
                name: if account.display_name.is_empty() {
                    account.handle_display.clone()
                } else {
                    account.display_name.clone()
                },
            });
        }
    }
    for s in rows::list(r, &viewer.ws, None, None)? {
        if s.status != "active" {
            continue;
        }
        if let Some(e) = effective(r, &s)?
            && e.persona.id == p.id
        {
            out.spaces.push(UsedSpace {
                own: !e.inherited(),
                id: s.id,
                name: s.name,
            });
        }
    }
    Ok(out)
}
