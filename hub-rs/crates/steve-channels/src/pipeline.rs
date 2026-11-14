//! Deterministic output routing on the existing C6 sender. This module does not own a transport,
//! runner or retry loop. The intent and `chan_messages` row commit together; the ordinary outbox
//! then owns approvals, ambiguous outcomes, dedupe-aware retry, failure decisions and recall.

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde_json::json;
use steve_modules::outbound::{DeliveryIntent, Identity};
use steve_modules::{ApiError, Caller};
use steve_store::base::Viewer;
use steve_store::{Tx, uid};

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

#[derive(Debug, Clone, Serialize)]
pub struct Delivery {
    pub id: String,
    pub message_id: String,
    pub space_id: String,
    pub connection_id: String,
    /// Live channel-message state: draft/pending/unknown/sent/failed/discarded/recalled.
    pub status: String,
    pub decision_id: Option<String>,
    pub attempts: i64,
    pub last_error: Option<String>,
    pub replied_at: Option<i64>,
    pub reply_message_id: Option<String>,
}

/// A recorded principal is not permission: membership is rechecked at submission AND queue send.
pub fn require_requester(c: &Connection, who: &Caller) -> ChannelResult<()> {
    let live: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM workspace_members WHERE workspace_id=? AND user_id=?)",
        params![who.ws, who.me],
        |r| r.get(0),
    )?;
    if !live {
        return Err(ApiError::Unauthenticated);
    }
    Ok(())
}

fn lookup(c: &Connection, who: &Caller, key: &str) -> ChannelResult<Option<(String, Delivery)>> {
    Ok(c.query_row(
        "SELECT d.fingerprint,d.id,d.message_id,d.space_id,d.connection_id,m.status,m.decision_id,d.attempts,d.last_error,d.replied_at,d.reply_message_id
         FROM output_deliveries d JOIN chan_messages m ON m.id=d.message_id
         WHERE d.workspace_id=? AND d.requester=? AND d.idempotency_key=?",
        params![who.ws,who.me,key], |r| Ok((r.get(0)?, Delivery {
            id:r.get(1)?,message_id:r.get(2)?,space_id:r.get(3)?,connection_id:r.get(4)?,
            status:r.get(5)?,decision_id:r.get(6)?,attempts:r.get(7)?,last_error:r.get(8)?,replied_at:r.get(9)?,reply_message_id:r.get(10)?,
        })),
    ).optional()?)
}

pub(crate) fn existing(c: &Connection, e: &Envelope) -> ChannelResult<Option<Delivery>> {
    require_requester(c, &e.who)?;
    match lookup(c, &e.who, &e.intent.key)? {
        Some((fingerprint, d)) if fingerprint == e.fingerprint => Ok(Some(d)),
        Some(_) => Err(ApiError::conflict(
            "idempotency key already names a different intent",
        )),
        None => Ok(None),
    }
}

pub(crate) fn outcome(d: &Delivery) -> Delivered {
    match (d.status.as_str(), d.decision_id.as_ref()) {
        ("draft", Some(id)) => Delivered::Drafted {
            message_id: d.message_id.clone(),
            decision_id: id.clone(),
        },
        ("failed", Some(id)) => Delivered::Failed {
            message_id: d.message_id.clone(),
            decision_id: id.clone(),
        },
        ("sent" | "recalled", _) => Delivered::Sent {
            message_id: d.message_id.clone(),
        },
        ("discarded", _) => Delivered::Skipped,
        _ => Delivered::Queued {
            message_id: d.message_id.clone(),
        },
    }
}

/// Executed in the SAME transaction as `insert_out`; a crash cannot leave an untracked message.
pub(crate) fn record(
    tx: &Tx,
    e: &Envelope,
    req: &OutRequest,
    message: &str,
    now: i64,
) -> ChannelResult<()> {
    require_requester(tx, &e.who)?;
    let t = &req.target;
    tx.execute(
        "INSERT INTO output_deliveries(id,workspace_id,requester,idempotency_key,fingerprint,intent,connection_id,space_id,source_message_id,message_id,created)
         VALUES (?,?,?,?,?,?,?,?,?,?,?)",
        params![uid("od"),e.who.ws,e.who.me,e.intent.key,e.fingerprint,serde_json::to_string(&e.intent)?,t.connection_id,t.space_id,t.source_message_id,message,now],
    )?;
    steve_store::base::audit(
        tx,
        &e.who.ws,
        &e.who.me,
        "output.delivery.created",
        message,
        &json!({"key":e.intent.key,"source":t.source_message_id,"space":t.space_id,"connection":t.connection_id,"reason":e.intent.reason,"urgency":e.intent.urgency}),
    )?;
    Ok(())
}

/// The queue cannot silently reroute a stored intent to another voice/account, nor keep using an
/// account after its requester leaves. Legacy outbound messages have no intent and keep C6 rules.
pub(crate) fn verify_route(c: &Connection, message: &str) -> ChannelResult<Result<(), String>> {
    let row: Option<(String,String,String,String,String)> = c.query_row(
        "SELECT workspace_id,requester,connection_id,space_id,intent FROM output_deliveries WHERE message_id=?",
        [message], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)),
    ).optional()?;
    let Some((ws, user, conn, space, intent)) = row else {
        return Ok(Ok(()));
    };
    let who = Caller { ws, me: user };
    if require_requester(c, &who).is_err() {
        return Ok(Err("requester is no longer a member".into()));
    }
    let viewer = Viewer::load(c, &who.ws, &who.me)?;
    let i: DeliveryIntent = serde_json::from_str(&intent)?;
    match reachable(c, &viewer, &space, i.identity, i.party_id.as_deref()) {
        Ok(t) if t.connection_id == conn => Ok(Ok(())),
        _ => Ok(Err("stored output route is no longer reachable".into())),
    }
}

fn reachable(
    c: &Connection,
    viewer: &Viewer,
    id: &str,
    identity: Identity,
    party: Option<&str>,
) -> ChannelResult<OutTarget> {
    let s = spaces::own(c, viewer, id)?;
    let linked:bool=c.query_row(
        "SELECT EXISTS(SELECT 1 FROM space_identities i JOIN connections c ON c.id=i.connection_id
         WHERE i.workspace_id=? AND i.space_id=? AND i.connection_id=? AND i.left_at IS NULL AND c.status IN ('ok','error','paused','reauth'))",
        params![viewer.ws,id,s.connection_id], |r| r.get(0),
    )?;
    let set = spaces::settings(c, id)?;
    if !linked
        || s.status != "active"
        || set.attention == spaces::Attention::Blocked
        || set.disclosure == "never"
    {
        return Err(ApiError::not_found("reachable conversation"));
    }
    let (account, mode) = bindings::connection_account(c, &viewer.ws, &s.connection_id)?;
    let a = steve_modules::accounts::service::own_account(c, viewer, &account)?;
    let owner = steve_modules::accounts::service::current_party(c, &a.party_id)?;
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
        let mine:bool=c.query_row(
            "SELECT EXISTS(SELECT 1 FROM accounts a JOIN parties p ON p.id=a.party_id WHERE a.id=? AND a.workspace_id=? AND p.user_id=?)",
            params![account,viewer.ws,viewer.user],|r| r.get(0),
        )?;
        if !mine {
            return Err(ApiError::Unauthenticated);
        }
    }
    if let Some(party) = party {
        steve_modules::accounts::service::own_owner(c, viewer, party)?;
        let member:bool=c.query_row(
            "SELECT EXISTS(SELECT 1 FROM space_members sm JOIN accounts a ON a.id=sm.account_id JOIN parties p ON p.id=a.party_id
             WHERE sm.workspace_id=? AND sm.space_id=? AND p.workspace_id=? AND p.id=? AND p.status='active')",
            params![viewer.ws,id,viewer.ws,party],|r|r.get(0),
        )?;
        if !member {
            return Err(ApiError::not_found("recipient in conversation"));
        }
    }
    Ok(OutTarget {
        ws: viewer.ws.clone(),
        connection_id: s.connection_id,
        mode,
        space_id: s.id,
        space_name: s.name,
        reply_to: None,
        thread_key: None,
        persona_id: None,
        source_message_id: None,
    })
}

fn route(c: &Connection, viewer: &Viewer, i: &DeliveryIntent) -> ChannelResult<OutTarget> {
    let source = if let Some(id) = &i.source_message_id {
        let row:Option<(String,Option<String>,Option<String>)>=c.query_row(
            "SELECT space_id,external_id,thread_key FROM chan_messages WHERE workspace_id=? AND id=? AND direction='in' AND purged IS NULL",
            params![viewer.ws,id], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
        ).optional()?;
        let row = row.ok_or_else(|| ApiError::not_found("source message"))?;
        spaces::own(c, viewer, &row.0)?;
        Some(row)
    } else {
        None
    };
    // A reply stays in its source, even when a learned/manual preference differs. Switching needs
    // an explicit space id. Preferences only choose a new conversation for a person.
    let id = if let Some(id) = &i.space_id {
        id.clone()
    } else if let Some((id, _, _)) = &source {
        id.clone()
    } else if let Some(p) = &i.party_id {
        let pref:Option<(Option<String>,Option<String>)>=c.query_row(
                "SELECT manual_space_id,learned_space_id FROM output_channel_preferences WHERE workspace_id=? AND user_id=? AND party_id=?",
                params![viewer.ws,viewer.user,p],|r|Ok((r.get(0)?,r.get(1)?)),
            ).optional()?;
        if let Some((Some(manual), _)) = pref {
            manual
        } else {
            let learned = pref
                .and_then(|(_, l)| l)
                .filter(|id| reachable(c, viewer, id, i.identity, Some(p)).is_ok());
            if let Some(id) = learned {
                id
            } else {
                let mut st=c.prepare("SELECT DISTINCT s.id FROM spaces s JOIN space_members m ON m.space_id=s.id JOIN accounts a ON a.id=m.account_id WHERE s.workspace_id=? AND s.kind='dm' AND a.party_id=? ORDER BY s.id")?;
                let ids = st
                    .query_map(params![viewer.ws, p], |r| r.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                ids.into_iter()
                    .find(|id| reachable(c, viewer, id, i.identity, Some(p)).is_ok())
                    .ok_or_else(|| ApiError::not_found("reachable recipient"))?
            }
        }
    } else {
        return Err(ApiError::invalid(
            "intent needs a source, conversation or person",
        ));
    };
    let mut t = reachable(c, viewer, &id, i.identity, i.party_id.as_deref())?;
    if let Some((source_space, external, thread)) = source {
        if source_space == id {
            t.reply_to = crate::fallback::platform(external);
            t.thread_key = thread;
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
        if let Some(d) = ch.store().reader(|c| existing(c, &e))? {
            return Ok(d);
        }
        let viewer = ch.modules().viewer(who)?;
        let target = ch.store().reader(|c| {
            require_requester(c, who)?;
            route(c, &viewer, &e.intent)
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
        };
        self.deliver_enveloped(ch, &req, Some(&e)).await?;
        ch.store()
            .reader(|c| existing(c, &e))?
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
        let source = ch.store().reader(|c| {
            require_requester(c, who)?;
            crate::outbound::source_message(c, &who.ws, item)
        })?;
        let Some(source) = source else {
            return Ok(None);
        };
        let viewer = ch.modules().viewer(who)?;
        let mode = ch.store().reader(|c| -> ChannelResult<_> {
            let source_space: String = c.query_row(
                "SELECT space_id FROM chan_messages WHERE workspace_id=? AND id=?",
                params![who.ws, source],
                |r| r.get(0),
            )?;
            let s = spaces::own(c, &viewer, space.as_deref().unwrap_or(&source_space))?;
            Ok(bindings::connection_account(c, &who.ws, &s.connection_id)?.1)
        })?;
        self.submit(
            ch,
            who,
            DeliveryIntent {
                key: key.into(),
                identity: if mode == Mode::Bot {
                    Identity::Bot
                } else {
                    Identity::AsYou
                },
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
    ch.store().transaction(|tx| {
        require_requester(tx,who)?;
        steve_modules::accounts::service::own_owner(tx,&viewer,party)?;
        if let Some(space)=space {
            let s=spaces::own(tx,&viewer,space)?;
            let (_,mode)=bindings::connection_account(tx,&who.ws,&s.connection_id)?;
            reachable(tx,&viewer,space,if mode==Mode::Bot {Identity::Bot}else{Identity::AsYou},Some(party))?;
            if s.kind!="dm" {return Err(ApiError::invalid("preference must be a direct conversation"));}
        }
        tx.execute("INSERT INTO output_channel_preferences(workspace_id,user_id,party_id,manual_space_id,updated) VALUES(?,?,?,?,?) ON CONFLICT(workspace_id,user_id,party_id) DO UPDATE SET manual_space_id=excluded.manual_space_id,updated=excluded.updated",params![who.ws,who.me,party,space,ch.now()])?;
        Ok(())
    })
}

/// A live inbound reply, on the exact conversation and provider message, is a receipt and a weak
/// channel preference observation. History import, guessed identities and unrelated messages do
/// not teach preferences. An observation never changes `manual_space_id`.
pub(crate) fn observe_reply(
    tx: &Tx,
    ws: &str,
    space: &str,
    reply_to: Option<&str>,
    party: Option<&str>,
    message: &str,
    now: i64,
) -> ChannelResult<()> {
    let (Some(reply), Some(party)) = (reply_to, party) else {
        return Ok(());
    };
    let mut st = tx.prepare(
        "SELECT d.id,d.requester FROM output_deliveries d JOIN chan_messages m ON m.id=d.message_id
         JOIN workspace_members w ON w.workspace_id=d.workspace_id AND w.user_id=d.requester
         WHERE d.workspace_id=? AND d.space_id=? AND m.external_id=? AND m.status='sent'",
    )?;
    let rows = st
        .query_map(params![ws, space, reply], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (id, user) in rows {
        tx.execute("UPDATE output_deliveries SET replied_at=coalesce(replied_at,?),reply_message_id=coalesce(reply_message_id,?) WHERE id=?",params![now,message,id])?;
        let dm: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM spaces WHERE workspace_id=? AND id=? AND kind='dm')",
            params![ws, space],
            |r| r.get(0),
        )?;
        if dm {
            tx.execute("INSERT INTO output_channel_preferences(workspace_id,user_id,party_id,learned_space_id,updated) VALUES(?,?,?,?,?) ON CONFLICT(workspace_id,user_id,party_id) DO UPDATE SET learned_space_id=excluded.learned_space_id,updated=excluded.updated",params![ws,user,party,space,now])?;
        }
        steve_store::base::audit(
            tx,
            ws,
            &user,
            "output.delivery.replied",
            &id,
            &json!({"message":message,"space":space}),
        )?;
    }
    Ok(())
}

/// Only the original live requester can read an intent (shared bot conversations do not expose
/// another person's delivery reasons or content). State is joined from the authoritative outbox.
pub fn get(ch: &Channels, who: &Caller, id: &str) -> ChannelResult<Delivery> {
    ch.store().reader(|c| {
        require_requester(c,who)?;
        let key:Option<String>=c.query_row("SELECT idempotency_key FROM output_deliveries WHERE id=? AND workspace_id=? AND requester=?",params![id,who.ws,who.me],|r|r.get(0)).optional()?;
        let key=key.ok_or_else(||ApiError::not_found("delivery"))?;
        lookup(c,who,&key)?.map(|(_,d)|d).ok_or_else(||ApiError::not_found("delivery"))
    })
}
