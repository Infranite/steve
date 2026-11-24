-- Dynamic script workflow ledger (WF-3): 2 core aggregates (`wf_runs`, `wf_calls`) plus 3 strict
-- subordinate tables (`wf_script_versions`, `wf_call_attempts`, `wf_version_calls`).
-- Written in portable SQLite / PostgreSQL DDL; zero cached budget counters or separate audit tables.

CREATE TABLE wf_runs (
    id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL,
    parent_item_id TEXT NOT NULL,
    parent_cycle BIGINT NOT NULL,
    goal_version BIGINT NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('running','paused','completed','failed','cancelled','superseded')),
    pause_reason_code TEXT,
    active_script_version BIGINT NOT NULL DEFAULT 1,
    run_revision BIGINT NOT NULL DEFAULT 1,
    single_session_override BIGINT NOT NULL DEFAULT 0 CHECK(single_session_override IN (0,1)),
    review_verified_call_id TEXT,
    extra_attempts_budget BIGINT NOT NULL DEFAULT 0,
    result_json TEXT,
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL,
    UNIQUE(workspace_id, parent_item_id, parent_cycle, goal_version)
);

CREATE INDEX wf_runs_active ON wf_runs(status, workspace_id);
CREATE INDEX wf_runs_parent_cycle ON wf_runs(parent_item_id, parent_cycle);

CREATE TABLE wf_script_versions (
    run_id TEXT NOT NULL REFERENCES wf_runs(id) ON DELETE CASCADE,
    version BIGINT NOT NULL,
    script_source TEXT NOT NULL,
    script_sha256 TEXT NOT NULL,
    created_by_session_id TEXT,
    created_at BIGINT NOT NULL,
    PRIMARY KEY (run_id, version)
);

CREATE TABLE wf_calls (
    id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL REFERENCES wf_runs(id) ON DELETE CASCADE,
    workspace_id TEXT NOT NULL,
    identity_hash TEXT NOT NULL,
    phase TEXT,
    title TEXT,
    prompt TEXT NOT NULL,
    options_canonical_json TEXT NOT NULL,
    schema_json TEXT,
    occurrence BIGINT NOT NULL,
    is_review BIGINT NOT NULL DEFAULT 0 CHECK(is_review IN (0,1)),
    mutex_key TEXT,
    status TEXT NOT NULL CHECK(status IN ('queued','dispatched','completed','failed','cancelled')),
    result_json TEXT,
    artifact_ids_json TEXT,
    settled_at_revision BIGINT,
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL,
    UNIQUE(run_id, identity_hash)
);

CREATE INDEX wf_calls_run_status ON wf_calls(run_id, status);
CREATE INDEX wf_calls_mutex ON wf_calls(workspace_id, mutex_key)
    WHERE mutex_key IS NOT NULL AND status = 'dispatched';

CREATE TABLE wf_call_attempts (
    id TEXT PRIMARY KEY,
    call_id TEXT NOT NULL REFERENCES wf_calls(id) ON DELETE CASCADE,
    run_id TEXT NOT NULL REFERENCES wf_runs(id) ON DELETE CASCADE,
    parent_item_id TEXT NOT NULL,
    parent_cycle BIGINT NOT NULL,
    attempt_no BIGINT NOT NULL,
    child_item_id TEXT UNIQUE,
    bot_id TEXT,
    placement_id TEXT,
    harness TEXT,
    model TEXT,
    status TEXT NOT NULL CHECK(status IN ('blocked','dispatched','closing','succeeded','failed','cancelled')),
    dispatched_at_revision BIGINT NOT NULL,
    closing_delivery_revision BIGINT,
    candidate_result_json TEXT,
    candidate_artifact_ids_json TEXT,
    error_code TEXT,
    error_message TEXT,
    created_at BIGINT NOT NULL,
    settled_at BIGINT,
    UNIQUE(call_id, attempt_no)
);

CREATE UNIQUE INDEX wf_call_attempts_one_active ON wf_call_attempts(call_id)
    WHERE status IN ('blocked','dispatched','closing');
CREATE UNIQUE INDEX wf_call_attempts_one_succeeded ON wf_call_attempts(call_id)
    WHERE status = 'succeeded';
CREATE INDEX wf_call_attempts_cycle_budget ON wf_call_attempts(parent_item_id, parent_cycle);
CREATE INDEX wf_call_attempts_run_status ON wf_call_attempts(run_id, status);

CREATE TABLE wf_version_calls (
    run_id TEXT NOT NULL REFERENCES wf_runs(id) ON DELETE CASCADE,
    script_version BIGINT NOT NULL,
    seq_no BIGINT NOT NULL,
    call_id TEXT NOT NULL REFERENCES wf_calls(id) ON DELETE CASCADE,
    hydrated_from_version BIGINT,
    PRIMARY KEY (run_id, script_version, seq_no)
);

CREATE INDEX wf_version_calls_call ON wf_version_calls(call_id);
