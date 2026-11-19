//! Typed contract entrance to the sole comms digest implementation. No fixture SQL.
use super::*;
use crate::digest::{self, Scope};
use steve_storage_api::ops::comms::digest::{EntryPoint, Source, SyncPendingMembership};
use steve_storage_api::uow::Op;

pub fn apply(c: &Connection, request: &SyncPendingMembership, _: Rev) -> Result<(), CommitError> {
    request.validate().map_err(CommitError::Fatal)?;
    match request.source {
        Source::Receipt => apply_receipt(c, request),
        Source::ChanMessage => apply_message(c, request, digest::Source::ChanMessage),
        Source::MailMessage => apply_message(c, request, digest::Source::MailMessage),
    }
}
fn apply_receipt(c: &Connection, request: &SyncPendingMembership) -> Result<(), CommitError> {
    let (conv, id) = request.receipt_identity().map_err(CommitError::Fatal)?;
    let scope = Scope {
        workspace: request.workspace_id.as_str().into(),
        conv,
    };
    if !digest::receipt_exists(c, &scope, &id).map_err(sql_error)? {
        return Err(invalid("receipt must exist in the work unit"));
    }
    match request.entry {
        EntryPoint::DequeueOnReceipt => {
            digest::dequeue_on_receipt(c, &scope.workspace, &scope.conv, [&id]).map_err(sql_error)
        }
        EntryPoint::SyncPendingConversation => {
            digest::reconcile_conversations(c, [scope]).map_err(sql_error)
        }
        EntryPoint::EnqueueDigestible => Err(invalid("receipt is not a message source")),
    }
}
fn apply_message(
    c: &Connection,
    request: &SyncPendingMembership,
    source: digest::Source,
) -> Result<(), CommitError> {
    let current = source.scope(c, &request.id).map_err(sql_error)?;
    // Source::scope now names the *new* row. Recover old scopes from pending, never
    // pretend the new workspace/conversation is the pre-mutation source scope.
    let old = digest::pending_scopes(c, source, &request.id).map_err(sql_error)?;
    let workspace = request.workspace_id.as_str();
    if current.as_ref().is_some_and(|s| s.workspace != workspace)
        || (current.is_none() && old.iter().any(|s| s.workspace != workspace))
    {
        return Err(invalid("digest source workspace mismatch"));
    }
    match request.entry {
        EntryPoint::EnqueueDigestible => digest::sync_message_scopes(c, source, &request.id, old),
        EntryPoint::SyncPendingConversation => {
            digest::reconcile_conversations(c, old.into_iter().chain(current))
        }
        EntryPoint::DequeueOnReceipt => return Err(invalid("receipt source required")),
    }
    .map_err(sql_error)
}
