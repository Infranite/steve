//! Database observations for isolated process-level Node tests.

use std::path::Path;

use crate::Result;

macro_rules! inspect {
    ($name:ident($($arg:ident: $ty:ty),* $(,)?) -> $ret:ty, $sql:literal $(,)?) => {
        pub fn $name(path: &Path, $($arg: $ty),*) -> Result<$ret> {
            let db = super::inspect_database(path)?;
            Ok(db.query_row($sql, rusqlite::params![$($arg),*], |row| row.get(0))?)
        }
    };
}

macro_rules! text {
    ($name:ident($($arg:ident: $ty:ty),* $(,)?), $sql:literal $(,)?) => {
        pub fn $name(path: &Path, $($arg: $ty),*) -> Result<Option<String>> {
            let db = super::inspect_database(path)?;
            Ok(db.query_row($sql, rusqlite::params![$($arg),*], |row| row.get(0)).ok())
        }
    };
}

text!(
    attempt_a_execution_by_session(session: &str),
    "SELECT a.execution FROM runtime_attempts a JOIN runtime_turns t USING(turn_id) WHERE t.session_id=?1"
);

inspect!(
    attempt_count_by_session(session: &str) -> i64,
    "SELECT COUNT(*) FROM runtime_attempts a JOIN runtime_turns t USING(turn_id) WHERE t.session_id=?1"
);

inspect!(
    attempt_count_by_session_by_state(session: &str, state: &str) -> i64,
    "SELECT COUNT(*) FROM runtime_attempts a JOIN runtime_turns t USING(turn_id) WHERE t.session_id=?1 AND a.state=?2"
);

inspect!(
    attempt_count_by_session_finished(session: &str) -> i64,
    "SELECT COUNT(*) FROM runtime_attempts a JOIN runtime_turns t USING(turn_id) WHERE t.session_id=?1 AND a.state='finished'"
);

inspect!(
    attempt_count_by_session_local(session: &str) -> i64,
    "SELECT COUNT(*) FROM runtime_attempts a JOIN runtime_turns t ON t.turn_id=a.turn_id WHERE t.session_id=?1 AND a.node_id='local'"
);

inspect!(
    attempt_count_by_session_not_finished(session: &str) -> i64,
    "SELECT COUNT(*) FROM runtime_attempts a JOIN runtime_turns t USING(turn_id) WHERE t.session_id=?1 AND a.state<>'finished'"
);

inspect!(
    attempt_count_by_turn(turn: &str) -> i64,
    "SELECT COUNT(*) FROM runtime_attempts WHERE turn_id=?1 AND json_extract(completion,'$.stop_reason')='node_restarted'"
);

inspect!(
    attempt_count_by_turn_runtime_attempts(turn: &str) -> i64,
    "SELECT COUNT(*) FROM runtime_attempts WHERE turn_id=?1"
);

inspect!(
    binding_count_by_session(session: &str) -> i64,
    "SELECT COUNT(*) FROM runtime_sessions WHERE session_id=?1"
);

inspect!(
    binding_count_by_session_local(session: &str) -> i64,
    "SELECT COUNT(*) FROM runtime_sessions WHERE session_id=?1 AND node_id='local'"
);

text!(
    binding_node_id_by_session(session: &str),
    "SELECT node_id FROM runtime_sessions WHERE session_id=?1"
);

text!(
    binding_node_revision_by_session(session: &str),
    "SELECT node_revision FROM runtime_sessions WHERE session_id=?1"
);

text!(
    binding_phase_by_session(session: &str),
    "SELECT phase FROM runtime_sessions WHERE session_id=?1"
);

inspect!(
    command_count_configure_session_finished(key1: &str, key2: &str) -> i64,
    "SELECT COUNT(*) FROM node_commands WHERE method='configure_session' AND json_extract(request,'$.body.session')=?1 AND json_extract(request,'$.body.policy.permissions')=?2 AND state='finished' AND json_extract(receipt,'$.success')=1"
);

inspect!(command_count_open_session() -> i64, "SELECT COUNT(*) FROM node_commands WHERE method='open_session'");

inspect!(
    command_count_open_session_finished(key1: &str) -> i64,
    "SELECT COUNT(*) FROM node_commands WHERE method='open_session' AND json_extract(request,'$.body.session')=?1 AND json_extract(request,'$.body.policy.permissions')='auto' AND state='finished' AND json_extract(receipt,'$.success')=1"
);

inspect!(command_count_prepare_workspace_finished() -> i64, "SELECT COUNT(*) FROM node_commands WHERE method='prepare_workspace' AND state='finished' AND json_extract(receipt,'$.success')=1");

inspect!(
    command_count_release_session(key1: &str) -> i64,
    "SELECT COUNT(*) FROM node_commands WHERE method='release_session' AND json_extract(request,'$.body')=?1"
);

inspect!(
    command_count_start_turn(key1: &str) -> i64,
    "SELECT COUNT(*) FROM node_commands WHERE method='start_turn' AND instr(request, 'one-8T') > 0 AND json_extract(request,'$.body.session')=?1"
);

inspect!(
    command_count_tidy_workspace(key1: &str) -> i64,
    "SELECT COUNT(*) FROM node_commands WHERE method='tidy_workspace' AND json_extract(request,'$.subject')=?1"
);

text!(
    command_receipt_by_operation(operation: &str),
    "SELECT receipt FROM node_commands WHERE operation_id=?1"
);

inspect!(
    decision_count_by_id_fail_open(id: &str) -> i64,
    "SELECT COUNT(*) FROM decisions d JOIN sessions s ON s.item_id=d.item_id WHERE s.id=?1 AND d.kind='fail' AND d.status='open' AND instr(d.question,'dontAsk')>0"
);

inspect!(
    decision_count_by_id_fail_open_for_selected_row(id: &str) -> i64,
    "SELECT COUNT(*) FROM decisions WHERE item_id=(SELECT item_id FROM sessions WHERE id=?1) AND kind='fail' AND status='open'"
);

inspect!(
    decision_count_by_item_fail_open(item: &str) -> i64,
    "SELECT COUNT(*) FROM decisions WHERE item_id=?1 AND kind='fail' AND status='open'"
);

inspect!(
    decision_count_by_session(session: &str) -> i64,
    "SELECT COUNT(*) FROM decisions WHERE session_id=?1"
);

inspect!(
    decision_count_by_session_tool_call(session: &str) -> i64,
    "SELECT COUNT(*) FROM decisions WHERE session_id=?1 AND subject_type='tool_call'"
);

text!(
    decision_id_by_session_tool_call_open(session: &str),
    "SELECT id FROM decisions WHERE session_id=?1 AND subject_type='tool_call' AND status='open'"
);

text!(
    decision_question_by_item_fail_open(item: &str),
    "SELECT question FROM decisions WHERE item_id=?1 AND kind='fail' AND status='open'"
);

text!(
    decision_status_by_id(id: &str),
    "SELECT status FROM decisions WHERE id=?1"
);

inspect!(
    deliveries_count_by_session_with_reply(session: &str) -> i64,
    "SELECT COUNT(*) FROM deliveries WHERE session_id=?1 AND reply=1"
);

inspect!(
    deliveries_count_by_workspace_with_reply(workspace: &str) -> i64,
    "SELECT COUNT(*) FROM deliveries WHERE workspace_id=?1 AND reply=1"
);

inspect!(
    event_coalesce_max_seq_0_by_session(session: &str) -> i64,
    "SELECT COALESCE(MAX(seq),0) FROM events WHERE session_id=?1"
);

inspect!(
    event_count_by_session(session: &str) -> i64,
    "SELECT COUNT(*) FROM events WHERE session_id=?1 AND instr(data, 'Hub 重启') > 0"
);

inspect!(
    event_count_by_session_by_seq_steve_v1_selected(session: &str, seq: i64, key3: &str) -> i64,
    "SELECT COUNT(*) FROM events WHERE session_id=?1 AND seq>?2 AND format='steve.v1' AND kind NOT IN ('user','queued','text') AND instr(data, ?3) > 0"
);

inspect!(
    event_count_by_session_events(session: &str) -> i64,
    "SELECT COUNT(*) FROM events WHERE session_id=?1"
);

inspect!(
    event_count_by_session_not_finished(session: &str) -> i64,
    "SELECT COUNT(*) FROM events WHERE session_id=?1 AND format<>'steve.v1'"
);

inspect!(
    event_count_by_session_permission(session: &str) -> i64,
    "SELECT COUNT(*) FROM events WHERE session_id=?1 AND kind='permission'"
);

inspect!(
    event_count_by_session_selected(session: &str) -> i64,
    "SELECT COUNT(*) FROM events WHERE session_id=?1 AND kind NOT IN ('user','queued') AND instr(data,'survived-V6')>0"
);

inspect!(
    event_count_by_session_system(session: &str) -> i64,
    "SELECT COUNT(*) FROM events WHERE session_id=?1 AND kind='system' AND json_extract(data,'$.parked')=1"
);

inspect!(
    event_count_by_session_system_system(session: &str) -> i64,
    "SELECT COUNT(*) FROM events WHERE session_id=?1 AND kind='system' AND json_extract(data,'$.refused')=1 AND instr(data,'dontAsk')>0"
);

inspect!(
    event_count_by_session_tool(session: &str) -> i64,
    "SELECT COUNT(*) FROM events WHERE session_id=?1 AND kind='tool'"
);

inspect!(
    event_count_by_session_user_steve_v1(session: &str, key2: &str) -> i64,
    "SELECT COUNT(*) FROM events WHERE session_id=?1 AND kind='user' AND format='steve.v1' AND instr(data, ?2) > 0"
);

text!(
    session_work_event_trace(session: &str),
    "SELECT group_concat(kind||':'||data,char(10)) FROM events WHERE session_id=?1 AND kind IN ('text','turn_end','system','error')"
);

text!(
    first_decision_d_question_by_id_fail_open(id: &str, key2: i64),
    "SELECT d.question FROM decisions d JOIN sessions s ON s.item_id=d.item_id WHERE s.id=?1 AND d.kind='fail' AND d.status='open' AND d.created>=?2 AND instr(d.question, '会话出错')=1 ORDER BY d.created LIMIT 1"
);

inspect!(
    item_takeover_notice_count(item: &str) -> i64,
    "SELECT COUNT(*) FROM timeline WHERE item_id=?1 AND actor='steve' AND text LIKE 'Steve 接手：%'"
);

text!(
    items_cwd_by_id(id: &str),
    "SELECT cwd FROM items WHERE id=?1"
);

text!(
    items_status_by_id(id: &str),
    "SELECT phase FROM items WHERE id=?1"
);

text!(
    latest_command_request_open_session(),
    "SELECT request FROM node_commands WHERE method='open_session' ORDER BY created DESC LIMIT 1"
);

text!(
    latest_event_text_by_session_text(session: &str),
    "SELECT json_extract(data,'$.text') FROM events WHERE session_id=?1 AND kind='text' AND instr(data,'/api/file-deliveries/')>0 ORDER BY rowid DESC LIMIT 1"
);

text!(
    latest_event_text_by_session_text_text(session: &str),
    "SELECT json_extract(data,'$.text') FROM events WHERE session_id=?1 AND kind='text' AND instr(data,'多块图片')>0 AND instr(data,'/api/file-deliveries/')>0 ORDER BY rowid DESC LIMIT 1"
);

inspect!(
    node_audit_count_by_node_id_foreign_event(node_id: &str) -> i64,
    "SELECT COUNT(*) FROM node_audit WHERE node_id=?1 AND what='foreign_event'"
);

inspect!(
    node_count_by_node_id_with_local_with_generation(node_id: &str) -> i64,
    "SELECT COUNT(*) FROM nodes WHERE node_id=?1 AND local=1 AND generation>0"
);

inspect!(
    node_generation_by_node_id(node_id: &str) -> i64,
    "SELECT generation FROM nodes WHERE node_id=?1"
);

inspect!(
    runtime_turns_count_by_session(session: &str) -> i64,
    "SELECT COUNT(*) FROM runtime_turns WHERE session_id=?1"
);

inspect!(
    runtime_turns_count_by_session_not_finished(session: &str) -> i64,
    "SELECT COUNT(*) FROM runtime_turns WHERE session_id=?1 AND state<>'finished'"
);

text!(
    runtime_turns_turn_id_by_session_not_finished(session: &str),
    "SELECT turn_id FROM runtime_turns WHERE session_id=?1 AND state<>'finished'"
);

text!(
    session_reported_cost_text(id: &str),
    "SELECT CAST(MAX(COALESCE(u.cost_reported,0), COALESCE(s.cost,0)) AS TEXT) FROM sessions s LEFT JOIN usage_records u ON u.session_id=s.id WHERE s.id=?1"
);

text!(
    session_cwd_by_id(id: &str),
    "SELECT cwd FROM sessions WHERE id=?1"
);

text!(
    session_id_by_item(item: &str),
    "SELECT id FROM sessions WHERE item_id=?1"
);

text!(
    session_status_by_id(id: &str),
    "SELECT status FROM sessions WHERE id=?1"
);

inspect!(
    timeline_count_by_item_matching_text(item: &str) -> i64,
    "SELECT COUNT(*) FROM timeline WHERE item_id=?1 AND text LIKE '删除了工作区%'"
);

inspect!(
    timeline_count_by_item_matching_text_timeline(item: &str) -> i64,
    "SELECT COUNT(*) FROM timeline WHERE item_id=?1 AND text LIKE '%删除了已合并的分支%'"
);

pub fn tool_journal_calls_count_by_session(path: &Path, session: &str) -> Result<i64> {
    Ok(super::inspect_database(path).and_then(|db| Ok(db.query_row("SELECT COUNT(*) FROM calls WHERE session=?1 AND instr(name,'steve_knowledge_search')>0", rusqlite::params![session], |row| row.get(0))?)).unwrap_or(0))
}

pub fn tool_journal_calls_count_by_session_done(path: &Path, session: &str) -> Result<i64> {
    Ok(super::inspect_database(path).and_then(|db| Ok(db.query_row("SELECT COUNT(*) FROM calls WHERE session=?1 AND instr(name,'steve_knowledge_search')>0 AND state='done'", rusqlite::params![session], |row| row.get(0))?)).unwrap_or(0))
}

inspect!(
    completed_node_turn_count(session: &str) -> i64,
    "SELECT COUNT(*) FROM agent_turns WHERE session_id=?1 AND node_attempt_id IS NOT NULL AND ended IS NOT NULL"
);

text!(
    turn_tool_kinds_by_session(session: &str),
    "SELECT tool_kinds FROM agent_turns WHERE session_id=?1"
);

inspect!(
    usage_records_coalesce_sum_output_0_by_session(session: &str) -> i64,
    "SELECT COALESCE(SUM(output),0) FROM usage_records WHERE session_id=?1"
);

text!(
    attempt_a_operation_id_by_session(session: &str),
    "SELECT a.operation_id FROM runtime_attempts a JOIN runtime_turns t USING(turn_id) WHERE t.session_id=?1"
);

text!(
    command_kind_type_prepare_workspace(key1: &str),
    "SELECT json_extract(request,'$.body.kind.type') FROM node_commands WHERE method='prepare_workspace' AND json_extract(request,'$.body.resource')=?1"
);

inspect!(event_count() -> i64, "SELECT COUNT(*) FROM events WHERE format != 'steve.v1'");

text!(
    first_command_request_open_session(),
    "SELECT request FROM node_commands WHERE method='open_session' ORDER BY created LIMIT 1"
);

text!(
    output_text_after(session: &str, seq: i64),
    "SELECT group_concat(t, '') FROM (SELECT COALESCE(json_extract(data,'$.text'),'') AS t FROM events WHERE session_id=?1 AND seq>?2 AND format='steve.v1' AND kind='text' ORDER BY seq)"
);

inspect!(item_stats_count_with_dirty_for_selected_row() -> i64, "SELECT COUNT(*) FROM item_stats WHERE dirty=1 OR updated >= (SELECT max(ended) FROM agent_turns)");

inspect!(realtime_output_samples_count_end() -> i64, "SELECT COUNT(*) FROM realtime_output_samples WHERE kind='end'");

inspect!(
    session_count_for_item(item: &str) -> i64,
    "SELECT COUNT(*) FROM sessions WHERE item_id=?1"
);
inspect!(session_count() -> i64, "SELECT COUNT(*) FROM sessions");

inspect!(session_count_with_readonly() -> i64, "SELECT COUNT(*) FROM sessions WHERE readonly=1");

text!(
    session_item_id_by_id(id: &str),
    "SELECT item_id FROM sessions WHERE id=?1"
);

text!(
    setting_value_default_by_key(key: &str),
    "SELECT value FROM settings WHERE workspace_id='default' AND key=?1"
);

inspect!(turn_count() -> i64, "SELECT COUNT(*) FROM agent_turns");

inspect!(turn_count_with_ended_with_value() -> i64, "SELECT COUNT(*) FROM agent_turns WHERE ended IS NOT NULL");

inspect!(usage_records_count() -> i64, "SELECT COUNT(*) FROM usage_records");

pub fn agent_text_count(path: &Path, session: &str, needle: &str) -> Result<i64> {
    let db = super::inspect_database(path)?;
    Ok(db.query_row("SELECT COUNT(*) FROM events WHERE session_id=? AND kind='text' AND format='steve.v1' AND instr(data, ?) > 0", [session, needle], |r| r.get(0))?)
}

pub fn attempt_count(path: &Path, session: &str, state: &str) -> Result<i64> {
    let db = super::inspect_database(path)?;
    Ok(db.query_row("SELECT COUNT(*) FROM runtime_attempts a JOIN runtime_turns t USING(turn_id) WHERE t.session_id=? AND a.state=?", [session, state], |r| r.get(0))?)
}

pub fn session_dump(path: &Path, sid: &str) -> Result<String> {
    let db = super::inspect_database(path)?;
    let mut out = String::new();
    for sql in [
        "SELECT seq||' '||kind||' '||substr(data,1,160) FROM events WHERE session_id=?1 ORDER BY seq",
        "SELECT 'attempt '||a.attempt_id||' '||a.state FROM runtime_attempts a JOIN runtime_turns t USING(turn_id) WHERE t.session_id=?1",
        "SELECT 'turn '||turn_id||' '||state FROM runtime_turns WHERE session_id=?1",
        "SELECT 'cmd '||operation_id||' '||method||' '||state FROM node_commands WHERE json_extract(request,'$.body.session')=?1 OR json_extract(request,'$.body')=?1 OR target_attempt IN (SELECT a.attempt_id FROM runtime_attempts a JOIN runtime_turns t USING(turn_id) WHERE t.session_id=?1)",
    ] {
        let mut st = db.prepare(sql)?;
        for r in st.query_map([sid], |r| r.get::<_, String>(0))? {
            out.push_str(&r?);
            out.push('\n');
        }
    }
    Ok(out)
}

pub fn seed_remote_attempt(path: &Path, session: &str, node: &str) -> Result<()> {
    let db = super::api::writable(path)?;
    db.execute(
        "CREATE TEMP TABLE sb AS SELECT * FROM sessions WHERE id=?",
        [session],
    )?;
    db.execute_batch(
        "UPDATE sb SET id='s_b_own', item_id=NULL; INSERT INTO sessions SELECT * FROM sb;",
    )?;
    db.execute("INSERT INTO runtime_sessions(session_id,node_id,workspace_ref,node_session_id,config_revision,phase,updated) VALUES ('s_b_own',?,'{}','s_b_own','r','ready',0)", [node])?;
    db.execute_batch("INSERT INTO runtime_turns(turn_id,session_id,state,created,updated) VALUES ('t_b_own','s_b_own','running',0,0);")?;
    db.execute("INSERT INTO node_commands(operation_id,node_id,method,request,digest,state,created,updated) VALUES ('op_b_own',?,'start_turn','{}','d','finished',0,0)", [node])?;
    db.execute("INSERT INTO runtime_attempts(attempt_id,turn_id,ordinal,node_id,operation_id,execution,spec,digest,state,created,updated) VALUES ('a_b_own','t_b_own',0,?,'op_b_own','{}','{}','d','running',0,0)", [node])?;
    Ok(())
}

pub fn seed_nonowner_member(path: &Path) -> Result<()> {
    super::api::writable(path)?.execute_batch(
        "INSERT INTO users(id,workspace_id,subject,name,role,created)
       VALUES ('fixture_member','default','fixture:bot-team-member','Fixture member','member',1);
       INSERT INTO workspace_members(workspace_id,user_id,role,created)
       VALUES ('default','fixture_member','member',1);",
    )?;
    Ok(())
}

pub fn agent_output_count(path: &Path, session: &str, needle: &str) -> Result<i64> {
    let db = super::inspect_database(path)?;
    Ok(db.query_row("SELECT COUNT(*) FROM events WHERE session_id=? AND format='steve.v1'
             AND kind NOT IN ('user','queued') AND COALESCE(json_extract(data,'$.role'),'') <> 'user'
             AND instr(data, ?) > 0", [session, needle], |r| r.get(0))?)
}

pub fn make_unsent_turn_retry_due(path: &Path, session: &str) -> Result<usize> {
    Ok(super::api::writable(path)?.execute("UPDATE node_commands SET retry_at=0 WHERE method='start_turn' AND state='pending' AND maybe_sent=0 AND target_attempt=(SELECT active_attempt FROM runtime_sessions WHERE session_id=?)", [session])?)
}

pub fn decision_state(path: &Path, id: &str) -> Result<(String, String)> {
    let db = super::inspect_database(path)?;
    Ok(db.query_row(
        "SELECT status, COALESCE(result,'') FROM decisions WHERE id=?",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?)
}

pub fn recorded_session_view(path: &Path, id: &str) -> Result<(String, Option<i64>)> {
    let db = super::inspect_database(path)?;
    Ok(db.query_row(
        "SELECT json_extract(view,'$.rw'), extra_count FROM view_shadow WHERE session_id=?",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?)
}

pub fn file_location(path: &Path, id: &str) -> Result<(String, String)> {
    let db = super::inspect_database(path)?;
    Ok(db.query_row(
        "SELECT workspace_id, sha256 FROM files WHERE id=?",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?)
}

pub fn disable_claude_permission_prompts(path: &Path) -> Result<()> {
    super::api::writable(path)?
        .execute_batch("UPDATE bots SET perm_mode='dontAsk' WHERE harness='claude-code'")?;
    Ok(())
}

pub fn attempt_states(path: &Path, session: &str) -> Result<Vec<String>> {
    let db = super::inspect_database(path)?;
    let mut q = db.prepare("SELECT a.state FROM runtime_attempts a JOIN runtime_turns t USING(turn_id) WHERE t.session_id=?")?;
    Ok(q.query_map([session], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

pub fn session_text(path: &Path, session: &str) -> Result<Option<String>> {
    let db = super::inspect_database(path)?;
    Ok(db
        .query_row(
            "SELECT group_concat(json_extract(data,'$.text'),'') FROM
               (SELECT data FROM events WHERE session_id=? AND kind='text' ORDER BY seq)",
            [session],
            |r| r.get(0),
        )
        .ok())
}

pub fn command_trace(path: &Path) -> Result<Vec<String>> {
    let db = super::inspect_database(path)?;
    let mut q = db.prepare(
        "SELECT method, state, COALESCE(receipt,'') FROM node_commands ORDER BY created, rowid",
    )?;
    Ok(q.query_map([], |r| {
        Ok(format!(
            "{} {} {}",
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?
        ))
    })?
    .collect::<rusqlite::Result<_>>()?)
}

pub fn history_events(path: &Path, session: &str) -> Result<Vec<(String, String)>> {
    let db = super::inspect_database(path)?;
    let mut q = db.prepare("SELECT kind, data FROM events WHERE session_id=? ORDER BY seq")?;
    Ok(q.query_map([session], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?)
}

pub fn imported_session(
    path: &Path,
    session: &str,
) -> Result<(String, String, String, String, String, i64, i64)> {
    let db = super::inspect_database(path)?;
    Ok(db.query_row("SELECT harness, cwd, title, source, import_key, created, readonly FROM sessions WHERE id=?", [session], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)))?)
}

pub fn finished_turns(path: &Path, session: &str) -> Result<i64> {
    let db = super::inspect_database(path)?;
    Ok(db.query_row(
        "SELECT COUNT(*) FROM runtime_turns WHERE session_id=? AND state='finished'",
        [session],
        |r| r.get(0),
    )?)
}

pub fn unfinished_turns(path: &Path, session: &str) -> Result<i64> {
    let db = super::inspect_database(path)?;
    Ok(db.query_row(
        "SELECT COUNT(*) FROM runtime_turns WHERE session_id=? AND state<>'finished'",
        [session],
        |r| r.get(0),
    )?)
}

pub fn command_count_by_method(path: &Path, method: &str) -> Result<i64> {
    let db = super::inspect_database(path)?;
    Ok(db.query_row(
        "SELECT COUNT(*) FROM node_commands WHERE method=?",
        [method],
        |r| r.get(0),
    )?)
}

pub fn unknown_attempts(path: &Path, session: &str) -> Result<i64> {
    let db = super::inspect_database(path)?;
    Ok(db.query_row("SELECT COUNT(*) FROM runtime_attempts a JOIN runtime_turns t USING(turn_id) WHERE t.session_id=? AND a.state='unknown'", [session], |r| r.get(0))?)
}

pub fn seed_held_message(path: &Path, session: &str, text: &str) -> Result<()> {
    super::api::writable(path)?.execute(
        "INSERT INTO queued(id,session_id,text,via,created) VALUES ('q-held',?1,?2,'me',1)",
        [session, text],
    )?;
    Ok(())
}

pub fn break_thread_recovery(path: &Path) -> Result<()> {
    super::api::writable(path)?
        .execute_batch("ALTER TABLE thread_reply_tasks RENAME TO thread_reply_tasks_gone")?;
    Ok(())
}

pub fn dispatch_jobs(path: &Path) -> Result<Vec<(String, String)>> {
    let db = super::inspect_database(path)?;
    let mut q = db.prepare(
        "SELECT job_id, state FROM runtime_jobs WHERE kind='think' AND purpose='dispatch' ORDER BY created, rowid",
    )?;
    Ok(q.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?)
}

pub fn dispatch_job_trace(path: &Path) -> Result<Vec<String>> {
    let db = super::inspect_database(path)?;
    let mut q = db.prepare(
        "SELECT j.job_id, j.state, a.attempt_id, a.model, a.state, COALESCE(a.completion, '')
         FROM runtime_jobs j
         LEFT JOIN runtime_job_attempts a ON a.job_id = j.job_id
         WHERE j.kind='think' AND j.purpose='dispatch'
         ORDER BY j.created, a.ordinal",
    )?;
    Ok(q.query_map([], |r| {
        Ok(format!(
            "job={} ({}) attempt={} model={} state={} completion={}",
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<String>>(2)?.unwrap_or_default(),
            r.get::<_, Option<String>>(3)?.unwrap_or_default(),
            r.get::<_, Option<String>>(4)?.unwrap_or_default(),
            r.get::<_, String>(5)?,
        ))
    })?
    .collect::<rusqlite::Result<_>>()?)
}

inspect!(
    pending_item_dispatch_jobs() -> i64,
    "SELECT COUNT(*) FROM item_dispatch_jobs"
);

text!(
    item_bot_id(id: &str),
    "SELECT bot_id FROM items WHERE id=?1"
);

pub fn item_initial_placement(
    path: &Path,
    item: &str,
) -> Result<Option<(Option<String>, Option<String>)>> {
    let db = super::inspect_database(path)?;
    Ok(db
        .query_row(
            "SELECT fact, blocked FROM bot_placements WHERE item_id=?1 AND session_id IS NULL ORDER BY seq DESC LIMIT 1",
            [item],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok())
}

pub fn item_placement_wait_blocked(path: &Path, item: &str) -> Result<Option<String>> {
    let db = super::inspect_database(path)?;
    Ok(db
        .query_row(
            "SELECT blocked FROM item_placement_waits WHERE item_id=?1",
            [item],
            |r| r.get(0),
        )
        .ok())
}

pub fn item_timeline_lines(path: &Path, item: &str) -> Result<Vec<String>> {
    let db = super::inspect_database(path)?;
    let mut q = db.prepare(
        "SELECT actor || ': ' || text FROM timeline WHERE item_id=?1 ORDER BY ts, rowid",
    )?;
    Ok(q.query_map([item], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

pub fn tool_journal_calls_for_holder(
    path: &Path,
    holder: &str,
) -> Result<Vec<(String, String, String)>> {
    if !path.is_file() {
        return Ok(vec![]);
    }
    let db = super::inspect_database(path)?;
    let mut q = db.prepare(
        "SELECT name, state, COALESCE(json_extract(result, '$.status'), '')
         FROM calls
         WHERE session=?1
         ORDER BY created, rowid",
    )?;
    Ok(
        q.query_map([holder], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?,
    )
}

pub fn tool_journal_all_calls(path: &Path) -> Result<Vec<(String, String, String, String)>> {
    if !path.is_file() {
        return Ok(vec![]);
    }
    let db = super::inspect_database(path)?;
    let mut q = db.prepare(
        "SELECT session, name, state, COALESCE(json_extract(result, '$.status'), '')
         FROM calls
         ORDER BY created, rowid",
    )?;
    Ok(
        q.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<rusqlite::Result<_>>()?,
    )
}
