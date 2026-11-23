//! Storage fixtures and assertions for YOLO integration tests.

use crate::Result;
use serde_json::{Value, json};
use std::path::Path;

pub fn audit_actions(path: &Path) -> Result<Vec<String>> {
    let db = super::inspect_database(path)?;
    let mut q = db.prepare("SELECT action FROM audit WHERE action LIKE 'yolo.%' ORDER BY id")?;
    Ok(q.query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

pub fn window_end_reasons(path: &Path) -> Result<Vec<String>> {
    let db = super::inspect_database(path)?;
    let mut q = db.prepare("SELECT ended_why FROM yolo_windows")?;
    Ok(q.query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

pub fn scope_revision_types(path: &Path) -> Result<Vec<String>> {
    let db = super::inspect_database(path)?;
    let mut q =
        db.prepare("SELECT DISTINCT object_type FROM revisions WHERE object_type = 'yolo_scope'")?;
    Ok(q.query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

pub fn enable_audits(path: &Path) -> Result<Vec<String>> {
    let db = super::inspect_database(path)?;
    let mut q = db.prepare("SELECT action FROM audit WHERE action = 'yolo.on'")?;
    Ok(q.query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

pub fn move_scope_project(path: &Path, project: &str) -> Result<()> {
    let c = super::api::writable(path)?;
    c.execute_batch("INSERT INTO workspaces (id, name, created) VALUES ('ws_b', 'b', 1)")?;
    c.execute(
        "UPDATE projects SET workspace_id='ws_b' WHERE id=?",
        [project],
    )?;
    Ok(())
}

pub fn seed_declined_action(path: &Path) -> Result<()> {
    super::api::writable(path)?.execute_batch("INSERT INTO yolo_actions (id, workspace_id, kind, subject_type, rule, outcome, reason, created)
         VALUES ('ya_x', 'default', 'approve', 'tool_call', 'limit.5', 'declined', '硬限制：x', 1)")?;
    Ok(())
}

fn cells(db: &Path, sql: &str, p: &[&dyn rusqlite::ToSql]) -> Result<Vec<Vec<Value>>> {
    let c = super::inspect_database(db)?;
    let mut st = c.prepare(sql)?;
    let n = st.column_count();
    Ok(st
        .query_map(p, |r| {
            Ok((0..n)
                .map(|i| match r.get_ref(i).expect("col") {
                    rusqlite::types::ValueRef::Null => Value::Null,
                    rusqlite::types::ValueRef::Integer(x) => json!(x),
                    rusqlite::types::ValueRef::Real(x) => json!(x),
                    rusqlite::types::ValueRef::Text(t) => json!(String::from_utf8_lossy(t)),
                    rusqlite::types::ValueRef::Blob(_) => Value::Null,
                })
                .collect())
        })?
        .collect::<rusqlite::Result<_>>()?)
}

pub fn decision_actions(path: &Path, session: &str) -> Result<Vec<Vec<Value>>> {
    cells(path, "SELECT d.status, d.result, a.outcome, a.rule, a.choice FROM decisions d JOIN yolo_actions a ON a.decision_id = d.id
          WHERE d.subject_type = 'tool_call' AND d.session_id = ?", &[&session])
}

pub fn decision_resolvers(path: &Path, session: &str) -> Result<Vec<Vec<Value>>> {
    cells(path, "SELECT a.actor FROM audit a JOIN decisions d ON d.id = a.target
          WHERE a.action = 'decision.resolve' AND d.subject_type = 'tool_call' AND d.session_id = ?", &[&session])
}

pub fn tool_outcomes(path: &Path, session: &str) -> Result<Vec<Vec<Value>>> {
    cells(
        path,
        "SELECT a.outcome FROM decisions d JOIN yolo_actions a ON a.decision_id = d.id
                  WHERE d.subject_type = 'tool_call' AND d.session_id = ?",
        &[&session],
    )
}

pub fn decline_reasons(path: &Path, session: &str) -> Result<Vec<Vec<Value>>> {
    cells(path, "SELECT d.status, a.outcome, a.reason FROM decisions d JOIN yolo_actions a ON a.decision_id = d.id
          WHERE d.subject_type = 'tool_call' AND d.session_id = ?", &[&session])
}

pub fn permission_cards(path: &Path, session: &str) -> Result<Vec<Vec<Value>>> {
    cells(
        path,
        "SELECT id FROM decisions WHERE subject_type = 'tool_call' AND session_id = ?",
        &[&session],
    )
}

pub fn mode_audits(path: &Path) -> Result<Vec<Vec<Value>>> {
    cells(
        path,
        "SELECT action, json_extract(detail, '$.mode') FROM audit WHERE action LIKE 'yolo.%' ORDER BY rowid",
        &[],
    )
}

pub fn decision_outcomes(path: &Path, session: &str) -> Result<Vec<Vec<Value>>> {
    cells(
        path,
        "SELECT d.status, a.outcome FROM decisions d JOIN yolo_actions a ON a.decision_id = d.id
          WHERE d.subject_type = 'tool_call' AND d.session_id = ?",
        &[&session],
    )
}

pub fn decision_rules(path: &Path, session: &str) -> Result<Vec<Vec<Value>>> {
    cells(path, "SELECT d.status, a.outcome, a.rule FROM decisions d JOIN yolo_actions a ON a.decision_id = d.id
                  WHERE d.subject_type = 'tool_call' AND d.session_id = ? ORDER BY a.rowid", &[&session])
}

pub fn open_permission_cards(path: &Path, session: &str) -> Result<Vec<Vec<Value>>> {
    cells(
        path,
        "SELECT id FROM decisions WHERE subject_type = 'tool_call' AND status = 'open' AND session_id = ?",
        &[&session],
    )
}

pub fn remove_session_bot(path: &Path, session: &str) -> Result<()> {
    super::api::writable(path)?
        .execute("UPDATE sessions SET bot_id = NULL WHERE id = ?", [session])?;
    Ok(())
}
