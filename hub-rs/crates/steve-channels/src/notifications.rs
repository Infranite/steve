//! Personal notification intake, not another sender. Durable facts and inbox rows commit with the
//! outbox receipt; recovery submits the same event/member key to E1, whose C6 ledger owns every send.
//! No owner inference: a real member explicitly opts in and names their user-linked party.

use crate::{ChannelResult, Channels, Outbox};
use chrono::Timelike;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use steve_modules::outbound::{Content, DeliveryIntent, Identity};
use steve_modules::{ApiError, Caller};
use steve_store::outbox::{Applied, ConsumeError, Consumer, Event, Since};
use steve_store::{Tx, uid};

pub const PREFIX: &str = "notification:";
const KEEP_MS: i64 = 30 * 86_400_000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub enabled: bool,
    pub mode: Mode,
    pub party_id: Option<String>,
    /// Explicit conversation overrides E1's manual/learned/default bot DM routing.
    pub space_id: Option<String>,
    pub timezone: String,
    /// Minutes since local midnight, [start,end); start > end crosses midnight.
    pub quiet_start: Option<u16>,
    pub quiet_end: Option<u16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Now,
    Digest,
    Log,
}
impl Mode {
    fn code(self) -> &'static str {
        match self {
            Self::Now => "now",
            Self::Digest => "digest",
            Self::Log => "log",
        }
    }
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: Mode::Log,
            party_id: None,
            space_id: None,
            timezone: "UTC".into(),
            quiet_start: None,
            quiet_end: None,
        }
    }
}
impl Policy {
    fn validate(&self) -> ChannelResult<()> {
        self.timezone
            .parse::<chrono_tz::Tz>()
            .map_err(|_| ApiError::invalid("IANA timezone"))?;
        match (self.quiet_start, self.quiet_end) {
            (None, None) => {}
            (Some(s), Some(e)) if s < 1440 && e < 1440 && s != e => {}
            _ => {
                return Err(ApiError::invalid(
                    "quiet hours need distinct start/end minutes in 0..1440",
                ));
            }
        }
        Ok(())
    }
    pub fn quiet(&self, now: i64) -> ChannelResult<bool> {
        let tz = self
            .timezone
            .parse::<chrono_tz::Tz>()
            .map_err(|_| ApiError::invalid("IANA timezone"))?;
        let at = chrono::DateTime::from_timestamp_millis(now)
            .ok_or_else(|| ApiError::invalid("timestamp"))?
            .with_timezone(&tz);
        let minute = (at.hour() * 60 + at.minute()) as u16;
        Ok(match (self.quiet_start, self.quiet_end) {
            (Some(s), Some(e)) if s > e => minute >= s || minute < e,
            (Some(s), Some(e)) => minute >= s && minute < e,
            _ => false,
        })
    }
}

fn load(c: &Connection, who: &Caller) -> ChannelResult<Option<Policy>> {
    let row:Option<(bool,String,Option<String>,Option<String>,String,Option<u16>,Option<u16>)>=c.query_row(
        "SELECT enabled,mode,party_id,space_id,timezone,quiet_start,quiet_end FROM notification_policies WHERE workspace_id=? AND user_id=?",
        params![who.ws,who.me],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?)),
    ).optional()?;
    row.map(
        |(enabled, mode, party_id, space_id, timezone, quiet_start, quiet_end)| {
            Ok(Policy {
                enabled,
                mode: serde_json::from_value(serde_json::Value::String(mode))?,
                party_id,
                space_id,
                timezone,
                quiet_start,
                quiet_end,
            })
        },
    )
    .transpose()
}

fn require_person(c: &Connection, who: &Caller) -> ChannelResult<()> {
    if who.me.starts_with("svc:") {
        return Err(ApiError::Unauthenticated);
    }
    crate::pipeline::require_requester(c, who)
}

/// Stronger than contact ownership: this party must be the authenticated member themselves.
fn self_party(c: &Connection, who: &Caller, party: &str) -> ChannelResult<()> {
    require_person(c, who)?;
    let own:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM parties WHERE id=? AND workspace_id=? AND user_id=? AND status='active')",params![party,who.ws,who.me],|r|r.get(0))?;
    if !own {
        return Err(ApiError::Unauthenticated);
    }
    Ok(())
}

fn intent(p: &Policy, key: &str, body: &str) -> DeliveryIntent {
    DeliveryIntent {
        key: key.into(),
        identity: Identity::Bot,
        content: Content {
            markdown: body.into(),
            ..Default::default()
        },
        source_message_id: None,
        space_id: p.space_id.clone(),
        party_id: p.party_id.clone(),
        reason: "Personal work.item.done notification (explicit member policy)".into(),
        urgency: "normal".into(),
    }
}

/// Validates E1's chosen DM, not an arbitrary reachable group or a service-owned party.
fn self_route(c: &Connection, who: &Caller, i: &DeliveryIntent) -> ChannelResult<()> {
    let p = i
        .party_id
        .as_deref()
        .ok_or_else(|| ApiError::invalid("notification needs your user-linked party"))?;
    self_party(c, who, p)?;
    let viewer = steve_store::base::Viewer::load(c, &who.ws, &who.me)?;
    let t = crate::pipeline::route(c, &viewer, i)?;
    self_dm(c, who, p, &t.space_id)
}

fn self_dm(c: &Connection, who: &Caller, p: &str, space: &str) -> ChannelResult<()> {
    self_party(c, who, p)?;
    let dm:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM spaces s WHERE s.id=? AND s.workspace_id=? AND s.kind='dm' AND NOT EXISTS(
        SELECT 1 FROM space_members sm JOIN accounts a ON a.id=sm.account_id
        WHERE sm.space_id=s.id AND a.party_id<>? AND a.id<>(SELECT account_id FROM connections WHERE id=s.connection_id)))",
        params![space,who.ws,p],|r|r.get(0))?;
    if !dm {
        return Err(ApiError::invalid(
            "notification route must be a DM with yourself and the bot only",
        ));
    }
    Ok(())
}

pub fn get_policy(ch: &Channels, who: &Caller) -> ChannelResult<Policy> {
    ch.store().reader(|c| {
        require_person(c, who)?;
        Ok(load(c, who)?.unwrap_or_default())
    })
}
pub fn put_policy(ch: &Channels, who: &Caller, p: &Policy) -> ChannelResult<()> {
    p.validate()?;
    ch.store().transaction(|tx| {
        require_person(tx,who)?;
        if p.enabled {
            if let Some(party)=&p.party_id { self_party(tx,who,party)?; }
            if p.mode==Mode::Now { self_route(tx,who,&intent(p,"validation","validation"))?; }
        }
        let now=ch.now();
        tx.execute("INSERT INTO notification_policies(workspace_id,user_id,enabled,mode,party_id,space_id,timezone,quiet_start,quiet_end,consented_at,consent_since_seq,updated)
            VALUES(?,?,?,?,?,?,?,?,?,?,(SELECT coalesce(max(seq),0) FROM outbox_events),?) ON CONFLICT(workspace_id,user_id) DO UPDATE SET enabled=excluded.enabled,mode=excluded.mode,party_id=excluded.party_id,space_id=excluded.space_id,
            timezone=excluded.timezone,quiet_start=excluded.quiet_start,quiet_end=excluded.quiet_end,
            consented_at=CASE WHEN notification_policies.enabled=0 AND excluded.enabled=1 THEN excluded.consented_at ELSE notification_policies.consented_at END,
            consent_since_seq=CASE WHEN notification_policies.enabled=0 AND excluded.enabled=1 THEN excluded.consent_since_seq ELSE notification_policies.consent_since_seq END,updated=excluded.updated",
            params![who.ws,who.me,p.enabled,p.mode.code(),p.party_id,p.space_id,p.timezone,p.quiet_start,p.quiet_end,now,now])?;
        // A disabled policy never resurrects an old pending notification on a later opt-in.
        if !p.enabled { tx.execute("UPDATE personal_notifications SET state='cancelled' WHERE workspace_id=? AND user_id=? AND state IN ('waiting','held','submitted')",params![who.ws,who.me])?; }
        if p.mode!=Mode::Now { tx.execute("UPDATE personal_notifications SET state='held',mode=? WHERE workspace_id=? AND user_id=? AND state='waiting'",params![p.mode.code(),who.ws,who.me])?; }
        steve_store::base::audit(tx,&who.ws,&who.me,"notification.policy.updated",&who.me,&serde_json::to_value(p)?)?;
        Ok(())
    })
}

pub struct Intake;
impl Consumer for Intake {
    fn id(&self) -> &'static str {
        "notifications.work_done"
    }
    fn topic(&self) -> &'static str {
        steve_work_api::ITEM_DONE
    }
    fn since(&self) -> Since {
        Since::Start
    }
    fn apply(&self, tx: &Tx, ev: &Event) -> Result<Applied, ConsumeError> {
        if ev.version != steve_work_api::ITEM_DONE_V {
            return Err(ConsumeError::Reject(
                "unknown work.item.done version".into(),
            ));
        }
        let d: steve_work_api::ItemDone = serde_json::from_value(ev.payload.clone())
            .map_err(|e| ConsumeError::Reject(e.to_string()))?;
        let apply = || -> ChannelResult<()> {
            // No receipt, owner, bot or unique-member heuristic grants personal notification consent.
            let mut st=tx.prepare("SELECT p.user_id FROM notification_policies p JOIN workspace_members m ON m.workspace_id=p.workspace_id AND m.user_id=p.user_id WHERE p.workspace_id=? AND p.enabled=1 AND p.user_id NOT LIKE 'svc:%' AND p.consented_at<=? AND p.consent_since_seq<?")?;
            let users = st
                .query_map(params![ev.workspace_id, ev.created, ev.seq], |r| {
                    r.get::<_, String>(0)
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let body = format!(
                "Work item completed: {}\nItem: {}\nProject: {}\nCycle: {} · Revision: {}",
                d.title,
                d.item_id,
                d.project_id.as_deref().unwrap_or("—"),
                d.cycle.map(|v| v.to_string()).unwrap_or_else(|| "—".into()),
                d.revision
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "—".into())
            );
            for user in users {
                let who = Caller {
                    ws: ev.workspace_id.clone(),
                    me: user,
                };
                let p = load(tx, &who)?.expect("policy selected in same transaction");
                let state = if p.mode == Mode::Now {
                    "waiting"
                } else {
                    "held"
                };
                tx.execute("INSERT INTO personal_notifications(id,workspace_id,user_id,event_id,delivery_key,mode,body,state,created) VALUES(?,?,?,?,?,?,?,?,?) ON CONFLICT(workspace_id,user_id,event_id) DO NOTHING",
                    params![uid("ntf"),who.ws,who.me,ev.id,format!("{PREFIX}{}:{}",ev.id,who.me),p.mode.code(),body,state,ev.created])?;
            }
            Ok(())
        };
        apply().map_err(|e| ConsumeError::Retry(e.to_string()))?;
        Ok(Applied::noted("personal notification policy recorded"))
    }
}

/// Before E1 commits or retries, revalidate the stored personal consent, exact content and self DM.
/// Reserved keys cannot be used by a caller to create an untracked notification.
pub(crate) fn verify_intent(
    c: &Connection,
    who: &Caller,
    i: &DeliveryIntent,
    now: i64,
) -> ChannelResult<()> {
    if !i.key.starts_with(PREFIX) {
        return Ok(());
    }
    let row:Option<(Option<String>,String)>=c.query_row("SELECT body,state FROM personal_notifications WHERE workspace_id=? AND user_id=? AND delivery_key=?",params![who.ws,who.me,i.key],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    let Some((Some(body), state)) = row else {
        return Err(ApiError::Unauthenticated);
    };
    let p = load(c, who)?.ok_or(ApiError::Unauthenticated)?;
    if !p.enabled
        || p.mode != Mode::Now
        || !matches!(state.as_str(), "waiting" | "submitted")
        || p.quiet(now)?
        || *i != intent(&p, &i.key, &body)
    {
        return Err(ApiError::Unauthenticated);
    }
    self_route(c, who, i)
}

/// Queue checks the immutable destination, not a freshly resolved personal preference.
pub(crate) fn verify_pinned(
    c: &Connection,
    who: &Caller,
    i: &DeliveryIntent,
    space: &str,
) -> ChannelResult<()> {
    if i.key.starts_with(PREFIX) {
        self_dm(
            c,
            who,
            i.party_id.as_deref().ok_or(ApiError::Unauthenticated)?,
            space,
        )?;
    }
    Ok(())
}

pub(crate) fn attach(
    tx: &Tx,
    who: &Caller,
    i: &DeliveryIntent,
    delivery: &str,
) -> ChannelResult<()> {
    if i.key.starts_with(PREFIX) {
        tx.execute("UPDATE personal_notifications SET delivery_id=?,state='submitted' WHERE workspace_id=? AND user_id=? AND delivery_key=?",params![delivery,who.ws,who.me,i.key])?;
    }
    Ok(())
}

/// DND is a postponement, never a failed send or a reason to bypass the normal C6 authorization.
/// Digest/log changes also park already queued sends; disabling is handled by verify_intent.
pub(crate) fn deferred(c: &Connection, message: &str, now: i64) -> ChannelResult<bool> {
    let who:Option<(String,String)>=c.query_row("SELECT workspace_id,user_id FROM personal_notifications WHERE delivery_id=(SELECT id FROM output_deliveries WHERE message_id=?)",[message],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    let Some((ws, me)) = who else {
        return Ok(false);
    };
    let Some(p) = load(c, &Caller { ws, me })? else {
        return Ok(false);
    };
    Ok(p.enabled && (p.mode != Mode::Now || p.quiet(now)?))
}

#[derive(Serialize)]
pub struct Notification {
    pub id: String,
    pub event_id: String,
    pub mode: String,
    pub body: Option<String>,
    pub state: String,
    pub delivery_id: Option<String>,
    pub seen_at: Option<i64>,
    pub created: i64,
}
pub fn list(ch: &Channels, who: &Caller) -> ChannelResult<Vec<Notification>> {
    ch.store().reader(|c| {
        require_person(c,who)?;
        let mut st=c.prepare("SELECT id,event_id,mode,body,state,delivery_id,seen_at,created FROM personal_notifications WHERE workspace_id=? AND user_id=? ORDER BY created DESC,id DESC LIMIT 100")?;
        Ok(st.query_map(params![who.ws,who.me],|r|Ok(Notification{id:r.get(0)?,event_id:r.get(1)?,mode:r.get(2)?,body:r.get(3)?,state:r.get(4)?,delivery_id:r.get(5)?,seen_at:r.get(6)?,created:r.get(7)?}))?.collect::<rusqlite::Result<Vec<_>>>()?)
    })
}
pub fn seen(ch: &Channels, who: &Caller, id: &str) -> ChannelResult<()> {
    ch.store().transaction(|tx| {
        require_person(tx,who)?;
        if tx.execute("UPDATE personal_notifications SET seen_at=coalesce(seen_at,?) WHERE id=? AND workspace_id=? AND user_id=?",params![ch.now(),id,who.ws,who.me])? == 0 { return Err(ApiError::not_found("notification")); }
        Ok(())
    })
}

/// A bounded recovery pass. Only unsubmitted `now` entries can create an E1 delivery. Held digest
/// and log records never turn into individual sends on a later preference change.
pub async fn pump(ch: &Channels, outbox: &Outbox) -> ChannelResult<usize> {
    let now = ch.now();
    let rows=ch.store().reader(|c|->ChannelResult<Vec<(String,String,String,String)>> {
        let mut st=c.prepare("SELECT workspace_id,user_id,delivery_key,body FROM personal_notifications WHERE state='waiting' AND delivery_id IS NULL AND body IS NOT NULL AND next_attempt_at<=? AND created>=? ORDER BY next_attempt_at,created LIMIT 50")?;
        Ok(st.query_map([now,now-KEEP_MS],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?.collect::<rusqlite::Result<Vec<_>>>()?)
    })?;
    let mut n = 0;
    for (ws, me, key, body) in rows {
        // Round-robin due work even when the first member is quiet or has a broken route.
        ch.store().transaction(|tx|->ChannelResult<()> { tx.execute("UPDATE personal_notifications SET next_attempt_at=? WHERE delivery_key=? AND state='waiting'",params![now+10_000,key])?;Ok(()) })?;
        let who = Caller { ws, me };
        let p = ch.store().reader(|c| -> ChannelResult<Option<Policy>> {
            if require_person(c, &who).is_err() {
                return Ok(None);
            }
            load(c, &who)
        })?;
        let Some(p) = p.filter(|p| p.enabled) else {
            ch.store().transaction(|tx|->ChannelResult<()> {tx.execute("UPDATE personal_notifications SET state='cancelled' WHERE delivery_key=? AND state='waiting'",[&key])?;Ok(())})?;
            continue;
        };
        if p.mode != Mode::Now || p.quiet(now)? {
            continue;
        }
        let i = intent(&p, &key, &body);
        if let Err(e) = outbox.submit(ch, &who, i).await {
            tracing::warn!(key, "personal notification pending: {e}");
        } else {
            n += 1;
        }
    }
    // Inbox body retention is independent of source outbox/transport retention; tombstone keys and
    // delivery receipts stay so pruning never turns a replay into another external send.
    ch.store().transaction(|tx|->ChannelResult<()> {
        tx.execute("UPDATE personal_notifications SET body=NULL,state='purged' WHERE id IN (SELECT id FROM personal_notifications WHERE created<? AND body IS NOT NULL LIMIT 1000)",[now-KEEP_MS])?;
        Ok(())
    })?;
    Ok(n)
}

pub async fn run(ch: Channels, outbox: Arc<Outbox>) {
    loop {
        if let Err(e) = pump(&ch, &outbox).await {
            tracing::error!("personal notification recovery: {e}");
        }
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
    }
}
