//! Whole-state reads share the Store/engine read pool and one snapshot per call.
use std::collections::HashMap;

use rusqlite::Connection;
use serde::Serialize;

#[derive(Debug, Clone, PartialEq)]
pub struct CloseoutRead {
    pub item_closed: bool,
    pub evidence_json: String,
    pub awaiting_merge: bool,
}

#[derive(Debug, Default, PartialEq)]
pub struct ItemExtrasRead {
    pub goal: String,
    pub goals: usize,
    pub tags: Vec<String>,
    pub deps: Vec<String>,
    pub sessions: Vec<(String, String)>,
    pub open: Vec<String>,
    pub queued: usize,
    pub closeout: Option<CloseoutRead>,
    /// No other state query produced this item; Hub may omit it if evidence is not a PR.
    pub closeout_only: bool,
    pub workflow_paused: Option<bool>,
    pub workflow_cycle: Option<i64>,
    pub workflow_failed: Vec<String>,
    pub workflow_attempt: Option<i64>,
    pub workflow_superseded: bool,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct QueuedStateRead {
    pub id: String,
    pub session_id: String,
    pub text: String,
    pub via: String,
    pub created: i64,
    // Preserve arbitrary array entries and fields, as the legacy API does.
    pub files: serde_json::Value,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct McpServerRead {
    pub id: String,
    pub name: String,
    pub transport: String,
    pub enabled: bool,
}

#[derive(Debug, Default, PartialEq)]
pub struct BotExtrasRead {
    pub workspaces: HashMap<String, Vec<String>>,
    pub memories: HashMap<String, i64>,
    pub mcp: HashMap<String, Vec<McpServerRead>>,
}

/// Runs `sql` with one text parameter and calls `row` per result row.
fn each(
    c: &rusqlite::Connection,
    sql: &str,
    p: &str,
    mut row: impl FnMut(&rusqlite::Row) -> rusqlite::Result<()>,
) -> rusqlite::Result<()> {
    let mut st = c.prepare_cached(sql)?;
    let mut rows = st.query([p])?;
    while let Some(r) = rows.next()? {
        row(r)?;
    }
    Ok(())
}

fn pr_closeouts(
    c: &rusqlite::Connection,
    scope: &str,
    param: &str,
    mut found: impl FnMut(String, CloseoutRead),
) -> rusqlite::Result<()> {
    each(c, &format!(
        "SELECT i.id, i.phase = 'closed', o.evidence,
                EXISTS (SELECT 1 FROM decisions d WHERE d.item_id = i.id
                        AND d.subject_type = 'pr_awaiting_merge' AND d.subject_id = o.id AND d.status = 'open')
         FROM items i JOIN item_operations o ON o.id = (
            SELECT p.id FROM item_operations p WHERE p.item_id = i.id AND p.cycle = i.cycle
                AND p.revision = i.revision AND p.kind = 'release'
                AND json_extract(p.evidence, '$.pr_slug') IS NOT NULL
                AND p.status IN ('pending', 'succeeded')
            ORDER BY p.created DESC, p.id DESC LIMIT 1
         )
         WHERE {scope} AND i.kind = 'task'
           AND (i.phase = 'closing' OR (i.phase = 'closed' AND i.status = 'done'))"
    ), param, |r| {
        found(r.get(0)?, CloseoutRead {
            item_closed: r.get(1)?,
            evidence_json: r.get(2)?,
            awaiting_merge: r.get(3)?,
        });
        Ok(())
    })
}

const ITEMS: &str = "SELECT id FROM items WHERE workspace_id = ?1 AND kind = 'task'";

fn item_relations(
    c: &Connection,
    ws: &str,
    m: &mut HashMap<String, ItemExtrasRead>,
) -> rusqlite::Result<()> {
    each(
        c,
        &format!(
            "SELECT it.item_id, it.tag_id FROM item_tags it LEFT JOIN tags t ON t.id = it.tag_id WHERE it.item_id IN ({ITEMS}) ORDER BY it.item_id, coalesce(t.cat = '类型', 0) DESC, it.rowid"
        ),
        ws,
        |r| {
            m.entry(r.get(0)?).or_default().tags.push(r.get(1)?);
            Ok(())
        },
    )?;
    each(
        c,
        &format!(
            "SELECT item_id, dep_id FROM item_deps WHERE item_id IN ({ITEMS}) ORDER BY item_id, created, dep_id"
        ),
        ws,
        |r| {
            m.entry(r.get(0)?).or_default().deps.push(r.get(1)?);
            Ok(())
        },
    )?;
    each(
        c,
        &format!(
            "SELECT item_id, id, status FROM sessions WHERE item_id IN ({ITEMS}) ORDER BY item_id, no, rowid"
        ),
        ws,
        |r| {
            m.entry(r.get(0)?)
                .or_default()
                .sessions
                .push((r.get(1)?, r.get(2)?));
            Ok(())
        },
    )?;
    each(
        c,
        &format!(
            "SELECT item_id, id FROM decisions WHERE status = 'open' AND item_id IN ({ITEMS}) ORDER BY item_id, created, rowid"
        ),
        ws,
        |r| {
            m.entry(r.get(0)?).or_default().open.push(r.get(1)?);
            Ok(())
        },
    )?;
    Ok(())
}

fn workflow_extras(
    c: &Connection,
    ws: &str,
    m: &mut HashMap<String, ItemExtrasRead>,
) -> rusqlite::Result<()> {
    each(
        c,
        &format!(
            "SELECT w.item_id, w.paused, w.cycle FROM workflows w WHERE w.item_id IN ({ITEMS})"
        ),
        ws,
        |r| {
            let x = m.entry(r.get(0)?).or_default();
            x.workflow_paused = Some(r.get(1)?);
            x.workflow_cycle = Some(r.get(2)?);
            Ok(())
        },
    )?;
    each(
        c,
        &format!(
            "SELECT u.workflow_id, u.item_id FROM workflow_units u JOIN workflows w ON w.item_id = u.workflow_id JOIN items i ON i.id = w.item_id AND i.cycle = w.cycle AND i.phase != 'closed' WHERE u.status = 'failed' AND u.item_id IS NOT NULL AND u.workflow_id IN ({ITEMS}) ORDER BY u.workflow_id, u.position"
        ),
        ws,
        |r| {
            m.entry(r.get(0)?)
                .or_default()
                .workflow_failed
                .push(r.get(1)?);
            Ok(())
        },
    )?;
    each(
        c,
        &format!(
            "SELECT u.item_id, u.attempt FROM workflow_units u WHERE u.item_id IS NOT NULL AND u.item_id IN ({ITEMS})"
        ),
        ws,
        |r| {
            m.entry(r.get(0)?).or_default().workflow_attempt = Some(r.get(1)?);
            Ok(())
        },
    )?;
    each(
        c,
        &format!(
            "SELECT a.item_id, max(a.attempt) FROM workflow_attempts a WHERE a.item_id IN ({ITEMS}) AND NOT EXISTS (SELECT 1 FROM workflow_units u WHERE u.item_id = a.item_id) GROUP BY a.item_id"
        ),
        ws,
        |r| {
            let x = m.entry(r.get(0)?).or_default();
            x.workflow_superseded = true;
            if x.workflow_attempt.is_none() {
                x.workflow_attempt = r.get(1)?;
            }
            Ok(())
        },
    )?;
    Ok(())
}

pub(super) fn item_extras(
    c: &Connection,
    ws: &str,
) -> rusqlite::Result<HashMap<String, ItemExtrasRead>> {
    let mut m: HashMap<String, ItemExtrasRead> = HashMap::new();
    // same orders as the per-item queries of repo.ts (index order for the PK lookups)
    each(
        c,
        &format!(
            "SELECT item_id, text FROM item_goals WHERE item_id IN ({ITEMS}) ORDER BY item_id, version"
        ),
        ws,
        |r| {
            let x = m.entry(r.get(0)?).or_default();
            x.goal = r.get(1)?;
            x.goals += 1;
            Ok(())
        },
    )?;
    item_relations(c, ws, &mut m)?;
    let mut counts: HashMap<String, usize> = HashMap::new();
    each(
        c,
        &format!(
            "SELECT q.session_id, count(*) FROM queued q JOIN sessions s ON s.id = q.session_id WHERE s.item_id IN ({ITEMS}) GROUP BY q.session_id"
        ),
        ws,
        |r| {
            counts.insert(r.get(0)?, r.get::<_, i64>(1)? as usize);
            Ok(())
        },
    )?;
    workflow_extras(c, ws, &mut m)?;
    pr_closeouts(c, "i.workspace_id = ?1", ws, |id, closeout| {
        m.entry(id)
            .or_insert_with(|| ItemExtrasRead {
                closeout_only: true,
                ..Default::default()
            })
            .closeout = Some(closeout);
    })?;
    for x in m.values_mut() {
        if let Some((sid, _)) = x.sessions.iter().rev().find(|(_, st)| st != "ended") {
            x.queued = counts.get(sid).copied().unwrap_or(0);
        }
    }
    Ok(m)
}
pub(super) fn item_closeout(c: &Connection, id: &str) -> rusqlite::Result<Option<CloseoutRead>> {
    let mut closeout = None;
    pr_closeouts(c, "i.id = ?1", id, |_, p| closeout = Some(p))?;
    Ok(closeout)
}

pub(super) fn queued_by_session(
    c: &Connection,
    ws: &str,
) -> rusqlite::Result<HashMap<String, Vec<QueuedStateRead>>> {
    let mut m: HashMap<String, Vec<QueuedStateRead>> = HashMap::new();
    each(
        c,
        "SELECT id, session_id, text, via, created, files FROM queued WHERE session_id IN (SELECT id FROM sessions WHERE workspace_id = ?1 ORDER BY updated DESC LIMIT 300) ORDER BY created, rowid",
        ws,
        |r| {
            let session_id: String = r.get(1)?;
            let files = r
                .get::<_, Option<String>>(5)?
                .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
                .filter(|v| v.is_array())
                .unwrap_or_else(|| serde_json::Value::Array(vec![]));
            m.entry(session_id.clone())
                .or_default()
                .push(QueuedStateRead {
                    id: r.get(0)?,
                    session_id,
                    text: r.get(2)?,
                    via: r.get(3)?,
                    created: r.get(4)?,
                    files,
                });
            Ok(())
        },
    )?;
    Ok(m)
}

pub(super) fn bot_extras(c: &Connection, ws: &str) -> rusqlite::Result<BotExtrasRead> {
    let mut x = BotExtrasRead::default();
    let mut st = c.prepare_cached(
        "SELECT bot_id, workspace_id FROM bot_workspaces ORDER BY bot_id, workspace_id",
    )?;
    let mut rows = st.query([])?;
    while let Some(r) = rows.next()? {
        x.workspaces.entry(r.get(0)?).or_default().push(r.get(1)?);
    }
    let mut st = c.prepare_cached(
                "SELECT a.bot_id, s.id, s.name, s.transport, a.enabled FROM bot_capabilities a
                   JOIN mcp_servers s ON s.id = a.entry_id WHERE a.kind = 'mcp' ORDER BY a.bot_id, lower(s.name)",
            )?;
    let mut rows = st.query([])?;
    while let Some(r) = rows.next()? {
        x.mcp.entry(r.get(0)?).or_default().push(mcp_summary(r)?);
    }
    each(
        c,
        "SELECT bot_id, count(*) FROM bot_memories WHERE workspace_id = ?1 GROUP BY bot_id",
        ws,
        |r| {
            x.memories.insert(r.get(0)?, r.get(1)?);
            Ok(())
        },
    )?;
    Ok(x)
}

pub(super) fn bot_mcp_servers(c: &Connection, id: &str) -> rusqlite::Result<Vec<McpServerRead>> {
    let mut st = c.prepare_cached(
        "SELECT a.bot_id, s.id, s.name, s.transport, a.enabled FROM bot_capabilities a
           JOIN mcp_servers s ON s.id = a.entry_id WHERE a.kind = 'mcp' AND a.bot_id = ? ORDER BY lower(s.name)",
    )?;
    st.query_map([id], mcp_summary)?.collect()
}

fn mcp_summary(r: &rusqlite::Row) -> rusqlite::Result<McpServerRead> {
    Ok(McpServerRead {
        id: r.get(1)?,
        name: r.get(2)?,
        transport: r.get(3)?,
        enabled: r.get::<_, i64>(4)? != 0,
    })
}
