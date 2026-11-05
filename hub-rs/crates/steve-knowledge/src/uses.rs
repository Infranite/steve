//! What knowledge a consumer was given (`kb_uses`, `GET /kb/uses`): the hits are
//! stored as ids and numbers only, so titles, labels and links are looked up again here under
//! today's visibility. Whatever is gone, or no longer visible to the caller, shows as “已删除”.

use rusqlite::{Connection, OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};
use steve_modules::{ApiError, ApiResult, Caller, Modules};
use steve_store::Result;
use steve_store::base::{Level, Viewer, bind};

/// Shown instead of the title and label of a hit that is gone or no longer visible.
pub const DELETED: &str = "已删除";
pub const CONSUMERS: [&str; 5] = ["session", "chat", "draft", "flow", "search"];

/// One stored hit (`kb_uses.hits[]`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct UseHit {
    pub n: usize,
    pub obj_type: String,
    pub obj_id: String,
    #[serde(default)]
    pub seq: i64,
    #[serde(default)]
    pub score: f32,
    pub level: Level,
    #[serde(default)]
    pub label: String,
}

/// One `kb_uses` row.
#[derive(Debug, Clone, PartialEq)]
pub struct UseRow {
    pub id: String,
    pub query: String,
    pub hits: Vec<UseHit>,
    pub created: i64,
}

/// The uses of one consumer, oldest first.
pub fn uses_of(c: &Connection, ws: &str, ctype: &str, cid: &str) -> Result<Vec<UseRow>> {
    let mut st = c.prepare_cached(
        "SELECT id, query, hits, created FROM kb_uses
          WHERE workspace_id = ? AND consumer_type = ? AND consumer_id = ? ORDER BY created, rowid",
    )?;
    let rows = st.query_map(params![ws, ctype, cid], |r| {
        let hits: String = r.get(2)?;
        Ok(UseRow {
            id: r.get(0)?,
            query: r.get(1)?,
            hits: serde_json::from_str(&hits).unwrap_or_default(),
            created: r.get(3)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// The highest `n` a session has been given so far (opening prompt and earlier searches), so the
/// next `steve_knowledge_search` continues the numbering.
pub fn last_n(c: &Connection, ws: &str, session_id: &str) -> Result<usize> {
    Ok(uses_of(c, ws, "session", session_id)?
        .iter()
        .flat_map(|u| u.hits.iter().map(|h| h.n))
        .max()
        .unwrap_or(0))
}

/// An index entry as the caller may see it now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Current {
    pub title: String,
    pub label: String,
    pub href: String,
    /// The chunk's heading path (“标题 > 小节”), empty for memories.
    pub heading: String,
}

/// Title, label, link and heading of an index entry as `viewer` may see it now; `None` = gone,
/// `never` or not visible.
pub fn current(c: &Connection, viewer: &Viewer, h: &UseHit) -> Result<Option<Current>> {
    let scope = viewer.sql("c");
    let sql = format!(
        "SELECT c.label, c.source_kind, d.title, d.ref, c.heading FROM kb_chunks c
           LEFT JOIN kb_docs d ON c.obj_type = 'chunk' AND d.id = c.obj_id
          WHERE c.workspace_id = ? AND c.obj_type = ? AND c.obj_id = ? AND c.seq = ?
            AND c.level <> 'never' AND {}",
        scope.sql
    );
    let mut p: Vec<rusqlite::types::Value> = vec![
        viewer.ws.clone().into(),
        h.obj_type.clone().into(),
        h.obj_id.clone().into(),
        h.seq.into(),
    ];
    p.extend(scope.params);
    type Row = (String, String, Option<String>, Option<String>, String);
    let row: Option<Row> = c
        .prepare(&sql)?
        .query_row(bind(p), |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .optional()?;
    let Some((label, source_kind, title, r#ref, heading)) = row else {
        return Ok(None);
    };
    let (title, href) = match h.obj_type.as_str() {
        "mem" => (
            crate::sync::mem_title(&label).to_string(),
            format!("#/knowledge?mem={}", h.obj_id),
        ),
        "bot_mem" => {
            let bot: Option<String> = c
                .prepare_cached("SELECT bot_id FROM bot_memories WHERE id = ?")?
                .query_row([&h.obj_id], |r| r.get(0))
                .optional()?;
            (
                label.clone(),
                bot.map_or_else(|| "#/bots".into(), |b| format!("#/bots/{b}")),
            )
        }
        _ if source_kind == "items" => (
            title.unwrap_or_default(),
            format!("#/items/{}", r#ref.as_deref().unwrap_or(&h.obj_id)),
        ),
        _ => (
            title.unwrap_or_default(),
            format!("#/knowledge/doc/{}?seq={}", h.obj_id, h.seq),
        ),
    };
    Ok(Some(Current {
        title,
        label,
        href,
        heading,
    }))
}

/// Does the consumer exist in the caller's workspace (and, for searches, is it the caller)?
fn owns(c: &Connection, who: &Caller, ctype: &str, cid: &str) -> Result<bool> {
    let table = match ctype {
        "search" => return Ok(cid == who.me),
        "session" => "sessions",
        "chat" => "messages",
        "draft" => "mail_drafts",
        "flow" => "flow_runs",
        _ => return Ok(false),
    };
    Ok(c.prepare_cached(&format!(
        "SELECT 1 FROM {table} WHERE id = ? AND workspace_id = ?"
    ))?
    .query_row(params![cid, who.ws], |_| Ok(()))
    .optional()?
    .is_some())
}

fn noun(ctype: &str) -> &'static str {
    match ctype {
        "session" => "会话",
        "chat" => "消息",
        "draft" => "草稿",
        "flow" => "运行",
        _ => "使用记录",
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct UsesParams {
    pub consumer_type: Option<String>,
    pub consumer_id: Option<String>,
}

/// `GET /kb/uses?consumer_type=&consumer_id=` → `{hits: […]}`: every hit the consumer was given, in
/// order, each with `use_id`, `query`, `created` and today's `title`/`label`/`href` (`deleted:true`
/// and “已删除” when it is gone or no longer visible to the caller).
pub fn list(m: &Modules, who: &Caller, p: UsesParams) -> ApiResult<Value> {
    let ctype = p.consumer_type.unwrap_or_default();
    let cid = p.consumer_id.unwrap_or_default();
    let mut bad = Vec::new();
    if !CONSUMERS.contains(&ctype.as_str()) {
        bad.push("consumer_type".to_string());
    }
    if cid.is_empty() {
        bad.push("consumer_id".to_string());
    }
    if !bad.is_empty() {
        return Err(ApiError::Invalid(bad));
    }
    let viewer = m.viewer(who)?;
    m.store.reader(|c| {
        if !owns(c, who, &ctype, &cid)? {
            return Err(ApiError::not_found(noun(&ctype)));
        }
        let mut out = Vec::new();
        for u in uses_of(c, &who.ws, &ctype, &cid)? {
            for h in &u.hits {
                let now = current(c, &viewer, h)?;
                let deleted = now.is_none();
                let cur = now.unwrap_or_else(|| Current {
                    title: DELETED.into(),
                    label: DELETED.into(),
                    href: String::new(),
                    heading: String::new(),
                });
                out.push(json!({
                    "n": h.n, "type": h.obj_type, "id": h.obj_id, "seq": h.seq, "score": h.score,
                    "level": h.level, "title": cur.title, "label": cur.label, "href": cur.href,
                    "heading": cur.heading, "deleted": deleted,
                    "use_id": u.id, "query": u.query, "created": u.created,
                }));
            }
        }
        Ok(json!({ "hits": out }))
    })
}
