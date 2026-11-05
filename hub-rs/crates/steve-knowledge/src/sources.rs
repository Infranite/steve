//! Sources: the six built-in ones every workspace gets on first use, and
//! the `Source` JSON shape. Folder sources, source writes and uploads are in [`folder`] and [`files`].

pub mod files;
pub mod folder;

use rusqlite::Connection;
use serde_json::{Value, json};
use steve_store::base::{Level, Visibility};
use steve_store::{Result, Tx, uid};

use crate::repo::{self, SourceRow};

// K3
pub mod chat;
pub mod items;

/// `(kind, name, visibility, level)` of the built-in sources. The `mems` defaults are unused:
/// every memory carries its own scope and level.
pub const BUILTIN: [(&str, &str, &str, &str); 6] = [
    ("notes", "我的笔记", "private", "conclusion"),
    ("files", "上传的文件", "private", "conclusion"),
    ("items", "事项交付", "workspace", "conclusion"),
    ("chat", "主线对话", "workspace", "conclusion"),
    ("mems", "记忆", "private", "conclusion"),
    ("bot_mems", "Bot 记忆", "workspace", "conclusion"),
];

pub fn is_builtin(kind: &str) -> bool {
    BUILTIN.iter().any(|b| b.0 == kind)
}

/// Are all built-in sources there? (A read, so most requests skip the write transaction.)
pub fn has_builtin(c: &Connection, ws: &str) -> Result<bool> {
    let n: i64 = c
        .prepare_cached(
            "SELECT COUNT(DISTINCT kind) FROM kb_sources WHERE workspace_id = ? AND kind IN ('notes','files','items','chat','mems','bot_mems')",
        )?
        .query_row([ws], |r| r.get(0))?;
    Ok(n as usize == BUILTIN.len())
}

/// Creates the missing built-in sources of `ws` (deduplicated by `(workspace_id, kind)`).
/// Returns the ids created.
pub fn ensure_builtin(tx: &Tx, ws: &str, actor: &str, now: i64) -> Result<Vec<String>> {
    let mut made = Vec::new();
    for (kind, name, vis, level) in BUILTIN {
        if repo::source_of_kind(tx, ws, kind)?.is_some() {
            continue;
        }
        let id = uid("ks");
        repo::insert_source(
            tx,
            &id,
            ws,
            kind,
            name,
            &json!({}),
            &Visibility::from_db(vis),
            Level::from_db(level),
            actor,
            now,
        )?;
        made.push(id);
    }
    Ok(made)
}

/// The source of a built-in kind, creating the built-ins if needed.
pub fn builtin(tx: &Tx, ws: &str, kind: &str, actor: &str, now: i64) -> Result<SourceRow> {
    if let Some(s) = repo::source_of_kind(tx, ws, kind)? {
        return Ok(s);
    }
    ensure_builtin(tx, ws, actor, now)?;
    repo::source_of_kind(tx, ws, kind)?.ok_or(steve_store::StoreError::NotFound("没有这个知识来源"))
}

/// `Source` with live document and chunk counts.
pub fn source_json(c: &Connection, s: &SourceRow) -> Result<Value> {
    let (docs, chunks) = repo::source_counts(c, s)?;
    let skipped = s.stats.get("skipped").and_then(Value::as_i64).unwrap_or(0);
    let cfg = &s.config;
    let list = |k: &str| {
        cfg.get(k)
            .cloned()
            .filter(Value::is_array)
            .unwrap_or(json!([]))
    };
    Ok(json!({
        "id": s.id,
        "kind": s.kind,
        "name": s.name,
        "path": cfg.get("path").cloned().unwrap_or(Value::Null),
        "include": list("include"),
        "exclude": list("exclude"),
        "visibility": s.visibility,
        "level": s.level,
        "paused": s.paused,
        "status": s.status,
        "error": s.error,
        "stats": {"docs": docs, "chunks": chunks, "skipped": skipped},
        "last_sync": s.last_sync,
        "builtin": is_builtin(&s.kind),
        "revision": s.revision,
    }))
}
