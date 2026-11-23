//! What knowledge a consumer was given (`kb_uses`, `GET /kb/uses`): the hits are
//! stored as ids and numbers only, so titles, labels and links are looked up again here under
//! today's visibility. Whatever is gone, or no longer visible to the caller, shows as “已删除”.

use serde::Deserialize;
use serde_json::{Value, json};
use steve_modules::{ApiError, ApiResult, Caller, Modules};
use steve_store::base::Viewer;
use steve_store::knowledge::{BotMemories, ChunkFilter, Chunks, KnowledgeUses};
pub use steve_store::knowledge::{UseHit, UseRow};
use steve_store::{Read, References, Result};

pub const CONSUMERS: [&str; 5] = ["session", "chat", "draft", "flow", "search"];

/// The uses of one consumer, oldest first.
pub fn uses_of(c: &mut impl Read, ws: &str, ctype: &str, cid: &str) -> Result<Vec<UseRow>> {
    KnowledgeUses(c).by_consumer(ws, ctype, cid)
}

/// The highest `n` a session has been given so far (opening prompt and earlier searches), so the
/// next `steve_knowledge_search` continues the numbering.
pub fn last_n(c: &mut impl Read, ws: &str, session_id: &str) -> Result<usize> {
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
pub fn current(c: &mut impl Read, viewer: &Viewer, h: &UseHit) -> Result<Option<Current>> {
    let filter = ChunkFilter {
        ws: &viewer.ws,
        workspace_only: viewer.is_service(),
        kinds: None,
        paused_docs: &[],
        paused_mems: false,
        paused_bot_mems: false,
    };
    let Some(row) = Chunks(c).chunk_in_scope(&filter, &h.obj_type, &h.obj_id, h.seq)? else {
        return Ok(None);
    };
    let steve_store::knowledge::Candidate {
        label,
        source_kind,
        title,
        doc_ref: r#ref,
        heading,
        ..
    } = row;
    let (title, href) = match h.obj_type.as_str() {
        "mem" => (
            crate::sync::mem_title(&label).to_string(),
            format!("#/knowledge?mem={}", h.obj_id),
        ),
        "bot_mem" => {
            let bot = BotMemories(c).bot(&h.obj_id)?;
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
fn owns(c: &mut impl Read, who: &Caller, ctype: &str, cid: &str) -> Result<bool> {
    if ctype == "search" {
        return Ok(cid == who.me);
    }
    References(c).counted_in(&who.ws, ctype, cid)
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
    crate::read_api(&m.store, |c| {
        if !owns(c, who, &ctype, &cid)? {
            return Err(ApiError::not_found(noun(&ctype)));
        }
        let mut out = Vec::new();
        for u in uses_of(c, &who.ws, &ctype, &cid)? {
            for h in &u.hits {
                let now = current(c, &viewer, h)?;
                let deleted = now.is_none();
                let cur = now.unwrap_or_else(|| Current {
                    title: steve_i18n::t!("knowledge.uses.deleted"),
                    label: steve_i18n::t!("knowledge.uses.deleted"),
                    href: String::new(),
                    heading: String::new(),
                });
                out.push(json!({
                    "n": h.n, "type": h.obj_type, "id": h.obj_id, "seq": h.seq, "score": h.score,
                    "level": h.level, "title": crate::sources::chat_day_shown(&cur.title),
                    "label": crate::index::doc_label_shown(&cur.label), "href": cur.href,
                    "heading": cur.heading, "deleted": deleted,
                    "use_id": u.id, "query": u.query, "created": u.created,
                }));
            }
        }
        Ok(json!({ "hits": out }))
    })
}
