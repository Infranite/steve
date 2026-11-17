//! Personal notification intake, not another sender. Durable facts and inbox rows commit with the
//! outbox receipt; recovery submits the same event/member key to E1, whose C6 ledger owns every send.
//! No owner inference: a real member explicitly opts in and names their user-linked party.
//!
//! Each notification has one topic ([`Topic`], `personal_notifications.topic`), and the topic alone decides
//! how it is sent and which feed kind it is:
//! - a work item done (the `work.item.done` event, [`Intake`], for the members who opted in);
//! - an alarm a member set themselves ([`AlarmIntake`]: a todo's reminder `todo.due`, a snoozed todo back
//!   `snooze.wake`; steve-flow [`steve_flow::alarms::TOPICS`]). It is for its owner only and always lands in
//!   their feed; it is sent to their own DM only under a policy that sends now. The alarm clock already held it
//!   through the quiet hours, and several that came due together are one notification;
//! - a starred contact's message to you on your own account (`contact.starred`, [`queue_starred`], from
//!   [`crate::starred`]). It is the urgent one: it goes out now under `digest` too, to the private chat you chose
//!   for it (no route is looked up for you under `digest`: none chosen keeps it in the inbox, as `log` does;
//!   [`Topic::sends`]), and within your quiet hours when you allowed it (`starred_in_quiet`). Whether you
//!   answered it yourself is asked again before it is submitted and before every attempt: once you did, it is
//!   cancelled and nothing goes out ([`answered`]).
//!
//! `personal_notifications.kind` (migration 101) is legacy: migration 102 folded it into `topic`, and it is never
//! written again.

use crate::{ChannelResult, Channels, Outbox};
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
    /// A starred contact's message is sent within the quiet hours too (off by default).
    #[serde(default)]
    pub starred_in_quiet: bool,
}

/// What a personal notification is about (`personal_notifications.topic`). The topic alone decides how it
/// is sent ([`Topic::sends`], [`Topic::quiet_allowed`], [`Topic::urgency`], [`Topic::reason`],
/// [`Topic::checks_answered`]) and the feed entry's kind ([`Topic::feed_kind`]; migration 102's insert trigger
/// maps the topics the same way). Every match on it is exhaustive: a new topic must state each rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Topic {
    /// A work item was done (`work.item.done`).
    ItemDone,
    /// A todo's reminder came due (`todo.due`).
    TodoDue,
    /// A snoozed todo is back (`snooze.wake`).
    SnoozeWake,
    /// A starred contact wrote to you on your own account (`contact.starred`).
    ContactStarred,
}

/// The topic of a starred contact's message to you.
pub const CONTACT_STARRED: &str = "contact.starred";

impl Topic {
    pub const ALL: [Topic; 4] = [
        Topic::ItemDone,
        Topic::TodoDue,
        Topic::SnoozeWake,
        Topic::ContactStarred,
    ];
    /// `personal_notifications.topic`.
    pub fn code(self) -> &'static str {
        match self {
            Self::ItemDone => steve_work_api::ITEM_DONE,
            Self::TodoDue => steve_flow::alarms::TODO_DUE,
            Self::SnoozeWake => steve_flow::alarms::SNOOZE_WAKE,
            Self::ContactStarred => CONTACT_STARRED,
        }
    }
    /// The topic stored as `s`; `None` for one this Hub does not know (the column's CHECK refuses those).
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.code() == s)
    }
    /// `feed_entries.kind` of its feed entry.
    pub fn feed_kind(self) -> &'static str {
        match self {
            Self::ItemDone => "completion",
            Self::TodoDue | Self::SnoozeWake => "reminder",
            Self::ContactStarred => "starred",
        }
    }
    /// Whether a notification of this topic goes out under policy `p` (not only into the inbox):
    /// everything under `now`; under `digest` a starred contact's message only, and only to the
    /// private chat you chose for it (`space_id`) — no route is looked up for you there.
    pub fn sends(self, p: &Policy) -> bool {
        match p.mode {
            Mode::Now => true,
            Mode::Digest => match self {
                Self::ContactStarred => p.space_id.is_some(),
                Self::ItemDone | Self::TodoDue | Self::SnoozeWake => false,
            },
            Mode::Log => false,
        }
    }
    /// Whether it may go out within the quiet hours under policy `p`: a starred contact's message when you
    /// allowed it (`starred_in_quiet`). An alarm was already held through them by the alarm clock.
    pub fn quiet_allowed(self, p: &Policy) -> bool {
        match self {
            Self::ContactStarred => p.starred_in_quiet,
            Self::ItemDone | Self::TodoDue | Self::SnoozeWake => false,
        }
    }
    /// The delivery's urgency.
    pub fn urgency(self) -> &'static str {
        match self {
            Self::ContactStarred => "high",
            Self::ItemDone | Self::TodoDue | Self::SnoozeWake => "normal",
        }
    }
    /// Why it goes out (the delivery's stated reason).
    pub fn reason(self) -> &'static str {
        match self {
            Self::ItemDone => "Personal work.item.done notification (explicit member policy)",
            Self::TodoDue => "Personal todo reminder (set by the member)",
            Self::SnoozeWake => "Personal snoozed todo is back (set by the member)",
            Self::ContactStarred => "A starred contact's message to you (explicit member policy)",
        }
    }
    /// Whether it is asked again, before it is submitted and before every attempt, whether it is still wanted
    /// ([`unwanted`]: you answered the message yourself, or set its space aside).
    pub fn checks_answered(self) -> bool {
        match self {
            Self::ContactStarred => true,
            Self::ItemDone | Self::TodoDue | Self::SnoozeWake => false,
        }
    }
}

/// The topic stored as `s`, or an error naming it.
fn topic_of(s: &str) -> ChannelResult<Topic> {
    Topic::parse(s).ok_or_else(|| ApiError::invalid("personal notification topic"))
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
            starred_in_quiet: false,
        }
    }
}
impl Policy {
    /// The quiet hours hold a notification of `topic` at `now`.
    pub fn holds(&self, topic: Topic, now: i64) -> ChannelResult<bool> {
        Ok(self.quiet(now)? && !topic.quiet_allowed(self))
    }
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
        chrono::DateTime::from_timestamp_millis(now)
            .ok_or_else(|| ApiError::invalid("timestamp"))?;
        Ok(
            steve_modules::zone::Quiet::from_parts(self.quiet_start, self.quiet_end)
                .is_some_and(|q| q.contains(tz, now)),
        )
    }
}

fn load(c: &Connection, who: &Caller) -> ChannelResult<Option<Policy>> {
    type Row = (
        bool,
        String,
        Option<String>,
        Option<String>,
        String,
        Option<u16>,
        Option<u16>,
        bool,
    );
    let row:Option<Row>=c.query_row(
        "SELECT enabled,mode,party_id,space_id,timezone,quiet_start,quiet_end,starred_in_quiet FROM notification_policies WHERE workspace_id=? AND user_id=?",
        params![who.ws,who.me],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?)),
    ).optional()?;
    row.map(
        |(
            enabled,
            mode,
            party_id,
            space_id,
            timezone,
            quiet_start,
            quiet_end,
            starred_in_quiet,
        )| {
            Ok(Policy {
                enabled,
                mode: serde_json::from_value(serde_json::Value::String(mode))?,
                party_id,
                space_id,
                timezone,
                quiet_start,
                quiet_end,
                starred_in_quiet,
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

fn intent(p: &Policy, key: &str, body: &str, topic: Topic) -> DeliveryIntent {
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
        reason: topic.reason().into(),
        urgency: topic.urgency().into(),
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

/// A selectable explicit bot DM, not a contact/owner guess or a promise of later delivery.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Target {
    pub party_id: String,
    pub space_id: String,
    pub party_name: String,
    pub space_name: String,
    /// Public channel system identifier, never account handles or connection credentials.
    pub channel: String,
}

/// Read only the caller's linked parties and their recorded DM membership. In particular, do not
/// infer a counterpart from message history, cached member_count, ownership or a sole member.
pub fn targets(ch: &Channels, who: &Caller) -> ChannelResult<Vec<Target>> {
    ch.store().reader(|c| {
        require_person(c, who)?;
        let viewer = steve_store::base::Viewer::load(c, &who.ws, &who.me)?;
        let mut st = c.prepare_cached(
            "SELECT DISTINCT p.id,s.id,p.name,s.name,s.system_id
             FROM parties p JOIN accounts a ON a.party_id=p.id AND a.workspace_id=p.workspace_id
             JOIN space_members m ON m.account_id=a.id AND m.workspace_id=p.workspace_id
             JOIN spaces s ON s.id=m.space_id AND s.workspace_id=p.workspace_id
             WHERE p.workspace_id=? AND p.user_id=? AND p.status='active'
               AND coalesce(p.contact_kind,'')<>'bot' AND p.type<>'bot'
               AND a.status='active' AND a.kind NOT IN ('bot','app_bot') AND s.kind='dm'
             ORDER BY p.id,s.id",
        )?;
        let candidates = st
            .query_map(params![who.ws, who.me], |r| {
                Ok(Target {
                    party_id: r.get(0)?,
                    space_id: r.get(1)?,
                    party_name: r.get(2)?,
                    space_name: r.get(3)?,
                    channel: r.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut targets = Vec::new();
        for target in candidates {
            let p = Policy {
                party_id: Some(target.party_id.clone()),
                space_id: Some(target.space_id.clone()),
                ..Policy::default()
            };
            // Pin the explicit bot route through E1's live visibility/account/identity checks.
            // Internal failures must not masquerade as an empty picker.
            let route = self_party(c, who, &target.party_id)
                .and_then(|_| {
                    crate::pipeline::route(
                        c,
                        &viewer,
                        &intent(&p, "validation", "validation", Topic::ItemDone),
                    )
                })
                .and_then(|route| {
                    self_dm(c, who, &target.party_id, &route.space_id)?;
                    Ok(route)
                });
            let route = match route {
                Ok(route) => route,
                Err(e @ ApiError::Internal(_)) => return Err(e),
                Err(_) => continue,
            };
            // E1 selects the effective voice, which need not be spaces.connection_id. A bot-mode
            // override cannot turn someone's self account (or a disabled account) into a bot.
            // The sender may be implicit in a provider's roster; every recorded member must still
            // resolve to an active, same-workspace self account or that exact sending bot account.
            let valid: bool = c.query_row(
                "SELECT EXISTS(SELECT 1 FROM connections cn
                 JOIN accounts bot ON bot.id=cn.account_id AND bot.workspace_id=cn.workspace_id
                 JOIN parties bp ON bp.id=bot.party_id AND bp.workspace_id=cn.workspace_id
                 WHERE cn.id=?1 AND cn.workspace_id=?2 AND bot.status='active'
                   AND bot.kind IN ('bot','app_bot') AND bp.status='active' AND bp.user_id IS NULL
                   AND bp.id<>?3 AND NOT EXISTS(
                     SELECT 1 FROM space_members sm LEFT JOIN accounts a ON a.id=sm.account_id
                     WHERE sm.space_id=?4 AND (sm.workspace_id<>?2 OR a.id IS NULL
                       OR a.workspace_id<>?2 OR a.status<>'active'
                       OR (a.id<>bot.id AND (a.party_id<>?3 OR a.kind IN ('bot','app_bot'))))))",
                params![
                    route.connection_id,
                    who.ws,
                    target.party_id,
                    target.space_id
                ],
                |r| r.get(0),
            )?;
            if valid {
                targets.push(target);
            }
        }
        Ok(targets)
    })
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
            // what is sent goes to a private chat with yourself and the bot: under digest only the one you chose
            if p.mode==Mode::Now || (p.mode==Mode::Digest && p.space_id.is_some()) { self_route(tx,who,&intent(p,"validation","validation",Topic::ItemDone))?; }
        }
        let now=ch.now();
        tx.execute("INSERT INTO notification_policies(workspace_id,user_id,enabled,mode,party_id,space_id,timezone,quiet_start,quiet_end,starred_in_quiet,consented_at,consent_since_seq,updated)
            VALUES(?,?,?,?,?,?,?,?,?,?,?,(SELECT coalesce(max(seq),0) FROM outbox_events),?) ON CONFLICT(workspace_id,user_id) DO UPDATE SET enabled=excluded.enabled,mode=excluded.mode,party_id=excluded.party_id,space_id=excluded.space_id,
            timezone=excluded.timezone,quiet_start=excluded.quiet_start,quiet_end=excluded.quiet_end,starred_in_quiet=excluded.starred_in_quiet,
            consented_at=CASE WHEN notification_policies.enabled=0 AND excluded.enabled=1 THEN excluded.consented_at ELSE notification_policies.consented_at END,
            consent_since_seq=CASE WHEN notification_policies.enabled=0 AND excluded.enabled=1 THEN excluded.consent_since_seq ELSE notification_policies.consent_since_seq END,updated=excluded.updated",
            params![who.ws,who.me,p.enabled,p.mode.code(),p.party_id,p.space_id,p.timezone,p.quiet_start,p.quiet_end,p.starred_in_quiet,now,now])?;
        // A disabled policy never resurrects an old pending notification on a later opt-in.
        if !p.enabled { tx.execute("UPDATE personal_notifications SET state='cancelled' WHERE workspace_id=? AND user_id=? AND state IN ('waiting','held','submitted')",params![who.ws,who.me])?; }
        // what this policy no longer sends waits in the inbox only (Topic::sends: a starred contact's message
        // still goes out under digest when you chose where)
        if p.mode!=Mode::Now { tx.execute("UPDATE personal_notifications SET state='held',mode=?1 WHERE workspace_id=?2 AND user_id=?3 AND state='waiting' AND (?1<>'digest' OR topic<>?5 OR ?4 IS NULL)",params![p.mode.code(),who.ws,who.me,p.space_id,CONTACT_STARRED])?; }
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
                insert(tx, &who, ev, p.mode, &body, state, Topic::ItemDone)?;
            }
            Ok(())
        };
        apply().map_err(|e| ConsumeError::Retry(e.to_string()))?;
        Ok(Applied::noted("personal notification policy recorded"))
    }
}

/// One inbox row (and, by trigger, its feed entry) for `who` about event `ev`.
fn insert(
    tx: &Tx,
    who: &Caller,
    ev: &Event,
    mode: Mode,
    body: &str,
    state: &str,
    topic: Topic,
) -> ChannelResult<()> {
    tx.execute("INSERT INTO personal_notifications(id,workspace_id,user_id,event_id,delivery_key,mode,body,state,created,topic) VALUES(?,?,?,?,?,?,?,?,?,?) ON CONFLICT(workspace_id,user_id,event_id) DO NOTHING",
        params![uid("ntf"),who.ws,who.me,ev.id,format!("{PREFIX}{}:{}",ev.id,who.me),mode.code(),body,state,ev.created,topic.code()])?;
    Ok(())
}

/// The notification of an alarm topic (`todo.due`, `snooze.wake`; [`steve_flow::alarms::AlarmFired`]):
/// for the alarm's owner only, as long as they are a member. It is sent under a policy that sends now;
/// otherwise (no policy, off, `digest`, `log`) it is kept in their feed only.
pub struct AlarmIntake(&'static steve_flow::alarms::Topic);

impl AlarmIntake {
    /// One consumer per alarm topic.
    pub fn all() -> Vec<Arc<dyn Consumer>> {
        steve_flow::alarms::TOPICS
            .iter()
            .filter(|t| t.alarm.is_some())
            .map(|t| Arc::new(AlarmIntake(t)) as Arc<dyn Consumer>)
            .collect()
    }
}

/// The text of an alarm notification (stored, in zh-CN like the other stored texts): a title line, then
/// one line per todo when there are several, then whether some came late. It is plain text: the same
/// body goes to the feed and, as a Feishu `text` message, to the owner's Bot DM, so it carries no link
/// (a relative `#/…` fragment cannot be opened from a DM). [`steve_flow::alarms::AlarmLine::link`] stays
/// in the event payload only.
pub fn alarm_body(topic: &str, a: &steve_flow::alarms::AlarmFired) -> String {
    steve_i18n::stored(|| {
        let snooze = topic == steve_flow::alarms::SNOOZE_WAKE;
        let mut out = match a.lines.as_slice() {
            [one] => {
                let mut s = if snooze {
                    steve_i18n::t!("channels.notify.snooze_wake_one", text = one.text)
                } else {
                    steve_i18n::t!("channels.notify.todo_due_one", text = one.text)
                };
                if let Some(d) = &one.detail {
                    s.push_str(&format!("\n{d}"));
                }
                s
            }
            many => {
                let mut s = if snooze {
                    steve_i18n::t!("channels.notify.snooze_wake_many", count = many.len())
                } else {
                    steve_i18n::t!("channels.notify.todo_due_many", count = many.len())
                };
                for l in many {
                    s.push_str(&format!("\n- {}", l.text));
                    if let Some(d) = &l.detail {
                        s.push_str(&format!(" · {d}"));
                    }
                }
                s
            }
        };
        if a.late {
            out.push_str(&format!("\n{}", steve_i18n::t!("channels.notify.late")));
        }
        out
    })
}

impl Consumer for AlarmIntake {
    fn id(&self) -> &'static str {
        self.0.consumer
    }
    fn topic(&self) -> &'static str {
        self.0.name
    }
    fn since(&self) -> Since {
        Since::Start
    }
    fn apply(&self, tx: &Tx, ev: &Event) -> Result<Applied, ConsumeError> {
        if ev.version != steve_flow::alarms::ALARM_FIRED_V {
            return Err(ConsumeError::Reject(format!(
                "unknown {} version",
                self.0.name
            )));
        }
        let a: steve_flow::alarms::AlarmFired = serde_json::from_value(ev.payload.clone())
            .map_err(|e| ConsumeError::Reject(e.to_string()))?;
        if a.lines.is_empty() || a.owner.starts_with("svc:") {
            return Ok(Applied::noted("nothing to notify"));
        }
        let apply = || -> ChannelResult<Applied> {
            let member: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM workspace_members WHERE workspace_id=? AND user_id=?)",
                params![ev.workspace_id, a.owner],
                |r| r.get(0),
            )?;
            if !member {
                return Ok(Applied::noted("the alarm's owner is no longer a member"));
            }
            let who = Caller {
                ws: ev.workspace_id.clone(),
                me: a.owner.clone(),
            };
            let (mode, state) = match load(tx, &who)? {
                Some(p) if p.enabled && p.mode == Mode::Now => (Mode::Now, "waiting"),
                Some(p) if p.enabled => (p.mode, "held"),
                _ => (Mode::Log, "held"),
            };
            let topic = Topic::parse(self.0.name)
                .ok_or_else(|| ApiError::invalid("personal notification topic"))?;
            insert(
                tx,
                &who,
                ev,
                mode,
                &alarm_body(self.0.name, &a),
                state,
                topic,
            )?;
            Ok(Applied::noted("personal alarm notification recorded"))
        };
        apply().map_err(|e| ConsumeError::Retry(e.to_string()))
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
    type Row = (Option<String>, String, String, Option<String>);
    let row:Option<Row>=c.query_row("SELECT body,state,topic,message_id FROM personal_notifications WHERE workspace_id=? AND user_id=? AND delivery_key=?",params![who.ws,who.me,i.key],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
    let Some((Some(body), state, topic, message)) = row else {
        return Err(ApiError::Unauthenticated);
    };
    let topic = Topic::parse(&topic).ok_or(ApiError::Unauthenticated)?;
    let p = load(c, who)?.ok_or(ApiError::Unauthenticated)?;
    if !p.enabled
        || !topic.sends(&p)
        || !matches!(state.as_str(), "waiting" | "submitted")
        || p.holds(topic, now)?
        || *i != intent(&p, &i.key, &body, topic)
        || (topic.checks_answered() && unwanted(c, &who.ws, message.as_deref(), now)?.is_some())
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
    let who:Option<(String,String,String)>=c.query_row("SELECT workspace_id,user_id,topic FROM personal_notifications WHERE delivery_id=(SELECT id FROM output_deliveries WHERE message_id=?)",[message],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
    let Some((ws, me, topic)) = who else {
        return Ok(false);
    };
    let topic = topic_of(&topic)?;
    let Some(p) = load(c, &Caller { ws, me })? else {
        return Ok(false);
    };
    Ok(p.enabled && (!topic.sends(&p) || p.holds(topic, now)?))
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
        let mut st=c.prepare("SELECT n.id,n.event_id,n.mode,n.body,n.state,n.delivery_id,r.read_at,n.created FROM personal_notifications n JOIN feed_entries e ON e.notification_id=n.id LEFT JOIN feed_reads r ON r.entry_id=e.id AND r.workspace_id=n.workspace_id AND r.user_id=n.user_id WHERE n.workspace_id=? AND n.user_id=? ORDER BY n.created DESC,n.id DESC LIMIT 100")?;
        Ok(st.query_map(params![who.ws,who.me],|r|Ok(Notification{id:r.get(0)?,event_id:r.get(1)?,mode:r.get(2)?,body:r.get(3)?,state:r.get(4)?,delivery_id:r.get(5)?,seen_at:r.get(6)?,created:r.get(7)?}))?.collect::<rusqlite::Result<Vec<_>>>()?)
    })
}
pub fn seen(ch: &Channels, who: &Caller, id: &str) -> ChannelResult<()> {
    ch.store().transaction(|tx| {
        require_person(tx,who)?;
        let entry: Option<String> = tx.query_row("SELECT id FROM feed_entries WHERE notification_id=? AND workspace_id=? AND recipient_id=?", params![id,who.ws,who.me], |r|r.get(0)).optional()?;
        let entry = entry.ok_or_else(||ApiError::not_found("notification"))?;
        steve_store::feed::read_ids(tx, &who.ws, &who.me, &[entry], ch.now())?;
        Ok(())
    })
}

/// A bounded recovery pass. Only unsubmitted `now` entries can create an E1 delivery. Held digest
/// and log records never turn into individual sends on a later preference change.
pub async fn pump(ch: &Channels, outbox: &Outbox) -> ChannelResult<usize> {
    let now = ch.now();
    type Due = (String, String, String, String, String, Option<String>);
    let rows=ch.store().reader(|c|->ChannelResult<Vec<Due>> {
        let mut st=c.prepare("SELECT workspace_id,user_id,delivery_key,body,topic,message_id FROM personal_notifications WHERE state='waiting' AND delivery_id IS NULL AND body IS NOT NULL AND next_attempt_at<=? AND created>=? ORDER BY next_attempt_at,created LIMIT 50")?;
        Ok(st.query_map([now,now-KEEP_MS],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)))?.collect::<rusqlite::Result<Vec<_>>>()?)
    })?;
    let mut n = 0;
    for (ws, me, key, body, topic, message) in rows {
        let Some(topic) = Topic::parse(&topic) else {
            tracing::warn!(key, topic, "personal notification of an unknown topic left waiting");
            continue;
        };
        // a starred contact's message you answered yourself (or whose space you set aside) meanwhile
        if topic.checks_answered() {
            let why = ch
                .store()
                .reader(|c| unwanted(c, &ws, message.as_deref(), now))?;
            if let Some(why) = why {
                ch.store().transaction(|tx| cancel(tx, &ws, &key, why))?;
                continue;
            }
        }
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
        if !topic.sends(&p) || p.holds(topic, now)? {
            continue;
        }
        let i = intent(&p, &key, &body, topic);
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

// ---------------------------------------------------------------- a starred contact's message

/// Why a starred contact's notification about inbound message `message` is no longer wanted at
/// `now`: you are there for it yourself ([`crate::presence::owner_engaged`]: you answered it, or are
/// chatting there), or its space is set aside (不提醒, 屏蔽, or `reply_as_you = off`). `None`: still
/// wanted. A message that is gone is not wanted.
pub(crate) fn unwanted(
    c: &Connection,
    ws: &str,
    message: Option<&str>,
    now: i64,
) -> ChannelResult<Option<&'static str>> {
    let Some(message) = message else {
        return Ok(Some(GONE));
    };
    let space: Option<String> = c
        .query_row(
            "SELECT space_id FROM chan_messages WHERE id = ? AND workspace_id = ? AND direction = 'in'",
            params![message, ws],
            |r| r.get(0),
        )
        .optional()?;
    let Some(space) = space else {
        return Ok(Some(GONE));
    };
    if crate::presence::owner_engaged_chat(c, ws, message, now)?.is_some() {
        return Ok(Some(crate::presence::REASON_REPLIED));
    }
    if crate::starred::set_aside_chat(c, ws, &space)? {
        return Ok(Some(crate::presence::REASON_SETTING));
    }
    Ok(None)
}

/// `audit.why` of a notification whose message is no longer stored.
const GONE: &str = "message_gone";

fn cancel(tx: &Tx, ws: &str, key: &str, why: &str) -> ChannelResult<bool> {
    let n = tx.execute(
        "UPDATE personal_notifications SET state='cancelled' WHERE delivery_key=? AND state IN ('waiting','held','submitted')",
        [key],
    )?;
    if n > 0 {
        steve_store::base::audit(
            tx,
            ws,
            "steve",
            "notification.starred.cancelled",
            key,
            &serde_json::json!({ "why": why }),
        )?;
    }
    Ok(n > 0)
}

/// What became of a starred contact's message as a notification ([`queue_starred`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Queued {
    /// In your inbox (`ntf_…`), and to be sent now (`true`) or kept there (`log`: `false`).
    Yes { id: String, sends: bool },
    /// The same sender's earlier message in that space is notified already and you have not spoken
    /// there since.
    Again,
    /// You have no personal notification policy turned on: nothing is queued.
    Off,
}

/// Queues the notification of user `who` about starred contact's inbound message `message`, `body`
/// its text ([`crate::starred`] decided it is one). One per sender and space while you have not
/// spoken there since the last one, within one stretch of talk ([`crate::context::SEGMENT_GAP_MS`]).
pub(crate) fn queue_starred(
    tx: &Tx,
    who: &Caller,
    message: &str,
    body: &str,
    now: i64,
) -> ChannelResult<Queued> {
    if who.me.starts_with("svc:") {
        return Ok(Queued::Off);
    }
    let member: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM workspace_members WHERE workspace_id=? AND user_id=?)",
        params![who.ws, who.me],
        |r| r.get(0),
    )?;
    let Some(p) = load(tx, who)?.filter(|p| p.enabled && member) else {
        return Ok(Queued::Off);
    };
    let (space, sender): (String, Option<String>) = tx.query_row(
        "SELECT space_id, coalesce(sender_party_id, sender_account_id) FROM chan_messages WHERE id = ?",
        [message],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let last: Option<i64> = tx
        .query_row(
            "SELECT max(n.created) FROM personal_notifications n
               JOIN chan_messages m ON m.id = n.message_id
              WHERE n.workspace_id = ?1 AND n.user_id = ?2 AND n.topic = ?6 AND n.state <> 'cancelled'
                AND m.space_id = ?3 AND coalesce(m.sender_party_id, m.sender_account_id) IS ?4
                AND n.created > ?5",
            params![
                who.ws,
                who.me,
                space,
                sender,
                now - crate::context::SEGMENT_GAP_MS,
                CONTACT_STARRED
            ],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    if let Some(at) = last
        && !crate::presence::you_spoke(tx, &who.ws, &space, at, now)?
    {
        return Ok(Queued::Again);
    }
    let sends = Topic::ContactStarred.sends(&p);
    let id = uid("ntf");
    let n = tx.execute(
        "INSERT INTO personal_notifications(id,workspace_id,user_id,event_id,delivery_key,mode,body,state,topic,message_id,created)
         VALUES(?,?,?,?,?,?,?,?,?,?,?) ON CONFLICT(workspace_id,user_id,event_id) DO NOTHING",
        params![
            id,
            who.ws,
            who.me,
            message,
            format!("{PREFIX}{message}:{}", who.me),
            p.mode.code(),
            body,
            if sends { "waiting" } else { "held" },
            CONTACT_STARRED,
            message,
            now
        ],
    )?;
    if n == 0 {
        // this message was notified already (processed again)
        return Ok(Queued::Again);
    }
    steve_store::base::audit(
        tx,
        &who.ws,
        "steve",
        "notification.starred.queued",
        message,
        &serde_json::json!({ "notification": id, "sends": sends }),
    )?;
    Ok(Queued::Yes { id, sends })
}

/// For outbound message `out` about to be claimed or tried: when it is a starred contact's
/// notification that is no longer wanted ([`unwanted`]), the notification is cancelled and `true`
/// returned — the caller lets the message go instead of sending it.
pub(crate) fn answered(tx: &Tx, out: &str, now: i64) -> ChannelResult<bool> {
    let row: Option<(String, String, Option<String>)> = tx
        .query_row(
            "SELECT workspace_id, delivery_key, message_id FROM personal_notifications
              WHERE topic = ?2 AND delivery_id = (SELECT id FROM output_deliveries WHERE message_id = ?1)",
            params![out, CONTACT_STARRED],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((ws, key, message)) = row else {
        return Ok(false);
    };
    match unwanted(tx, &ws, message.as_deref(), now)? {
        Some(why) => {
            cancel(tx, &ws, &key, why)?;
            Ok(true)
        }
        None => Ok(false),
    }
}
