//! The policy for allocating an operation identity from persisted command facts.

use super::*;
use steve_store::runtime::{IntentHead, IntentState};

#[derive(Clone, Copy)]
pub(super) enum Advance {
    NewInput,
    NewWorkspace,
    ExplicitRetry,
    ChangedConfig,
    RetryTurn,
}

/// The caller and insertion share one Store write. A Node's unresolved command always owns
/// the current revision, including after either process restarts.
pub(super) fn next_revision(
    w: &mut W<'_>,
    intent: Intent,
    advance: Advance,
) -> Result<IntentKey, StoreError> {
    let revision = match rt::intent_head_in(w, &intent)? {
        None => 0,
        Some(IntentHead { revision, state }) => {
            let allowed = match (advance, state, &intent) {
                (_, IntentState::Unsettled, _) => false,
                (Advance::NewInput, IntentState::Succeeded, _) => true,
                (Advance::NewInput, IntentState::Failed, _) => false,
                (
                    Advance::NewWorkspace,
                    IntentState::Succeeded | IntentState::Failed,
                    Intent::Tidy(_),
                ) => true,
                (Advance::ExplicitRetry, IntentState::Failed, _) => true,
                (
                    Advance::ChangedConfig,
                    IntentState::Succeeded | IntentState::Failed,
                    Intent::Configure(_),
                ) => true,
                (Advance::RetryTurn, IntentState::Failed, Intent::StartTurn(_)) => revision < 3,
                (
                    Advance::NewWorkspace
                    | Advance::ExplicitRetry
                    | Advance::RetryTurn
                    | Advance::ChangedConfig,
                    IntentState::Succeeded | IntentState::Failed,
                    _,
                ) => false,
            };
            if !allowed {
                return Err(conflict(
                    "the existing intent must be resolved before another revision",
                ));
            }
            revision
                .checked_add(1)
                .ok_or_else(|| conflict("intent revision overflow"))?
        }
    };
    Ok(IntentKey { intent, revision })
}
