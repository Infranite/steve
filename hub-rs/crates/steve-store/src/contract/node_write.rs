//! Node ledger operations scoped to the Hub's existing store write.
pub mod runtime;

use serde_json::Value;

use crate::{Queued, Result, W};
use runtime::{
    CommandLease, NewAttempt, NewCommand, NewPermission, PermissionRow, ReceiptState,
    SessionBinding, SessionRoom, SessionUpdate, TurnUpdate,
};

pub fn bind_session_in(w: &mut W<'_>, b: &SessionBinding<'_>, now: i64) -> Result<bool> {
    runtime::bind_session_in(w.conn, b, now)
}

pub fn session_cas_in(
    w: &mut W<'_>,
    session: &str,
    revision: i64,
    u: &SessionUpdate<'_>,
    now: i64,
) -> Result<bool> {
    runtime::session_cas_in(w.conn, session, revision, u, now)
}

pub fn claim_in(w: &mut W<'_>, session: &str, turn: &str, now: i64) -> Result<Vec<Queued>> {
    runtime::claim_in(w.conn, session, turn, now)
}

pub fn deliver_into_turn_in(
    w: &mut W<'_>,
    session: &str,
    turn: &str,
    now: i64,
) -> Result<Vec<Queued>> {
    runtime::deliver_into_turn_in(w.conn, session, turn, now)
}

pub fn insert_attempt_in(w: &mut W<'_>, a: &NewAttempt<'_>, now: i64) -> Result<bool> {
    runtime::insert_attempt_in(w.conn, a, now)
}

pub fn attempt_cas_in(
    w: &mut W<'_>,
    id: &str,
    revision: i64,
    state: &str,
    receipt: Option<&Value>,
    completion: Option<&Value>,
    now: i64,
) -> Result<bool> {
    runtime::attempt_cas_in(w.conn, id, revision, state, receipt, completion, now)
}

pub fn cancel_attempt_in(w: &mut W<'_>, id: &str, revision: i64, now: i64) -> Result<bool> {
    runtime::cancel_attempt_in(w.conn, id, revision, now)
}

pub fn turn_cas_in(
    w: &mut W<'_>,
    turn: &str,
    revision: i64,
    u: &TurnUpdate<'_>,
    now: i64,
) -> Result<bool> {
    runtime::turn_cas_in(w.conn, turn, revision, u, now)
}

pub fn dispatch_receipt_in(w: &mut W<'_>, turn: &str, state: &str, now: i64) -> Result<usize> {
    runtime::dispatch_receipt_in(w.conn, turn, state, now)
}

pub fn insert_command_in(w: &mut W<'_>, c: &NewCommand<'_>, now: i64) -> Result<bool> {
    runtime::insert_command_in(w.conn, c, now)
}

pub fn lease_command_in(
    w: &mut W<'_>,
    node: &str,
    owner: &str,
    now: i64,
    until: i64,
) -> Result<Option<CommandLease>> {
    runtime::lease_command_in(w.conn, node, owner, now, until)
}

pub fn retry_command_in(
    w: &mut W<'_>,
    lease: &CommandLease,
    now: i64,
    retry_at: i64,
    uncertain: bool,
) -> Result<bool> {
    runtime::retry_command_in(w.conn, lease, now, retry_at, uncertain)
}

pub fn merge_receipt_in(
    w: &mut W<'_>,
    operation: &str,
    state: ReceiptState,
    success: bool,
    payload: &Value,
    now: i64,
) -> Result<bool> {
    runtime::merge_receipt_in(w.conn, operation, state, success, payload, now)
}

pub fn insert_permission_in(w: &mut W<'_>, p: &NewPermission<'_>, now: i64) -> Result<bool> {
    runtime::insert_permission_in(w.conn, p, now)
}

pub fn permission_in(
    w: &mut W<'_>,
    node: &str,
    attempt: &str,
    permission: &str,
) -> Result<Option<PermissionRow>> {
    runtime::permission_in(w.conn, node, attempt, permission)
}

pub fn permission_of_decision_in(w: &mut W<'_>, decision: &str) -> Result<Option<PermissionRow>> {
    runtime::permission_of_decision_in(w.conn, decision)
}

pub fn permission_of_answer_in(w: &mut W<'_>, operation: &str) -> Result<Option<PermissionRow>> {
    runtime::permission_of_answer_in(w.conn, operation)
}

pub fn permission_cas_in(
    w: &mut W<'_>,
    decision: &str,
    revision: i64,
    state: &str,
    answer_operation: Option<&str>,
    now: i64,
) -> Result<bool> {
    runtime::permission_cas_in(w.conn, decision, revision, state, answer_operation, now)
}

pub fn due_permissions_in(
    w: &mut W<'_>,
    node: &str,
    now: i64,
    limit: i64,
) -> Result<Vec<PermissionRow>> {
    runtime::due_permissions_in(w.conn, node, now, limit)
}

pub fn permissions_of_attempt_in(w: &mut W<'_>, attempt: &str) -> Result<Vec<PermissionRow>> {
    runtime::permissions_of_attempt_in(w.conn, attempt)
}

pub fn waiting_commands(w: &mut W<'_>, node: &str) -> Result<Vec<(String, Option<String>)>> {
    runtime::waiting_commands(w.conn, node)
}

pub fn session_room(w: &mut W<'_>, node: &str) -> Result<SessionRoom> {
    runtime::session_room_on(w.conn, node)
}

pub fn record_placement(w: &mut W<'_>, item: &str, placement: &Value) -> Result<()> {
    crate::bot_dispatch::decision(w.conn, item, placement)
}
