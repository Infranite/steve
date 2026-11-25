//! Sources: the four built-in knowledge sources every workspace gets on first use, and
//! the `Source` JSON shape. Folder sources, source writes and uploads are in [`folder`] and [`files`].

pub mod files;
pub mod folder;
pub mod folder_node;

use serde_json::{Value, json};
use steve_store::base::{Level, Visibility};
use steve_store::knowledge::{Documents, SourceRole, SourceRow, WriteMode};
use steve_store::{Read, Result, W, uid};

// Extraction origins (no longer indexed as knowledge documents)
pub mod chat;
pub mod items;

/// `(kind, name, visibility, level)` of the built-in sources. The `mems` defaults are unused:
/// every memory carries its own scope and level.
pub const BUILTIN: [(&str, &str, &str, &str); 4] = [
    ("notes", "我的笔记", "private", "conclusion"),
    ("files", "上传的文件", "private", "conclusion"),
    ("mems", "记忆", "private", "conclusion"),
    ("bot_mems", "Bot 记忆", "workspace", "conclusion"),
];

/// A source's name as shown: a built-in one still under its seeded (zh-CN) name in the request's
/// language, any other as named.
pub fn name_shown(kind: &str, stored: &str) -> String {
    let seeded = BUILTIN.iter().any(|b| b.0 == kind && b.1 == stored);
    if !seeded {
        return stored.to_owned();
    }
    match kind {
        "notes" => steve_i18n::t!("knowledge.builtin.notes"),
        "files" => steve_i18n::t!("knowledge.builtin.files"),
        "mems" => steve_i18n::t!("knowledge.builtin.mems"),
        "bot_mems" => steve_i18n::t!("knowledge.builtin.bot_mems"),
        _ => stored.to_owned(),
    }
}

/// A day of the main conversation as stored (`2026-10-31 的对话`, `10-31 的对话`: a label of what
/// came from it) in the request's language; anything else as written.
pub fn chat_day_shown(stored: &str) -> String {
    steve_i18n::reword!(stored, &["knowledge.chat.day"]).unwrap_or_else(|| stored.to_owned())
}

/// A stored `source_label` / `origin_label` in the request's language.
pub fn label_shown(stored: &str) -> String {
    steve_i18n::reword!(stored, &["knowledge.chat.day"])
        .unwrap_or_else(|| steve_modules::shown::source_label(stored))
}

pub fn is_builtin(kind: &str) -> bool {
    BUILTIN.iter().any(|b| b.0 == kind)
}

/// Are all built-in sources there? (A read, so most requests skip the write transaction.)
pub fn has_builtin(c: &mut impl Read, ws: &str) -> Result<bool> {
    Documents(c).has_builtins(ws)
}

/// Creates the missing built-in sources of `ws` (deduplicated by `(workspace_id, kind)`).
/// Returns the ids created.
pub fn ensure_builtin(w: &mut W<'_>, ws: &str, actor: &str, now: i64) -> Result<Vec<String>> {
    let mut made = Vec::new();
    for (kind, name, vis, level) in BUILTIN {
        if Documents(w).source_of_kind(ws, kind)?.is_some() {
            continue;
        }
        let (role, write_mode) = if kind == "mems" {
            (SourceRole::Memory, WriteMode::DirectWrite)
        } else {
            (SourceRole::Knowledge, WriteMode::ReadOnly)
        };
        let id = uid("ks");
        Documents(w).insert_source(
            &id,
            ws,
            kind,
            name,
            &json!({}),
            &Visibility::from_db(vis),
            Level::from_db(level),
            role,
            write_mode,
            actor,
            now,
        )?;
        made.push(id);
    }
    Ok(made)
}

/// The source of a built-in kind, creating the built-ins if needed.
pub fn builtin(w: &mut W<'_>, ws: &str, kind: &str, actor: &str, now: i64) -> Result<SourceRow> {
    if let Some(s) = Documents(w).source_of_kind(ws, kind)? {
        return Ok(s);
    }
    ensure_builtin(w, ws, actor, now)?;
    Documents(w)
        .source_of_kind(ws, kind)?
        .ok_or(steve_store::StoreError::NotFound("没有这个知识来源"))
}

/// `Source` with live document and chunk counts.
pub fn source_json(c: &mut impl Read, s: &SourceRow) -> Result<Value> {
    let (docs, chunks) = Documents(c).source_counts(s)?;
    let skipped = s.stats.get("skipped").and_then(Value::as_i64).unwrap_or(0);
    let mut stats = json!({"docs": docs, "chunks": chunks, "skipped": skipped});
    // set only by a folder scan that could not see everything (nothing was deleted)
    for k in ["kept", "incomplete"] {
        if let Some(v) = s.stats.get(k) {
            stats[k] = v.clone();
        }
    }
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
        "name": name_shown(&s.kind, &s.name),
        "path": cfg.get("path").cloned().unwrap_or(Value::Null),
        "include": list("include"),
        "exclude": list("exclude"),
        "visibility": s.visibility,
        "level": s.level,
        "role": s.role,
        "write_mode": s.write_mode,
        "paused": s.paused,
        "status": s.status,
        "error": s.error,
        "stats": stats,
        "last_sync": s.last_sync,
        "builtin": is_builtin(&s.kind),
        "revision": s.revision,
    }))
}

#[cfg(test)]
mod shown_tests {
    use super::*;
    use steve_i18n::{EN, ZH_CN, with};

    #[test]
    fn built_in_names_and_chat_days_by_language() {
        assert_eq!(with(EN, || name_shown("notes", "我的笔记")), "My notes");
        assert_eq!(
            with(EN, || name_shown("bot_mems", "Bot 记忆")),
            "Bot memories"
        );
        // renamed, or not a built-in one: as named
        assert_eq!(with(EN, || name_shown("notes", "随笔")), "随笔");
        assert_eq!(with(EN, || name_shown("folder", "我的笔记")), "我的笔记");
        assert_eq!(with(ZH_CN, || name_shown("notes", "我的笔记")), "我的笔记");
        assert_eq!(
            with(EN, || chat_day_shown("2026-11-12 的对话")),
            "Conversation on 2026-11-12"
        );
        assert_eq!(
            with(EN, || label_shown("11-12 的对话")),
            "Conversation on 11-12"
        );
        assert_eq!(with(EN, || label_shown("你写的")), "Written by you");
        assert_eq!(with(ZH_CN, || label_shown("11-12 的对话")), "11-12 的对话");
    }
}
