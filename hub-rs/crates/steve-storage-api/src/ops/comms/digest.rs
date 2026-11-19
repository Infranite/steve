//! Digest operation contracts. Backend triggers are provenance, not contract requirements.
use crate::{
    Domain,
    registry::{EntryKind, RegistryEntry},
};

/// Synchronize digest pending membership, receipt acknowledgement and per-conversation count/bounds atomically. Parameters declared here; backend implementation is separate.
pub const SYNC_PENDING_MEMBERSHIP: RegistryEntry = RegistryEntry {
    name: "comms.digest.sync_pending_membership",
    domain: Domain::Comms,
    kind: EntryKind::Operation,
    parameter_type: "SyncPendingMembership",
    semantics: "Synchronize digest pending membership, receipt acknowledgement and per-conversation count/bounds atomically.",
    implemented: false,
    triggers: &[
        "digest_pending_insert",
        "digest_pending_delete",
        "digest_receipt_ack",
        "digest_chan_messages_insert",
        "digest_chan_messages_update",
        "digest_chan_messages_delete",
        "digest_mail_messages_insert",
        "digest_mail_messages_update",
        "digest_mail_messages_delete",
    ],
};

use crate::{Error, uow::Op};
use serde::{Deserialize, Serialize};
use steve_types::WorkspaceId;

/// Source row kind. Receipt id identifies its (conv, message_id) pair encoded as JSON
/// text of a two-string tuple, since digest_receipts has no standalone id column.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    ChanMessage,
    MailMessage,
    Receipt,
}
/// Three entrances to the same atomic operation family, not three independent capabilities.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryPoint {
    EnqueueDigestible,
    DequeueOnReceipt,
    SyncPendingConversation,
}
/// Membership synchronization matching migration 112:
/// - enqueue: received/sent messages without receipts enter the pending inbox once;
///   channels use space:<space_id>/created, mail uses mail_thread:<thread_id>/date;
/// - moves, recalls and deletions remove old membership before inserting eligible new
///   membership, using the old pending row to recover its original scope;
/// - receipt: remove exactly (workspace, conv, message_id), never a whole conversation;
/// - every insertion/deletion updates conversation count/min/max; remove zero-count
///   conversations. Insert positions are durable, monotone, and not reused after deletion.
/// sync_pending_conversation reconciles the conversation(s) identified by the source
/// against pending rows; it never re-enqueues acknowledged messages or changes FIFO order.
/// Call in the same transaction as the source change. Backends must retain old source
/// scope through deletion/move processing. Initial cursor lookback is migration-only.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncPendingMembership {
    pub source: Source,
    pub id: String,
    pub workspace_id: WorkspaceId,
    pub entry: EntryPoint,
}
impl Op for SyncPendingMembership {
    const NAME: &'static str = "comms.digest.sync_pending_membership";
    const DOMAIN: Domain = Domain::Comms;
}
impl SyncPendingMembership {
    /// Message intake/update/delete; Receipt is rejected here instead of misinterpreted.
    pub fn enqueue_digestible(
        source: Source,
        id: String,
        workspace_id: WorkspaceId,
    ) -> Result<Self, Error> {
        if source == Source::Receipt {
            return Err(crate::error(
                steve_types::Code::Invalid,
                "receipt is not a message source",
            ));
        }
        Ok(Self {
            source,
            id,
            workspace_id,
            entry: EntryPoint::EnqueueDigestible,
        })
    }
    /// Exact receipt acknowledgement. Encoding the composite identity avoids ambiguous separators.
    pub fn dequeue_on_receipt(
        conv: &str,
        message_id: &str,
        workspace_id: WorkspaceId,
    ) -> Result<Self, Error> {
        let id = serde_json::to_string(&(conv, message_id))
            .map_err(|_| crate::error(steve_types::Code::Invalid, "invalid receipt identity"))?;
        Ok(Self {
            source: Source::Receipt,
            id,
            workspace_id,
            entry: EntryPoint::DequeueOnReceipt,
        })
    }
    /// Reconcile per-conversation metadata for the source row without changing FIFO positions.
    pub fn sync_pending_conversation(
        source: Source,
        id: String,
        workspace_id: WorkspaceId,
    ) -> Self {
        Self {
            source,
            id,
            workspace_id,
            entry: EntryPoint::SyncPendingConversation,
        }
    }
}
