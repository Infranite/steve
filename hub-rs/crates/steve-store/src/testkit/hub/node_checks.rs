//! Persisted state inspected by the Hub integration tests.

use crate::{Result, Store};

macro_rules! scalar {
    ($name:ident($($arg:ident: $ty:ty),* $(,)?) -> $ret:ty, $sql:literal $(,)?) => {
        pub fn $name(store: &Store, $($arg: $ty),*) -> Result<$ret> {
            store.reader(|db| Ok(db.query_row($sql, rusqlite::params![$($arg),*], |row| row.get(0))?))
        }
    };
}

scalar!(
    active_attempt_retry_at(session: &str) -> i64,
    "SELECT retry_at FROM node_commands WHERE target_attempt=(SELECT active_attempt FROM runtime_sessions WHERE session_id=?1)"
);

scalar!(
    attempt_a_execution_by_session(session: &str) -> String,
    "SELECT a.execution FROM runtime_attempts a JOIN runtime_turns t ON t.turn_id=a.turn_id WHERE t.session_id=?1"
);

scalar!(attempt_attempt_id() -> String, "SELECT attempt_id FROM runtime_attempts");

scalar!(
    attempt_cancel_requested_by_attempt(attempt: &str) -> i64,
    "SELECT cancel_requested FROM runtime_attempts WHERE attempt_id=?1"
);

scalar!(attempt_choice_model() -> String, "SELECT json_extract(spec,'$.run.choice.model') FROM runtime_attempts");

scalar!(attempt_completion() -> String, "SELECT completion FROM runtime_attempts");

scalar!(attempt_count() -> i64, "SELECT COUNT(*) FROM runtime_attempts");

scalar!(
    attempt_count_by_state(state: &str) -> i64,
    "SELECT COUNT(*) FROM runtime_attempts WHERE state=?1"
);

scalar!(attempt_distinct_turn_id_count() -> i64, "SELECT COUNT(DISTINCT turn_id) FROM runtime_attempts");

scalar!(attempt_execution() -> String, "SELECT execution FROM runtime_attempts LIMIT 1");

scalar!(
    attempt_operation_id_by_attempt(attempt: &str) -> String,
    "SELECT operation_id FROM runtime_attempts WHERE attempt_id=?1"
);

scalar!(attempt_spec() -> String, "SELECT spec FROM runtime_attempts");

scalar!(
    attempt_state_for(attempt: &str) -> String,
    "SELECT state FROM runtime_attempts WHERE attempt_id=?"
);
scalar!(attempt_state() -> String, "SELECT state FROM runtime_attempts");

scalar!(audit_action_target_detail_thread_research_violation() -> String, "SELECT action||' '||target||' '||detail FROM audit WHERE action='thread.research_violation'");

scalar!(
    audit_count_by_action(action: &str) -> i64,
    "SELECT COUNT(*) FROM audit WHERE action=?1"
);

scalar!(
    binding_count_by_session(session: &str) -> i64,
    "SELECT COUNT(*) FROM runtime_sessions WHERE session_id=?1"
);

scalar!(binding_count_s1_ready() -> i64, "SELECT COUNT(*) FROM runtime_sessions WHERE session_id='s1' AND phase='ready'");

scalar!(
    binding_desired_config_by_session(session: &str) -> Option<String>,
    "SELECT desired_config FROM runtime_sessions WHERE session_id=?1"
);

scalar!(binding_node_revision() -> String, "SELECT node_revision FROM runtime_sessions");

scalar!(
    binding_phase_by_session(session: &str) -> String,
    "SELECT phase FROM runtime_sessions WHERE session_id=?1"
);

scalar!(
    binding_state_dir_by_session(session: &str) -> String,
    "SELECT state_dir FROM runtime_sessions WHERE session_id=?1"
);

scalar!(
    binding_workspace_ref_by_session(session: &str) -> String,
    "SELECT workspace_ref FROM runtime_sessions WHERE session_id=?1"
);

scalar!(
    blocked_node_release_notice_count(session: &str) -> i64,
    "SELECT COUNT(*) FROM events WHERE session_id=?1 AND kind='system' AND data LIKE '%还没能在 Node 上释放%'"
);

scalar!(
    command_accept_before_by_operation(operation: &str) -> i64,
    "SELECT json_extract(request,'$.accept_before') FROM node_commands WHERE operation_id=?1"
);

scalar!(command_coalesce_max_lease_generation_0_start_turn() -> i64, "SELECT COALESCE(MAX(lease_generation),0) FROM node_commands WHERE method='start_turn'");

scalar!(
    command_count_by_method(method: &str) -> i64,
    "SELECT COUNT(*) FROM node_commands WHERE method=?1"
);

scalar!(
    command_count_by_node_id_start_turn(node_id: &str) -> i64,
    "SELECT COUNT(*) FROM node_commands WHERE node_id=?1 AND method='start_turn'"
);

scalar!(
    command_count_by_operation(operation: &str) -> i64,
    "SELECT COUNT(*) FROM node_commands WHERE operation_id=?1"
);

scalar!(command_count_cancel_rejected() -> i64, "SELECT COUNT(*) FROM node_commands WHERE method='cancel' AND state='rejected'");

scalar!(command_count_cancel_with_target_job_attempt() -> i64, "SELECT COUNT(*) FROM node_commands WHERE method='cancel' AND target_job_attempt IS NOT NULL");

scalar!(command_count_open_session() -> i64, "SELECT COUNT(*) FROM node_commands WHERE method='open_session' AND json_extract(request,'$.body.session')='s1'");

scalar!(command_count_prepare_workspace() -> i64, "SELECT COUNT(*) FROM node_commands WHERE method='prepare_workspace' AND json_extract(request,'$.body.resource')='item:i1'");

scalar!(
    upgrade_deferred_start_count(generation: i64) -> i64,
    "SELECT COUNT(*) FROM node_commands WHERE method='start_turn' AND lease_generation>? AND lease_owner IS NULL
     AND json_extract(receipt,'$.last_error.code')='offline' AND json_extract(receipt,'$.last_error.safe_detail') LIKE '%while upgrading%'"
);

scalar!(
    unleased_start_count_by_generation(lease_generation: i64) -> i64,
    "SELECT COUNT(*) FROM node_commands WHERE method='start_turn' AND lease_owner IS NULL AND lease_generation>?1"
);

scalar!(command_count_start_turn_pending() -> i64, "SELECT COUNT(*) FROM node_commands WHERE method='start_turn' AND state='pending'");

scalar!(command_count_start_turn_rejected() -> i64, "SELECT COUNT(*) FROM node_commands WHERE method='start_turn' AND state='rejected'");

scalar!(command_count_stops() -> i64, "SELECT COUNT(*) FROM node_commands WHERE method IN ('cancel','abandon')");

scalar!(command_count_tidy_workspace_finished() -> i64, "SELECT COUNT(*) FROM node_commands WHERE method='tidy_workspace' AND state='finished'");

scalar!(
    command_decision_by_operation(operation: &str) -> String,
    "SELECT json_extract(request,'$.body.decision') FROM node_commands WHERE operation_id=?1"
);

scalar!(command_expected_configure_session() -> String, "SELECT json_extract(request,'$.body.expected') FROM node_commands WHERE method='configure_session'");

scalar!(command_expected_recover() -> String, "SELECT json_extract(request,'$.body.expected') FROM node_commands WHERE method='recover'");

scalar!(command_last_error_code_start_turn() -> String, "SELECT json_extract(receipt,'$.last_error.code') FROM node_commands WHERE method='start_turn'");

scalar!(
    command_lease_generation_by_method(method: &str) -> i64,
    "SELECT lease_generation FROM node_commands WHERE method=?1"
);

scalar!(command_lease_generation_start_turn_pending_for_selected_row() -> i64, "SELECT lease_generation FROM node_commands WHERE operation_id=(SELECT operation_id FROM node_commands WHERE method='start_turn' AND state='pending')");

scalar!(command_max_json_extract_receipt_exhausted_start_turn() -> Option<i64>, "SELECT MAX(json_extract(receipt,'$.exhausted')) FROM node_commands WHERE method='start_turn'");

scalar!(
    command_maybe_sent_by_method(method: &str) -> i64,
    "SELECT maybe_sent FROM node_commands WHERE method=?1"
);

scalar!(
    command_operation_id_by_method(method: &str) -> String,
    "SELECT operation_id FROM node_commands WHERE method=?1"
);

scalar!(
    command_operation_id_by_node_id_start_turn(node_id: &str) -> String,
    "SELECT operation_id FROM node_commands WHERE node_id=?1 AND method='start_turn'"
);

scalar!(command_operation_id_start_turn_pending() -> String, "SELECT operation_id FROM node_commands WHERE method='start_turn' AND state='pending'");

scalar!(
    command_receipt_by_operation(operation: &str) -> Option<String>,
    "SELECT receipt FROM node_commands WHERE operation_id=?1"
);

scalar!(command_receipt_pending_start_turn() -> String, "SELECT receipt FROM node_commands WHERE state='pending' AND method='start_turn'");

scalar!(
    command_request_by_method(method: &str) -> String,
    "SELECT request FROM node_commands WHERE method=?1"
);

scalar!(
    command_request_by_operation(operation: &str) -> String,
    "SELECT request FROM node_commands WHERE operation_id=?1"
);

scalar!(
    command_retry_at_by_method(method: &str) -> i64,
    "SELECT retry_at FROM node_commands WHERE method=?1"
);

scalar!(
    command_state_by_method(method: &str) -> String,
    "SELECT state FROM node_commands WHERE method=?1"
);

scalar!(
    command_state_by_operation(operation: &str) -> String,
    "SELECT state FROM node_commands WHERE operation_id=?1"
);

scalar!(command_subject_op_x1() -> String, "SELECT json_extract(request,'$.subject') FROM node_commands WHERE operation_id='op_x1'");

scalar!(command_subject_tidy_workspace() -> String, "SELECT json_extract(request,'$.subject') FROM node_commands WHERE method='tidy_workspace'");

scalar!(command_target_abandon() -> String, "SELECT json_extract(request,'$.body.target') FROM node_commands WHERE method='abandon'");

scalar!(command_workspace_id_tidy_workspace() -> String, "SELECT json_extract(request,'$.body.workspace.id') FROM node_commands WHERE method='tidy_workspace'");

scalar!(
    conflicting_replay_notice_count(session: &str) -> i64,
    "SELECT COUNT(*) FROM events WHERE session_id=?1 AND data LIKE '%conflicting%'"
);

scalar!(cumulative_gateway_usage_report_count() -> i64, "SELECT COUNT(*) FROM gateway_usage_reports WHERE json_extract(payload,'$.measurement')='cumulative' AND json_extract(payload,'$.tokens.input') IS NOT NULL");

scalar!(decision_count() -> i64, "SELECT COUNT(*) FROM decisions");

scalar!(
    decision_count_by_status(status: &str) -> i64,
    "SELECT COUNT(*) FROM decisions WHERE status=?1"
);

scalar!(
    decision_count_by_subject_type(subject_type: &str) -> i64,
    "SELECT COUNT(*) FROM decisions WHERE subject_type=?1"
);

scalar!(decision_count_session_start_open() -> i64, "SELECT COUNT(*) FROM decisions WHERE subject_type='session_start' AND status='open'");

scalar!(decision_count_tool_call_open() -> i64, "SELECT COUNT(*) FROM decisions WHERE subject_type='tool_call' AND status='open'");

scalar!(
    decision_id_by_status(status: &str) -> String,
    "SELECT id FROM decisions WHERE status=?1"
);

scalar!(
    decision_id_by_subject_type(subject_type: &str) -> String,
    "SELECT id FROM decisions WHERE subject_type=?1"
);

scalar!(
    decision_question_by_subject_type(subject_type: &str) -> String,
    "SELECT question FROM decisions WHERE subject_type=?1"
);

scalar!(
    decision_refused_by_id(id: &str) -> String,
    "SELECT json_extract(result,'$.refused') FROM decisions WHERE id=?1"
);

scalar!(decision_status() -> String, "SELECT status FROM decisions");

scalar!(
    decision_status_by_id(id: &str) -> String,
    "SELECT status FROM decisions WHERE id=?1"
);

scalar!(
    decision_status_by_subject_type(subject_type: &str) -> String,
    "SELECT status FROM decisions WHERE subject_type=?1"
);

scalar!(
    dispatch_count_by_text(text: &str) -> i64,
    "SELECT COUNT(*) FROM session_dispatch WHERE text=?1"
);

scalar!(dispatch_count_continue_the_original_goal_claimed() -> i64, "SELECT COUNT(*) FROM session_dispatch WHERE text='continue the original goal' AND state='claimed'");

scalar!(dispatch_state_node() -> String, "SELECT state FROM session_dispatch WHERE backend='node'");

scalar!(dispatch_state_node_one() -> String, "SELECT state FROM session_dispatch WHERE backend='node' AND text='one'");

scalar!(duplicate_node_event_count() -> i64, "SELECT COUNT(*) FROM (SELECT 1 FROM events WHERE source_node IS NOT NULL GROUP BY source_node,source_stream,source_epoch,source_seq HAVING COUNT(*)>1)");

scalar!(event_count() -> i64, "SELECT COUNT(*) FROM events");

scalar!(
    event_count_by_kind(kind: &str) -> i64,
    "SELECT COUNT(*) FROM events WHERE kind=?1"
);

scalar!(
    event_count_by_session_system_by_text(session: &str, pattern: &str) -> i64,
    "SELECT COUNT(*) FROM events WHERE session_id=?1 AND kind='system' AND data LIKE ?2"
);

scalar!(
    event_count_by_session_user_matching_text(session: &str) -> i64,
    "SELECT COUNT(*) FROM events WHERE session_id=?1 AND kind='user' AND data LIKE '%again%'"
);

scalar!(
    event_count_by_source_node(source_node: &str) -> i64,
    "SELECT COUNT(*) FROM events WHERE source_node=?1"
);

scalar!(
    event_count_by_source_node_matching_text(source_node: &str) -> i64,
    "SELECT COUNT(*) FROM events WHERE source_node=?1 AND data LIKE '%forged%'"
);

scalar!(event_count_matching_text() -> i64, "SELECT COUNT(*) FROM events WHERE data LIKE '%tampered%'");

scalar!(
    event_count_system_by_text(pattern: &str) -> i64,
    "SELECT COUNT(*) FROM events WHERE kind='system' AND data LIKE ?1"
);

scalar!(event_count_text_steve_v1() -> i64, "SELECT COUNT(*) FROM events WHERE kind='text' AND format='steve.v1'");

scalar!(event_count_text_with_value() -> i64, "SELECT COUNT(*) FROM events WHERE kind='text' AND source_node IS NOT NULL");

scalar!(event_count_user_steve_v1() -> i64, "SELECT COUNT(*) FROM events WHERE kind='user' AND format='steve.v1'");

scalar!(
    faulted_session_notice_count(session: &str) -> i64,
    "SELECT COUNT(*) FROM events WHERE session_id=?1 AND kind='system' AND json_extract(data,'$.faulted')=1 AND data LIKE '%新会话%'"
);

scalar!(files_count() -> i64, "SELECT COUNT(*) FROM files");

scalar!(files_created_by_matching_text() -> String, "SELECT created_by FROM files WHERE name LIKE 'job-input-%'");

scalar!(final_gateway_usage_report_count() -> i64, "SELECT COUNT(*) FROM gateway_usage_reports WHERE json_extract(payload,'$.measurement')='final' AND json_extract(payload,'$.tokens.input') IS NOT NULL");

scalar!(first_attempt_count_for_selected_row() -> i64, "SELECT COUNT(*) FROM runtime_attempts WHERE turn_id=(SELECT turn_id FROM runtime_attempts ORDER BY created, rowid LIMIT 1)");

scalar!(first_attempt_state() -> String, "SELECT state FROM runtime_attempts ORDER BY created LIMIT 1");

scalar!(first_command_resume_open_session() -> Option<String>, "SELECT json_extract(request,'$.body.resume') FROM node_commands WHERE method='open_session' ORDER BY created, rowid LIMIT 1");

scalar!(first_dispatch_state_node() -> String, "SELECT state FROM session_dispatch WHERE backend='node' ORDER BY rowid LIMIT 1");

scalar!(first_event_coalesce_group_concat_kind_n_from_select_kind_count_n_with_value() -> String, "SELECT COALESCE(group_concat(kind||':'||n), '') FROM (SELECT kind, COUNT(*) n FROM events WHERE source_node IS NOT NULL GROUP BY kind ORDER BY kind)");

scalar!(folded_node_text() -> String, "SELECT group_concat(json_extract(data,'$.text'),'') FROM (SELECT data FROM events WHERE kind='text' AND source_node IS NOT NULL ORDER BY seq)");

scalar!(first_usage_call_id() -> String, "SELECT json_extract(detail,'$.call_id') FROM usage_records LIMIT 1");

scalar!(first_usage_records_attempts() -> i64, "SELECT attempts FROM usage_records ORDER BY id LIMIT 1");

scalar!(gateway_attempts_count() -> i64, "SELECT COUNT(*) FROM gateway_attempts");

scalar!(gateway_attempts_count_with_previous_attempt() -> i64, "SELECT COUNT(*) FROM gateway_attempts WHERE previous_attempt IS NOT NULL");

scalar!(gateway_attempts_distinct_call_id_count() -> i64, "SELECT COUNT(DISTINCT call_id) FROM gateway_attempts");

scalar!(gateway_calls_call_id() -> String, "SELECT call_id FROM gateway_calls");

scalar!(gateway_calls_count_with_ended() -> i64, "SELECT COUNT(*) FROM gateway_calls WHERE ended IS NOT NULL");

scalar!(gateway_calls_count_with_ok_with_ended() -> i64, "SELECT COUNT(*) FROM gateway_calls WHERE ended IS NOT NULL AND ok=0");

scalar!(
    gateway_calls_projection_dirty_by_call(call: &str) -> i64,
    "SELECT projection_dirty FROM gateway_calls WHERE call_id=?1"
);

scalar!(gateway_local_inbox_count_with_applied() -> i64, "SELECT COUNT(*) FROM gateway_local_inbox WHERE applied=0");

scalar!(gateway_local_inbox_count_with_applied_with_value() -> i64, "SELECT COUNT(*) FROM gateway_local_inbox WHERE applied=0 AND json_extract(payload,'$.measurement')='final' AND json_extract(payload,'$.tokens.input') IS NOT NULL");

scalar!(gateway_result_ready_count() -> i64, "SELECT COUNT(*) FROM gateway_result_ready");

scalar!(gateway_usage_reports_count() -> i64, "SELECT COUNT(*) FROM gateway_usage_reports");

scalar!(job_count() -> i64, "SELECT COUNT(*) FROM runtime_jobs");

scalar!(
    job_count_by_state(state: &str) -> i64,
    "SELECT COUNT(*) FROM runtime_jobs WHERE state=?1"
);

scalar!(
    job_count_by_thread_run(thread_run: &str) -> i64,
    "SELECT COUNT(*) FROM runtime_jobs WHERE thread_run_id=?1"
);

scalar!(job_count_finished_think_answer() -> i64, "SELECT COUNT(*) FROM runtime_jobs WHERE state='finished' AND kind='think' AND purpose='answer'");

scalar!(job_kind_purpose_thread_run_id_state() -> String, "SELECT kind||' '||purpose||' '||thread_run_id||' '||state FROM runtime_jobs");

scalar!(large_text_count() -> i64, "SELECT COUNT(*) FROM events WHERE kind='text' AND data LIKE '%bigxxx%'");

scalar!(
    last_inserted_command_id_by_method(method: &str) -> String,
    "SELECT operation_id FROM node_commands WHERE method=?1 ORDER BY rowid DESC LIMIT 1"
);

scalar!(latest_attempt_policy_permissions() -> String, "SELECT json_extract(spec,'$.run.policy.permissions') FROM runtime_attempts ORDER BY created DESC, rowid DESC LIMIT 1");

scalar!(latest_attempt_spec() -> String, "SELECT spec FROM runtime_attempts ORDER BY ordinal DESC LIMIT 1");

scalar!(latest_attempt_state() -> String, "SELECT state FROM runtime_attempts ORDER BY created DESC, rowid DESC LIMIT 1");

scalar!(
    latest_command_operation_id_by_method(method: &str) -> String,
    "SELECT operation_id FROM node_commands WHERE method=?1 ORDER BY created DESC, rowid DESC LIMIT 1"
);

scalar!(latest_command_payload_state_result_revision_open_session() -> String, "SELECT json_extract(receipt,'$.payload.state.result.revision') FROM node_commands WHERE method='open_session' ORDER BY created DESC, rowid DESC LIMIT 1");

scalar!(latest_command_policy_permissions_open_session() -> String, "SELECT json_extract(request,'$.body.policy.permissions') FROM node_commands WHERE method='open_session' ORDER BY created DESC, rowid DESC LIMIT 1");

scalar!(latest_command_resource_prepare_workspace() -> String, "SELECT json_extract(request,'$.body.resource') FROM node_commands WHERE method='prepare_workspace' ORDER BY rowid DESC LIMIT 1");

scalar!(latest_command_resume_open_session() -> Option<String>, "SELECT json_extract(request,'$.body.resume') FROM node_commands WHERE method='open_session' ORDER BY created DESC, rowid DESC LIMIT 1");

scalar!(latest_command_retry_delay_start_turn() -> i64, "SELECT retry_at - created FROM node_commands WHERE method='start_turn' ORDER BY created DESC, rowid DESC LIMIT 1");

scalar!(
    latest_command_state_by_method(method: &str) -> String,
    "SELECT state FROM node_commands WHERE method=?1 ORDER BY created DESC, rowid DESC LIMIT 1"
);

scalar!(latest_command_target_recover() -> String, "SELECT json_extract(request,'$.body.target') FROM node_commands WHERE method='recover' ORDER BY created DESC, rowid DESC LIMIT 1");

scalar!(latest_dispatch_state_node() -> String, "SELECT state FROM session_dispatch WHERE backend='node' ORDER BY rowid DESC LIMIT 1");

scalar!(latest_dispatch_text_node() -> String, "SELECT text FROM session_dispatch WHERE backend='node' ORDER BY rowid DESC LIMIT 1");

scalar!(latest_runtime_job_attempts_execution() -> String, "SELECT execution FROM runtime_job_attempts ORDER BY rowid DESC LIMIT 1");

scalar!(latest_runtime_turns_retry_ordinal() -> i64, "SELECT retry_ordinal FROM runtime_turns ORDER BY rowid DESC LIMIT 1");

scalar!(
    messages_count_by_space(space: &str) -> i64,
    "SELECT COUNT(*) FROM messages WHERE space=?1"
);

scalar!(
    node_audit_count_by_node_id_foreign_event(node_id: &str) -> i64,
    "SELECT COUNT(*) FROM node_audit WHERE node_id=?1 AND what='foreign_event'"
);

scalar!(
    foreign_event_audit_count(node_id: &str) -> i64,
    "SELECT COUNT(*) FROM node_audit WHERE node_id=?1 AND what='foreign_event' AND safe_detail LIKE 'replayed%'"
);

scalar!(
    node_audit_count_by_what(what: &str) -> i64,
    "SELECT COUNT(*) FROM node_audit WHERE what=?1"
);

scalar!(node_audit_count_remove_u1() -> i64, "SELECT COUNT(*) FROM node_audit WHERE what='remove' AND actor='u1'");

scalar!(node_count() -> i64, "SELECT COUNT(*) FROM nodes");

scalar!(
    node_fingerprint_by_node_id(node_id: &str) -> String,
    "SELECT fingerprint FROM nodes WHERE node_id=?1"
);

scalar!(
    node_generation_by_node_id(node_id: &str) -> i64,
    "SELECT generation FROM nodes WHERE node_id=?1"
);

scalar!(node_host_is_not_null_with_local_with_value() -> i64, "SELECT host IS NOT NULL FROM nodes WHERE local=0");

scalar!(
    node_incarnation_by_node_id(node_id: &str) -> Option<i64>,
    "SELECT incarnation FROM nodes WHERE node_id=?1"
);

scalar!(node_pairings_code_hash() -> String, "SELECT code_hash FROM node_pairings");

scalar!(node_refusal_notice_count() -> i64, "SELECT COUNT(*) FROM events WHERE kind='system' AND data LIKE '%Node 暂不接新工作%'");

scalar!(
    node_removals_phase_by_operation(operation: &str) -> String,
    "SELECT phase FROM node_removals WHERE operation_id=?1"
);

scalar!(
    node_state_by_node_id(node_id: &str) -> String,
    "SELECT state FROM nodes WHERE node_id=?1"
);

scalar!(
    nth_attempt_prompt_0_text(offset: i64) -> String,
    "SELECT json_extract(spec,'$.run.prompt[0].text') FROM runtime_attempts ORDER BY created, rowid LIMIT 1 OFFSET ?1"
);

scalar!(offline_pending_start_count() -> i64, "SELECT COUNT(*) FROM node_commands WHERE method='start_turn' AND state='pending' AND json_extract(receipt,'$.last_error.code')='offline'");

scalar!(optional_binding_node_revision() -> Option<String>, "SELECT node_revision FROM runtime_sessions");

scalar!(
    optional_session_cost_by_id(id: &str) -> Option<f64>,
    "SELECT cost FROM sessions WHERE id=?1"
);

scalar!(
    permission_state_by_decision(decision: &str) -> String,
    "SELECT state FROM runtime_permissions WHERE decision_id=?1"
);

scalar!(possibly_sent_turn_count() -> i64, "SELECT COALESCE(MAX(maybe_sent),0) FROM node_commands WHERE method='start_turn' AND state='pending' AND lease_owner IS NULL");

scalar!(queued_count() -> i64, "SELECT COUNT(*) FROM queued");

scalar!(
    queued_count_by_session(session: &str) -> i64,
    "SELECT COUNT(*) FROM queued WHERE session_id=?1"
);

scalar!(
    queued_count_by_session_next(session: &str) -> i64,
    "SELECT COUNT(*) FROM queued WHERE session_id=?1 AND text='next'"
);

scalar!(
    queued_id_by_session(session: &str) -> String,
    "SELECT id FROM queued WHERE session_id=?1"
);

scalar!(
    queued_id_by_session_by_text(session: &str, text: &str) -> String,
    "SELECT id FROM queued WHERE session_id=?1 AND text=?2"
);

scalar!(
    queued_text_by_session(session: &str) -> String,
    "SELECT text FROM queued WHERE session_id=?1"
);

scalar!(
    release_command_count_for_session(value: &str) -> i64,
    "SELECT COUNT(*) FROM node_commands WHERE method='release_session' AND json_extract(request,'$.body')=?1"
);

scalar!(replayed_text_count() -> i64, "SELECT COUNT(*) FROM events WHERE kind='text' AND data LIKE '%missed%'");

scalar!(
    required_stream_fault_by_stream(stream: &str) -> String,
    "SELECT fault FROM node_streams WHERE stream=?1"
);

scalar!(runtime_job_attempts_coalesce_completion_none() -> String, "SELECT COALESCE(completion,'none') FROM runtime_job_attempts");

scalar!(runtime_job_attempts_count() -> i64, "SELECT COUNT(*) FROM runtime_job_attempts");

scalar!(
    runtime_job_attempts_count_by_state(state: &str) -> i64,
    "SELECT COUNT(*) FROM runtime_job_attempts WHERE state=?1"
);

scalar!(runtime_job_attempts_count_finished_with_ordinal() -> i64, "SELECT COUNT(*) FROM runtime_job_attempts WHERE state='finished' AND ordinal=0");

scalar!(runtime_job_attempts_execution() -> String, "SELECT execution FROM runtime_job_attempts");

scalar!(runtime_job_attempts_limits_deadline() -> i64, "SELECT json_extract(spec, '$.run.limits.deadline') FROM runtime_job_attempts");

scalar!(runtime_job_attempts_operation_id() -> String, "SELECT operation_id FROM runtime_job_attempts");

scalar!(runtime_job_attempts_outcome() -> Option<String>, "SELECT json_extract(completion,'$.outcome') FROM runtime_job_attempts");

scalar!(
    runtime_job_attempts_outcome_by_attempt(attempt: &str) -> Option<String>,
    "SELECT json_extract(completion,'$.outcome') FROM runtime_job_attempts WHERE attempt_id=?1"
);

scalar!(runtime_job_attempts_spec() -> String, "SELECT spec FROM runtime_job_attempts");

scalar!(runtime_job_attempts_state() -> String, "SELECT state FROM runtime_job_attempts");

scalar!(runtime_turns_count() -> i64, "SELECT COUNT(*) FROM runtime_turns");

scalar!(runtime_turns_retry_ordinal() -> i64, "SELECT retry_ordinal FROM runtime_turns");

scalar!(
    secret_uses_count_by_actor(actor: &str) -> i64,
    "SELECT COUNT(*) FROM secret_uses WHERE actor_id=?1"
);

scalar!(
    session_acp_session_id_by_id(id: &str) -> Option<String>,
    "SELECT acp_session_id FROM sessions WHERE id=?1"
);

scalar!(
    session_cost_by_id(id: &str) -> f64,
    "SELECT cost FROM sessions WHERE id=?1"
);

scalar!(
    session_model_by_id(id: &str) -> Option<String>,
    "SELECT model FROM sessions WHERE id=?1"
);

scalar!(session_one_active_attempt_retry_at() -> i64, "SELECT retry_at FROM node_commands WHERE target_attempt=(SELECT active_attempt FROM runtime_sessions WHERE session_id='s1')");

scalar!(
    session_reasoning_effort_by_id(id: &str) -> String,
    "SELECT reasoning_effort FROM sessions WHERE id=?1"
);

scalar!(
    session_status_by_id(id: &str) -> String,
    "SELECT status FROM sessions WHERE id=?1"
);

scalar!(
    session_used_by_id(id: &str) -> i64,
    "SELECT used FROM sessions WHERE id=?1"
);

scalar!(settled_abandon_command_count() -> i64, "SELECT COUNT(*) FROM node_commands WHERE method='abandon' AND state IN ('finished','rejected')");

scalar!(start_turn_retry_delay_since_update() -> i64, "SELECT retry_at-updated FROM node_commands WHERE method='start_turn'");

scalar!(stat_daily_sum_input() -> i64, "SELECT SUM(input) FROM stat_daily");

scalar!(stat_daily_sum_turns() -> i64, "SELECT SUM(turns) FROM stat_daily");

scalar!(
    stream_applied_by_stream_j1(stream: &str) -> i64,
    "SELECT applied FROM node_streams WHERE stream=?1 AND epoch='j1'"
);

scalar!(
    stream_applied_or_zero(node_id: &str, stream: &str, epoch: &str) -> i64,
    "SELECT COALESCE(MAX(applied),0) FROM node_streams WHERE node_id=?1 AND stream=?2 AND epoch=?3"
);

scalar!(
    stream_count_by_node_id(node_id: &str) -> i64,
    "SELECT COUNT(*) FROM node_streams WHERE node_id=?1"
);

scalar!(
    stream_count_by_node_id_with_value(node_id: &str) -> i64,
    "SELECT COUNT(*) FROM node_streams WHERE node_id=?1 AND fault IS NOT NULL"
);

scalar!(
    stream_count_by_stream_e1_without_fault(stream: &str) -> i64,
    "SELECT COUNT(*) FROM node_streams WHERE stream=?1 AND epoch='e1' AND fault IS NULL"
);

scalar!(stream_count_with_value() -> i64, "SELECT COUNT(*) FROM node_streams WHERE fault IS NOT NULL");

scalar!(
    stream_fault_by_stream(stream: &str) -> Option<String>,
    "SELECT fault FROM node_streams WHERE stream=?1"
);

scalar!(tailcat_enabled() -> bool, "SELECT value='true' FROM settings WHERE key='nodes.tailcat'");

scalar!(text_before_second_user_message() -> bool, "SELECT (SELECT MAX(rowid) FROM events WHERE kind='text') < (SELECT rowid FROM events WHERE kind='user' AND data LIKE '%second%')");

scalar!(text_during_disconnect_count() -> i64, "SELECT COUNT(*) FROM events WHERE kind='text' AND data LIKE '%during%'");

scalar!(
    turn_count_by_operation(operation: &str) -> i64,
    "SELECT COUNT(*) FROM agent_turns t JOIN runtime_attempts a ON a.attempt_id=t.node_attempt_id WHERE a.operation_id=?1"
);

scalar!(turn_count_without_ended() -> i64, "SELECT COUNT(*) FROM agent_turns WHERE ended IS NULL");

scalar!(
    unapplied_event_notice_count(session: &str) -> i64,
    "SELECT COUNT(*) FROM events WHERE session_id=?1 AND kind='system' AND data LIKE '%无法应用%'"
);

scalar!(uncertain_start_count() -> i64, "SELECT COUNT(*) FROM node_commands WHERE method='start_turn' AND state='pending' AND lease_owner IS NULL AND maybe_sent=1");

scalar!(unprocessed_acp_event_count() -> i64, "SELECT COUNT(*) FROM events WHERE data LIKE '%sessionUpdate%'");

scalar!(usage_records_call_id() -> String, "SELECT json_extract(detail,'$.call_id') FROM usage_records");

scalar!(usage_records_coalesce_sum_input_0() -> i64, "SELECT COALESCE(SUM(input),0) FROM usage_records");

scalar!(usage_records_count() -> i64, "SELECT COUNT(*) FROM usage_records");

scalar!(usage_records_distinct_json_extract_detail_call_id_count() -> i64, "SELECT COUNT(DISTINCT json_extract(detail,'$.call_id')) FROM usage_records");

scalar!(usage_records_ok() -> i64, "SELECT ok FROM usage_records");

scalar!(
    usage_records_ok_by_live_call(live_call: &str) -> i64,
    "SELECT ok FROM usage_records WHERE live_call_id=?1"
);

scalar!(usage_records_pending_result_128() -> i64, "SELECT json_extract(detail,'$.pending') FROM usage_records WHERE live_call_id='result-128'");

scalar!(usage_records_sum_cost_reported() -> Option<f64>, "SELECT SUM(cost_reported) FROM usage_records");

scalar!(usage_records_sum_input() -> i64, "SELECT SUM(input) FROM usage_records");

scalar!(usage_reports_count() -> i64, "SELECT COUNT(*) FROM usage_reports");

use crate::StoreError;

pub fn pairing_storage_values(store: &Store) -> Result<Vec<String>> {
    store.reader(|c| {
        let mut out = vec![];
        for t in ["nodes", "node_pairings", "node_audit"] {
            let mut st = c.prepare(&format!("SELECT * FROM {t}"))?;
            let n = st.column_count();
            let rows = st.query_map([], |r| {
                let mut v = vec![];
                for i in 0..n {
                    let s = match r.get_ref(i)? {
                        rusqlite::types::ValueRef::Text(t) => String::from_utf8_lossy(t).into(),
                        rusqlite::types::ValueRef::Blob(b) => {
                            b.iter().map(|byte| format!("{byte:02x}")).collect()
                        }
                        other => format!("{other:?}"),
                    };
                    v.push(s);
                }
                Ok(v)
            })?;
            for r in rows {
                out.extend(r?);
            }
        }
        Ok::<_, StoreError>(out)
    })
}

scalar!(
    refused_node_notice_count(session: &str) -> i64,
    "SELECT COUNT(*) FROM events WHERE session_id=? AND kind='system' AND json_extract(data,'$.refused')=1"
);
scalar!(
    cumulative_gateway_input_report_count(input: i64) -> i64,
    "SELECT COUNT(*) FROM gateway_usage_reports WHERE json_extract(payload,'$.measurement')='cumulative' AND json_extract(payload,'$.tokens.input')=?"
);

scalar!(
    running_attempt_count(session: &str) -> i64,
    "SELECT COUNT(*) FROM runtime_attempts a JOIN runtime_turns t ON t.turn_id=a.turn_id WHERE t.session_id=? AND a.state='running'"
);

pub fn usage_token_totals(store: &Store) -> Result<[Option<i64>; 4]> {
    store.reader(|db| {
        Ok(db.query_row(
            "SELECT SUM(input), SUM(output), SUM(cache_read), SUM(cache_write) FROM usage_records",
            [],
            |row| Ok([row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?]),
        )?)
    })
}

scalar!(mode_refusal_code() -> Option<String>, "SELECT mode_refusal_code FROM runtime_sessions");
scalar!(
    attempt_start_committed(attempt: &str) -> bool,
    "SELECT EXISTS(SELECT 1 FROM runtime_attempts a JOIN node_commands c USING(operation_id)
      WHERE a.attempt_id=? AND a.state='running' AND c.state='active')"
);
