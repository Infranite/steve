//! Persona bindings (channels-personas.md §1, §2.2, §2.7): which persona speaks for a bot account
//! (`space_id = ''`, the account default) and, optionally, in one space. The effective persona of a
//! space = space binding ?? account default ?? the workspace's default persona “Steve”.
//!
//! Accounts of your own (`mode=self`) never use a persona: you speak as yourself, so they have no
//! rows here and setting one is `400`.

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use steve_modules::ApiError;
use steve_store::base::Viewer;
use steve_store::{ChangeEvent, Tx};

use crate::ChannelResult;
use crate::personas::{self, Moved, PersonaRow, UsedAccount, UsedBy, UsedSpace};
use crate::spaces::{self, Mode, SpaceRow};

/// `400` for a persona on an account of your own (PATCH /channels and PATCH /spaces).
pub const SELF_NO_PERSONA: &str = "以你本人接入的账号用你自己的身份说话，不能设对外身份";

/// The account behind a connection and the connection's mode: `channel_settings.mode`, or — before
/// the settings exist — `self` when the account belongs to a user's own contact (like the inbound
/// pipeline decides).
pub fn connection_account(c: &Connection, ws: &str, conn: &str) -> ChannelResult<(String, Mode)> {
    let (account, personal): (String, bool) = c
        .query_row(
            "SELECT c.account_id, EXISTS (SELECT 1 FROM accounts a JOIN parties p ON p.id = a.party_id
                 WHERE a.id = c.account_id AND p.user_id IS NOT NULL)
               FROM connections c WHERE c.id = ? AND c.workspace_id = ?",
            params![conn, ws],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
        .ok_or_else(|| ApiError::not_found("连接"))?;
    let mode = match spaces::channel_settings(c, ws, conn)? {
        Some(s) => s.mode,
        None if personal => Mode::Own,
        None => Mode::Bot,
    };
    Ok((account, mode))
}

/// The live persona bound at `(account, space)` (`space = ""` for the account default).
fn bound(
    c: &Connection,
    ws: &str,
    account: &str,
    space: &str,
) -> ChannelResult<Option<PersonaRow>> {
    let id: Option<String> = c
        .query_row(
            "SELECT b.persona_id FROM persona_bindings b JOIN personas p ON p.id = b.persona_id AND p.deleted = 0
              WHERE b.workspace_id = ? AND b.account_id = ? AND b.space_id = ?",
            params![ws, account, space],
            |r| r.get(0),
        )
        .optional()?;
    match id {
        Some(id) => personas::get(c, &id),
        None => Ok(None),
    }
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

/// The effective persona of a space (§2.7). `None` for spaces of your own accounts, or when the
/// workspace has no default persona yet (no Bot to seed it with).
pub fn effective(c: &Connection, space: &SpaceRow) -> ChannelResult<Option<Effective>> {
    // disconnected: nobody speaks there any more
    if space.connection_id.is_empty() {
        return Ok(None);
    }
    let ws = &space.workspace_id;
    let (account, mode) = connection_account(c, ws, &space.connection_id)?;
    if mode == Mode::Own {
        return Ok(None);
    }
    if let Some(p) = bound(c, ws, &account, &space.id)? {
        return Ok(Some(Effective {
            persona: p,
            source: Source::Space,
        }));
    }
    account_effective(c, ws, &account)
}

fn account_effective(c: &Connection, ws: &str, account: &str) -> ChannelResult<Option<Effective>> {
    if let Some(p) = bound(c, ws, account, "")? {
        return Ok(Some(Effective {
            persona: p,
            source: Source::Account,
        }));
    }
    Ok(personas::default_persona(c, ws)?.map(|p| Effective {
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
pub fn space_persona(c: &Connection, space: &SpaceRow) -> ChannelResult<Option<PersonaRef>> {
    Ok(effective(c, space)?.map(|e| PersonaRef {
        inherited: Some(e.inherited()),
        id: e.persona.id,
        name: e.persona.name,
    }))
}

/// `ChannelAccount.persona`: the account's default persona, `null` for your own accounts.
pub fn account_persona(c: &Connection, ws: &str, conn: &str) -> ChannelResult<Option<PersonaRef>> {
    let (account, mode) = connection_account(c, ws, conn)?;
    if mode == Mode::Own {
        return Ok(None);
    }
    Ok(account_effective(c, ws, &account)?.map(|e| PersonaRef {
        id: e.persona.id,
        name: e.persona.name,
        inherited: None,
    }))
}

/// The persona bound to the space itself (not inherited), for revisions.
pub fn space_binding(c: &Connection, space: &SpaceRow) -> ChannelResult<Option<String>> {
    if space.connection_id.is_empty() {
        return Ok(None);
    }
    let (account, _) = connection_account(c, &space.workspace_id, &space.connection_id)?;
    Ok(bound(c, &space.workspace_id, &account, &space.id)?.map(|p| p.id))
}

/// Sets (`Some`) or clears (`None` = follow the account) the space's own persona. Your own
/// accounts' spaces → `400`; the persona must be a live one of this workspace. Publishes `space`.
/// The caller writes the space's revision.
pub fn set_space(tx: &Tx, space: &SpaceRow, persona: Option<&str>, now: i64) -> ChannelResult<()> {
    let ws = &space.workspace_id;
    let (account, mode) = connection_account(tx, ws, &space.connection_id)?;
    match persona {
        Some(p) => {
            if mode == Mode::Own {
                return Err(ApiError::rule(SELF_NO_PERSONA));
            }
            personas::own(tx, ws, p)?;
            upsert(tx, ws, &account, &space.id, p, now)?;
        }
        None => {
            tx.execute(
                "DELETE FROM persona_bindings WHERE workspace_id = ? AND account_id = ? AND space_id = ?",
                params![ws, account, space.id],
            )?;
        }
    }
    spaces::event(tx, ws, &space.id);
    Ok(())
}

/// Sets a bot account's default persona (`POST /channels`, `PATCH /channels/:id` with
/// `persona_id`). Your own accounts → `400`. Seeds the default persona if needed. Publishes
/// `channel`; the caller writes the channel's revision.
pub fn set_account_default(
    tx: &Tx,
    viewer: &Viewer,
    conn: &str,
    persona: &str,
    now: i64,
) -> ChannelResult<()> {
    let ws = &viewer.ws;
    let (account, mode) = connection_account(tx, ws, conn)?;
    if mode == Mode::Own {
        return Err(ApiError::rule(SELF_NO_PERSONA));
    }
    personas::own(tx, ws, persona)?;
    personas::ensure_default(tx, ws, now)?;
    upsert(tx, ws, &account, "", persona, now)?;
    tx.publish(ChangeEvent::Channel {
        id: conn.into(),
        ws: ws.clone(),
    });
    Ok(())
}

fn upsert(
    tx: &Tx,
    ws: &str,
    account: &str,
    space: &str,
    persona: &str,
    now: i64,
) -> ChannelResult<()> {
    tx.execute(
        "INSERT INTO persona_bindings (workspace_id, account_id, space_id, persona_id, created) VALUES (?, ?, ?, ?, ?)
         ON CONFLICT (workspace_id, account_id, space_id) DO UPDATE SET persona_id = excluded.persona_id, created = excluded.created",
        params![ws, account, space, persona, now],
    )?;
    Ok(())
}

/// A persona is going away (§3): account defaults on it move to `fallback`, space bindings on it are
/// dropped (back to following the account). Publishes `channel` / `space` for what moved.
pub(crate) fn release(
    tx: &Tx,
    ws: &str,
    persona: &str,
    fallback: &str,
    now: i64,
) -> ChannelResult<Moved> {
    let mut st = tx.prepare_cached(
        "SELECT b.space_id, coalesce(c.id, '') FROM persona_bindings b LEFT JOIN connections c ON c.account_id = b.account_id
          WHERE b.workspace_id = ? AND b.persona_id = ? ORDER BY b.space_id",
    )?;
    let rows = st
        .query_map(params![ws, persona], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(st);
    let mut moved = Moved::default();
    for (space, conn) in rows {
        if space.is_empty() {
            moved.accounts += 1;
            if !conn.is_empty() {
                tx.publish(ChangeEvent::Channel {
                    id: conn,
                    ws: ws.into(),
                });
            }
        } else {
            moved.spaces += 1;
            spaces::event(tx, ws, &space);
        }
    }
    tx.execute(
        "UPDATE persona_bindings SET persona_id = ?, created = ? WHERE workspace_id = ? AND persona_id = ? AND space_id = ''",
        params![fallback, now, ws, persona],
    )?;
    tx.execute(
        "DELETE FROM persona_bindings WHERE workspace_id = ? AND persona_id = ? AND space_id <> ''",
        params![ws, persona],
    )?;
    Ok(moved)
}

/// `Persona.used_by`: bot accounts whose default it is (for the default persona also those with
/// no default bound), and the active spaces the viewer sees where it speaks.
pub fn used_by(c: &Connection, viewer: &Viewer, p: &PersonaRow) -> ChannelResult<UsedBy> {
    let ws = &p.workspace_id;
    let mut st = c.prepare_cached(
        "SELECT c.id, a.id, CASE WHEN a.display_name <> '' THEN a.display_name ELSE a.handle_display END
           FROM channel_settings cs JOIN connections c ON c.id = cs.connection_id JOIN accounts a ON a.id = c.account_id
          WHERE cs.workspace_id = ? AND cs.mode = 'bot' AND c.status <> 'revoked' ORDER BY a.display_name, a.id",
    )?;
    let accs = st
        .query_map([ws], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(st);
    let mut out = UsedBy::default();
    for (_conn, account, name) in accs {
        if account_effective(c, ws, &account)?.is_some_and(|e| e.persona.id == p.id) {
            out.accounts.push(UsedAccount {
                account_id: account,
                name,
            });
        }
    }
    for s in spaces::list(c, viewer, None, None)? {
        if s.status != "active" {
            continue;
        }
        if let Some(e) = effective(c, &s)?
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
