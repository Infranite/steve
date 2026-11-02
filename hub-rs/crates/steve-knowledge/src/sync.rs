//! Keeping memories in the index (knowledge.md §2.1): confirmed `mems` (one entry each, prefixed
//! with who or what they are about) and the old `bot_memories` (read only, “<Bot> 的记忆”).
//!
//! Event-driven ([`on_event`] on the bus's `mm` and `bot` events, wired by the integrator) with a
//! periodic [`sweep`] (`kb.sync_mems`, every 5 minutes) that catches anything an event missed.

use std::collections::HashMap;
use std::time::Duration;

use rusqlite::{Connection, params};
use steve_modules::Modules;
use steve_store::base::mems::{self, Mem};
use steve_store::base::{Level, ObjRef};
use steve_store::{ChangeEvent, Result, StoreError, Tx, sha};

use crate::chunk::Chunk;
use crate::index::{self, ChunkMeta};
use crate::repo;
use crate::sources;

/// How often the sweep runs (§10).
pub const SWEEP_EVERY: Duration = Duration::from_secs(5 * 60);

/// Names of the objects a memory is about, “、”-joined; unknown objects are left out.
pub fn about_names(c: &Connection, about: &[ObjRef]) -> Result<String> {
    let mut names = Vec::new();
    for a in about {
        if let Some(n) = repo::ref_name(c, a)?.filter(|n| !n.is_empty())
            && !names.contains(&n)
        {
            names.push(n);
        }
    }
    Ok(names.join("、"))
}

/// Does this memory belong in the index? Confirmed, not `never`, not expired.
fn usable(m: &Mem, now: i64) -> bool {
    m.status == "confirmed" && m.level != Level::Never && m.valid_until.is_none_or(|v| v > now)
}

/// Re-indexes one memory (or removes it: deleted, candidate, stale, `never`, expired).
pub fn sync_mem(tx: &Tx, id: &str, now: i64) -> Result<bool> {
    let Some(m) = mems::get(tx, id)? else {
        index::remove(tx, "mem", id)?;
        return Ok(false);
    };
    if !usable(&m, now) {
        index::remove(tx, "mem", id)?;
        return Ok(false);
    }
    let names = about_names(tx, &m.about)?;
    let text = if names.is_empty() {
        m.text.clone()
    } else {
        format!("{names}：{}", m.text)
    };
    let label = if names.is_empty() {
        "记忆".to_string()
    } else {
        format!("记忆 · {names}")
    };
    let len = text.chars().count();
    let c = Chunk {
        seq: 0,
        heading: String::new(),
        text,
        start: 0,
        end: len,
    };
    index::replace(
        tx,
        &ChunkMeta {
            ws: &m.workspace_id,
            obj_type: "mem",
            obj_id: &m.id,
            title: "",
            visibility: &m.visibility,
            level: m.level,
            created_by: &m.created_by,
            source_kind: "mems",
            label: &label,
            updated: m.updated,
        },
        &[c],
    )?;
    Ok(true)
}

/// Memories of `ws` changed since the last sweep, plus index entries whose memory is gone or expired.
pub fn sweep_mems(tx: &Tx, ws: &str, now: i64) -> Result<usize> {
    let since: i64 = repo::mark(tx, ws, "sync:mems")?
        .and_then(|v| v.parse().ok())
        .unwrap_or(-1);
    let changed: Vec<(String, i64)> = {
        let mut st = tx.prepare_cached(
            "SELECT id, updated FROM mems WHERE workspace_id = ? AND updated > ? ORDER BY updated",
        )?;
        st.query_map(params![ws, since], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let stale: Vec<String> = {
        let mut st = tx.prepare_cached(
            "SELECT c.obj_id FROM kb_chunks c LEFT JOIN mems m ON m.id = c.obj_id
              WHERE c.workspace_id = ?1 AND c.obj_type = 'mem'
                AND (m.id IS NULL OR m.status <> 'confirmed' OR m.level = 'never' OR (m.valid_until IS NOT NULL AND m.valid_until <= ?2))",
        )?;
        st.query_map(params![ws, now], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let mut n = 0;
    let mut mark = since;
    for (id, updated) in &changed {
        sync_mem(tx, id, now)?;
        mark = mark.max(*updated);
        n += 1;
    }
    for id in &stale {
        index::remove(tx, "mem", id)?;
        n += 1;
    }
    if mark != since {
        repo::set_mark(tx, ws, "sync:mems", &mark.to_string(), now)?;
    }
    Ok(n)
}

/// Makes the `bot_mem` entries of `ws` match `bot_memories` (the old table has no `updated` and is
/// small, so it is compared whole; a hash in `kb_marks` skips the work when nothing changed).
pub fn sync_bot_mems(tx: &Tx, ws: &str, actor: &str, now: i64) -> Result<usize> {
    let src = sources::builtin(tx, ws, "bot_mems", actor, now)?;
    let rows: Vec<(String, String, String, String)> = {
        let mut st = tx.prepare_cached(
            "SELECT m.id, m.bot_id, m.text, COALESCE(b.name, '') FROM bot_memories m LEFT JOIN bots b ON b.id = m.bot_id
              WHERE m.workspace_id = ? ORDER BY m.id",
        )?;
        st.query_map([ws], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let mut all = format!("{}|{}\n", src.visibility.code(), src.level.code());
    for (id, bot, text, name) in &rows {
        all.push_str(&format!("{id}\t{bot}\t{name}\t{text}\n"));
    }
    let hash = sha(&all);
    if repo::mark(tx, ws, "sync:bot_mems")?.as_deref() == Some(hash.as_str()) {
        return Ok(0);
    }
    let have: HashMap<String, (String, String)> = {
        let mut st = tx.prepare_cached(
            "SELECT obj_id, text, label FROM kb_chunks WHERE workspace_id = ? AND obj_type = 'bot_mem'",
        )?;
        st.query_map([ws], |r| Ok((r.get(0)?, (r.get(1)?, r.get(2)?))))?
            .collect::<rusqlite::Result<HashMap<_, _>>>()?
    };
    let mut n = 0;
    for (id, _bot, text, name) in &rows {
        let label = format!("{name} 的记忆");
        if have.get(id).is_some_and(|(t, l)| t == text && *l == label) {
            continue;
        }
        let len = text.chars().count();
        index::replace(
            tx,
            &ChunkMeta {
                ws,
                obj_type: "bot_mem",
                obj_id: id,
                title: name,
                visibility: &src.visibility,
                level: src.level,
                created_by: &src.created_by,
                source_kind: "bot_mems",
                label: &label,
                updated: now,
            },
            &[Chunk {
                seq: 0,
                heading: String::new(),
                text: text.clone(),
                start: 0,
                end: len,
            }],
        )?;
        n += 1;
    }
    for id in have.keys() {
        if !rows.iter().any(|r| &r.0 == id) {
            index::remove(tx, "bot_mem", id)?;
            n += 1;
        }
    }
    // Scope changes of the source reach entries whose text did not change.
    index::rescope_source(tx, &src)?;
    repo::set_mark(tx, ws, "sync:bot_mems", &hash, now)?;
    Ok(n)
}

fn workspaces(c: &Connection) -> Result<Vec<String>> {
    let mut st = c.prepare_cached("SELECT id FROM workspaces ORDER BY id")?;
    let rows = st.query_map([], |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Reacts to a bus event: `mm` re-indexes that memory, `bot` re-compares the old Bot memories of
/// every workspace (its event has no workspace).
pub fn on_event(m: &Modules, e: &ChangeEvent) -> Result<()> {
    let now = m.clock.now();
    match e {
        ChangeEvent::Mm { id, .. } => {
            m.store.transaction(|tx| sync_mem(tx, id, now))?;
        }
        ChangeEvent::Bot { .. } => {
            let wss = m.store.reader(workspaces)?;
            m.store.transaction(|tx| {
                for ws in &wss {
                    sync_bot_mems(tx, ws, "steve", now)?;
                }
                Ok::<_, StoreError>(())
            })?;
        }
        _ => {}
    }
    Ok(())
}

/// One sweep over every workspace: built-in sources, memories, Bot memories.
pub fn sweep(m: &Modules) -> Result<usize> {
    let now = m.clock.now();
    let wss = m.store.reader(workspaces)?;
    m.store.transaction(|tx| {
        let mut n = 0;
        for ws in &wss {
            sources::ensure_builtin(tx, ws, "steve", now)?;
            n += sweep_mems(tx, ws, now)?;
            n += sync_bot_mems(tx, ws, "steve", now)?;
        }
        Ok(n)
    })
}

/// The `kb.sync_mems` job: a sweep now, then every [`SWEEP_EVERY`].
pub fn spawn_sweeper(m: Modules) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(SWEEP_EVERY);
        loop {
            tick.tick().await;
            let mm = m.clone();
            match tokio::task::spawn_blocking(move || sweep(&mm)).await {
                Ok(Ok(_)) => {}
                Ok(Err(e)) => tracing::warn!("kb.sync_mems: {e}"),
                Err(e) => tracing::warn!("kb.sync_mems: task failed: {e}"),
            }
        }
    })
}
