//! Messages without a provider id: how the Hub tells them apart and matches their copies.
//!
//! An adapter without platform message ids ([`crate::ext::MessageIds::None`]) gives each delivery
//! its remote's own id when it has one ([`crate::InboundMessage::source_id`]): a redelivery with the
//! same id through the same connection is the message stored before (`chan_message_seen`). Every
//! message also gets a content key: a versioned hash of its sender (in the side's handle system),
//! its text and the kinds of its attachments. A copy that has no provider id, or whose other copy
//! has none, is matched by it within the time precision of the sides that see the space, nearest
//! first; a connection never matches a message it supplied itself, so two identical messages stay
//! two. When the sender's party has other handles than the one its key was made of (a side that
//! reported a handle of another system first), a copy without a key match also tries the keys made
//! of each of them. A copy with a provider id upgrades the message to it. Messages stored without
//! one get an id of the Hub's own (`fp:…`), which never leaves the Hub.
//!
//! Known costs: a redelivery without a delivery id is a new message, as two identical messages
//! are, since a connection never matches its own copies: an adapter without platform ids gives a
//! `source_id` or keeps its own cursor so it does not deliver a message twice. An edit's new text
//! is not applied.

use serde_json::Value;
use sha2::{Digest, Sha256};
use steve_store::channel_message_repo::duplicates::Probe;
use steve_store::channel_message_repo::ingress::SYNTHETIC_PREFIX as FP;
use steve_store::channel_spaces::rows;
use steve_store::connections::Connections;
use steve_store::connections::lifecycle;
use steve_store::{Accounts, Read, Systems};

use crate::channel::HandleRef;
use crate::ext::PRECISION_MS;
use crate::{ChannelResult, Channels};

/// The version of the content key's formula (part of every key).
pub const KEY_VERSION: &str = "v1";
/// The time precision of a side that declared none.
const DEFAULT_PRECISION_MS: i64 = 1000;

/// A Hub-made id (`fp:…`), not a provider's.
pub fn is_fallback(ext: &str) -> bool {
    ext.starts_with(FP)
}

/// A provider id as it may leave the Hub: `None` for none, an empty one or a Hub-made one.
pub fn platform(ext: Option<String>) -> Option<String> {
    ext.filter(|x| !x.is_empty() && !is_fallback(x))
}

/// The content key of a message from `system:handle` with this text and these attachments:
/// `v1:` and the first 32 hex digits of a SHA-256. The text is compared as echoes are
/// ([`crate::arbiter::echo_key`]).
pub fn key_of(system: &str, handle: &str, text: &str, attachments: &[Value]) -> String {
    let kinds: Vec<&str> = attachments
        .iter()
        .filter_map(|a| a["type"].as_str())
        .collect();
    let body = format!(
        "{system}:{handle}\n{}\n{}",
        crate::arbiter::echo_key(text),
        kinds.join(",")
    );
    let hash = hex::encode(Sha256::digest(body.as_bytes()));
    format!("{KEY_VERSION}:{}", &hash[..32])
}

/// Whom a key names the sender by: the party's first handle in `system` that is valid everywhere,
/// so every copy of a message names the same person the same way whichever handle its side
/// reported; else the handle reported.
pub fn key_sender(
    r: &mut impl Read,
    ws: &str,
    party: Option<&str>,
    system: &str,
    reported: &HandleRef,
) -> ChannelResult<(String, String)> {
    if let Some(party) = party
        && let Some(system) = Systems(r).get(ws, system)?
        && let Some(handle) =
            Accounts(r).preferred_handle(ws, party, &system.family, &system.id, None)?
    {
        return Ok((system.id, handle));
    }
    Ok((reported.system.clone(), reported.handle.trim().to_string()))
}

/// The handle system a key of connection side `adapter` uses: its declared one, else `reported`.
pub fn key_system(ch: &Channels, adapter: &str, reported: &str) -> String {
    ch.decl(adapter)
        .map(|d| d.handle_system.clone())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| reported.to_string())
}

/// The content key of a message sent through connection `conn` with this text: its account is the
/// sender.
pub fn out_key(
    ch: &Channels,
    r: &mut impl Read,
    ws: &str,
    conn: &str,
    text: &str,
) -> ChannelResult<Option<String>> {
    let Some(source) = Connections::new(r).source(ws, conn)? else {
        return Ok(None);
    };
    let Some(account) = Accounts(r).get(&source.account_id)? else {
        return Ok(None);
    };
    let want = key_system(ch, &source.adapter, &account.system_id);
    let (s, h) = key_sender(
        r,
        ws,
        Some(&account.party_id),
        &want,
        &HandleRef::new(account.system_id, account.handle),
    )?;
    Ok(Some(key_of(&s, &h, text, &[])))
}

/// How far apart two copies' times may be in space `space_ext`: the coarsest precision of the
/// sides that see it and of `adapter`'s.
pub fn tolerance(
    ch: &Channels,
    r: &mut impl Read,
    ws: &str,
    adapter: &str,
    space_ext: &str,
) -> ChannelResult<i64> {
    let precision = |a: &str| {
        ch.decl(a)
            .map(|d| d.created_precision_ms)
            .unwrap_or(DEFAULT_PRECISION_MS)
            .clamp(*PRECISION_MS.start(), *PRECISION_MS.end())
    };
    let connections = rows::chat_connections(r, ws, space_ext)?;
    let others = lifecycle::adapters(r, &connections)?;
    Ok(others
        .iter()
        .map(|a| precision(a))
        .fold(precision(adapter), i64::max))
}

/// The keys the same message may have been stored under by a side that named its sender by
/// another handle (one of another system, before the party had one in the side's): one for each
/// handle of the sender's party, other than the probe's own key.
pub fn other_keys(
    r: &mut impl Read,
    p: &Probe,
    text: &str,
    attachments: &[Value],
) -> ChannelResult<Vec<String>> {
    let Some(party) = p.party else {
        return Ok(Vec::new());
    };
    let handles = Accounts(r).matching_handles(p.ws, party, 64)?;
    let mut keys: Vec<String> = handles
        .iter()
        .map(|(s, h)| key_of(s, h.trim(), text, attachments))
        .filter(|k| k != p.key)
        .collect();
    keys.sort();
    keys.dedup();
    Ok(keys)
}
