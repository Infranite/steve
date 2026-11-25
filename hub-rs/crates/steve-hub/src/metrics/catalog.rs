//! Every metric the Hub exports, in one place: name, unit, what it measures and its labels. Names
//! are `steve_<layer>_<object>_<unit>`. Each is recorded at the one entry of its boundary (named
//! after the description); labels take bounded values only.
use metrics::Unit::{Bytes, Count, Seconds};
use metrics::{describe_counter, describe_gauge, describe_histogram};

/// Histograms of counts, not seconds.
pub(super) const COUNTS: &[&str] = &[
    "steve_http_store_calls_per_request",
    "steve_node_ingest_events",
    "steve_projection_snapshot_store_calls",
    "steve_projection_flush_entities",
    "steve_transcript_page_events",
];
/// Histograms of durations up to days.
pub(super) const LONG: &[&str] = &[
    "steve_node_link_uptime_seconds",
    "steve_model_call_seconds",
    "steve_backup_operation_seconds",
    "steve_backup_phase_seconds",
    "steve_backup_engine_command_seconds",
];
/// Histograms of token counts across a turn's context window.
pub(super) const TOKENS: &[&str] = &["steve_turn_context_peak_tokens"];

pub(super) fn describe() {
    // Session approval: applied-mode gauge and post-commit request/turn counts.
    describe_gauge!(
        "steve_approval_auto_sessions",
        Count,
        "Sessions with the native auto mode applied, sampled every minute."
    );
    describe_counter!(
        "steve_approval_auto_turns_total",
        "Logical session turns started in the harness-native approval mode."
    );
    describe_counter!(
        "steve_approval_requests_total",
        "Session ACP permission requests received by Steve, by resolver and ingress outcome."
    );
    describe_counter!(
        "steve_approval_questions_total",
        "Agent question cards created for the owner."
    );
    describe_histogram!(
        "steve_item_owner_finish_seconds",
        Seconds,
        "Time to commit an owner's explicit item finish."
    );
    describe_histogram!(
        "steve_bot_selection_seconds",
        Seconds,
        "Time to assess and rank one Bot dispatch request."
    );
    describe_counter!(
        "steve_bot_selection_blocked_total",
        Count,
        "Requests with no eligible placement. Counted once for each distinct reason. Label: reason."
    );
    describe_counter!(
        "steve_dispatch_no_executor_total",
        Count,
        "Automatic dispatch requests where no eligible non-main Bot could execute. Labels: policy (ask, steve), reason."
    );
    describe_counter!(
        "steve_no_executor_decisions_opened_total",
        Count,
        "Workspace-scoped no-executor AskOwner decision cards opened."
    );
    describe_counter!(
        "steve_no_executor_decisions_settled_total",
        Count,
        "Workspace-scoped no-executor decision cards settled. Label: choice (assign_bot, confirm_proposal, later, dismiss, supersede)."
    );
    describe_counter!(
        "steve_coordinator_self_executions_total",
        Count,
        "Items taken over by the coordinator (Steve) when no executor Bot was eligible. Label: source (card, setting)."
    );
    describe_counter!(
        "steve_item_automatic_delivery_total",
        Count,
        "Automatic delivery checks. Label: result (verified or unverified)."
    );
    describe_counter!(
        "steve_item_stale_delivery_total",
        Count,
        "Delivery verification rejected because its item cycle or scope changed."
    );
    describe_histogram!(
        "steve_decision_transition_seconds",
        Seconds,
        "Time spent applying a decision transition in the store. Label: operation."
    );
    describe_counter!(
        "steve_decision_duplicate_prompts_total",
        Count,
        "Repeated permission requests suppressed by their existing attempt and request identity."
    );
    describe_gauge!(
        "steve_decision_open",
        Count,
        "Open decisions in the Hub projection. Label: tier (decide, draft, or folded)."
    );
    describe_counter!(
        "steve_wait_timeouts_total",
        Count,
        "Durable waits settled by the unified WaitContract due sweep. Labels: kind, action."
    );
    describe_histogram!(
        "steve_wait_sweep_duration_seconds",
        Seconds,
        "Duration of one unified WaitContract due sweep."
    );
    // Dynamic script workflows (WF-3)
    describe_histogram!(
        "steve_wf_step_duration_seconds",
        Seconds,
        "Wall-clock duration of one sandboxed workflow JS step. Label: outcome."
    );
    describe_counter!(
        "steve_wf_calls_total",
        Count,
        "Workflow calls by outcome, review flag, and cross-version hydration. Labels: outcome, review, hydrated."
    );
    describe_counter!(
        "steve_wf_deliver_schema_reject_total",
        Count,
        "Workflow child steve_deliver calls rejected before file reception. Label: reason."
    );
    describe_counter!(
        "steve_wf_review_gate_total",
        Count,
        "Hub-authoritative independent final review gate evaluations. Label: result."
    );
    describe_gauge!(
        "steve_wf_active_runs",
        Count,
        "Currently non-terminal (running or paused) script workflow runs."
    );
    // HTTP (metrics::http, the outermost layer)
    describe_counter!(
        "steve_http_requests_total",
        Count,
        "Requests answered. Labels: route (template), method, status (class)."
    );
    describe_histogram!(
        "steve_http_request_duration_seconds",
        Seconds,
        "Time to response headers. Labels: route, method, status."
    );
    describe_counter!(
        "steve_http_response_bytes_total",
        Bytes,
        "Response body bytes sent. Labels: route, method, status."
    );
    describe_histogram!(
        "steve_http_store_calls_per_request",
        Count,
        "Store connections one request took. Labels: route."
    );
    describe_counter!(
        "steve_http_sse_connections_total",
        Count,
        "Event streams opened. Labels: route."
    );
    // SSE (api::stream, the one subscriber loop)
    describe_counter!(
        "steve_sse_frames_total",
        Count,
        "Frames written to event streams. Labels: type (patch, live, event, control)."
    );
    describe_counter!(
        "steve_sse_bytes_total",
        Bytes,
        "Bytes written to event streams (frames, replays, pings)."
    );
    describe_histogram!(
        "steve_sse_delivery_seconds",
        Seconds,
        "From the publish after commit to the frame's write."
    );
    describe_counter!(
        "steve_sse_coalesced_frames_total",
        Count,
        "Frames that joined a chunk already being written."
    );
    describe_counter!(
        "steve_sse_lagged_total",
        Count,
        "Times a stream fell behind the bus and replayed or reset."
    );
    describe_gauge!(
        "steve_sse_active_connections",
        Count,
        "Currently open SSE event stream connections."
    );
    // Bus (bus::dispatch, the one dispatcher thread)
    describe_counter!(
        "steve_bus_published_total",
        Count,
        "Events published. Labels: type."
    );
    describe_counter!(
        "steve_bus_tap_evicted_total",
        Count,
        "Events an internal subscriber lost to its full overflow."
    );
    describe_gauge!(
        "steve_bus_subscriber_lag",
        Count,
        "Frames the slowest stream subscriber has not read yet."
    );
    describe_histogram!(
        "steve_backup_operation_seconds",
        Seconds,
        "Backup operation time. Labels: operation, purpose."
    );
    describe_histogram!(
        "steve_backup_phase_seconds",
        Seconds,
        "Backup phase duration across SQLite, staging and repository boundaries. Labels: phase, purpose."
    );
    describe_counter!(
        "steve_backup_engine_commands_total",
        Count,
        "Backup engine subprocess invocations. Labels: command, result."
    );
    describe_histogram!(
        "steve_backup_engine_command_seconds",
        Seconds,
        "Backup engine subprocess duration. Labels: command."
    );
    describe_counter!(
        "steve_backup_written_bytes_total",
        Bytes,
        "Bytes written by repository initialization, completed backups or restores. Labels: operation, purpose."
    );
    describe_counter!(
        "steve_backup_staged_files_total",
        Count,
        "Allowlisted installation files staged for backup. Labels: purpose."
    );
    describe_counter!(
        "steve_backup_staged_bytes_total",
        Bytes,
        "Bytes copied while staging allowlisted installation files for backup. Labels: purpose."
    );
    describe_counter!(
        "steve_backup_skipped_caches_total",
        Count,
        "Disposable workspace cache directories excluded during backup staging. Labels: purpose."
    );
    describe_counter!(
        "steve_backup_snapshots_deleted_total",
        Count,
        "Expired or incomplete backup snapshots forgotten during retention. Labels: purpose."
    );
    describe_counter!(
        "steve_backup_reclaimed_bytes_total",
        Bytes,
        "Repository bytes reclaimed by backup pruning."
    );
    describe_counter!(
        "steve_backup_failures_total",
        Count,
        "Failed backup operations. Labels: operation, purpose, code."
    );
    describe_gauge!(
        "steve_backup_repository_bytes",
        Bytes,
        "Total bytes in the backup repository."
    );
    describe_gauge!(
        "steve_backup_destination_available_bytes",
        Bytes,
        "Available bytes at the backup destination."
    );
    describe_gauge!(
        "steve_backup_destination_required_bytes",
        Bytes,
        "Estimated space for staging and writing a full database backup, based on the SQLite database and WAL sizes."
    );
    // Read projection (projection: its one task)
    describe_histogram!(
        "steve_projection_snapshot_store_calls",
        Count,
        "Store connections the snapshot handler took after authorization."
    );
    describe_histogram!(
        "steve_projection_flush_seconds",
        Seconds,
        "One flush: reading dirty views again and sending what changed. Labels: kind."
    );
    describe_histogram!(
        "steve_projection_flush_entities",
        Count,
        "Views one flush read again. Labels: kind."
    );
    describe_counter!(
        "steve_projection_patch_ops_total",
        Count,
        "Changes sent in patches. Labels: kind, op (upsert, remove)."
    );
    describe_histogram!(
        "steve_projection_load_seconds",
        Seconds,
        "Time to load every hot view at boot. Labels: kind."
    );
    describe_histogram!(
        "steve_projection_delay_seconds",
        Seconds,
        "From the first commit a patch carries to the patch’s publish. Labels: kind."
    );
    describe_gauge!(
        "steve_projection_entities",
        Count,
        "Hot views held. Labels: kind."
    );
    describe_gauge!(
        "steve_projection_bytes",
        Bytes,
        "Encoded bytes of the hot views held. Labels: kind."
    );
    describe_counter!(
        "steve_projection_reconcile_diff_total",
        Count,
        "Views that differed from the store with no change on its way (a write that published no event; must stay 0). Labels: kind."
    );
    describe_histogram!(
        "steve_projection_reconcile_seconds",
        Seconds,
        "One reconciliation of every workspace's hot views."
    );
    describe_counter!(
        "steve_projection_rebuilds_total",
        Count,
        "Hot views read again as a whole. Labels: reason (start, tap_lost, reconcile)."
    );
    // Store (steve_store::instrumentation)
    describe_counter!(
        "steve_store_transactions_total",
        Count,
        "Write transactions begun."
    );
    describe_counter!(
        "steve_store_transaction_outcomes_total",
        Count,
        "Write transactions finished. Labels: kind (root, savepoint), outcome (commit, rollback)."
    );
    describe_histogram!(
        "steve_store_transaction_duration_seconds",
        Seconds,
        "Write transaction duration. Labels: kind (root, savepoint), outcome (commit, rollback)."
    );
    describe_counter!(
        "steve_store_queries_total",
        Count,
        "Storage backend operations. Labels: op (query, exec, batch), family, result (ok, err)."
    );
    describe_histogram!(
        "steve_store_query_duration_seconds",
        Seconds,
        "Storage backend operation duration. Labels: op (query, exec, batch), family."
    );
    describe_histogram!(
        "steve_store_read_pool_wait_seconds",
        Seconds,
        "Time waiting to borrow a read connection from the pool."
    );
    describe_counter!(
        "steve_store_read_pool_acquisitions_total",
        Count,
        "Read pool acquisitions. Labels: result (ok, err)."
    );
    describe_gauge!(
        "steve_store_read_pool_active",
        Count,
        "Read connections currently checked out from the pool."
    );
    describe_histogram!(
        "steve_lock_wait_seconds",
        Seconds,
        "Waiting for a lock. Labels: lock (store = the write connection, core), caller (source line)."
    );
    describe_histogram!(
        "steve_lock_hold_seconds",
        Seconds,
        "Holding a lock. Labels: lock, caller."
    );
    describe_counter!(
        "steve_lock_acquisitions_total",
        Count,
        "Lock acquisitions. Labels: lock, caller."
    );
    describe_counter!(
        "steve_retention_affected_total",
        Count,
        "Rows retention removed. Labels: entity."
    );
    describe_histogram!(
        "steve_retention_slice_seconds",
        Seconds,
        "One retention slice. Labels: entity."
    );
    describe_counter!(
        "steve_retention_overruns_total",
        Count,
        "Retention slices over their budget. Labels: entity."
    );
    // Runtime (steve_runtime: cache, jobs, due loops)
    describe_counter!(
        "steve_cache_requests_total",
        Count,
        "Cache lookups. Labels: space, result (hit, miss, off)."
    );
    describe_histogram!(
        "steve_cache_load_seconds",
        Seconds,
        "Loading a missed entry. Labels: space."
    );
    describe_counter!(
        "steve_cache_invalidations_total",
        Count,
        "Tag invalidations. Labels: kind."
    );
    describe_gauge!("steve_cache_entries", Count, "Entries held. Labels: space.");
    describe_gauge!("steve_cache_bytes", Bytes, "Bytes held. Labels: space.");
    describe_counter!(
        "steve_job_runs_total",
        Count,
        "Background job runs. Labels: job."
    );
    describe_counter!(
        "steve_job_panics_total",
        Count,
        "Background job panics (restarted with bounded backoff). Labels: job."
    );
    describe_counter!(
        "steve_job_restarts_total",
        Count,
        "Background job restarts after a panic. Labels: job."
    );
    describe_gauge!(
        "steve_job_up",
        Count,
        "Whether a background job is running (1) or stopped after consecutive panics (0). Labels: job."
    );
    describe_histogram!(
        "steve_job_duration_seconds",
        Seconds,
        "One background job run. Labels: job."
    );
    describe_histogram!(
        "steve_due_lag_seconds",
        Seconds,
        "How late a due loop woke. Labels: loop."
    );
    describe_counter!(
        "steve_due_runs_total",
        Count,
        "Due loop cycles completed. Labels: loop, result (ok, err)."
    );
    describe_histogram!(
        "steve_due_run_duration_seconds",
        Seconds,
        "Duration of one due loop cycle. Labels: loop."
    );
    describe_counter!(
        "steve_due_errors_total",
        Count,
        "Due loop failures. Labels: loop, stage (next_due, run_due)."
    );
    describe_counter!(
        "steve_periodic_task_evaluations_total",
        Count,
        "Periodic task gate evaluations. Labels: task, outcome (triggered, skipped), reason (changed, no_change, queue_driven, disabled, not_due, overlap)."
    );
    describe_counter!(
        "steve_periodic_task_tokens_total",
        Count,
        "Model tokens spent by periodic tasks. Labels: task, kind (input, output, cache_read, cache_write)."
    );
    describe_counter!(
        "steve_model_calls_avoided_total",
        Count,
        "Model calls avoided by deterministic gating, batching, deduplication, or caching. Labels: purpose, reason (no_change, queue_driven, batched, cached, deduplicated, deterministic)."
    );
    // Node protocol, Hub side (node_runtime: receive, send_one; RemoteNode attach/detach)
    describe_counter!(
        "steve_session_steve_tools_missing_total",
        Count,
        "Attempts whose harness never listed Steve's tools (a Node diagnostic)."
    );
    describe_counter!(
        "steve_node_ingest_commits_total",
        Count,
        "Event batches committed. Labels: node."
    );
    describe_histogram!(
        "steve_node_ingest_events",
        Count,
        "Events per committed batch. Labels: node."
    );
    describe_histogram!(
        "steve_node_ingest_seconds",
        Seconds,
        "Applying and committing one batch. Labels: node."
    );
    describe_counter!(
        "steve_node_commands_total",
        Count,
        "Commands sent. Labels: node, method, result (ok or error code)."
    );
    describe_histogram!(
        "steve_node_command_seconds",
        Seconds,
        "From sending a command to its receipt. Labels: node, method."
    );
    describe_histogram!(
        "steve_node_mode_state_seconds",
        Seconds,
        "Reading a session's permission mode state. Labels: node."
    );
    describe_counter!(
        "steve_orchestration_rework_total",
        Count,
        "Accepted changes to a work graph due to rework evidence. Labels: cause."
    );
    describe_counter!(
        "steve_orchestration_stalls_total",
        Count,
        "Logical turns held by a repeated failure or lack of progress. Labels: reason."
    );
    describe_histogram!(
        "steve_orchestration_stall_seconds",
        Seconds,
        "Time from the first observed stall evidence to the hold. Labels: reason."
    );
    describe_histogram!(
        "steve_orchestration_stall_stop_seconds",
        Seconds,
        "Time from a stall hold to the observed end of the attempt. Labels: reason."
    );
    describe_counter!(
        "steve_orchestration_stall_tokens_total",
        Count,
        "Positive accounting increments reported after a stall hold. Labels: reason, kind."
    );
    describe_counter!(
        "steve_node_command_retries_total",
        Count,
        "Commands to send again later. Labels: node, method."
    );
    describe_gauge!(
        "steve_node_link_up",
        Count,
        "Whether a remote Node's link is up (1) or not (0). Labels: node."
    );
    describe_gauge!(
        "steve_node_link_mode",
        Count,
        "Current link mode of a Node (1 for active mode, 0 otherwise). Labels: node, mode (normal, upgrade_only)."
    );
    describe_gauge!(
        "steve_node_link_protocol_delta",
        Count,
        "Protocol version difference between Hub and linked Node (hub_proto - node_proto; 0 when matching or offline). Labels: node."
    );
    describe_counter!(
        "steve_node_link_connects_total",
        Count,
        "Links established. Labels: node."
    );
    describe_counter!(
        "steve_node_link_disconnects_total",
        Count,
        "Links lost or replaced. Labels: node."
    );
    describe_histogram!(
        "steve_node_link_uptime_seconds",
        Seconds,
        "How long a link lasted. Labels: node."
    );
    describe_counter!(
        "steve_subtask_updates_total",
        Count,
        "Subtask state updates observed from Nodes. Labels: origin (subagent, background_task, native_workflow), status."
    );
    describe_counter!(
        "steve_subtask_cancels_total",
        Count,
        "Subtask cancel requests sent to Nodes. Labels: node, outcome (cancelled, noop, error)."
    );
    describe_counter!(
        "steve_hub_harness_upgrades_total",
        Count,
        "Harness upgrades finished on Nodes. Labels: trigger (manual, auto), outcome (succeeded, failed)."
    );
    describe_counter!(
        "steve_hub_harness_auto_upgrade_policy_changes_total",
        Count,
        "Owner updates to per-Node or per-harness automatic upgrade policy."
    );
    // Node meters, from each Node's vitals (nodes::vitals); totals count from the Node's start
    describe_gauge!(
        "steve_node_sample_timestamp_seconds",
        Seconds,
        "Hub receipt time of the latest observation. Label: node."
    );
    describe_gauge!(
        "steve_node_cpu_load_ratio",
        "One-minute machine load per logical CPU. Label: node."
    );
    describe_gauge!(
        "steve_node_memory_total_bytes",
        Bytes,
        "Total memory on the Node. Label: node."
    );
    describe_gauge!(
        "steve_node_memory_available_bytes",
        Bytes,
        "Memory available without swapping. Label: node."
    );
    describe_gauge!(
        "steve_node_memory_used_ratio",
        "Fraction of memory in use on the Node. Label: node."
    );
    describe_gauge!(
        "steve_node_rtt_seconds",
        Seconds,
        "Round trip to a remote Node. Label: node."
    );
    describe_gauge!(
        "steve_node_network_received_bytes_per_second",
        "Receive rate across non-loopback interfaces. Label: node."
    );
    describe_gauge!(
        "steve_node_network_sent_bytes_per_second",
        "Send rate across non-loopback interfaces. Label: node."
    );
    describe_gauge!(
        "steve_node_network_received_bytes",
        Bytes,
        "Bytes received by the host's non-loopback interfaces. Labels: node."
    );
    describe_gauge!(
        "steve_node_network_sent_bytes",
        Bytes,
        "Bytes sent by the host's non-loopback interfaces. Labels: node."
    );
    describe_gauge!(
        "steve_node_journal_bytes",
        Bytes,
        "Live journal bytes. Labels: node."
    );
    describe_gauge!(
        "steve_node_journal_file_bytes",
        Bytes,
        "Journal file size. Labels: node."
    );
    describe_gauge!(
        "steve_node_journal_commits",
        Count,
        "Journal commits since the Node started (one fsync each). Labels: node."
    );
    describe_gauge!(
        "steve_node_event_backlog",
        Count,
        "Events the Hub has not acknowledged. Labels: node."
    );
    describe_gauge!(
        "steve_node_event_backlog_bytes",
        Bytes,
        "Their bytes. Labels: node."
    );
    describe_gauge!(
        "steve_node_adapters",
        Count,
        "Harness process groups running. Labels: node."
    );
    describe_gauge!(
        "steve_node_adapter_cpu_seconds",
        Seconds,
        "CPU time of their live processes. Labels: node."
    );
    describe_gauge!(
        "steve_node_adapter_resident_bytes",
        Bytes,
        "Resident memory of their live processes. Labels: node."
    );
    describe_gauge!(
        "steve_node_process_cpu_seconds",
        Seconds,
        "The Node process's CPU time since it started. Labels: node."
    );
    describe_gauge!(
        "steve_node_process_resident_bytes",
        Bytes,
        "The Node process's resident memory. Labels: node."
    );
    describe_gauge!(
        "steve_node_process_open_fds",
        Count,
        "The Node process's open descriptors. Labels: node."
    );
    describe_gauge!(
        "steve_node_process_threads",
        Count,
        "The Node process's threads. Labels: node."
    );
    describe_gauge!(
        "steve_node_process_disk_read_bytes",
        Bytes,
        "Bytes the Node process read from disk since it started. Labels: node."
    );
    describe_gauge!(
        "steve_node_process_disk_written_bytes",
        Bytes,
        "Bytes the Node process wrote to disk since it started. Labels: node."
    );
    // Plugins (ext_ws sessions, measured by the Hub; plugins::run)
    describe_counter!(
        "steve_plugin_calls_total",
        Count,
        "Requests to a channel service. Labels: plugin, method, result (ok or reason)."
    );
    describe_histogram!(
        "steve_plugin_call_seconds",
        Seconds,
        "One request to its answer. Labels: plugin, method."
    );
    describe_gauge!(
        "steve_plugin_inbound_backlog",
        Count,
        "Notifications received and not taken yet. Labels: plugin."
    );
    describe_counter!(
        "steve_plugin_restarts_total",
        Count,
        "Plugin processes that exited and are started again. Labels: plugin."
    );
    describe_gauge!(
        "steve_plugin_processes",
        Count,
        "Running child processes of a plugin. Labels: plugin."
    );
    describe_gauge!(
        "steve_plugin_process_cpu_seconds",
        Seconds,
        "CPU time of running plugin child processes. Labels: plugin."
    );
    describe_gauge!(
        "steve_plugin_process_resident_bytes",
        Bytes,
        "Resident memory of running plugin child processes. Labels: plugin."
    );
    describe_gauge!(
        "steve_plugin_process_open_fds",
        Count,
        "Open file descriptors of running plugin child processes. Labels: plugin."
    );
    describe_gauge!(
        "steve_plugin_process_threads",
        Count,
        "Threads of running plugin child processes. Labels: plugin."
    );
    describe_gauge!(
        "steve_plugin_process_disk_read_bytes",
        Bytes,
        "Disk bytes read by running plugin child processes. Labels: plugin."
    );
    describe_gauge!(
        "steve_plugin_process_disk_written_bytes",
        Bytes,
        "Disk bytes written by running plugin child processes. Labels: plugin."
    );
    describe_counter!(
        "steve_plugin_upstream_requests_total",
        Count,
        "Upstream requests reported by a plugin via metrics.report. Labels: plugin."
    );
    describe_counter!(
        "steve_plugin_upstream_errors_total",
        Count,
        "Upstream request errors reported by a plugin via metrics.report. Labels: plugin."
    );
    describe_histogram!(
        "steve_plugin_upstream_latency_seconds",
        Seconds,
        "Upstream request latency reported by a plugin via metrics.report. Labels: plugin."
    );
    describe_counter!(
        "steve_plugin_inbound_events_total",
        Count,
        "Inbound events reported by a plugin via metrics.report. Labels: plugin."
    );
    describe_counter!(
        "steve_plugin_outbound_events_total",
        Count,
        "Outbound events reported by a plugin via metrics.report. Labels: plugin."
    );
    describe_gauge!(
        "steve_plugin_queue_depth",
        Count,
        "Plugin internal queue depth reported via metrics.report. Labels: plugin."
    );
    describe_gauge!(
        "steve_plugin_upstream_connected",
        Count,
        "Whether a plugin's upstream link is connected (1) or not (0). Labels: plugin."
    );
    describe_counter!(
        "steve_plugin_upstream_reconnects_total",
        Count,
        "Plugin upstream reconnects reported via metrics.report. Labels: plugin."
    );
    // Built-in integrations (steve-mail, steve-calendar, Feishu)
    describe_counter!(
        "steve_mail_sync_total",
        Count,
        "Mailbox sync runs finished. Labels: adapter, result (ok, auth, transient, error)."
    );
    describe_histogram!(
        "steve_mail_sync_duration_seconds",
        Seconds,
        "Duration of one mailbox sync. Labels: adapter."
    );
    describe_counter!(
        "steve_mail_messages_synced_total",
        Count,
        "Messages fetched by mailbox syncs. Labels: adapter."
    );
    describe_counter!(
        "steve_mail_sends_total",
        Count,
        "Approved mail sends attempted. Labels: adapter, result (sent, blocked, failed, unknown, error)."
    );
    describe_histogram!(
        "steve_mail_send_duration_seconds",
        Seconds,
        "Duration of one mail send attempt. Labels: adapter."
    );
    describe_counter!(
        "steve_calendar_sync_total",
        Count,
        "Calendar connection sync runs finished. Labels: source, result (ok, err)."
    );
    describe_histogram!(
        "steve_calendar_sync_duration_seconds",
        Seconds,
        "Duration of one calendar connection sync. Labels: source."
    );
    describe_counter!(
        "steve_calendar_events_synced_total",
        Count,
        "Calendar events written by syncs. Labels: source, op (changed, deleted)."
    );
    describe_counter!(
        "steve_feishu_api_requests_total",
        Count,
        "Feishu open-platform API calls. Labels: endpoint, result."
    );
    describe_histogram!(
        "steve_feishu_api_duration_seconds",
        Seconds,
        "Duration of one Feishu open-platform API call. Labels: endpoint."
    );
    describe_counter!(
        "steve_feishu_sends_total",
        Count,
        "Feishu outbound message sends. Labels: result (ok, err)."
    );
    describe_histogram!(
        "steve_feishu_send_duration_seconds",
        Seconds,
        "Duration of one Feishu outbound message send."
    );
    describe_counter!(
        "steve_feishu_inbound_events_total",
        Count,
        "Feishu inbound events handled. Labels: kind."
    );
    describe_histogram!(
        "steve_feishu_inbound_duration_seconds",
        Seconds,
        "Duration of handling one Feishu inbound event. Labels: kind."
    );
    describe_counter!(
        "steve_feishu_ws_connections_total",
        Count,
        "Feishu long-connection lifecycle transitions. Labels: result (connected, reconnected, disconnected, failed)."
    );
    // Models and agents (a finished agent turn; a finished gateway call)
    describe_counter!(
        "steve_model_calls_total",
        Count,
        "Agent turns and gateway calls finished. Labels: source (agent, gateway), harness, model, result (ok or reason code)."
    );
    describe_counter!(
        "steve_gateway_timeout_total",
        Count,
        "Gateway calls that timed out. Labels: phase (queue, first_byte, execution, unknown)."
    );
    describe_counter!(
        "steve_gateway_preflight_rejections_total",
        Count,
        "Gateway calls stopped before dispatch. Labels: reason (missing_capability, tool_denied)."
    );
    describe_counter!(
        "steve_gateway_handoffs_total",
        Count,
        "Controlled gateway handoffs that created a work item."
    );
    describe_counter!(
        "steve_gateway_requests_total",
        Count,
        "Think prompts confirmed submitted by a Node's adapter transport."
    );
    describe_histogram!(
        "steve_model_call_seconds",
        Seconds,
        "One turn or call. Labels: source, harness."
    );
    describe_counter!(
        "steve_model_tokens_total",
        Count,
        "Tokens. Labels: source, harness, model, kind (input, output, cache_read, cache_write)."
    );
    describe_counter!(
        "steve_model_cost_micro_usd_total",
        Count,
        "Reported cost in millionths of a dollar. Labels: source, harness, model."
    );
    describe_histogram!(
        "steve_turn_context_peak_tokens",
        Count,
        "Peak prompt context tokens observed during an agent turn. Labels: harness."
    );
    describe_counter!(
        "steve_handoffs_total",
        Count,
        "Session handoffs executed. Labels: reason (quota_exhausted, provider_error, owner_request), from_harness, to_harness, switched_bot."
    );
    describe_counter!(
        "steve_handoff_outcomes_total",
        Count,
        "First-turn outcomes of sessions created via handoff. Labels: reason, outcome."
    );
    describe_histogram!(
        "steve_handoff_package_bytes",
        Bytes,
        "Serialized size of the working-memory opening prompt sent on session handoff. Labels: reason."
    );
    // This process (sampled when read)
    describe_gauge!(
        "steve_process_cpu_seconds",
        Seconds,
        "CPU time since the Hub started."
    );
    describe_gauge!("steve_process_resident_bytes", Bytes, "Resident memory.");
    describe_gauge!("steve_process_open_fds", Count, "Open descriptors.");
    describe_gauge!("steve_process_threads", Count, "Threads.");
    describe_gauge!(
        "steve_process_disk_read_bytes",
        Bytes,
        "Bytes read from disk since the Hub started."
    );
    describe_gauge!(
        "steve_process_disk_written_bytes",
        Bytes,
        "Bytes written to disk since the Hub started."
    );
    // Web client (POST /api/system/web-perf)
    describe_counter!(
        "steve_web_reports_total",
        Count,
        "Performance reports the web client sent."
    );
    describe_histogram!(
        "steve_web_long_task_seconds",
        Seconds,
        "Main-thread tasks over 50 ms. Labels: page (bounded nav destination or detail route)."
    );
    describe_histogram!(
        "steve_web_interaction_seconds",
        Seconds,
        "Interaction latency, input to next paint (the samples of INP). Labels: page."
    );
    describe_histogram!(
        "steve_web_patch_apply_seconds",
        Seconds,
        "Merging one patch frame into the web client's views. Labels: page."
    );
    describe_histogram!(
        "steve_web_snapshot_seconds",
        Seconds,
        "Applying a full view snapshot in the web client. Labels: page."
    );
    describe_gauge!(
        "steve_web_dom_nodes",
        Count,
        "DOM element count sampled by the web client. Labels: page."
    );
    describe_histogram!(
        "steve_transcript_page_events",
        Count,
        "Events delivered in a session transcript page."
    );
    describe_histogram!(
        "steve_web_transcript_open_seconds",
        Seconds,
        "Time from mounting a session transcript to rendering its latest turns. Labels: page."
    );
    describe_counter!(
        "steve_web_nav_opens_total",
        Count,
        "Menu item opens reported by the web client. Labels: item."
    );
    describe_counter!(
        "steve_web_errors_total",
        Count,
        "Uncaught JS errors and unhandled promise rejections reported by the web client. Labels: page, kind."
    );
    describe_counter!(
        "steve_web_sse_reconnects_total",
        Count,
        "SSE stream reconnect attempts reported by the web client. Labels: reason (error, stale, wake)."
    );
    describe_counter!(
        "steve_metrics_collection_errors_total",
        Count,
        "Figures that could not be collected. Labels: source."
    );
    // Knowledge extraction, project AGENTS.md proposals, and steward trigger gating
    describe_counter!(
        "steve_knowledge_extract_produced_total",
        Count,
        "Knowledge items produced by extraction. Labels: kind (fact, preference, rule, event, agents_proposal)."
    );
    describe_counter!(
        "steve_knowledge_extract_dropped_total",
        Count,
        "Extraction outputs dropped by deterministic validation. Labels: reason (duplicate, shape, no_evidence, one_off, restates_goal, missing_about, unresolved_about, cap_reached)."
    );
    describe_counter!(
        "steve_knowledge_proposals_total",
        Count,
        "Project AGENTS.md proposals lifecycle events. Labels: action (created, accepted, dismissed)."
    );
    describe_counter!(
        "steve_knowledge_steward_runs_total",
        Count,
        "Knowledge steward schedule trigger evaluations. Labels: outcome (triggered, skipped), reason (threshold, manual, nothing_to_tidy, below_threshold)."
    );
    describe_counter!(
        "steve_security_refusals_total",
        Count,
        "Actions refused at a security or tenant boundary. Labels: boundary, reason."
    );
}
