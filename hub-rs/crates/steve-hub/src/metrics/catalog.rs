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
];
/// Histograms of durations up to days.
pub(super) const LONG: &[&str] = &["steve_node_link_uptime_seconds", "steve_model_call_seconds"];

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
        "Background job panics (restarted). Labels: job."
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
        "steve_node_command_retries_total",
        Count,
        "Commands to send again later. Labels: node, method."
    );
    describe_gauge!(
        "steve_node_link_up",
        Count,
        "Whether a remote Node's link is up (1) or not (0). Labels: node."
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
    // Models and agents (a finished agent turn; a finished gateway call)
    describe_counter!(
        "steve_model_calls_total",
        Count,
        "Agent turns and gateway calls finished. Labels: source (agent, gateway), harness, model, result (ok or reason code)."
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
        "Main-thread tasks over 50 ms. Labels: page (chat, items, item, sessions, session, inbox, other)."
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
        "steve_metrics_collection_errors_total",
        Count,
        "Figures that could not be collected. Labels: source."
    );
}
