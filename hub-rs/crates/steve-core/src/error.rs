use thiserror::Error;

/// Typed refusal reasons for session/item handoffs. Core emits only the stable ASCII error code;
/// the Hub/UI layer localizes user-facing text through `steve-i18n`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandoffError {
    NoEligibleSession,
    ReadonlySession,
    SessionNotBoundToItem,
    AutoBotSwitchForbidden,
}

impl HandoffError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::NoEligibleSession => "handoff.no_eligible_session",
            Self::ReadonlySession => "handoff.readonly_session",
            Self::SessionNotBoundToItem => "handoff.session_not_bound_to_item",
            Self::AutoBotSwitchForbidden => "handoff.auto_bot_switch_forbidden",
        }
    }

    pub const fn http_status(self) -> u16 {
        match self {
            Self::NoEligibleSession | Self::ReadonlySession | Self::SessionNotBoundToItem => 409,
            Self::AutoBotSwitchForbidden => 400,
        }
    }
}

impl std::fmt::Display for HandoffError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}

impl std::error::Error for HandoffError {}

/// Errors of the domain layer. `Msg` carries the user-facing Chinese text the API returns verbatim as
/// `{"error": msg}` (the TS Hub serves these as 500); the others are infrastructure failures.
#[derive(Debug, Error)]
pub enum CoreError {
    #[error("{0}")]
    Msg(String),
    #[error("release.maintainer_workflow_required")]
    MaintainerWorkflowRequired,
    #[error("{0}")]
    Handoff(HandoffError),
    #[error("{0}")]
    Placement(steve_types::PlacementBlocked),
    #[error(transparent)]
    Contract(#[from] steve_types::Error),
    #[error(transparent)]
    Store(#[from] steve_store::StoreError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// A module refused or failed (e.g. a module decision's `before_undo`); the API serves it with its
    /// own status (`409 邮件已经发出，不能撤回`).
    #[error(transparent)]
    Module(#[from] steve_modules::ApiError),
    /// A refusal with its own status (`404 没有这条消息`, `409 已经处理过了`).
    #[error("{1}")]
    Status(u16, String),
    #[error(transparent)]
    Gateway(#[from] steve_modules::gateway::GatewayError),
}

pub type Result<T> = std::result::Result<T, CoreError>;

/// A user-facing error with a fixed message.
pub(crate) fn msg(s: impl Into<String>) -> CoreError {
    CoreError::Msg(s.into())
}
