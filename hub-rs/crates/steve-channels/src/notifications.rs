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
//! - a member's morning brief (`briefing.ready`): its alarm is a `brief` schedule's, and steve-digest has its
//!   own intake that composes the day ([`brief_delivery`], [`record`]); sent like an alarm's;
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
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use steve_modules::outbound::{Content, DeliveryIntent, Identity};
use steve_modules::{ApiError, Caller};
use steve_store::base::Viewer;
use steve_store::channel_spaces::notification_routes;
use steve_store::outbox::{Applied, ConsumeError, Consumer, Event, Since};
use steve_store::{Members, Read, W, audit_repo, notification_repo as repo};

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
/// [`Topic::checks_answered`]) and the feed entry's kind ([`Topic::feed_kind`]; migrations 102 and 103's insert
/// triggers map the topics the same way). Every match on it is exhaustive: a new topic must state each rule.
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
    /// A member's morning brief is ready (`briefing.ready`; steve-digest `brief`).
    BriefingReady,
}

/// The topic of a starred contact's message to you.
pub const CONTACT_STARRED: &str = "contact.starred";

impl Topic {
    pub const ALL: [Topic; 5] = [
        Topic::ItemDone,
        Topic::TodoDue,
        Topic::SnoozeWake,
        Topic::ContactStarred,
        Topic::BriefingReady,
    ];
    /// `personal_notifications.topic`.
    pub fn code(self) -> &'static str {
        match self {
            Self::ItemDone => steve_work_api::ITEM_DONE,
            Self::TodoDue => steve_flow::alarms::TODO_DUE,
            Self::SnoozeWake => steve_flow::alarms::SNOOZE_WAKE,
            Self::ContactStarred => CONTACT_STARRED,
            Self::BriefingReady => steve_flow::alarms::BRIEFING_READY,
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
            Self::BriefingReady => "briefing",
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
                Self::ItemDone | Self::TodoDue | Self::SnoozeWake | Self::BriefingReady => false,
            },
            Mode::Log => false,
        }
    }
    /// Whether it may go out within the quiet hours under policy `p`: a starred contact's message when you
    /// allowed it (`starred_in_quiet`). An alarm was already held through them by the alarm clock.
    pub fn quiet_allowed(self, p: &Policy) -> bool {
        match self {
            Self::ContactStarred => p.starred_in_quiet,
            Self::ItemDone | Self::TodoDue | Self::SnoozeWake | Self::BriefingReady => false,
        }
    }
    /// The delivery's urgency.
    pub fn urgency(self) -> &'static str {
        match self {
            Self::ContactStarred => "high",
            Self::ItemDone | Self::TodoDue | Self::SnoozeWake | Self::BriefingReady => "normal",
        }
    }
    /// Why it goes out (the delivery's stated reason).
    pub fn reason(self) -> &'static str {
        match self {
            Self::ItemDone => "Personal work.item.done notification (explicit member policy)",
            Self::TodoDue => "Personal todo reminder (set by the member)",
            Self::SnoozeWake => "Personal snoozed todo is back (set by the member)",
            Self::ContactStarred => "A starred contact's message to you (explicit member policy)",
            Self::BriefingReady => "Personal morning brief (set by the member)",
        }
    }
    /// Whether it is asked again, before it is submitted and before every attempt, whether it is still wanted
    /// ([`unwanted`]: you answered the message yourself, or set its space aside).
    pub fn checks_answered(self) -> bool {
        match self {
            Self::ContactStarred => true,
            Self::ItemDone | Self::TodoDue | Self::SnoozeWake | Self::BriefingReady => false,
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

fn load(r: &mut impl Read, who: &Caller) -> ChannelResult<Option<Policy>> {
    let Some(row) = repo::policy(r, &who.ws, &who.me)? else {
        return Ok(None);
    };
    let minute = |value: Option<i64>| -> ChannelResult<Option<u16>> {
        value
            .map(|n| u16::try_from(n).map_err(|_| ApiError::invalid("quiet hours")))
            .transpose()
    };
    Ok(Some(Policy {
        enabled: row.enabled,
        mode: serde_json::from_value(serde_json::Value::String(row.mode))?,
        party_id: row.party,
        space_id: row.space,
        timezone: row.timezone,
        quiet_start: minute(row.quiet_start)?,
        quiet_end: minute(row.quiet_end)?,
        starred_in_quiet: row.starred_in_quiet,
    }))
}

fn require_person(r: &mut impl Read, who: &Caller) -> ChannelResult<()> {
    if who.me.starts_with("svc:") || Members::new(r).role(&who.ws, &who.me)?.is_none() {
        return Err(ApiError::Unauthenticated);
    }
    Ok(())
}

fn self_party(r: &mut impl Read, who: &Caller, party: &str) -> ChannelResult<()> {
    require_person(r, who)?;
    let own = steve_store::parties::Parties::new(r).active_user(&who.ws, &who.me)?;
    if own.as_deref() != Some(party) {
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
fn self_route(
    r: &mut impl steve_store::Read,
    who: &Caller,
    i: &DeliveryIntent,
) -> ChannelResult<()> {
    let p = i
        .party_id
        .as_deref()
        .ok_or_else(|| ApiError::invalid("notification needs your user-linked party"))?;
    self_party(r, who, p)?;
    let viewer = Viewer::bare(&who.ws, &who.me);
    let t = crate::pipeline::route(r, &viewer, i)?;
    self_dm(r, who, p, &t.space_id)
}

fn self_dm(r: &mut impl Read, who: &Caller, p: &str, space: &str) -> ChannelResult<()> {
    self_party(r, who, p)?;

    if !notification_routes::is_self_dm(r, &who.ws, p, space)? {
        return Err(ApiError::invalid(
            "notification route must be a DM with yourself and the bot only",
        ));
    }
    Ok(())
}

pub use notification_routes::Target;

/// Read only the caller's linked parties and their recorded DM membership. In particular, do not
/// infer a counterpart from message history, cached member_count, ownership or a sole member.
pub fn targets(ch: &Channels, who: &Caller) -> ChannelResult<Vec<Target>> {
    ch.store().read(|r| {
        require_person(r, who)?;
        let viewer = Viewer::bare(&who.ws, &who.me);

        let candidates = notification_routes::targets(r, &who.ws, &who.me)?;
        let mut targets = Vec::new();
        for target in candidates {
            let p = Policy {
                party_id: Some(target.party_id.clone()),
                space_id: Some(target.space_id.clone()),
                ..Policy::default()
            };
            // Pin the explicit bot route through E1's live visibility/account/identity checks.
            // Internal failures must not masquerade as an empty picker.
            let route = self_party(r, who, &target.party_id)
                .and_then(|_| {
                    crate::pipeline::route(
                        r,
                        &viewer,
                        &intent(&p, "validation", "validation", Topic::ItemDone),
                    )
                })
                .and_then(|route| {
                    self_dm(r, who, &target.party_id, &route.space_id)?;
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

            if notification_routes::bot_route(
                r,
                &who.ws,
                &route.connection_id,
                &target.party_id,
                &target.space_id,
            )? {
                targets.push(target);
            }
        }
        Ok(targets)
    })
}

pub fn get_policy(ch: &Channels, who: &Caller) -> ChannelResult<Policy> {
    ch.store().read(|r| {
        require_person(r, who)?;
        Ok(load(r, who)?.unwrap_or_default())
    })
}

pub fn put_policy(ch: &Channels, who: &Caller, p: &Policy) -> ChannelResult<()> {
    p.validate()?;
    ch.store().write(|w| {
        require_person(w, who)?;
        if p.enabled {
            if let Some(party) = &p.party_id {
                self_party(w, who, party)?;
            }
            if p.mode == Mode::Now || (p.mode == Mode::Digest && p.space_id.is_some()) {
                self_route(
                    w,
                    who,
                    &intent(p, "validation", "validation", Topic::ItemDone),
                )?;
            }
        }
        let stored = repo::Policy {
            enabled: p.enabled,
            mode: p.mode.code().into(),
            party: p.party_id.clone(),
            space: p.space_id.clone(),
            timezone: p.timezone.clone(),
            quiet_start: p.quiet_start.map(i64::from),
            quiet_end: p.quiet_end.map(i64::from),
            starred_in_quiet: p.starred_in_quiet,
        };
        repo::set_policy(w, &who.ws, &who.me, &stored, ch.now())?;
        repo::update_for_policy(w, &who.ws, &who.me, &stored, CONTACT_STARRED)?;
        audit_repo::record(
            w,
            &who.ws,
            &who.me,
            "notification.policy.updated",
            &who.me,
            &serde_json::to_value(p)?,
        )?;
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
    fn apply(&self, w: &mut W<'_>, ev: &Event) -> Result<Applied, ConsumeError> {
        if ev.version != steve_work_api::ITEM_DONE_V {
            return Err(ConsumeError::Reject(
                "unknown work.item.done version".into(),
            ));
        }
        let d: steve_work_api::ItemDone = serde_json::from_value(ev.payload.clone())
            .map_err(|e| ConsumeError::Reject(e.to_string()))?;

        (|| -> ChannelResult<()> {
            let users = repo::recipients(w, &ev.workspace_id, ev.created, ev.seq)?;
            let body = format!(
                "Work item completed: {}\nItem: {}\nProject: {}\nCycle: {} · Revision: {}",
                d.title,
                d.item_id,
                d.project_id.as_deref().unwrap_or("—"),
                d.cycle,
                d.revision
            );
            for user in users {
                let who = Caller {
                    ws: ev.workspace_id.clone(),
                    me: user,
                };
                let p = load(w, &who)?.expect("policy selected in same transaction");
                let state = if p.mode == Mode::Now {
                    "waiting"
                } else {
                    "held"
                };
                insert(w, &who, ev, p.mode, &body, state, Topic::ItemDone)?;
            }
            Ok(())
        })()
        .map_err(|e| ConsumeError::Retry(e.to_string()))?;
        Ok(Applied::noted("personal notification policy recorded"))
    }
}

/// How a notification a member set up themselves (an alarm's, a morning brief) is kept: sent under a
/// policy that sends now (`waiting`, for the pump), otherwise only in their feed (`held`, under their
/// policy's mode, or `log` without one or when it is off).
pub fn brief_delivery(
    r: &mut impl Read,
    ws: &str,
    user: &str,
) -> ChannelResult<(Mode, &'static str)> {
    let who = Caller {
        ws: ws.into(),
        me: user.into(),
    };
    Ok(match load(r, &who)? {
        Some(p) if p.enabled && p.mode == Mode::Now => (Mode::Now, "waiting"),
        Some(p) if p.enabled => (p.mode, "held"),
        _ => (Mode::Log, "held"),
    })
}

/// [`insert`] for a module with its own notification topic (steve-digest's morning brief): one inbox row,
/// and by trigger its feed entry, for `user` about `ev`; nothing when they already have one for it.
#[allow(clippy::too_many_arguments)]
pub fn record(
    w: &mut steve_store::W<'_>,
    ws: &str,
    user: &str,
    ev: &Event,
    mode: Mode,
    body: &str,
    state: &str,
    topic: Topic,
) -> ChannelResult<()> {
    let who = Caller {
        ws: ws.into(),
        me: user.into(),
    };
    insert(w, &who, ev, mode, body, state, topic)
}

/// One inbox row (and, by trigger, its feed entry) for `who` about event `ev`.

fn insert(
    w: &mut W<'_>,
    who: &Caller,
    ev: &Event,
    mode: Mode,
    body: &str,
    state: &str,
    topic: Topic,
) -> ChannelResult<()> {
    repo::insert(
        w,
        &repo::NewNotice {
            ws: &who.ws,
            user: &who.me,
            event: &ev.id,
            key: &format!("{PREFIX}{}:{}", ev.id, who.me),
            mode: mode.code(),
            body,
            state,
            topic: topic.code(),
            message: None,
            created: ev.created,
        },
    )?;
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
            .filter(|t| t.alarm.is_some() && t.from_lines)
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
    fn apply(&self, w: &mut W<'_>, ev: &Event) -> Result<Applied, ConsumeError> {
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

        (|| -> ChannelResult<Applied> {
            let member = w.members().role(&ev.workspace_id, &a.owner)?.is_some();
            if !member {
                return Ok(Applied::noted("the alarm's owner is no longer a member"));
            }
            let who = Caller {
                ws: ev.workspace_id.clone(),
                me: a.owner.clone(),
            };
            let (mode, state) = brief_delivery(w, &who.ws, &who.me)?;
            let topic = Topic::parse(self.0.name)
                .ok_or_else(|| ApiError::invalid("personal notification topic"))?;
            insert(
                w,
                &who,
                ev,
                mode,
                &alarm_body(self.0.name, &a),
                state,
                topic,
            )?;
            Ok(Applied::noted("personal alarm notification recorded"))
        })()
        .map_err(|e| ConsumeError::Retry(e.to_string()))
    }
}

/// Before E1 commits or retries, revalidate the stored personal consent, exact content and self DM.
/// Reserved keys cannot be used by a caller to create an untracked notification.
pub(crate) fn verify_intent(
    r: &mut impl steve_store::Read,
    who: &Caller,
    i: &DeliveryIntent,
    now: i64,
) -> ChannelResult<()> {
    if !i.key.starts_with(PREFIX) {
        return Ok(());
    }

    let Some(repo::Pending {
        body: Some(body),
        state,
        topic,
        message,
    }) = repo::pending(r, &who.ws, &who.me, &i.key)?
    else {
        return Err(ApiError::Unauthenticated);
    };
    let topic = Topic::parse(&topic).ok_or(ApiError::Unauthenticated)?;
    let p = load(r, who)?.ok_or(ApiError::Unauthenticated)?;
    if !p.enabled
        || !topic.sends(&p)
        || !matches!(state.as_str(), "waiting" | "submitted")
        || p.holds(topic, now)?
        || *i != intent(&p, &i.key, &body, topic)
        || (topic.checks_answered() && unwanted(r, &who.ws, message.as_deref(), now)?.is_some())
    {
        return Err(ApiError::Unauthenticated);
    }
    self_route(r, who, i)
}

/// Queue checks the immutable destination, not a freshly resolved personal preference.
pub(crate) fn verify_pinned(
    r: &mut impl Read,
    who: &Caller,
    i: &DeliveryIntent,
    space: &str,
) -> ChannelResult<()> {
    if i.key.starts_with(PREFIX) {
        self_dm(
            r,
            who,
            i.party_id.as_deref().ok_or(ApiError::Unauthenticated)?,
            space,
        )?;
    }
    Ok(())
}

pub(crate) fn attach(
    w: &mut W<'_>,
    who: &Caller,
    i: &DeliveryIntent,
    delivery: &str,
) -> ChannelResult<()> {
    if i.key.starts_with(PREFIX) {
        repo::attach(w, &who.ws, &who.me, &i.key, delivery)?;
    }
    Ok(())
}

/// DND is a postponement, never a failed send or a reason to bypass the normal C6 authorization.
/// Digest/log changes also park already queued sends; disabling is handled by verify_intent.

pub(crate) fn deferred(r: &mut impl Read, message: &str, now: i64) -> ChannelResult<bool> {
    let Some(row) = repo::by_output(r, message)? else {
        return Ok(false);
    };
    let topic = topic_of(&row.topic)?;
    let who = Caller {
        ws: row.workspace,
        me: row.user,
    };
    let Some(p) = load(r, &who)? else {
        return Ok(false);
    };
    Ok(p.enabled && (!topic.sends(&p) || p.holds(topic, now)?))
}

pub use repo::Notice as Notification;

pub fn list(ch: &Channels, who: &Caller) -> ChannelResult<Vec<Notification>> {
    ch.store().read(|r| {
        require_person(r, who)?;
        Ok(repo::list(r, &who.ws, &who.me)?)
    })
}

pub fn seen(ch: &Channels, who: &Caller, id: &str) -> ChannelResult<()> {
    ch.store().write(|w| {
        require_person(w, who)?;
        if !steve_store::FeedReads(w).mark_notification(&who.ws, &who.me, id, ch.now())? {
            return Err(ApiError::not_found("notification"));
        }
        Ok(())
    })
}

/// A bounded recovery pass. Only unsubmitted `now` entries can create an E1 delivery. Held digest
/// and log records never turn into individual sends on a later preference change.
pub async fn pump(ch: &Channels, outbox: &Outbox) -> ChannelResult<usize> {
    pump_at(ch, outbox, ch.now()).await
}

async fn pump_at(ch: &Channels, outbox: &Outbox, now: i64) -> ChannelResult<usize> {
    let rows = ch.store().read(|r| repo::due(r, now, now - KEEP_MS))?;
    let mut n = 0;
    for repo::Due {
        workspace: ws,
        user: me,
        key,
        body,
        topic,
        message,
    } in rows
    {
        // Every attempted row advances, including topics this version cannot interpret.
        ch.store()
            .write(|w| repo::update_waiting(w, &key, repo::WaitingChange::RetryAt(now + 10_000)))?;
        let Some(topic) = Topic::parse(&topic) else {
            tracing::warn!(
                key,
                topic,
                "personal notification of an unknown topic left waiting"
            );
            continue;
        };
        // a starred contact's message you answered yourself (or whose space you set aside) meanwhile
        if topic.checks_answered() {
            let why = ch
                .store()
                .read(|r| unwanted(r, &ws, message.as_deref(), now))?;
            if let Some(why) = why {
                ch.store().write(|w| cancel(w, &ws, &key, why))?;
                continue;
            }
        }
        let who = Caller { ws, me };
        let p = ch.store().read(|r| -> ChannelResult<Option<Policy>> {
            if require_person(r, &who).is_err() {
                return Ok(None);
            }
            load(r, &who)
        })?;
        let Some(p) = p.filter(|p| p.enabled) else {
            ch.store()
                .write(|w| repo::update_waiting(w, &key, repo::WaitingChange::Cancel))?;
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
    ch.store().write(|w| repo::prune(w, now - KEEP_MS))?;
    Ok(n)
}

/// Waiting notification attempts and body expiry share one durable due source.
/// There is no retry-count dead letter: the existing 30-day body tombstone is terminal.
pub struct NotificationDue {
    pub channels: Channels,
    pub outbox: Arc<Outbox>,
}
impl steve_runtime::due::DueSource for NotificationDue {
    fn name(&self) -> &'static str {
        "channels.notifications"
    }

    async fn next_due(&self) -> Result<Option<i64>, steve_runtime::due::DueError> {
        self.channels
            .store()
            .read(|r| repo::next_due(r, KEEP_MS))
            .map_err(|e| steve_runtime::due::DueError(e.to_string()))
    }
    async fn run_due(&self, now: i64) -> Result<(), steve_runtime::due::DueError> {
        pump_at(&self.channels, &self.outbox, now)
            .await
            .map(|_| ())
            .map_err(|e| steve_runtime::due::DueError(e.to_string()))
    }
}

// The old pump waits ten seconds after both successful passes and source errors.
fn recovery_backoff() -> steve_runtime::policy::Backoff {
    let interval = std::time::Duration::from_secs(10);
    steve_runtime::policy::Backoff {
        base: interval,
        max: interval,
        factor: 1,
        jitter: steve_runtime::policy::Jitter::None,
        attempts: None,
        reset_after: None,
    }
}

/// Authoritative commit hints for the notification source and its wake regressions.
pub const NOTIFICATION_WAKE_TABLES: &[&str] = &["personal_notifications", "notification_policies"];

pub async fn run(ch: Channels, outbox: Arc<Outbox>) {
    let clock = ch.modules().clock.clone();
    let wake = ch.store().commit_wake(NOTIFICATION_WAKE_TABLES);
    let source = Arc::new(NotificationDue {
        channels: ch,
        outbox,
    });
    // Store commits cover intake, alarms, starred messages and external notification producers.
    steve_runtime::due::due_loop(source, clock, wake, recovery_backoff()).await;
}

// ---------------------------------------------------------------- a starred contact's message

/// Why a starred contact's notification about inbound message `message` is no longer wanted at
/// `now`: you are there for it yourself ([`crate::presence::owner_engaged`]: you answered it, or are
/// chatting there), or its space is set aside (不提醒, 屏蔽, or `reply_as_you = off`). `None`: still
/// wanted. A message that is gone is not wanted.
pub(crate) fn unwanted(
    r: &mut impl steve_store::Read,
    ws: &str,
    message: Option<&str>,
    now: i64,
) -> ChannelResult<Option<&'static str>> {
    let Some(message) = message else {
        return Ok(Some(GONE));
    };
    let Some(space) = steve_store::channel_message_repo::ingress::source_space(r, ws, message)?
    else {
        return Ok(Some(GONE));
    };
    if crate::presence::owner_engaged_chat(r, ws, message, now)?.is_some() {
        return Ok(Some(crate::presence::REASON_REPLIED));
    }
    if crate::starred::set_aside_chat(r, ws, &space)? {
        return Ok(Some(crate::presence::REASON_SETTING));
    }
    Ok(None)
}

/// `audit.why` of a notification whose message is no longer stored.
const GONE: &str = "message_gone";

fn cancel(w: &mut W<'_>, ws: &str, key: &str, why: &str) -> ChannelResult<bool> {
    let changed = repo::cancel(w, ws, key)?;
    if changed {
        audit_repo::record(
            w,
            ws,
            "steve",
            "notification.starred.cancelled",
            key,
            &serde_json::json!({"why": why}),
        )?;
    }
    Ok(changed)
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
    w: &mut W<'_>,
    who: &Caller,
    message: &str,
    body: &str,
    now: i64,
) -> ChannelResult<Queued> {
    if who.me.starts_with("svc:") {
        return Ok(Queued::Off);
    }

    let member = w.members().role(&who.ws, &who.me)?.is_some();
    let Some(p) = load(w, who)?.filter(|p| p.enabled && member) else {
        return Ok(Queued::Off);
    };
    let (space, sender) =
        steve_store::channel_message_repo::ingress::sender_in_space(w, &who.ws, message)?
            .ok_or_else(|| ApiError::not_found("消息"))?;
    let last = repo::recent_starred(
        w,
        &who.ws,
        &who.me,
        &space,
        sender.as_deref(),
        now - crate::context::SEGMENT_GAP_MS,
        CONTACT_STARRED,
    )?;
    if let Some(at) = last
        && !crate::presence::you_spoke(w, &who.ws, &space, at, now)?
    {
        return Ok(Queued::Again);
    }
    let sends = Topic::ContactStarred.sends(&p);

    let id = repo::insert(
        w,
        &repo::NewNotice {
            ws: &who.ws,
            user: &who.me,
            event: message,
            key: &format!("{PREFIX}{message}:{}", who.me),
            mode: p.mode.code(),
            body,
            state: if sends { "waiting" } else { "held" },
            topic: CONTACT_STARRED,
            message: Some(message),
            created: now,
        },
    )?;
    if let Some(id) = &id {
        audit_repo::record(
            w,
            &who.ws,
            "steve",
            "notification.starred.queued",
            message,
            &serde_json::json!({"notification": id, "sends": sends}),
        )?;
    }
    match id {
        Some(id) => Ok(Queued::Yes { id, sends }),
        None => Ok(Queued::Again),
    }
}

/// For outbound message `out` about to be claimed or tried: when it is a starred contact's
/// notification that is no longer wanted ([`unwanted`]), the notification is cancelled and `true`
/// returned — the caller lets the message go instead of sending it.
pub(crate) fn answered(w: &mut W<'_>, out: &str, now: i64) -> ChannelResult<bool> {
    let Some(row) = repo::by_output(w, out)?.filter(|row| row.topic == CONTACT_STARRED) else {
        return Ok(false);
    };
    match unwanted(w, &row.workspace, row.message.as_deref(), now)? {
        Some(why) => {
            cancel(w, &row.workspace, &row.key, why)?;
            Ok(true)
        }
        None => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(mode: Mode, space: bool, starred_in_quiet: bool) -> Policy {
        Policy {
            mode,
            space_id: space.then(|| "sp_1".into()),
            starred_in_quiet,
            ..Policy::default()
        }
    }

    /// Every topic is stored by its own name, has a feed kind and a whole rule set; steve-flow's alarm topics
    /// are among them with the same feed kind; a name no topic has is not read as any.
    #[test]
    fn every_topic_maps_to_a_feed_kind_and_a_rule_set() {
        let mut codes: Vec<_> = Topic::ALL.iter().map(|t| t.code()).collect();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), Topic::ALL.len());
        for t in Topic::ALL {
            assert_eq!(Topic::parse(t.code()), Some(t));
            assert!(matches!(
                t.feed_kind(),
                "completion" | "starred" | "reminder" | "briefing"
            ));
            assert!(matches!(t.urgency(), "normal" | "high"));
            assert!(!t.reason().is_empty());
            for mode in [Mode::Now, Mode::Digest, Mode::Log] {
                for space in [false, true] {
                    for allowed in [false, true] {
                        let p = policy(mode, space, allowed);
                        // everything goes out under `now`, nothing under `log`
                        match mode {
                            Mode::Now => assert!(t.sends(&p)),
                            Mode::Log => assert!(!t.sends(&p)),
                            Mode::Digest => {}
                        }
                        // only what you allowed goes out within the quiet hours
                        assert!(!t.quiet_allowed(&p) || allowed);
                    }
                }
            }
        }
        for f in steve_flow::alarms::TOPICS {
            let t = Topic::parse(f.name)
                .unwrap_or_else(|| panic!("{} has no notification topic", f.name));
            assert_eq!(t.feed_kind(), f.feed_kind, "{}", f.name);
        }
        assert_eq!(Topic::parse("completion"), None);
        assert_eq!(Topic::parse("starred"), None);
        assert!(topic_of("elsewhere").is_err());
    }

    /// The rules each topic is sent by.
    #[test]
    fn a_starred_contact_is_urgent_and_alarms_are_sent_like_completions() {
        let s = Topic::ContactStarred;
        assert_eq!(s.urgency(), "high");
        assert!(s.checks_answered());
        assert!(s.sends(&policy(Mode::Digest, true, false)));
        assert!(!s.sends(&policy(Mode::Digest, false, false)));
        assert!(s.quiet_allowed(&policy(Mode::Now, false, true)));
        assert!(!s.quiet_allowed(&policy(Mode::Now, false, false)));
        for t in [
            Topic::ItemDone,
            Topic::TodoDue,
            Topic::SnoozeWake,
            Topic::BriefingReady,
        ] {
            assert_eq!(t.urgency(), "normal");
            assert!(!t.checks_answered());
            assert!(t.sends(&policy(Mode::Now, false, false)));
            assert!(!t.sends(&policy(Mode::Digest, true, true)));
            assert!(!t.quiet_allowed(&policy(Mode::Now, false, true)));
        }
    }
}

#[cfg(test)]
mod recovery_wait_tests {
    #[test]
    fn source_errors_keep_the_original_fixed_ten_second_wait() {
        let policy = super::recovery_backoff();
        for attempt in [0, 1, 2, 8, 32] {
            assert_eq!(
                policy.delay(attempt, &mut rand::rng()),
                std::time::Duration::from_secs(10)
            );
        }
    }
}
