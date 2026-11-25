//! Durable thread storage: submit idempotency keys, per-space serial reply tasks,
//! immutable reply receipts, origin-level dispatch & outbound delivery refs, and typed notices.

use sea_query::{Alias, Expr, ExprTrait, OnConflict, Order, Query};
use serde::Serialize;
use serde_json::{Value, json};
use steve_types::{DomainEvent, WorkEvent};

use crate::contract::{Q, Row};
use crate::types::truthy;
use crate::{Read, Result, StoreError, W, now, sha, uid};

/// The main conversation space.
pub const MAIN_SPACE: &str = "steve";

// ── Types ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct SubmitInput<'a> {
    pub ws: &'a str,
    pub space: &'a str,
    pub actor_key: &'a str,
    pub submit_key: &'a str,
    pub payload_digest: &'a str,
    pub text: &'a str,
    pub item_id: Option<&'a str>,
    pub meta: &'a Value,
    pub user_id: Option<&'a str>,
    pub project_id: Option<&'a str>,
    pub conv_json: Option<&'a str>,
    pub to_item: Option<&'a str>,
    pub to_reply_to: Option<&'a str>,
    pub file_ids: &'a [String],
    pub files_json: Option<&'a str>,
    pub mentions_json: Option<&'a str>,
    pub enqueue_task: bool,
    pub validate_refs: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmitOutcome {
    Accepted {
        message_id: String,
        task_id: Option<String>,
        input_seq: i64,
    },
    Duplicate {
        message_id: String,
        task_id: Option<String>,
        input_seq: i64,
    },
    Conflict {
        reason: &'static str,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditQueuedOutcome {
    Updated,
    NotQueued,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WithdrawOutcome {
    Withdrawn { task_id: String, space: String },
    NotQueued,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StopTaskOutcome {
    CancelClaimed {
        task_id: String,
        space: String,
    },
    StoppedQueued {
        receipt: ReplyReceipt,
        reply_message_id: String,
        space: String,
    },
    AlreadyStopped,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ThreadTask {
    pub id: String,
    pub workspace_id: String,
    pub space: String,
    pub origin_message_id: String,
    pub reply_revision: i64,
    pub origin_version: i64,
    pub input_seq: i64,
    /// queued | claimed | settled | withdrawn
    pub status: String,
    pub generation: i64,
    pub lease_token: Option<String>,
    #[serde(skip)]
    pub token_hash: Option<String>,
    pub cancel_requested_at: Option<i64>,
    pub cancel_reason: Option<String>,
    pub user_id: Option<String>,
    pub project_id: Option<String>,
    pub conv_json: Option<String>,
    pub to_item: Option<String>,
    pub to_reply_to: Option<String>,
    pub files_json: Option<String>,
    pub mentions_json: Option<String>,
    pub reply_to_message_id: Option<String>,
    pub forced_kind: Option<String>,
    pub route_json: Option<String>,
    pub offer_json: Option<String>,
    pub prev_receipt_id: Option<String>,
    pub read_task_intent_key: Option<String>,
    pub receipt_id: Option<String>,
    pub created: i64,
    pub claimed_at: Option<i64>,
    pub updated: i64,
}

#[derive(Debug, Clone)]
pub struct NewRevisionTask<'a> {
    pub ws: &'a str,
    pub space: &'a str,
    pub origin_message_id: &'a str,
    pub user_id: Option<&'a str>,
    pub project_id: Option<&'a str>,
    pub conv_json: Option<&'a str>,
    pub to_item: Option<&'a str>,
    pub to_reply_to: Option<&'a str>,
    pub files_json: Option<&'a str>,
    pub mentions_json: Option<&'a str>,
    pub reply_to_message_id: Option<&'a str>,
    pub forced_kind: Option<&'a str>,
    pub route_json: Option<&'a str>,
    pub offer_json: Option<&'a str>,
    pub prev_receipt_id: Option<&'a str>,
    pub read_task_intent_key: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReplyReceipt {
    pub id: String,
    pub task_id: String,
    pub workspace_id: String,
    pub space: String,
    pub origin_message_id: String,
    pub reply_revision: i64,
    /// answered | handed_off | failed | unknown | stopped
    pub outcome: String,
    pub reply_message_id: Option<String>,
    pub error_code: Option<String>,
    pub route_kind: String,
    pub model_kind: String,
    pub route_guard: Option<String>,
    pub route_model: Option<String>,
    pub route_confidence: Option<f64>,
    pub route_signals: Vec<String>,
    pub route_fallback: bool,
    pub route_ms: i64,
    #[serde(rename = "override")]
    pub route_override: Option<String>,
    pub route_override_at: Option<i64>,
    pub route_outcome: Option<String>,
    pub item_id: Option<String>,
    pub dispatch_ref_id: Option<String>,
    pub delivery_ref_id: Option<String>,
    pub model_call_id: Option<String>,
    pub user_id: Option<String>,
    pub project_id: Option<String>,
    pub answer_model: Option<String>,
    pub harness: Option<String>,
    pub harness_session: Option<String>,
    pub tools: i64,
    pub turns: i64,
    pub first_text_ms: Option<i64>,
    pub error_detail: Option<String>,
    pub generation: i64,
    pub committed_at: i64,
}

/// Explicit dispatch linkage for a reply receipt. Callers cannot silently omit a dispatch ref
/// via `Option::None`; they must state `NoDispatch` or pass the durable `thread_dispatch_refs.id`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiptDispatch<'a> {
    /// The reply performed no item dispatch (`create_item` / `append_item`).
    NoDispatch,
    /// The reply performed an item dispatch backed by `thread_dispatch_refs.id`.
    Dispatched(&'a str),
}

impl<'a> ReceiptDispatch<'a> {
    pub fn as_deref(self) -> Option<&'a str> {
        match self {
            Self::NoDispatch => None,
            Self::Dispatched(id) => Some(id),
        }
    }
}

#[derive(Debug, Clone)]
pub struct CommitReplyInput<'a> {
    pub task_id: &'a str,
    pub lease_token: &'a str,
    pub expected_generation: i64,
    pub allow_cancelled_stop: bool,
    pub outcome: &'a str,
    pub reply_text: Option<&'a str>,
    pub existing_reply_id: Option<&'a str>,
    pub reply_item_id: Option<&'a str>,
    pub reply_meta: &'a Value,
    pub error_code: Option<&'a str>,
    pub route_kind: &'a str,
    pub model_kind: &'a str,
    pub route_guard: Option<&'a str>,
    pub route_model: Option<&'a str>,
    pub route_confidence: Option<f64>,
    pub route_signals: &'a [String],
    pub route_fallback: bool,
    pub route_ms: i64,
    pub route_outcome: Option<&'a str>,
    pub item_id: Option<&'a str>,
    pub dispatch: ReceiptDispatch<'a>,
    pub delivery_ref_id: Option<&'a str>,
    pub model_call_id: Option<&'a str>,
    pub user_id: Option<&'a str>,
    pub project_id: Option<&'a str>,
    pub answer_model: Option<&'a str>,
    pub harness: Option<&'a str>,
    pub harness_session: Option<&'a str>,
    pub tools: i64,
    pub turns: i64,
    pub first_text_ms: Option<i64>,
    pub error_detail: Option<&'a str>,
    pub record_attention_commit: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CommitOutcome {
    Committed {
        receipt: ReplyReceipt,
        reply_message_id: Option<String>,
    },
    Fenced,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RecoveredTask {
    pub task_id: String,
    pub workspace_id: String,
    pub space: String,
    pub origin_message_id: String,
    pub receipt_id: String,
    pub reply_message_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DispatchRef {
    pub id: String,
    pub workspace_id: String,
    pub origin_message_id: String,
    pub effect_kind: String,
    pub target_item_id: Option<String>,
    pub reply_message_id: Option<String>,
    pub from_question: bool,
    pub follow: bool,
    pub last_post: Option<i64>,
    pub state: String,
    pub created: i64,
    pub updated: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DispatchClaim {
    Claimed(DispatchRef),
    Existing(DispatchRef),
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DeliveryRefRow {
    pub id: String,
    pub workspace_id: String,
    pub space: String,
    pub origin_message_id: String,
    pub purpose: String,
    pub outbound_op_id: String,
    pub outbound_message_id: Option<String>,
    pub state: String,
    pub created: i64,
    pub updated: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DeliveryRefClaim {
    Claimed(DeliveryRefRow),
    Existing(DeliveryRefRow),
    Blocked(DeliveryRefRow),
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ThreadNotice {
    pub id: String,
    pub workspace_id: String,
    pub causal_key: Option<String>,
    pub host_message_id: Option<String>,
    pub kind: String,
    pub level: String,
    pub item_id: Option<String>,
    pub text: String,
    pub closed_at: Option<i64>,
    pub closed_by: Option<String>,
    pub created: i64,
}

// ── Column projections & decoders ────────────────────────────────────────────

const TASK_COLS: [&str; 29] = [
    "id",
    "workspace_id",
    "space",
    "origin_message_id",
    "reply_revision",
    "origin_version",
    "input_seq",
    "status",
    "generation",
    "lease_token",
    "token_hash",
    "cancel_requested_at",
    "cancel_reason",
    "user_id",
    "project_id",
    "conv_json",
    "to_item",
    "to_reply_to",
    "files_json",
    "mentions_json",
    "reply_to_message_id",
    "forced_kind",
    "route_json",
    "offer_json",
    "prev_receipt_id",
    "read_task_intent_key",
    "receipt_id",
    "created",
    "claimed_at",
];

fn decode_task(r: &Row<'_>) -> Result<ThreadTask> {
    Ok(ThreadTask {
        id: r.get(0)?,
        workspace_id: r.get(1)?,
        space: r.get(2)?,
        origin_message_id: r.get(3)?,
        reply_revision: r.get(4)?,
        origin_version: r.get(5)?,
        input_seq: r.get(6)?,
        status: r.get(7)?,
        generation: r.get(8)?,
        lease_token: r.get(9)?,
        token_hash: r.get(10)?,
        cancel_requested_at: r.get(11)?,
        cancel_reason: r.get(12)?,
        user_id: r.get(13)?,
        project_id: r.get(14)?,
        conv_json: r.get(15)?,
        to_item: r.get(16)?,
        to_reply_to: r.get(17)?,
        files_json: r.get(18)?,
        mentions_json: r.get(19)?,
        reply_to_message_id: r.get(20)?,
        forced_kind: r.get(21)?,
        route_json: r.get(22)?,
        offer_json: r.get(23)?,
        prev_receipt_id: r.get(24)?,
        read_task_intent_key: r.get(25)?,
        receipt_id: r.get(26)?,
        created: r.get(27)?,
        claimed_at: r.get(28)?,
        updated: r.get(29)?,
    })
}

fn task_select() -> sea_query::SelectStatement {
    let mut q = Query::select();
    q.columns(TASK_COLS)
        .column("updated")
        .from("thread_reply_tasks");
    q
}

const RECEIPT_COLS: [&str; 34] = [
    "id",
    "task_id",
    "workspace_id",
    "space",
    "origin_message_id",
    "reply_revision",
    "outcome",
    "reply_message_id",
    "error_code",
    "route_kind",
    "model_kind",
    "route_guard",
    "route_model",
    "route_confidence",
    "route_signals",
    "route_fallback",
    "route_ms",
    "route_override",
    "route_override_at",
    "route_outcome",
    "item_id",
    "dispatch_ref_id",
    "delivery_ref_id",
    "model_call_id",
    "user_id",
    "project_id",
    "answer_model",
    "harness",
    "harness_session",
    "tools",
    "turns",
    "first_text_ms",
    "error_detail",
    "generation",
];

fn decode_receipt(r: &Row<'_>) -> Result<ReplyReceipt> {
    let signals_raw: String = r.get(14)?;
    Ok(ReplyReceipt {
        id: r.get(0)?,
        task_id: r.get(1)?,
        workspace_id: r.get(2)?,
        space: r.get(3)?,
        origin_message_id: r.get(4)?,
        reply_revision: r.get(5)?,
        outcome: r.get(6)?,
        reply_message_id: r.get(7)?,
        error_code: r.get(8)?,
        route_kind: r.get(9)?,
        model_kind: r.get(10)?,
        route_guard: r.get(11)?,
        route_model: r.get(12)?,
        route_confidence: r.get(13)?,
        route_signals: serde_json::from_str(&signals_raw).unwrap_or_default(),
        route_fallback: r.get::<i64>(15)? != 0,
        route_ms: r.get(16)?,
        route_override: r.get(17)?,
        route_override_at: r.get(18)?,
        route_outcome: r.get(19)?,
        item_id: r.get(20)?,
        dispatch_ref_id: r.get(21)?,
        delivery_ref_id: r.get(22)?,
        model_call_id: r.get(23)?,
        user_id: r.get(24)?,
        project_id: r.get(25)?,
        answer_model: r.get(26)?,
        harness: r.get(27)?,
        harness_session: r.get(28)?,
        tools: r.get(29)?,
        turns: r.get(30)?,
        first_text_ms: r.get(31)?,
        error_detail: r.get(32)?,
        generation: r.get(33)?,
        committed_at: r.get(34)?,
    })
}

fn receipt_select() -> sea_query::SelectStatement {
    let mut q = Query::select();
    q.columns(RECEIPT_COLS)
        .column("committed_at")
        .from("thread_reply_receipts");
    q
}

const DISPATCH_COLS: [&str; 12] = [
    "id",
    "workspace_id",
    "origin_message_id",
    "effect_kind",
    "target_item_id",
    "reply_message_id",
    "from_question",
    "follow",
    "last_post",
    "state",
    "created",
    "updated",
];

fn decode_dispatch(r: &Row<'_>) -> Result<DispatchRef> {
    Ok(DispatchRef {
        id: r.get(0)?,
        workspace_id: r.get(1)?,
        origin_message_id: r.get(2)?,
        effect_kind: r.get(3)?,
        target_item_id: r.get(4)?,
        reply_message_id: r.get(5)?,
        from_question: r.get::<i64>(6)? != 0,
        follow: r.get::<i64>(7)? != 0,
        last_post: r.get(8)?,
        state: r.get(9)?,
        created: r.get(10)?,
        updated: r.get(11)?,
    })
}

const DELIVERY_COLS: [&str; 10] = [
    "id",
    "workspace_id",
    "space",
    "origin_message_id",
    "purpose",
    "outbound_op_id",
    "outbound_message_id",
    "state",
    "created",
    "updated",
];

fn decode_delivery(r: &Row<'_>) -> Result<DeliveryRefRow> {
    Ok(DeliveryRefRow {
        id: r.get(0)?,
        workspace_id: r.get(1)?,
        space: r.get(2)?,
        origin_message_id: r.get(3)?,
        purpose: r.get(4)?,
        outbound_op_id: r.get(5)?,
        outbound_message_id: r.get(6)?,
        state: r.get(7)?,
        created: r.get(8)?,
        updated: r.get(9)?,
    })
}

const NOTICE_COLS: [&str; 11] = [
    "id",
    "workspace_id",
    "causal_key",
    "host_message_id",
    "kind",
    "level",
    "item_id",
    "text",
    "closed_at",
    "closed_by",
    "created",
];

fn decode_notice(r: &Row<'_>) -> Result<ThreadNotice> {
    Ok(ThreadNotice {
        id: r.get(0)?,
        workspace_id: r.get(1)?,
        causal_key: r.get(2)?,
        host_message_id: r.get(3)?,
        kind: r.get(4)?,
        level: r.get(5)?,
        item_id: r.get(6)?,
        text: r.get(7)?,
        closed_at: r.get(8)?,
        closed_by: r.get(9)?,
        created: r.get(10)?,
    })
}

// ── Internal helpers ─────────────────────────────────────────────────────────

fn next_message_created(w: &mut W<'_>, ws: &str, space: &str, at: i64) -> Result<i64> {
    let last: Option<i64> = w
        .one(
            Query::select()
                .expr(Expr::cust("MAX(created)"))
                .from("messages")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("space").eq(space)),
            |r| r.get(0),
        )?
        .flatten();
    Ok(match last {
        Some(max_created) => std::cmp::max(at, max_created + 1),
        None => at,
    })
}

fn insert_message(
    w: &mut W<'_>,
    ws: &str,
    space: &str,
    author: &str,
    text: &str,
    item_id: Option<&str>,
    meta: &Value,
    at: i64,
) -> Result<String> {
    let id = uid("msg");
    let created = next_message_created(w, ws, space, at)?;
    let meta_str = truthy(meta)
        .then(|| serde_json::to_string(meta))
        .transpose()?;
    w.exec(
        Query::insert()
            .into_table("messages")
            .columns([
                "id",
                "workspace_id",
                "space",
                "author",
                "text",
                "item_id",
                "created",
                "meta",
            ])
            .values_panic([
                id.as_str().into(),
                ws.into(),
                space.into(),
                author.into(),
                text.into(),
                item_id.into(),
                created.into(),
                meta_str.as_deref().into(),
            ]),
    )?;
    w.emit(DomainEvent::from(WorkEvent::Message {
        id: id.clone().into(),
        ws: ws.into(),
        space: space.into(),
    }));
    Ok(id)
}

fn next_input_seq(r: &mut impl Read, ws: &str, space: &str) -> Result<i64> {
    let max_submit: Option<i64> = r
        .one(
            Query::select()
                .expr(Expr::cust("MAX(input_seq)"))
                .from("thread_submit_keys")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("space").eq(space)),
            |row| row.get(0),
        )?
        .flatten();
    let max_task: Option<i64> = r
        .one(
            Query::select()
                .expr(Expr::cust("MAX(input_seq)"))
                .from("thread_reply_tasks")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("space").eq(space)),
            |row| row.get(0),
        )?
        .flatten();
    Ok(std::cmp::max(max_submit.unwrap_or(0), max_task.unwrap_or(0)) + 1)
}

fn insert_attention_commit(
    w: &mut W<'_>,
    ws: &str,
    user_id: &str,
    message_id: &str,
    receipt_id: &str,
) -> Result<()> {
    w.exec(
        Query::insert()
            .into_table("thread_reply_commits")
            .columns(["message_id", "receipt_id", "workspace_id", "user_id"])
            .values_panic([
                message_id.into(),
                receipt_id.into(),
                ws.into(),
                user_id.into(),
            ])
            .on_conflict(OnConflict::column("message_id").do_nothing().to_owned()),
    )?;
    Ok(())
}

// ── Submit, edit, withdraw ───────────────────────────────────────────────────

pub fn submit_message(w: &mut W<'_>, input: &SubmitInput<'_>) -> Result<SubmitOutcome> {
    // 1. Check durable submit idempotency key first (also rejects withdrawn tombstones).
    let existing: Option<(String, String, String, i64)> = w.one(
        Query::select()
            .columns(["message_id", "payload_digest", "state", "input_seq"])
            .from("thread_submit_keys")
            .and_where(Expr::col("workspace_id").eq(input.ws))
            .and_where(Expr::col("space").eq(input.space))
            .and_where(Expr::col("actor_key").eq(input.actor_key))
            .and_where(Expr::col("submit_key").eq(input.submit_key)),
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    if let Some((message_id, digest, state, input_seq)) = existing {
        if state == "withdrawn" {
            return Ok(SubmitOutcome::Conflict {
                reason: "withdrawn",
            });
        }
        if digest != input.payload_digest {
            return Ok(SubmitOutcome::Conflict {
                reason: "payload_mismatch",
            });
        }
        let task_id: Option<String> = w.one(
            Query::select()
                .column("id")
                .from("thread_reply_tasks")
                .and_where(Expr::col("workspace_id").eq(input.ws))
                .and_where(Expr::col("space").eq(input.space))
                .and_where(Expr::col("origin_message_id").eq(&message_id))
                .and_where(Expr::col("reply_revision").eq(0_i64)),
            |r| r.get(0),
        )?;
        return Ok(SubmitOutcome::Duplicate {
            message_id,
            task_id,
            input_seq,
        });
    }

    // 2. Fail-closed cross-workspace / cross-space reference validation.
    if input.validate_refs {
        if let Some(item_id) = input.to_item {
            let exists = w
                .one(
                    Query::select()
                        .column("id")
                        .from("items")
                        .and_where(Expr::col("id").eq(item_id))
                        .and_where(Expr::col("workspace_id").eq(input.ws)),
                    |_| Ok(()),
                )?
                .is_some();
            if !exists {
                return Ok(SubmitOutcome::Conflict {
                    reason: "invalid_item",
                });
            }
        }
        if let Some(reply_to) = input.to_reply_to {
            let exists = w
                .one(
                    Query::select()
                        .column("id")
                        .from("messages")
                        .and_where(Expr::col("id").eq(reply_to))
                        .and_where(Expr::col("workspace_id").eq(input.ws))
                        .and_where(Expr::col("space").eq(input.space)),
                    |_| Ok(()),
                )?
                .is_some();
            if !exists {
                return Ok(SubmitOutcome::Conflict {
                    reason: "invalid_reply_to",
                });
            }
        }
        for fid in input.file_ids {
            let file_ws: Option<String> = w.one(
                Query::select()
                    .column("workspace_id")
                    .from("files")
                    .and_where(Expr::col("id").eq(fid.as_str())),
                |r| r.get(0),
            )?;
            if file_ws.as_deref() != Some(input.ws) {
                return Ok(SubmitOutcome::Conflict {
                    reason: "invalid_file",
                });
            }
        }
    }

    // 3. Atomic write: messages + thread_submit_keys + thread_reply_tasks (+ WorkEvent).
    let at = now();
    let input_seq = next_input_seq(w, input.ws, input.space)?;
    let message_id = insert_message(
        w,
        input.ws,
        input.space,
        "me",
        input.text,
        input.item_id,
        input.meta,
        at,
    )?;
    w.exec(
        Query::insert()
            .into_table("thread_submit_keys")
            .columns([
                "workspace_id",
                "space",
                "actor_key",
                "submit_key",
                "payload_digest",
                "message_id",
                "state",
                "input_seq",
                "input_version",
                "created",
                "updated",
            ])
            .values_panic([
                input.ws.into(),
                input.space.into(),
                input.actor_key.into(),
                input.submit_key.into(),
                input.payload_digest.into(),
                message_id.as_str().into(),
                "active".into(),
                input_seq.into(),
                1_i64.into(),
                at.into(),
                at.into(),
            ]),
    )?;
    let task_id = if input.enqueue_task {
        let tid = uid("tr");
        w.exec(
            Query::insert()
                .into_table("thread_reply_tasks")
                .columns([
                    "id",
                    "workspace_id",
                    "space",
                    "origin_message_id",
                    "reply_revision",
                    "origin_version",
                    "input_seq",
                    "status",
                    "generation",
                    "user_id",
                    "project_id",
                    "conv_json",
                    "to_item",
                    "to_reply_to",
                    "files_json",
                    "mentions_json",
                    "created",
                    "updated",
                ])
                .values_panic([
                    tid.as_str().into(),
                    input.ws.into(),
                    input.space.into(),
                    message_id.as_str().into(),
                    0_i64.into(),
                    1_i64.into(),
                    input_seq.into(),
                    "queued".into(),
                    1_i64.into(),
                    input.user_id.into(),
                    input.project_id.into(),
                    input.conv_json.into(),
                    input.to_item.into(),
                    input.to_reply_to.into(),
                    input.files_json.into(),
                    input.mentions_json.into(),
                    at.into(),
                    at.into(),
                ]),
        )?;
        Some(tid)
    } else {
        None
    };

    Ok(SubmitOutcome::Accepted {
        message_id,
        task_id,
        input_seq,
    })
}

pub fn edit_queued(
    w: &mut W<'_>,
    ws: &str,
    message_id: &str,
    new_text: &str,
    new_digest: &str,
) -> Result<EditQueuedOutcome> {
    let task: Option<(String, String)> = w.one(
        Query::select()
            .columns(["id", "space"])
            .from("thread_reply_tasks")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("origin_message_id").eq(message_id))
            .and_where(Expr::col("reply_revision").eq(0_i64))
            .and_where(Expr::col("status").eq("queued")),
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let Some((task_id, space)) = task else {
        return Ok(EditQueuedOutcome::NotQueued);
    };
    let at = now();
    w.exec(
        Query::update()
            .table("messages")
            .value("text", new_text)
            .and_where(Expr::col("id").eq(message_id))
            .and_where(Expr::col("workspace_id").eq(ws)),
    )?;
    w.exec(
        Query::update()
            .table("thread_submit_keys")
            .value("payload_digest", new_digest)
            .value("input_version", Expr::col("input_version").add(1_i64))
            .value("updated", at)
            .and_where(Expr::col("message_id").eq(message_id)),
    )?;
    w.exec(
        Query::update()
            .table("thread_reply_tasks")
            .value("origin_version", Expr::col("origin_version").add(1_i64))
            .value("updated", at)
            .and_where(Expr::col("id").eq(&task_id))
            .and_where(Expr::col("status").eq("queued")),
    )?;
    w.emit(DomainEvent::from(WorkEvent::Message {
        id: message_id.into(),
        ws: ws.into(),
        space,
    }));
    Ok(EditQueuedOutcome::Updated)
}

pub fn withdraw_queued(w: &mut W<'_>, ws: &str, message_id: &str) -> Result<WithdrawOutcome> {
    let task: Option<(String, String)> = w.one(
        Query::select()
            .columns(["id", "space"])
            .from("thread_reply_tasks")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("origin_message_id").eq(message_id))
            .and_where(Expr::col("reply_revision").eq(0_i64))
            .and_where(Expr::col("status").eq("queued")),
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let Some((task_id, space)) = task else {
        return Ok(WithdrawOutcome::NotQueued);
    };
    let at = now();
    w.exec(
        Query::update()
            .table("thread_reply_tasks")
            .value("status", "withdrawn")
            .value("generation", Expr::col("generation").add(1_i64))
            .value("updated", at)
            .and_where(Expr::col("id").eq(&task_id))
            .and_where(Expr::col("status").eq("queued")),
    )?;
    w.exec(
        Query::update()
            .table("thread_submit_keys")
            .value("state", "withdrawn")
            .value("updated", at)
            .and_where(Expr::col("message_id").eq(message_id)),
    )?;
    let existing_meta: Option<Option<String>> = w.one(
        Query::select()
            .column("meta")
            .from("messages")
            .and_where(Expr::col("id").eq(message_id))
            .and_where(Expr::col("workspace_id").eq(ws)),
        |r| r.get(0),
    )?;
    let mut meta = existing_meta
        .flatten()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    meta.remove("held");
    meta.remove("queued");
    meta.insert("hidden".into(), json!(true));
    meta.insert("withdrawn".into(), json!(true));
    let meta_json = serde_json::to_string(&Value::Object(meta))?;
    w.exec(
        Query::update()
            .table("messages")
            .value("meta", meta_json)
            .and_where(Expr::col("id").eq(message_id))
            .and_where(Expr::col("workspace_id").eq(ws)),
    )?;
    w.emit(DomainEvent::from(WorkEvent::Message {
        id: message_id.into(),
        ws: ws.into(),
        space: space.clone(),
    }));
    Ok(WithdrawOutcome::Withdrawn { task_id, space })
}

// ── Reply task queue, claim, stop, fence, revision ───────────────────────────

pub fn claim_head(
    w: &mut W<'_>,
    ws: &str,
    space: &str,
    lease_token: &str,
    at: i64,
) -> Result<Option<ThreadTask>> {
    let head = w.one(
        task_select()
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("space").eq(space))
            .and_where(Expr::col("status").is_in(["queued", "claimed"]))
            .order_by("input_seq", Order::Asc)
            .order_by("created", Order::Asc)
            .order_by("id", Order::Asc)
            .limit(1),
        decode_task,
    )?;
    let Some(head) = head else {
        return Ok(None);
    };
    if head.status != "queued" {
        return Ok(None);
    }
    let updated = w.exec(
        Query::update()
            .table("thread_reply_tasks")
            .value("status", "claimed")
            .value("lease_token", lease_token)
            .value("generation", Expr::col("generation").add(1_i64))
            .value("claimed_at", at)
            .value("updated", at)
            .and_where(Expr::col("id").eq(&head.id))
            .and_where(Expr::col("status").eq("queued")),
    )?;
    if updated == 0 {
        return Ok(None);
    }
    // A message that waited behind an earlier turn (`reply_revision == 0`) leaves the held strip
    // and enters the timeline after the previous turn's reply, keeping chronological order and
    // letting `history_before` / `resume_for` see what Steve just answered.
    if head.reply_revision == 0
        && let Some(origin_created) = w.one(
            Query::select()
                .column("created")
                .from("messages")
                .and_where(Expr::col("id").eq(&head.origin_message_id))
                .and_where(Expr::col("workspace_id").eq(ws)),
            |r| r.get::<i64>(0),
        )?
        && let Some(max_other) = w
            .one(
                Query::select()
                    .expr(Expr::cust("MAX(created)"))
                    .from("messages")
                    .and_where(Expr::col("workspace_id").eq(ws))
                    .and_where(Expr::col("space").eq(space))
                    .and_where(Expr::col("id").ne(&head.origin_message_id)),
                |r| r.get::<Option<i64>>(0),
            )?
            .flatten()
        && max_other >= origin_created
    {
        let released_at = std::cmp::max(at, max_other + 1);
        w.exec(
            Query::update()
                .table("messages")
                .value("created", released_at)
                .and_where(Expr::col("id").eq(&head.origin_message_id))
                .and_where(Expr::col("workspace_id").eq(ws)),
        )?;
    }
    w.emit(DomainEvent::from(WorkEvent::Message {
        id: head.origin_message_id.clone().into(),
        ws: head.workspace_id.clone().into(),
        space: head.space.clone(),
    }));
    task_by_id(w, &head.id)
}

pub fn has_active_or_queued(r: &mut impl Read, ws: &str, space: &str) -> Result<bool> {
    Ok(r.one(
        Query::select()
            .column("id")
            .from("thread_reply_tasks")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("space").eq(space))
            .and_where(Expr::col("status").is_in(["queued", "claimed"]))
            .limit(1),
        |_| Ok(()),
    )?
    .is_some())
}

pub fn has_claimed(r: &mut impl Read, ws: &str, space: &str) -> Result<bool> {
    Ok(r.one(
        Query::select()
            .column("id")
            .from("thread_reply_tasks")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("space").eq(space))
            .and_where(Expr::col("status").eq("claimed"))
            .limit(1),
        |_| Ok(()),
    )?
    .is_some())
}

pub fn list_active_tasks(r: &mut impl Read, ws: &str) -> Result<Vec<ThreadTask>> {
    r.all(
        task_select()
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("status").is_in(["queued", "claimed"]))
            .order_by("input_seq", Order::Asc)
            .order_by("created", Order::Asc)
            .order_by("id", Order::Asc),
        decode_task,
    )
}

pub fn list_queued_spaces(r: &mut impl Read) -> Result<Vec<(String, String)>> {
    r.all(
        Query::select()
            .distinct()
            .columns(["workspace_id", "space"])
            .from("thread_reply_tasks")
            .and_where(Expr::col("status").eq("queued")),
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
}

pub fn task_by_id(r: &mut impl Read, id: &str) -> Result<Option<ThreadTask>> {
    r.one(task_select().and_where(Expr::col("id").eq(id)), decode_task)
}

pub fn tasks_of_origin(r: &mut impl Read, origin_message_id: &str) -> Result<Vec<ThreadTask>> {
    r.all(
        task_select()
            .and_where(Expr::col("origin_message_id").eq(origin_message_id))
            .order_by("reply_revision", Order::Asc)
            .order_by("created", Order::Asc),
        decode_task,
    )
}

pub fn set_task_token(
    w: &mut W<'_>,
    task_id: &str,
    lease_token: &str,
    generation: i64,
    token_hash: Option<&str>,
) -> Result<bool> {
    let mut q = Query::update();
    q.table("thread_reply_tasks")
        .value("token_hash", token_hash)
        .and_where(Expr::col("id").eq(task_id));
    if token_hash.is_some() {
        q.and_where(Expr::col("status").eq("claimed"))
            .and_where(Expr::col("lease_token").eq(lease_token))
            .and_where(Expr::col("generation").eq(generation))
            .and_where(Expr::col("cancel_requested_at").is_null());
    }
    Ok(w.exec(&q)? > 0)
}

pub fn active_task_by_token(r: &mut impl Read, token: &str) -> Result<Option<ThreadTask>> {
    let hash = sha(token);
    r.one(
        task_select()
            .and_where(Expr::col("token_hash").eq(hash))
            .and_where(Expr::col("status").eq("claimed"))
            .and_where(Expr::col("cancel_requested_at").is_null()),
        decode_task,
    )
}

pub fn request_stop_space(
    w: &mut W<'_>,
    ws: &str,
    space: &str,
    reason: &str,
    at: i64,
) -> Result<Option<String>> {
    let task_id: Option<String> = w.one(
        Query::select()
            .column("id")
            .from("thread_reply_tasks")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("space").eq(space))
            .and_where(Expr::col("status").eq("claimed"))
            .and_where(Expr::col("cancel_requested_at").is_null())
            .limit(1),
        |r| r.get(0),
    )?;
    let Some(task_id) = task_id else {
        return Ok(None);
    };
    let changed = w.exec(
        Query::update()
            .table("thread_reply_tasks")
            .value("cancel_requested_at", at)
            .value("cancel_reason", reason)
            .value("token_hash", None::<String>)
            .value("updated", at)
            .and_where(Expr::col("id").eq(&task_id))
            .and_where(Expr::col("status").eq("claimed"))
            .and_where(Expr::col("cancel_requested_at").is_null()),
    )?;
    Ok((changed > 0).then_some(task_id))
}

pub fn request_stop_task(
    w: &mut W<'_>,
    ws: &str,
    task_id: &str,
    reason: &str,
    stopped_text: &str,
    at: i64,
) -> Result<StopTaskOutcome> {
    let Some(task) = task_by_id(w, task_id)? else {
        return Ok(StopTaskOutcome::AlreadyStopped);
    };
    if task.workspace_id != ws {
        return Ok(StopTaskOutcome::AlreadyStopped);
    }
    match task.status.as_str() {
        "claimed" => {
            if task.cancel_requested_at.is_some() {
                return Ok(StopTaskOutcome::AlreadyStopped);
            }
            let changed = w.exec(
                Query::update()
                    .table("thread_reply_tasks")
                    .value("cancel_requested_at", at)
                    .value("cancel_reason", reason)
                    .value("token_hash", None::<String>)
                    .value("updated", at)
                    .and_where(Expr::col("id").eq(&task.id))
                    .and_where(Expr::col("status").eq("claimed"))
                    .and_where(Expr::col("cancel_requested_at").is_null()),
            )?;
            if changed > 0 {
                Ok(StopTaskOutcome::CancelClaimed {
                    task_id: task.id,
                    space: task.space,
                })
            } else {
                Ok(StopTaskOutcome::AlreadyStopped)
            }
        }
        "queued" => {
            let receipt_id = uid("rcpt");
            let meta = json!({
                "origin": task.origin_message_id,
                "run": task.id,
                "receipt": receipt_id,
                "run_receipt": true,
                "stopped": true,
            });
            let reply_id = insert_message(
                w,
                &task.workspace_id,
                &task.space,
                "steve",
                stopped_text,
                None,
                &meta,
                at,
            )?;
            let next_gen = task.generation + 1;
            w.exec(
                Query::insert()
                    .into_table("thread_reply_receipts")
                    .columns([
                        "id",
                        "task_id",
                        "workspace_id",
                        "space",
                        "origin_message_id",
                        "reply_revision",
                        "outcome",
                        "reply_message_id",
                        "route_kind",
                        "model_kind",
                        "route_signals",
                        "route_fallback",
                        "route_ms",
                        "user_id",
                        "project_id",
                        "tools",
                        "turns",
                        "generation",
                        "committed_at",
                    ])
                    .values_panic([
                        receipt_id.as_str().into(),
                        task.id.as_str().into(),
                        task.workspace_id.as_str().into(),
                        task.space.as_str().into(),
                        task.origin_message_id.as_str().into(),
                        task.reply_revision.into(),
                        "stopped".into(),
                        Some(reply_id.as_str()).into(),
                        task.forced_kind.as_deref().unwrap_or("research").into(),
                        task.forced_kind.as_deref().unwrap_or("research").into(),
                        "[]".into(),
                        0_i64.into(),
                        0_i64.into(),
                        task.user_id.as_deref().into(),
                        task.project_id.as_deref().into(),
                        0_i64.into(),
                        0_i64.into(),
                        next_gen.into(),
                        at.into(),
                    ]),
            )?;
            w.exec(
                Query::update()
                    .table("thread_reply_tasks")
                    .value("status", "settled")
                    .value("generation", next_gen)
                    .value("cancel_requested_at", at)
                    .value("cancel_reason", reason)
                    .value("receipt_id", receipt_id.as_str())
                    .value("updated", at)
                    .and_where(Expr::col("id").eq(&task.id))
                    .and_where(Expr::col("status").eq("queued")),
            )?;
            if task.space == MAIN_SPACE
                && let Some(ref user_id) = task.user_id
            {
                insert_attention_commit(w, &task.workspace_id, user_id, &reply_id, &receipt_id)?;
            }
            let receipt = receipt_by_id(w, &receipt_id)?
                .ok_or_else(|| StoreError::Storage("inserted stopped receipt missing".into()))?;
            Ok(StopTaskOutcome::StoppedQueued {
                receipt,
                reply_message_id: reply_id,
                space: task.space,
            })
        }
        _ => Ok(StopTaskOutcome::AlreadyStopped),
    }
}

pub fn fence_origin_tasks(
    w: &mut W<'_>,
    ws: &str,
    origin_message_id: &str,
    reason: &str,
    at: i64,
) -> Result<Vec<String>> {
    let ids: Vec<String> = w.all(
        Query::select()
            .column("id")
            .from("thread_reply_tasks")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("origin_message_id").eq(origin_message_id))
            .and_where(Expr::col("status").is_in(["queued", "claimed"])),
        |r| r.get(0),
    )?;
    if ids.is_empty() {
        return Ok(ids);
    }
    w.exec(
        Query::update()
            .table("thread_reply_tasks")
            .value("status", "settled")
            .value("generation", Expr::col("generation").add(1_i64))
            .value("lease_token", None::<String>)
            .value("token_hash", None::<String>)
            .value(
                "cancel_requested_at",
                Expr::cust_with_values("COALESCE(cancel_requested_at, ?)", [at]),
            )
            .value(
                "cancel_reason",
                Expr::cust_with_values("COALESCE(cancel_reason, ?)", [reason]),
            )
            .value("updated", at)
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("origin_message_id").eq(origin_message_id))
            .and_where(Expr::col("status").is_in(["queued", "claimed"])),
    )?;
    Ok(ids)
}

pub fn enqueue_revision(w: &mut W<'_>, input: &NewRevisionTask<'_>) -> Result<ThreadTask> {
    let at = now();
    let max_rev: Option<i64> = w
        .one(
            Query::select()
                .expr(Expr::cust("MAX(reply_revision)"))
                .from("thread_reply_tasks")
                .and_where(Expr::col("workspace_id").eq(input.ws))
                .and_where(Expr::col("space").eq(input.space))
                .and_where(Expr::col("origin_message_id").eq(input.origin_message_id)),
            |r| r.get(0),
        )?
        .flatten();
    let rev = max_rev.map_or(0, |v| v + 1);
    let input_seq = next_input_seq(w, input.ws, input.space)?;
    let id = uid("tr");
    w.exec(
        Query::insert()
            .into_table("thread_reply_tasks")
            .columns([
                "id",
                "workspace_id",
                "space",
                "origin_message_id",
                "reply_revision",
                "origin_version",
                "input_seq",
                "status",
                "generation",
                "user_id",
                "project_id",
                "conv_json",
                "to_item",
                "to_reply_to",
                "files_json",
                "mentions_json",
                "reply_to_message_id",
                "forced_kind",
                "route_json",
                "offer_json",
                "prev_receipt_id",
                "read_task_intent_key",
                "created",
                "updated",
            ])
            .values_panic([
                id.as_str().into(),
                input.ws.into(),
                input.space.into(),
                input.origin_message_id.into(),
                rev.into(),
                1_i64.into(),
                input_seq.into(),
                "queued".into(),
                1_i64.into(),
                input.user_id.into(),
                input.project_id.into(),
                input.conv_json.into(),
                input.to_item.into(),
                input.to_reply_to.into(),
                input.files_json.into(),
                input.mentions_json.into(),
                input.reply_to_message_id.into(),
                input.forced_kind.into(),
                input.route_json.into(),
                input.offer_json.into(),
                input.prev_receipt_id.into(),
                input.read_task_intent_key.into(),
                at.into(),
                at.into(),
            ]),
    )?;
    task_by_id(w, &id)?.ok_or_else(|| StoreError::Storage("inserted revision task missing".into()))
}

// ── Atomic commit & boot recovery ────────────────────────────────────────────

pub fn commit_reply(w: &mut W<'_>, input: &CommitReplyInput<'_>) -> Result<CommitOutcome> {
    let Some(task) = task_by_id(w, input.task_id)? else {
        return Ok(CommitOutcome::Fenced);
    };
    if task.status != "claimed"
        || task.lease_token.as_deref() != Some(input.lease_token)
        || task.generation != input.expected_generation
        || (!input.allow_cancelled_stop && task.cancel_requested_at.is_some())
    {
        return Ok(CommitOutcome::Fenced);
    }

    let dispatch_ref_id = validate_receipt_dispatch(
        w,
        Some(&task.workspace_id),
        input.outcome == "handed_off",
        input.dispatch,
    )?;

    let at = now();
    let receipt_id = uid("rcpt");
    let reply_message_id = if let Some(text) = input.reply_text {
        let mut meta = input.reply_meta.clone();
        if let Some(obj) = meta.as_object_mut() {
            obj.entry("origin")
                .or_insert_with(|| Value::String(task.origin_message_id.clone()));
            obj.entry("run")
                .or_insert_with(|| Value::String(task.id.clone()));
            obj.insert("receipt".into(), Value::String(receipt_id.clone()));
            if input.record_attention_commit {
                obj.insert("run_receipt".into(), Value::Bool(true));
            }
        }
        Some(insert_message(
            w,
            &task.workspace_id,
            &task.space,
            "steve",
            text,
            input.reply_item_id,
            &meta,
            at,
        )?)
    } else {
        input.existing_reply_id.map(str::to_string)
    };

    let signals_json = serde_json::to_string(input.route_signals)?;
    w.exec(
        Query::insert()
            .into_table("thread_reply_receipts")
            .columns([
                "id",
                "task_id",
                "workspace_id",
                "space",
                "origin_message_id",
                "reply_revision",
                "outcome",
                "reply_message_id",
                "error_code",
                "route_kind",
                "model_kind",
                "route_guard",
                "route_model",
                "route_confidence",
                "route_signals",
                "route_fallback",
                "route_ms",
                "route_outcome",
                "item_id",
                "dispatch_ref_id",
                "delivery_ref_id",
                "model_call_id",
                "user_id",
                "project_id",
                "answer_model",
                "harness",
                "harness_session",
                "tools",
                "turns",
                "first_text_ms",
                "error_detail",
                "generation",
                "committed_at",
            ])
            .values_panic([
                receipt_id.as_str().into(),
                task.id.as_str().into(),
                task.workspace_id.as_str().into(),
                task.space.as_str().into(),
                task.origin_message_id.as_str().into(),
                task.reply_revision.into(),
                input.outcome.into(),
                reply_message_id.as_deref().into(),
                input.error_code.into(),
                input.route_kind.into(),
                input.model_kind.into(),
                input.route_guard.into(),
                input.route_model.into(),
                input.route_confidence.into(),
                signals_json.as_str().into(),
                (input.route_fallback as i64).into(),
                input.route_ms.into(),
                input.route_outcome.into(),
                input.item_id.into(),
                dispatch_ref_id.into(),
                input.delivery_ref_id.into(),
                input.model_call_id.into(),
                input.user_id.into(),
                input.project_id.into(),
                input.answer_model.into(),
                input.harness.into(),
                input.harness_session.into(),
                input.tools.into(),
                input.turns.into(),
                input.first_text_ms.into(),
                input.error_detail.into(),
                task.generation.into(),
                at.into(),
            ]),
    )?;

    let mut settle = Query::update();
    settle
        .table("thread_reply_tasks")
        .value("status", "settled")
        .value("receipt_id", receipt_id.as_str())
        .value("lease_token", None::<String>)
        .value("token_hash", None::<String>)
        .value("updated", at)
        .and_where(Expr::col("id").eq(&task.id))
        .and_where(Expr::col("status").eq("claimed"))
        .and_where(Expr::col("lease_token").eq(input.lease_token))
        .and_where(Expr::col("generation").eq(input.expected_generation));
    if !input.allow_cancelled_stop {
        settle.and_where(Expr::col("cancel_requested_at").is_null());
    }
    let settled = w.exec(&settle)?;
    if settled != 1 {
        return Err(StoreError::Conflict(
            "thread reply task fenced during commit".into(),
        ));
    }

    if input.record_attention_commit
        && task.space == MAIN_SPACE
        && let (Some(user_id), Some(reply_id)) = (input.user_id, reply_message_id.as_deref())
    {
        insert_attention_commit(w, &task.workspace_id, user_id, reply_id, &receipt_id)?;
    }

    let receipt = receipt_by_id(w, &receipt_id)?
        .ok_or_else(|| StoreError::Storage("inserted reply receipt missing".into()))?;
    Ok(CommitOutcome::Committed {
        receipt,
        reply_message_id,
    })
}

/// At Hub startup: settle every crashed `claimed` task to an explicit `unknown` receipt
/// (never auto-replaying side effects). Proven `queued` tasks remain queued for clean execution.
pub fn recover_claimed_tasks(
    w: &mut W<'_>,
    unknown_text: &str,
    at: i64,
) -> Result<Vec<RecoveredTask>> {
    let claimed = w.all(
        task_select()
            .and_where(Expr::col("status").eq("claimed"))
            .order_by("created", Order::Asc)
            .order_by("id", Order::Asc),
        decode_task,
    )?;
    let mut out = Vec::with_capacity(claimed.len());
    for task in claimed {
        if let Some(existing) = w.one(
            receipt_select().and_where(Expr::col("task_id").eq(&task.id)),
            decode_receipt,
        )? {
            w.exec(
                Query::update()
                    .table("thread_reply_tasks")
                    .value("status", "settled")
                    .value("lease_token", None::<String>)
                    .value("token_hash", None::<String>)
                    .value("receipt_id", existing.id.as_str())
                    .value("updated", at)
                    .and_where(Expr::col("id").eq(&task.id)),
            )?;
            out.push(RecoveredTask {
                task_id: task.id,
                workspace_id: task.workspace_id,
                space: task.space,
                origin_message_id: task.origin_message_id,
                receipt_id: existing.id,
                reply_message_id: existing.reply_message_id,
            });
            continue;
        }
        let receipt_id = uid("rcpt");
        let meta = json!({
            "origin": task.origin_message_id,
            "rerun": task.origin_message_id,
            "unknown": true,
            "run": task.id,
            "receipt": receipt_id,
            "run_receipt": true,
        });
        let reply_id = insert_message(
            w,
            &task.workspace_id,
            &task.space,
            "steve",
            unknown_text,
            None,
            &meta,
            at,
        )?;
        let next_gen = task.generation + 1;
        w.exec(
            Query::insert()
                .into_table("thread_reply_receipts")
                .columns([
                    "id",
                    "task_id",
                    "workspace_id",
                    "space",
                    "origin_message_id",
                    "reply_revision",
                    "outcome",
                    "reply_message_id",
                    "error_code",
                    "route_kind",
                    "model_kind",
                    "route_signals",
                    "route_fallback",
                    "route_ms",
                    "user_id",
                    "project_id",
                    "tools",
                    "turns",
                    "generation",
                    "committed_at",
                ])
                .values_panic([
                    receipt_id.as_str().into(),
                    task.id.as_str().into(),
                    task.workspace_id.as_str().into(),
                    task.space.as_str().into(),
                    task.origin_message_id.as_str().into(),
                    task.reply_revision.into(),
                    "unknown".into(),
                    Some(reply_id.as_str()).into(),
                    Some("crash_recovery").into(),
                    "unknown".into(),
                    "unknown".into(),
                    "[]".into(),
                    0_i64.into(),
                    0_i64.into(),
                    task.user_id.as_deref().into(),
                    task.project_id.as_deref().into(),
                    0_i64.into(),
                    0_i64.into(),
                    next_gen.into(),
                    at.into(),
                ]),
        )?;
        w.exec(
            Query::update()
                .table("thread_reply_tasks")
                .value("status", "settled")
                .value("generation", next_gen)
                .value("lease_token", None::<String>)
                .value("token_hash", None::<String>)
                .value("receipt_id", receipt_id.as_str())
                .value("updated", at)
                .and_where(Expr::col("id").eq(&task.id)),
        )?;
        if task.space == MAIN_SPACE
            && let Some(ref user_id) = task.user_id
        {
            insert_attention_commit(w, &task.workspace_id, user_id, &reply_id, &receipt_id)?;
        }
        out.push(RecoveredTask {
            task_id: task.id,
            workspace_id: task.workspace_id,
            space: task.space,
            origin_message_id: task.origin_message_id,
            receipt_id,
            reply_message_id: Some(reply_id),
        });
    }
    Ok(out)
}

// ── Receipt queries & annotations ────────────────────────────────────────────

pub fn receipt_by_id(r: &mut impl Read, id: &str) -> Result<Option<ReplyReceipt>> {
    r.one(
        receipt_select().and_where(Expr::col("id").eq(id)),
        decode_receipt,
    )
}

pub fn receipt_by_task(r: &mut impl Read, task_id: &str) -> Result<Option<ReplyReceipt>> {
    r.one(
        receipt_select().and_where(Expr::col("task_id").eq(task_id)),
        decode_receipt,
    )
}

pub fn receipt_by_origin(
    r: &mut impl Read,
    origin_message_id: &str,
) -> Result<Option<ReplyReceipt>> {
    r.one(
        receipt_select()
            .and_where(Expr::col("origin_message_id").eq(origin_message_id))
            .order_by("reply_revision", Order::Asc)
            .limit(1),
        decode_receipt,
    )
}

pub fn latest_receipt_by_origin(
    r: &mut impl Read,
    origin_message_id: &str,
) -> Result<Option<ReplyReceipt>> {
    r.one(
        receipt_select()
            .and_where(Expr::col("origin_message_id").eq(origin_message_id))
            .order_by("reply_revision", Order::Desc)
            .limit(1),
        decode_receipt,
    )
}

pub fn receipts_of_origin(r: &mut impl Read, origin_message_id: &str) -> Result<Vec<ReplyReceipt>> {
    r.all(
        receipt_select()
            .and_where(Expr::col("origin_message_id").eq(origin_message_id))
            .order_by("reply_revision", Order::Asc),
        decode_receipt,
    )
}

/// Latest answered receipt in `(ws, space)` that carries a harness session to `--resume`.
/// Scoped strictly by `(workspace_id, space)` so spaces never leak resume context.
pub fn last_resumable_receipt(
    r: &mut impl Read,
    ws: &str,
    space: &str,
) -> Result<Option<ReplyReceipt>> {
    r.one(
        receipt_select()
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("space").eq(space))
            .and_where(Expr::col("outcome").eq("answered"))
            .and_where(Expr::col("harness_session").is_not_null())
            .order_by("committed_at", Order::Desc)
            .order_by("id", Order::Desc)
            .limit(1),
        decode_receipt,
    )
}

fn validate_receipt_dispatch<'a>(
    r: &mut impl Read,
    ws: Option<&str>,
    require_dispatch: bool,
    dispatch: ReceiptDispatch<'a>,
) -> Result<Option<&'a str>> {
    match dispatch {
        ReceiptDispatch::NoDispatch => {
            if require_dispatch {
                return Err(StoreError::Rejected(
                    "handed_off reply receipt requires a dispatch ref".into(),
                ));
            }
            Ok(None)
        }
        ReceiptDispatch::Dispatched(id) => {
            if id.is_empty() {
                return Err(StoreError::Rejected("dispatch_ref_id is empty".into()));
            }
            let mut q = Query::select();
            q.column("id")
                .from("thread_dispatch_refs")
                .and_where(Expr::col("id").eq(id));
            if let Some(ws) = ws {
                q.and_where(Expr::col("workspace_id").eq(ws));
            }
            if r.one(&q, |_| Ok(()))?.is_none() {
                return Err(StoreError::Rejected(format!(
                    "dispatch_ref_id {id} does not exist"
                )));
            }
            Ok(Some(id))
        }
    }
}

pub fn update_receipt_override(
    w: &mut W<'_>,
    origin_message_id: &str,
    to: &str,
    item_id: Option<&str>,
    dispatch: ReceiptDispatch<'_>,
    at: i64,
) -> Result<bool> {
    let dispatch_ref_id = validate_receipt_dispatch(w, None, false, dispatch)?;
    let mut q = Query::update();
    q.table("thread_reply_receipts")
        .value("route_override", to)
        .value("route_override_at", at)
        .and_where(Expr::col("origin_message_id").eq(origin_message_id))
        .and_where(Expr::col("route_override").is_null());
    if let Some(item) = item_id {
        q.value(
            "item_id",
            Expr::cust_with_values("COALESCE(?, item_id)", [item]),
        );
    }
    if let Some(disp) = dispatch_ref_id {
        q.value(
            "dispatch_ref_id",
            Expr::cust_with_values("COALESCE(?, dispatch_ref_id)", [disp]),
        );
    }
    Ok(w.exec(&q)? > 0)
}

pub fn update_receipt_outcome(
    w: &mut W<'_>,
    origin_message_id: &str,
    outcome: &str,
    item_id: Option<&str>,
    dispatch: ReceiptDispatch<'_>,
) -> Result<()> {
    let dispatch_ref_id = validate_receipt_dispatch(w, None, false, dispatch)?;
    let mut q = Query::update();
    q.table("thread_reply_receipts")
        .value("route_outcome", outcome)
        .and_where(Expr::col("origin_message_id").eq(origin_message_id));
    if let Some(item) = item_id {
        q.value(
            "item_id",
            Expr::cust_with_values("COALESCE(?, item_id)", [item]),
        );
    }
    if let Some(disp) = dispatch_ref_id {
        q.value(
            "dispatch_ref_id",
            Expr::cust_with_values("COALESCE(?, dispatch_ref_id)", [disp]),
        );
    }
    w.exec(&q)?;
    Ok(())
}

pub fn list_receipts_range(
    r: &mut impl Read,
    ws: &str,
    from: i64,
    to: i64,
) -> Result<Vec<ReplyReceipt>> {
    r.all(
        receipt_select()
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("committed_at").gte(from))
            .and_where(Expr::col("committed_at").lt(to))
            .and_where(Expr::col("reply_revision").eq(0_i64))
            .order_by("committed_at", Order::Asc)
            .order_by("id", Order::Asc),
        decode_receipt,
    )
}

// ── Dispatch refs & forwarded items ──────────────────────────────────────────

pub fn claim_dispatch(
    w: &mut W<'_>,
    ws: &str,
    origin_message_id: &str,
    effect_kind: &str,
    from_question: bool,
    at: i64,
) -> Result<DispatchClaim> {
    if let Some(existing) = dispatch_by_origin(w, ws, origin_message_id, effect_kind)? {
        return Ok(DispatchClaim::Existing(existing));
    }
    let id = uid("disp");
    let inserted = w.exec(
        Query::insert()
            .into_table("thread_dispatch_refs")
            .columns([
                "id",
                "workspace_id",
                "origin_message_id",
                "effect_kind",
                "from_question",
                "follow",
                "state",
                "created",
                "updated",
            ])
            .values_panic([
                id.as_str().into(),
                ws.into(),
                origin_message_id.into(),
                effect_kind.into(),
                (from_question as i64).into(),
                1_i64.into(),
                "pending".into(),
                at.into(),
                at.into(),
            ])
            .on_conflict(
                OnConflict::columns(["workspace_id", "origin_message_id", "effect_kind"])
                    .do_nothing()
                    .to_owned(),
            ),
    )?;
    let row = dispatch_by_origin(w, ws, origin_message_id, effect_kind)?
        .ok_or_else(|| StoreError::Storage("claimed dispatch ref missing".into()))?;
    if inserted > 0 {
        Ok(DispatchClaim::Claimed(row))
    } else {
        Ok(DispatchClaim::Existing(row))
    }
}

pub fn settle_dispatch(
    w: &mut W<'_>,
    dispatch_id: &str,
    state: &str,
    target_item_id: Option<&str>,
    reply_message_id: Option<&str>,
    at: i64,
) -> Result<()> {
    let mut q = Query::update();
    q.table("thread_dispatch_refs")
        .value("state", state)
        .value("updated", at)
        .and_where(Expr::col("id").eq(dispatch_id));
    if let Some(item) = target_item_id {
        q.value("target_item_id", item);
    }
    if let Some(reply) = reply_message_id {
        q.value("reply_message_id", reply);
    }
    w.exec(&q)?;
    Ok(())
}

/// Record a completed `create_item` dispatch directly (for channel-originated items and fixtures).
#[allow(clippy::too_many_arguments)]
pub fn record_dispatch(
    w: &mut W<'_>,
    ws: &str,
    _space: &str,
    origin_message_id: &str,
    item_id: &str,
    reply_message_id: Option<&str>,
    from_question: bool,
    follow: bool,
    at: i64,
) -> Result<()> {
    let claim = claim_dispatch(w, ws, origin_message_id, "create_item", from_question, at)?;
    let id = match claim {
        DispatchClaim::Claimed(d) | DispatchClaim::Existing(d) => d.id,
    };
    settle_dispatch(w, &id, "applied", Some(item_id), reply_message_id, at)?;
    if !follow {
        set_dispatch_follow(w, item_id, false)?;
    }
    Ok(())
}

pub fn dispatch_by_id(r: &mut impl Read, id: &str) -> Result<Option<DispatchRef>> {
    r.one(
        Query::select()
            .columns(DISPATCH_COLS)
            .from("thread_dispatch_refs")
            .and_where(Expr::col("id").eq(id)),
        decode_dispatch,
    )
}

pub fn dispatch_by_origin(
    r: &mut impl Read,
    ws: &str,
    origin_message_id: &str,
    effect_kind: &str,
) -> Result<Option<DispatchRef>> {
    r.one(
        Query::select()
            .columns(DISPATCH_COLS)
            .from("thread_dispatch_refs")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("origin_message_id").eq(origin_message_id))
            .and_where(Expr::col("effect_kind").eq(effect_kind)),
        decode_dispatch,
    )
}

pub fn dispatch_by_item(r: &mut impl Read, item_id: &str) -> Result<Option<DispatchRef>> {
    r.one(
        Query::select()
            .columns(DISPATCH_COLS)
            .from("thread_dispatch_refs")
            .and_where(Expr::col("target_item_id").eq(item_id))
            .and_where(Expr::col("effect_kind").eq("create_item"))
            .limit(1),
        decode_dispatch,
    )
}

pub fn dispatches_of_workspace(r: &mut impl Read, ws: &str) -> Result<Vec<DispatchRef>> {
    r.all(
        Query::select()
            .columns(DISPATCH_COLS)
            .from("thread_dispatch_refs")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("effect_kind").eq("create_item"))
            .and_where(Expr::col("target_item_id").is_not_null())
            .order_by("created", Order::Desc)
            .order_by("id", Order::Desc),
        decode_dispatch,
    )
}

pub fn set_dispatch_reply(w: &mut W<'_>, item_id: &str, reply_message_id: &str) -> Result<()> {
    w.exec(
        Query::update()
            .table("thread_dispatch_refs")
            .value("reply_message_id", reply_message_id)
            .value("updated", now())
            .and_where(Expr::col("target_item_id").eq(item_id))
            .and_where(Expr::col("effect_kind").eq("create_item")),
    )?;
    Ok(())
}

pub fn set_dispatch_reply_by_id(
    w: &mut W<'_>,
    dispatch_id: &str,
    reply_message_id: &str,
) -> Result<()> {
    w.exec(
        Query::update()
            .table("thread_dispatch_refs")
            .value("reply_message_id", reply_message_id)
            .value("updated", now())
            .and_where(Expr::col("id").eq(dispatch_id)),
    )?;
    Ok(())
}

pub fn set_dispatch_reply_if_origin(
    w: &mut W<'_>,
    dispatch_id: &str,
    origin_message_id: &str,
    reply_message_id: &str,
) -> Result<()> {
    w.exec(
        Query::update()
            .table("thread_dispatch_refs")
            .value("reply_message_id", reply_message_id)
            .value("updated", now())
            .and_where(Expr::col("id").eq(dispatch_id))
            .and_where(Expr::col("origin_message_id").eq(origin_message_id)),
    )?;
    Ok(())
}

pub fn set_dispatch_follow(w: &mut W<'_>, item_id: &str, follow: bool) -> Result<bool> {
    Ok(w.exec(
        Query::update()
            .table("thread_dispatch_refs")
            .value("follow", follow as i64)
            .value("updated", now())
            .and_where(Expr::col("target_item_id").eq(item_id))
            .and_where(Expr::col("effect_kind").eq("create_item")),
    )? > 0)
}

pub fn set_dispatch_last_post(w: &mut W<'_>, item_id: &str, at: i64) -> Result<()> {
    w.exec(
        Query::update()
            .table("thread_dispatch_refs")
            .value("last_post", at)
            .value("updated", at)
            .and_where(Expr::col("target_item_id").eq(item_id))
            .and_where(Expr::col("effect_kind").eq("create_item")),
    )?;
    Ok(())
}

pub fn set_forwarded(
    w: &mut W<'_>,
    item_id: &str,
    ws: &str,
    origin_message_id: &str,
    asked: &str,
    at: i64,
) -> Result<()> {
    w.exec(
        Query::insert()
            .into_table("thread_forwarded")
            .columns([
                "item_id",
                "workspace_id",
                "origin_message_id",
                "asked",
                "created",
            ])
            .values_panic([
                item_id.into(),
                ws.into(),
                origin_message_id.into(),
                asked.into(),
                at.into(),
            ])
            .on_conflict(
                OnConflict::column("item_id")
                    .update_columns(["origin_message_id", "asked", "created"])
                    .to_owned(),
            ),
    )?;
    Ok(())
}

pub fn take_forwarded(w: &mut W<'_>, item_id: &str) -> Result<Option<(String, String)>> {
    let row: Option<(String, String)> = w.one(
        Query::select()
            .columns(["origin_message_id", "asked"])
            .from("thread_forwarded")
            .and_where(Expr::col("item_id").eq(item_id)),
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if row.is_some() {
        w.exec(
            Query::delete()
                .from_table("thread_forwarded")
                .and_where(Expr::col("item_id").eq(item_id)),
        )?;
    }
    Ok(row)
}

pub fn peek_forwarded(r: &mut impl Read, item_id: &str) -> Result<Option<(String, String)>> {
    r.one(
        Query::select()
            .columns(["origin_message_id", "asked"])
            .from("thread_forwarded")
            .and_where(Expr::col("item_id").eq(item_id)),
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
}

// ── Outbound delivery refs ───────────────────────────────────────────────────

pub fn delivery_refs_of_origin(
    r: &mut impl Read,
    ws: &str,
    space: &str,
    origin_message_id: &str,
) -> Result<Vec<DeliveryRefRow>> {
    r.all(
        Query::select()
            .columns(DELIVERY_COLS)
            .from("thread_delivery_refs")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("space").eq(space))
            .and_where(Expr::col("origin_message_id").eq(origin_message_id))
            .order_by("created", Order::Asc),
        decode_delivery,
    )
}

pub fn claim_delivery_ref(
    w: &mut W<'_>,
    ws: &str,
    space: &str,
    origin_message_id: &str,
    purpose: &str,
    outbound_op_id: &str,
    at: i64,
) -> Result<DeliveryRefClaim> {
    let existing = delivery_refs_of_origin(w, ws, space, origin_message_id)?;
    if let Some(blocker) = existing
        .iter()
        .find(|d| matches!(d.state.as_str(), "sent" | "unknown"))
    {
        return Ok(DeliveryRefClaim::Blocked(blocker.clone()));
    }
    if let Some(same_purpose) = existing.into_iter().find(|d| d.purpose == purpose) {
        return Ok(DeliveryRefClaim::Existing(same_purpose));
    }
    let id = uid("dlvr");
    w.exec(
        Query::insert()
            .into_table("thread_delivery_refs")
            .columns([
                "id",
                "workspace_id",
                "space",
                "origin_message_id",
                "purpose",
                "outbound_op_id",
                "state",
                "created",
                "updated",
            ])
            .values_panic([
                id.as_str().into(),
                ws.into(),
                space.into(),
                origin_message_id.into(),
                purpose.into(),
                outbound_op_id.into(),
                "pending".into(),
                at.into(),
                at.into(),
            ]),
    )?;
    let row = w
        .one(
            Query::select()
                .columns(DELIVERY_COLS)
                .from("thread_delivery_refs")
                .and_where(Expr::col("id").eq(&id)),
            decode_delivery,
        )?
        .ok_or_else(|| StoreError::Storage("claimed delivery ref missing".into()))?;
    Ok(DeliveryRefClaim::Claimed(row))
}

pub fn settle_delivery_ref(
    w: &mut W<'_>,
    ws: &str,
    space: &str,
    origin_message_id: &str,
    purpose: &str,
    state: &str,
    outbound_message_id: Option<&str>,
    at: i64,
) -> Result<()> {
    let mut q = Query::update();
    q.table("thread_delivery_refs")
        .value("state", state)
        .value("updated", at)
        .and_where(Expr::col("workspace_id").eq(ws))
        .and_where(Expr::col("space").eq(space))
        .and_where(Expr::col("origin_message_id").eq(origin_message_id))
        .and_where(Expr::col("purpose").eq(purpose));
    if let Some(mid) = outbound_message_id {
        q.value("outbound_message_id", mid);
    }
    w.exec(&q)?;
    Ok(())
}

// ── Typed notices ────────────────────────────────────────────────────────────

pub fn post_notice(
    w: &mut W<'_>,
    ws: &str,
    kind: &str,
    level: &str,
    text: &str,
    item_id: Option<&str>,
    post_host_line: bool,
    at: i64,
) -> Result<ThreadNotice> {
    let causal_key = if kind == "release" {
        item_id.map(|id| format!("release:{id}"))
    } else {
        None
    };

    // A later release notice for the same item supersedes prior open release failure notices
    // and emits WorkEvent::Message for their conversation lines in the same transaction.
    if let Some(ref key) = causal_key {
        let superseded: Vec<(String, Option<String>)> = w.all(
            Query::select()
                .columns(["id", "host_message_id"])
                .from("thread_notices")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("causal_key").eq(key.as_str()))
                .and_where(Expr::col("level").eq("fail"))
                .and_where(Expr::col("closed_at").is_null()),
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        for (old_id, host_mid) in superseded {
            w.exec(
                Query::update()
                    .table("thread_notices")
                    .value("closed_at", at)
                    .value("closed_by", "superseded")
                    .and_where(Expr::col("id").eq(&old_id)),
            )?;
            if let Some(mid) = host_mid {
                w.emit(DomainEvent::from(WorkEvent::Message {
                    id: mid.into(),
                    ws: ws.into(),
                    space: MAIN_SPACE.into(),
                }));
            }
        }
    }

    let id = uid("ntc");
    let host_message_id = if post_host_line {
        let meta = json!({
            "notice": {
                "kind": kind,
                "level": level,
                "ref": id,
            }
        });
        Some(insert_message(
            w, ws, MAIN_SPACE, "steve", text, item_id, &meta, at,
        )?)
    } else {
        None
    };

    w.exec(
        Query::insert()
            .into_table("thread_notices")
            .columns([
                "id",
                "workspace_id",
                "causal_key",
                "host_message_id",
                "kind",
                "level",
                "item_id",
                "text",
                "created",
            ])
            .values_panic([
                id.as_str().into(),
                ws.into(),
                causal_key.as_deref().into(),
                host_message_id.as_deref().into(),
                kind.into(),
                level.into(),
                item_id.into(),
                text.into(),
                at.into(),
            ]),
    )?;
    w.emit(DomainEvent::from(WorkEvent::Message {
        id: id.clone().into(),
        ws: ws.into(),
        space: "notify".into(),
    }));

    Ok(ThreadNotice {
        id,
        workspace_id: ws.into(),
        causal_key,
        host_message_id,
        kind: kind.into(),
        level: level.into(),
        item_id: item_id.map(Into::into),
        text: text.into(),
        closed_at: None,
        closed_by: None,
        created: at,
    })
}

pub fn close_notice(
    w: &mut W<'_>,
    ws: &str,
    id_or_host_msg: &str,
    closed_by: Option<&str>,
    at: i64,
) -> Result<bool> {
    let matches = w.all(
        Query::select()
            .columns(NOTICE_COLS)
            .from("thread_notices")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(
                Expr::col("id")
                    .eq(id_or_host_msg)
                    .or(Expr::col("host_message_id").eq(id_or_host_msg)),
            ),
        decode_notice,
    )?;
    if !matches.is_empty() {
        let mut closed_any = false;
        for row in matches {
            if row.closed_at.is_some() {
                continue;
            }
            w.exec(
                Query::update()
                    .table("thread_notices")
                    .value("closed_at", at)
                    .value("closed_by", closed_by)
                    .and_where(Expr::col("id").eq(&row.id)),
            )?;
            if let Some(mid) = row.host_message_id {
                w.emit(DomainEvent::from(WorkEvent::Message {
                    id: mid.into(),
                    ws: ws.into(),
                    space: MAIN_SPACE.into(),
                }));
            }
            w.emit(DomainEvent::from(WorkEvent::Message {
                id: row.id.into(),
                ws: ws.into(),
                space: "notify".into(),
            }));
            closed_any = true;
        }
        return Ok(closed_any);
    }

    // Dismissing an external alert/decision id not backed by a thread_notices row.
    let alert_id = format!("{ws}:alert:{id_or_host_msg}");
    let inserted = w.exec(
        Query::insert()
            .into_table("thread_notices")
            .columns([
                "id",
                "workspace_id",
                "causal_key",
                "kind",
                "level",
                "text",
                "closed_at",
                "closed_by",
                "created",
            ])
            .values_panic([
                alert_id.as_str().into(),
                ws.into(),
                Some(id_or_host_msg).into(),
                "alert".into(),
                "info".into(),
                "".into(),
                Some(at).into(),
                closed_by.into(),
                at.into(),
            ])
            .on_conflict(OnConflict::column("id").do_nothing().to_owned()),
    )?;
    if inserted > 0 {
        w.emit(DomainEvent::from(WorkEvent::Message {
            id: alert_id.into(),
            ws: ws.into(),
            space: "notify".into(),
        }));
    }
    Ok(inserted > 0)
}

pub fn list_notices(r: &mut impl Read, ws: &str, limit: i64) -> Result<Vec<ThreadNotice>> {
    if limit <= 0 {
        return Ok(Vec::new());
    }
    let order = r.db().dialect().insertion_order(None);
    r.all(
        Query::select()
            .columns(NOTICE_COLS)
            .from("thread_notices")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("kind").ne("alert"))
            .order_by("created", Order::Desc)
            .order_by_expr(order, Order::Desc)
            .limit(limit as u64),
        decode_notice,
    )
}

pub fn dismissed_alert_ids(r: &mut impl Read, ws: &str) -> Result<Vec<String>> {
    r.all(
        Query::select()
            .expr(Expr::cust(
                "CASE WHEN kind = 'alert' THEN COALESCE(causal_key, id) ELSE id END",
            ))
            .from("thread_notices")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("closed_at").is_not_null())
            .order_by("created", Order::Asc)
            .order_by(Alias::new("id"), Order::Asc),
        |row| row.get(0),
    )
}

pub fn notice_host_message_ids(r: &mut impl Read, ws: &str) -> Result<Vec<String>> {
    r.all(
        Query::select()
            .column("host_message_id")
            .from("thread_notices")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("host_message_id").is_not_null()),
        |row| row.get(0),
    )
}

pub fn edit_message(
    w: &mut W<'_>,
    ws: &str,
    message_id: &str,
    text: &str,
    meta: Option<&str>,
) -> Result<()> {
    let mut update = Query::update();
    update
        .table("messages")
        .value("text", text)
        .and_where(Expr::col("id").eq(message_id))
        .and_where(Expr::col("workspace_id").eq(ws));
    if let Some(meta) = meta {
        update.value("meta", meta);
    }
    w.exec(&update)?;
    Ok(())
}
