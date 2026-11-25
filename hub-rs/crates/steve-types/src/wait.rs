//! Unified wait contract: every durable wait in Steve carries an owner, a start clock,
//! a finite deadline, and a deterministic timeout action backed by named settings.

use serde::{Deserialize, Serialize};

use crate::Millis;

pub const MS_PER_MINUTE: i64 = 60_000;
pub const MS_PER_HOUR: i64 = 60 * MS_PER_MINUTE;
pub const MS_PER_DAY: i64 = 24 * MS_PER_HOUR;

/// Minimum allowed override for any wait timeout setting (1 minute): prevents hot busy-loops.
pub const WAIT_LIMIT_MIN_MS: i64 = MS_PER_MINUTE;
/// Maximum allowed override for general wait timeout settings (90 days): keeps every wait finite.
pub const WAIT_LIMIT_MAX_MS: i64 = 90 * MS_PER_DAY;
/// Maximum allowed override for external item operations (7 days).
pub const WAIT_LIMIT_OPERATION_MAX_MS: i64 = 7 * MS_PER_DAY;
/// Maximum allowed override for channel drafts and external write effects (30 days).
pub const WAIT_LIMIT_EXTERNAL_MAX_MS: i64 = 30 * MS_PER_DAY;

/// Default upper bound for item prerequisite waits (72 hours): prerequisites normally clear within
/// a few workdays; silent waiting beyond 3 days risks livelock and escalates to the owner.
pub const WAIT_LIMIT_DEPENDENCY_MS: i64 = 72 * MS_PER_HOUR;
/// Default upper bound for general owner decisions (7 days): unanswered cards older than a week are
/// stale and expire cleanly unless given a shorter domain TTL.
pub const WAIT_LIMIT_DECISION_MS: i64 = 7 * MS_PER_DAY;
/// Default upper bound for memory confirmation candidates (30 days), matching `steve-knowledge`.
pub const WAIT_LIMIT_MEMORY_CANDIDATE_MS: i64 = 30 * MS_PER_DAY;
/// Default upper bound for secret-use approval decisions (24 hours), matching `steve-modules/vault`.
pub const WAIT_LIMIT_SECRET_USE_MS: i64 = MS_PER_DAY;
/// Default upper bound for channel reply drafts when no shorter `card_ttl_h` is set (72 hours):
/// IM replies lose conversational relevance after 3 days.
pub const WAIT_LIMIT_CHANNEL_DRAFT_MS: i64 = 72 * MS_PER_HOUR;
/// Default upper bound for item external operations such as releases and wrap-up hooks (60 minutes):
/// operations finish in minutes; silent hangs past 1 hour escalate rather than blocking closure.
pub const WAIT_LIMIT_ITEM_OPERATION_MS: i64 = 60 * MS_PER_MINUTE;
/// Default upper bound for approved external effects such as GitHub PR writes (24 hours):
/// retries span transient outages, then stop silently retrying and escalate to the owner.
pub const WAIT_LIMIT_EXTERNAL_EFFECT_MS: i64 = 24 * MS_PER_HOUR;
/// Default upper bound for unsent/unapproved mail drafts (7 days).
pub const WAIT_LIMIT_MAIL_DRAFT_MS: i64 = 7 * MS_PER_DAY;
/// Default upper bound for workflow waits without an explicit node timeout (7 days).
pub const WAIT_LIMIT_FLOW_MS: i64 = 7 * MS_PER_DAY;

/// Named workspace `settings` keys controlling each wait category's upper bound (milliseconds).
pub const SETTING_WAIT_DEPENDENCY_MS: &str = "wait.dependency_ms";
pub const SETTING_WAIT_DECISION_MS: &str = "wait.decision_ms";
pub const SETTING_WAIT_CHANNEL_DRAFT_MS: &str = "wait.channel_draft_ms";
pub const SETTING_WAIT_ITEM_OPERATION_MS: &str = "wait.operation_ms";
pub const SETTING_WAIT_EXTERNAL_EFFECT_MS: &str = "wait.external_effect_ms";
pub const SETTING_WAIT_MAIL_DRAFT_MS: &str = "wait.mail_draft_ms";
pub const SETTING_WAIT_FLOW_MS: &str = "wait.flow_ms";

/// Who is responsible for advancing or unblocking a durable wait.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum WaitOwner {
    /// The workspace owner (human decision maker).
    Owner,
    /// A specific user.
    User { id: String },
    /// A Bot responsible for an item or session.
    Bot { id: String },
    /// A Steve subsystem / automation component.
    System { name: String },
}

impl WaitOwner {
    pub fn bot(id: impl Into<String>) -> Self {
        Self::Bot { id: id.into() }
    }

    pub fn user(id: impl Into<String>) -> Self {
        Self::User { id: id.into() }
    }

    pub fn system(name: impl Into<String>) -> Self {
        Self::System { name: name.into() }
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        match self {
            Self::Owner => Ok(()),
            Self::User { id } if !id.trim().is_empty() => Ok(()),
            Self::Bot { id } if !id.trim().is_empty() => Ok(()),
            Self::System { name } if !name.trim().is_empty() => Ok(()),
            _ => Err("wait owner identifier must not be empty"),
        }
    }

    /// Canonical compact code stored in SQL columns (`owner`, `user:<id>`, `bot:<id>`, `system:<name>`).
    pub fn to_code(&self) -> String {
        match self {
            Self::Owner => "owner".into(),
            Self::User { id } => format!("user:{id}"),
            Self::Bot { id } => format!("bot:{id}"),
            Self::System { name } => format!("system:{name}"),
        }
    }

    pub fn parse(raw: &str) -> Result<Self, &'static str> {
        if raw == "owner" {
            return Ok(Self::Owner);
        }
        let Some((prefix, rest)) = raw.split_once(':') else {
            return Err("invalid wait owner code");
        };
        if rest.trim().is_empty() {
            return Err("wait owner identifier must not be empty");
        }
        match prefix {
            "user" => Ok(Self::User { id: rest.into() }),
            "bot" => Ok(Self::Bot { id: rest.into() }),
            "system" => Ok(Self::System { name: rest.into() }),
            _ => Err("unknown wait owner kind"),
        }
    }
}

/// What the unified due scheduler does when a wait reaches its deadline.
#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    strum::AsRefStr,
    strum::EnumString,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum WaitTimeoutAction {
    /// Escalate to the workspace owner (open/ensure an owner card and stop silent waiting).
    EscalateToOwner,
    /// Expire, close, or discard the wait and its linked subject cleanly.
    Close,
}

impl WaitTimeoutAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::EscalateToOwner => "escalate_to_owner",
            Self::Close => "close",
        }
    }
}

/// Categories of durable waits covered by [`WaitContract`] and named timeout settings.
#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    strum::AsRefStr,
    strum::EnumString,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum WaitKind {
    Dependency,
    Decision,
    ChannelDraft,
    ItemOperation,
    ExternalEffect,
    MailDraft,
    FlowWait,
}

impl WaitKind {
    pub const ALL: [Self; 7] = [
        Self::Dependency,
        Self::Decision,
        Self::ChannelDraft,
        Self::ItemOperation,
        Self::ExternalEffect,
        Self::MailDraft,
        Self::FlowWait,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Dependency => "dependency",
            Self::Decision => "decision",
            Self::ChannelDraft => "channel_draft",
            Self::ItemOperation => "item_operation",
            Self::ExternalEffect => "external_effect",
            Self::MailDraft => "mail_draft",
            Self::FlowWait => "flow_wait",
        }
    }

    pub const fn setting_key(self) -> &'static str {
        match self {
            Self::Dependency => SETTING_WAIT_DEPENDENCY_MS,
            Self::Decision => SETTING_WAIT_DECISION_MS,
            Self::ChannelDraft => SETTING_WAIT_CHANNEL_DRAFT_MS,
            Self::ItemOperation => SETTING_WAIT_ITEM_OPERATION_MS,
            Self::ExternalEffect => SETTING_WAIT_EXTERNAL_EFFECT_MS,
            Self::MailDraft => SETTING_WAIT_MAIL_DRAFT_MS,
            Self::FlowWait => SETTING_WAIT_FLOW_MS,
        }
    }

    pub const fn default_limit_ms(self) -> i64 {
        match self {
            Self::Dependency => WAIT_LIMIT_DEPENDENCY_MS,
            Self::Decision => WAIT_LIMIT_DECISION_MS,
            Self::ChannelDraft => WAIT_LIMIT_CHANNEL_DRAFT_MS,
            Self::ItemOperation => WAIT_LIMIT_ITEM_OPERATION_MS,
            Self::ExternalEffect => WAIT_LIMIT_EXTERNAL_EFFECT_MS,
            Self::MailDraft => WAIT_LIMIT_MAIL_DRAFT_MS,
            Self::FlowWait => WAIT_LIMIT_FLOW_MS,
        }
    }

    pub const fn min_limit_ms(self) -> i64 {
        let _ = self;
        WAIT_LIMIT_MIN_MS
    }

    pub const fn max_limit_ms(self) -> i64 {
        match self {
            Self::ItemOperation => WAIT_LIMIT_OPERATION_MAX_MS,
            Self::ChannelDraft | Self::ExternalEffect => WAIT_LIMIT_EXTERNAL_MAX_MS,
            Self::Dependency | Self::Decision | Self::MailDraft | Self::FlowWait => {
                WAIT_LIMIT_MAX_MS
            }
        }
    }

    pub const fn default_on_timeout(self) -> WaitTimeoutAction {
        match self {
            Self::Dependency | Self::ItemOperation | Self::ExternalEffect => {
                WaitTimeoutAction::EscalateToOwner
            }
            Self::Decision | Self::ChannelDraft | Self::MailDraft | Self::FlowWait => {
                WaitTimeoutAction::Close
            }
        }
    }

    pub fn clamp_limit_ms(self, raw_ms: i64) -> i64 {
        raw_ms.clamp(self.min_limit_ms(), self.max_limit_ms())
    }
}

/// Named wait upper bounds resolved for a workspace.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct WaitLimits {
    #[cfg_attr(test, ts(type = "number"))]
    pub dependency_ms: i64,
    #[cfg_attr(test, ts(type = "number"))]
    pub decision_ms: i64,
    #[cfg_attr(test, ts(type = "number"))]
    pub channel_draft_ms: i64,
    #[cfg_attr(test, ts(type = "number"))]
    pub item_operation_ms: i64,
    #[cfg_attr(test, ts(type = "number"))]
    pub external_effect_ms: i64,
    #[cfg_attr(test, ts(type = "number"))]
    pub mail_draft_ms: i64,
    #[cfg_attr(test, ts(type = "number"))]
    pub flow_wait_ms: i64,
}

impl Default for WaitLimits {
    fn default() -> Self {
        Self {
            dependency_ms: WAIT_LIMIT_DEPENDENCY_MS,
            decision_ms: WAIT_LIMIT_DECISION_MS,
            channel_draft_ms: WAIT_LIMIT_CHANNEL_DRAFT_MS,
            item_operation_ms: WAIT_LIMIT_ITEM_OPERATION_MS,
            external_effect_ms: WAIT_LIMIT_EXTERNAL_EFFECT_MS,
            mail_draft_ms: WAIT_LIMIT_MAIL_DRAFT_MS,
            flow_wait_ms: WAIT_LIMIT_FLOW_MS,
        }
    }
}

impl WaitLimits {
    pub const fn limit_ms(&self, kind: WaitKind) -> i64 {
        match kind {
            WaitKind::Dependency => self.dependency_ms,
            WaitKind::Decision => self.decision_ms,
            WaitKind::ChannelDraft => self.channel_draft_ms,
            WaitKind::ItemOperation => self.item_operation_ms,
            WaitKind::ExternalEffect => self.external_effect_ms,
            WaitKind::MailDraft => self.mail_draft_ms,
            WaitKind::FlowWait => self.flow_wait_ms,
        }
    }
}

/// Unified wait contract carried by every durable wait row and view.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct WaitContract {
    pub owner: WaitOwner,
    pub started_at: Millis,
    pub deadline: Millis,
    pub on_timeout: WaitTimeoutAction,
}

impl WaitContract {
    pub fn new(
        owner: WaitOwner,
        started_at: Millis,
        deadline: Millis,
        on_timeout: WaitTimeoutAction,
    ) -> Result<Self, &'static str> {
        owner.validate()?;
        if deadline.0 < started_at.0 {
            return Err("wait deadline precedes started_at");
        }
        Ok(Self {
            owner,
            started_at,
            deadline,
            on_timeout,
        })
    }

    pub fn bounded(
        owner: WaitOwner,
        started_at: i64,
        limit_ms: i64,
        on_timeout: WaitTimeoutAction,
    ) -> Result<Self, &'static str> {
        if limit_ms <= 0 {
            return Err("wait limit must be positive");
        }
        let deadline = started_at
            .checked_add(limit_ms)
            .ok_or("wait deadline overflow")?;
        Self::new(owner, Millis(started_at), Millis(deadline), on_timeout)
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        self.owner.validate()?;
        if self.deadline.0 < self.started_at.0 {
            return Err("wait deadline precedes started_at");
        }
        Ok(())
    }

    pub const fn is_due(&self, now: i64) -> bool {
        self.deadline.0 <= now
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wait_owner_round_trips_and_rejects_empty_ids() {
        for owner in [
            WaitOwner::Owner,
            WaitOwner::user("usr_1"),
            WaitOwner::bot("dev"),
            WaitOwner::system("github"),
        ] {
            owner.validate().unwrap();
            let code = owner.to_code();
            assert_eq!(WaitOwner::parse(&code).unwrap(), owner);
        }
        for bad in ["", "nobody", "bot:", "user:  ", "system:", "other:x"] {
            assert!(WaitOwner::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn wait_contract_enforces_owner_and_clock_order() {
        let ok = WaitContract::bounded(
            WaitOwner::Owner,
            1_000,
            WAIT_LIMIT_DECISION_MS,
            WaitTimeoutAction::Close,
        )
        .unwrap();
        assert_eq!(ok.started_at, Millis(1_000));
        assert_eq!(ok.deadline, Millis(1_000 + WAIT_LIMIT_DECISION_MS));
        assert!(!ok.is_due(ok.deadline.0 - 1));
        assert!(ok.is_due(ok.deadline.0));

        assert!(
            WaitContract::bounded(WaitOwner::Owner, 1_000, 0, WaitTimeoutAction::Close).is_err()
        );
        assert!(
            WaitContract::new(
                WaitOwner::Owner,
                Millis(2_000),
                Millis(1_000),
                WaitTimeoutAction::Close,
            )
            .is_err()
        );
        assert!(
            WaitContract::new(
                WaitOwner::bot(" "),
                Millis(1_000),
                Millis(2_000),
                WaitTimeoutAction::EscalateToOwner,
            )
            .is_err()
        );
    }

    #[test]
    fn wait_kind_defaults_sit_within_named_bounds() {
        let defaults = WaitLimits::default();
        for kind in WaitKind::ALL {
            let ms = defaults.limit_ms(kind);
            assert_eq!(ms, kind.default_limit_ms());
            assert!(ms >= kind.min_limit_ms());
            assert!(ms <= kind.max_limit_ms());
            assert!(!kind.setting_key().is_empty());
        }
    }
}
