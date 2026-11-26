//! Deterministic output routing on the existing C6 sender. This module does not own a transport,
//! runner or retry loop. The intent and `chan_messages` row commit together; the ordinary outbox
//! then owns approvals, ambiguous outcomes, dedupe-aware retry, failure decisions and recall.

use serde_json::json;
use steve_modules::outbound::{DeliveryIntent, Identity};
use steve_modules::{ApiError, Caller};
use steve_store::base::Viewer;
use steve_store::channel_message_repo::{self, outputs};
use steve_store::channel_spaces::{members, registration, rows};
use steve_store::{Members, Read, W};

use crate::deliver::{Auth, Delivered, OutRequest, OutTarget, Purpose};
use crate::outbound::{Outbox, TEXT_MAX, text_hash};
use crate::spaces::{self, Mode};
use crate::{ChannelResult, Channels, bindings};

/// Validated immutable metadata passed into C6's message-insert transaction, never to an adapter.
pub(crate) struct Envelope {
    pub who: Caller,
    pub intent: DeliveryIntent,
    pub fingerprint: String,
}

pub use outputs::Delivery;

/// A recorded principal is not permission: membership is rechecked at submission AND queue send.
pub fn require_requester(r: &mut impl Read, who: &Caller) -> ChannelResult<()> {
    if Members::new(r).role(&who.ws, &who.me)?.is_none() {
        return Err(ApiError::Unauthenticated);
    }
    Ok(())
}

pub(crate) fn existing(r: &mut impl Read, e: &Envelope) -> ChannelResult<Option<Delivery>> {
    require_requester(r, &e.who)?;
    match outputs::by_key(r, &e.who.ws, &e.who.me, &e.intent.key)? {
        Some((fingerprint, d)) if fingerprint == e.fingerprint => Ok(Some(d)),
        Some(_) => Err(ApiError::conflict(
            "idempotency key already names a different intent",
        )),
        None => Ok(None),
    }
}

pub(crate) fn outcome(d: &Delivery) -> Delivered {
    crate::deliver::delivered_of(&d.message_id, &d.status, d.decision_id.as_deref())
}

/// Executed in the SAME transaction as `insert_out`; a crash cannot leave an untracked message.
pub(crate) fn record(
    w: &mut W<'_>,
    e: &Envelope,
    req: &OutRequest,
    message: &str,
    now: i64,
) -> ChannelResult<()> {
    require_requester(w, &e.who)?;
    let t = &req.target;
    crate::notifications::verify_intent(w, &e.who, &e.intent, now)?;
    crate::notifications::verify_pinned(w, &e.who, &e.intent, &t.space_id)?;
    let delivery = outputs::insert(
        w,
        &outputs::NewDelivery {
            ws: &e.who.ws,
            requester: &e.who.me,
            key: &e.intent.key,
            fingerprint: &e.fingerprint,
            intent: &serde_json::to_value(&e.intent)?,
            connection: &t.connection_id,
            space: &t.space_id,
            source: t.source_message_id.as_deref(),
            message,
            created: now,
        },
    )?;
    crate::notifications::attach(w, &e.who, &e.intent, &delivery)?;
    steve_store::audit_repo::record(
        w,
        &e.who.ws,
        &e.who.me,
        "output.delivery.created",
        message,
        &json!({"key":e.intent.key,"source":t.source_message_id,"space":t.space_id,"connection":t.connection_id,"reason":e.intent.reason,"urgency":e.intent.urgency}),
    )?;
    Ok(())
}

/// The queue cannot silently reroute a stored intent to another voice/account, nor keep using an
/// account after its requester leaves. Outbound messages without an intent (rule replies, drafts)
/// keep C6 rules.
pub(crate) fn verify_route(
    r: &mut impl Read,
    message: &str,
    now: i64,
) -> ChannelResult<Result<(), String>> {
    let Some(row) = outputs::by_message(r, message)? else {
        return Ok(Ok(()));
    };
    let who = Caller {
        ws: row.ws,
        me: row.requester,
    };
    if require_requester(r, &who).is_err() {
        return Ok(Err("requester is no longer a member".into()));
    }
    let viewer = Viewer::bare(&who.ws, &who.me);
    let i: DeliveryIntent = serde_json::from_value(row.intent)?;
    if let Err(e) = crate::notifications::verify_intent(r, &who, &i, now)
        .and_then(|_| crate::notifications::verify_pinned(r, &who, &i, &row.space))
    {
        return Ok(Err(format!("personal notification consent/route: {e}")));
    }
    match reachable(r, &viewer, &row.space, i.identity, i.party_id.as_deref()) {
        Ok(t) if t.connection_id == row.connection => Ok(Ok(())),
        _ => Ok(Err("stored output route is no longer reachable".into())),
    }
}

fn reachable(
    r: &mut impl steve_store::Read,
    viewer: &Viewer,
    id: &str,
    identity: Identity,
    party: Option<&str>,
) -> ChannelResult<OutTarget> {
    let s =
        rows::get_in_workspace(r, &viewer.ws, id)?.ok_or_else(|| ApiError::not_found("空间"))?;
    let linked = registration::routed_identity(r, &viewer.ws, id, &s.connection_id)?;
    let set = spaces::settings(r, id)?;
    if !linked
        || s.status != "active"
        || set.attention == spaces::Attention::Blocked
        || set.disclosure == "never"
    {
        return Err(ApiError::not_found("reachable conversation"));
    }
    let (account, mode) = bindings::connection_account(r, &viewer.ws, &s.connection_id)?;
    let a = steve_modules::accounts::service::own_account(r, viewer, &account)?;
    let owner = steve_store::Contacts::new(r).current_party(&a.party_id)?;
    if owner
        .and_then(|p| p.user_id)
        .is_some_and(|user| user != viewer.user)
    {
        return Err(ApiError::Unauthenticated);
    }
    if (identity == Identity::Bot) != (mode == Mode::Bot) {
        return Err(ApiError::invalid("route identity does not match account"));
    }
    if mode == Mode::Own {
        if steve_store::parties::user_id(r, &a.party_id)?.as_deref() != Some(&viewer.user) {
            return Err(ApiError::Unauthenticated);
        }
    }
    if let Some(party) = party {
        steve_modules::accounts::service::own_owner(r, viewer, party)?;

        if !members::has_party(r, &viewer.ws, id, party)? {
            return Err(ApiError::not_found("recipient in conversation"));
        }
    }
    Ok(OutTarget {
        ws: viewer.ws.clone(),
        connection_id: s.connection_id.clone(),
        mode,
        space_name: spaces::display_title(r, &s)?,
        space_id: s.id,
        reply_to: None,
        thread_key: None,
        persona_id: None,
        source_message_id: None,
        burst_id: None,
    })
}

pub(crate) fn route(
    r: &mut impl steve_store::Read,
    viewer: &Viewer,
    i: &DeliveryIntent,
) -> ChannelResult<OutTarget> {
    let source = if let Some(id) = &i.source_message_id {
        let row = outputs::source(r, &viewer.ws, id)?
            .ok_or_else(|| ApiError::not_found("source message"))?;
        rows::get_in_workspace(r, &viewer.ws, &row.space)?
            .ok_or_else(|| ApiError::not_found("空间"))?;
        Some(row)
    } else {
        None
    };
    // A reply stays in its source, even when a learned/manual preference differs. Switching needs
    // an explicit space id. Preferences only choose a new conversation for a person.
    let id = if let Some(id) = &i.space_id {
        id.clone()
    } else if let Some(source) = &source {
        source.space.clone()
    } else if let Some(p) = &i.party_id {
        let pref = outputs::route_preference(r, &viewer.ws, &viewer.user, p)?;
        if let Some(manual) = pref.as_ref().and_then(|pref| pref.manual.clone()) {
            manual
        } else {
            let learned = pref
                .and_then(|pref| pref.learned)
                .filter(|id| reachable(r, viewer, id, i.identity, Some(p)).is_ok());
            if let Some(id) = learned {
                id
            } else {
                let ids = members::direct_chats(r, &viewer.ws, p)?;
                ids.into_iter()
                    .find(|id| reachable(r, viewer, id, i.identity, Some(p)).is_ok())
                    .ok_or_else(|| ApiError::not_found("reachable recipient"))?
            }
        }
    } else {
        return Err(ApiError::invalid(
            "intent needs a source, conversation or person",
        ));
    };
    let mut t = reachable(r, viewer, &id, i.identity, i.party_id.as_deref())?;
    if let Some(source) = source {
        if source.space == id {
            t.reply_to = crate::fallback::platform(source.external);
            t.thread_key = source.thread;
        }
        // Kept even across channels for the timeline, not used as a foreign reply_to identifier.
        t.source_message_id = i.source_message_id.clone();
    }
    Ok(t)
}

impl Outbox {
    /// The public submit contract. Duplicate keys only report the existing ledger entry; even an
    /// unknown/failed delivery is NOT sent here again. C6 alone handles its explicit retry policy.
    pub async fn submit(
        &self,
        ch: &Channels,
        who: &Caller,
        intent: DeliveryIntent,
    ) -> ChannelResult<Delivery> {
        if intent.key.trim().is_empty()
            || intent.key.len() > 256
            || intent.reason.len() > 8192
            || intent.urgency.len() > 64
        {
            return Err(ApiError::invalid("intent key/reason/urgency"));
        }
        let text = intent.content.markdown();
        if text.trim().is_empty() || text.chars().count() > TEXT_MAX {
            return Err(ApiError::invalid("content must contain 1..4000 characters"));
        }
        let fingerprint = text_hash(&serde_json::to_string(&intent)?);
        let e = Envelope {
            who: who.clone(),
            intent,
            fingerprint,
        };
        if let Some(d) = ch.store().read(|r| existing(r, &e))? {
            return Ok(d);
        }
        let viewer = ch.modules().viewer(who)?;
        let target = ch.store().read(|r| {
            require_requester(r, who)?;
            crate::notifications::verify_intent(r, who, &e.intent, ch.now())?;
            route(r, &viewer, &e.intent)
        })?;
        // Account visibility is stricter than a shared conversation: never borrow somebody else's
        // personal connection. The queue revalidates the pinned route before dispatch too.
        ch.connections()
            .get(who, &target.connection_id)
            .map_err(|e| ApiError::invalid(e.to_string()))?;
        let req = OutRequest {
            target,
            text,
            purpose: Purpose::Source,
            approve: e.intent.identity == Identity::AsYou,
            generation: None,
            auth: if e.intent.identity == Identity::Bot {
                Auth::Requester {
                    user: who.me.clone(),
                }
            } else {
                Auth::None
            },
            operation_id: None,
        };
        self.deliver_enveloped(ch, &req, Some(&e)).await?;
        ch.store()
            .read(|r| existing(r, &e))?
            .ok_or_else(|| ApiError::Internal("output intent did not commit".into()))
    }

    /// The existing `steve_reply_source` path uses this, not a parallel transport. The requester
    /// comes from the session principal resolved by the Hub, never an item's owner or an audit.
    pub async fn reply_source_as(
        &self,
        ch: &Channels,
        who: &Caller,
        item: &str,
        text: &str,
        key: &str,
        space: Option<String>,
        party: Option<String>,
    ) -> ChannelResult<Option<Delivery>> {
        let source = ch.store().read(|r| {
            require_requester(r, who)?;
            crate::outbound::source_message(r, &who.ws, item)
        })?;
        let Some(source) = source else {
            return Ok(None);
        };
        // Identity is derived only for a NEW source reply. A timeout retry must fingerprint
        // the original intent, not reinterpret it using a conversation's newly selected voice.
        // Read only the scoped caller's record; submit still checks membership and the full hash.

        let recorded_identity = ch.store().read(|r| -> ChannelResult<_> {
            require_requester(r, who)?;
            Ok(outputs::recorded_identity(r, &who.ws, &who.me, key)?)
        })?;
        let identity = if let Some(identity) = recorded_identity {
            serde_json::from_value(serde_json::Value::String(identity))?
        } else {
            let viewer = ch.modules().viewer(who)?;

            let mode = ch.store().read(|r| -> ChannelResult<_> {
                let source_space = channel_message_repo::space_of(r, &who.ws, &source)?
                    .ok_or_else(|| ApiError::not_found("source message"))?;
                let id = space.as_deref().unwrap_or(&source_space);
                let s = rows::get_in_workspace(r, &viewer.ws, id)?
                    .ok_or_else(|| ApiError::not_found("空间"))?;
                Ok(bindings::connection_account(r, &who.ws, &s.connection_id)?.1)
            })?;
            if mode == Mode::Bot {
                Identity::Bot
            } else {
                Identity::AsYou
            }
        };
        self.submit(
            ch,
            who,
            DeliveryIntent {
                key: key.into(),
                identity,
                content: steve_modules::outbound::Content {
                    markdown: text.into(),
                    ..Default::default()
                },
                source_message_id: Some(source),
                space_id: space,
                party_id: party,
                reason: format!("item:{item}"),
                urgency: "normal".into(),
            },
        )
        .await
        .map(Some)
    }
}

/// Store a person's manual override. Null clears it without deleting the learned preference.
/// Only a currently reachable DM of that person is accepted; manual always wins over learning.
pub fn set_preference(
    ch: &Channels,
    who: &Caller,
    party: &str,
    space: Option<&str>,
) -> ChannelResult<()> {
    let viewer = ch.modules().viewer(who)?;

    ch.store().write(|w| {
        require_requester(w, who)?;
        steve_modules::accounts::service::own_owner(w, &viewer, party)?;
        if let Some(space) = space {
            let s = rows::get_in_workspace(w, &viewer.ws, space)?
                .ok_or_else(|| ApiError::not_found("空间"))?;
            let (_, mode) = bindings::connection_account(w, &who.ws, &s.connection_id)?;
            let role = if mode == Mode::Bot {
                Identity::Bot
            } else {
                Identity::AsYou
            };
            reachable(w, &viewer, space, role, Some(party))?;
            if s.kind != "dm" {
                return Err(ApiError::invalid(
                    "preference must be a direct conversation",
                ));
            }
        }
        outputs::set_manual(w, &who.ws, &who.me, party, space, ch.now())?;
        Ok(())
    })
}

/// A live inbound reply, on the exact conversation and provider message, is a receipt and a weak
/// channel preference observation. History import, guessed identities and unrelated messages do
/// not teach preferences. An observation never changes `manual_space_id`.
pub(crate) fn observe_reply(
    w: &mut W<'_>,
    ws: &str,
    space: &str,
    reply_to: Option<&str>,
    party: Option<&str>,
    message: &str,
    now: i64,
) -> ChannelResult<()> {
    use steve_store::channel_message_repo::outputs;

    let (Some(reply), Some(party)) = (reply_to, party) else {
        return Ok(());
    };
    let rows = outputs::replied_deliveries(w, ws, space, reply)?;
    if rows.is_empty() {
        return Ok(());
    }
    let dm = steve_store::channel_spaces::rows::get_in_workspace(w, ws, space)?
        .is_some_and(|row| row.kind == "dm");
    for delivery in rows {
        outputs::record_reply(w, &delivery.id, message, now)?;
        if dm {
            outputs::learn_route(w, ws, &delivery.requester, party, space, now)?;
        }
        steve_store::audit_repo::record(
            w,
            ws,
            &delivery.requester,
            "output.delivery.replied",
            &delivery.id,
            &json!({"message": message, "space": space}),
        )?;
    }
    Ok(())
}

/// Only the original live requester can read an intent (shared bot conversations do not expose
/// another person's delivery reasons or content). State is joined from the authoritative outbox.
pub fn get(ch: &Channels, who: &Caller, id: &str) -> ChannelResult<Delivery> {
    ch.store().read(|r| {
        require_requester(r, who)?;
        outputs::get(r, &who.ws, &who.me, id)?.ok_or_else(|| ApiError::not_found("delivery"))
    })
}
