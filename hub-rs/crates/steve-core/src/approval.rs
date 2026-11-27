//! The single policy dispatch port for session permission requests and agent questions.
//! The caller applies the returned intent via the decisions ledger. Resolving an ACP prompt
//! does not authorize a Steve capability; its domain still authorizes the eventual tool call.

use steve_harness_spec::NativeApprovalFallback;
use steve_node_api::{PermissionDecision, PermissionRequest};
use steve_types::ApprovalMode;

pub enum Request<'a> {
    Permission(&'a PermissionRequest),
    Question(Question<'a>),
}

pub struct Question<'a> {
    pub text: &'a str,
    pub options: &'a [String],
    pub recommended: Option<&'a str>,
    pub why: Option<&'a str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    OwnerDecision,
    /// An ACP answer, still subject to the same attempt, receipt and decision ledger.
    /// No resolver shipped here returns AllowOnce; only an owner can grant it.
    Answer(PermissionDecision),
}

#[derive(Debug, Clone, Copy)]
pub struct Resolution {
    pub resolver: &'static str,
    pub outcome: Outcome,
}

pub trait PermissionResolver {
    fn resolve(&self, request: Request<'_>) -> Resolution;
}

pub struct AskOwner;

impl PermissionResolver for AskOwner {
    fn resolve(&self, _: Request<'_>) -> Resolution {
        Resolution {
            resolver: "ask_owner",
            outcome: Outcome::OwnerDecision,
        }
    }
}

/// An adapter's explicit fallback for unexpected ACP permission requests.
pub struct HarnessNative(NativeApprovalFallback);

impl PermissionResolver for HarnessNative {
    fn resolve(&self, request: Request<'_>) -> Resolution {
        if matches!(request, Request::Question(_)) {
            return AskOwner.resolve(request);
        }
        Resolution {
            resolver: "harness_native",
            outcome: match self.0 {
                NativeApprovalFallback::AskOwner => Outcome::OwnerDecision,
                NativeApprovalFallback::Reject => Outcome::Answer(PermissionDecision::Reject),
            },
        }
    }
}

/// `applied_native` comes from the Node's confirmed mode and the adapter's declaration,
/// not the desired setting. A widening not yet applied continues to ask the owner.
pub fn resolve(
    policy: ApprovalMode,
    applied_native: Option<NativeApprovalFallback>,
    request: Request<'_>,
) -> Resolution {
    if matches!(request, Request::Question(_)) {
        return AskOwner.resolve(request);
    }
    match (policy, applied_native) {
        // A turn may still run in native mode while an owner edit to Ask is pending.
        (ApprovalMode::Ask | ApprovalMode::Auto | ApprovalMode::FullAccess, Some(fallback)) => {
            HarnessNative(fallback).resolve(request)
        }
        (ApprovalMode::Ask | ApprovalMode::Auto | ApprovalMode::FullAccess, None) => {
            AskOwner.resolve(request)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn questions_are_for_the_owner_even_in_native_mode() {
        let q = resolve(
            ApprovalMode::Auto,
            Some(NativeApprovalFallback::Reject),
            Request::Question(Question {
                text: "What next?",
                options: &[],
                recommended: None,
                why: None,
            }),
        );
        assert_eq!(q.resolver, "ask_owner");
        assert_eq!(q.outcome, Outcome::OwnerDecision);
    }
}
