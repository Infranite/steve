//! Retrieval: FTS candidates filtered by workspace, visibility, level and
//! paused sources in SQL, then re-ranked, deduplicated, numbered and given citation links.
//!
//! Ranking: `bm25(kb_fts, 1.0, 2.0)` (title weight 2), then ×1.5 when the hit is about one of the
//! anchors, ×1.2 for memories (short and precise). The strict query (short CJK runs as phrases)
//! goes first; when it finds fewer than 5 documents, the relaxed query (every bigram on its own,
//! so typos still match) fills in after it.
//!
//! Work items and main-thread chat messages are NOT indexed into knowledge; [`search_records`]
//! searches them separately as records without mixing them into knowledge recall.
//!
//! Visibility and `never` are filtered only here and in the list functions: callers never
//! build their own conditions.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use steve_store::base::{Level, ObjRef, Viewer, Visibility};
use steve_store::knowledge::{
    BotMemories, Chunks, Documents, EntryStatus, KnowledgeUses, Memories, MemoryEntries,
    MemoryKind, RecordSearch,
};
use steve_store::{R, Read, Result, W};

use crate::port::{Candidate, KnowledgeStore, Pass, Scope};
use crate::sync;
use crate::sync::about_names;

/// FTS candidates per pass.
pub const CANDIDATES: usize = 50;
/// Upper bound of `k`.
pub const MAX_K: usize = 20;
/// The strict pass is topped up by the relaxed one below this many distinct documents.
pub const FALLBACK_BELOW: usize = 5;
/// Chunks of one document in one result list.
pub const PER_DOC: usize = 2;

const ANCHOR_BOOST: f64 = 1.5;
const MEM_BOOST: f64 = 1.2;
const RECORD_SNIPPET_CHARS: usize = 400;

/// Who is asking, which decides extra filters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Purpose {
    Agent,
    Chat,
    Reply,
    Draft,
    Flow,
    Search,
}

/// One retrieval request.
#[derive(Clone)]
pub struct Query<'a> {
    /// Where to search (the workspace's knowledge store, [`crate::Knowledge::store`]).
    pub store: &'a dyn KnowledgeStore,
    /// Workspace and user the results are filtered for ([`Scope::for_viewer`]).
    pub viewer: &'a Viewer,
    pub text: &'a str,
    /// Sender, space, item, project …: hits about them rank higher.
    pub anchors: &'a [ObjRef],
    pub purpose: Purpose,
    /// Only these source kinds (`notes`, `folder`, `mems` …).
    pub kinds: Option<&'a [&'a str]>,
    /// At most this many hits (capped at [`MAX_K`]).
    pub k: usize,
    /// For the chat recency decay (Unix ms).
    pub now: i64,
}

impl<'a> Query<'a> {
    /// No anchors, all kinds, `k = 10`, now.
    pub fn new(
        store: &'a dyn KnowledgeStore,
        viewer: &'a Viewer,
        text: &'a str,
        purpose: Purpose,
    ) -> Self {
        Self {
            store,
            viewer,
            text,
            anchors: &[],
            purpose,
            kinds: None,
            k: 10,
            now: steve_store::now(),
        }
    }
}

/// One numbered result. Serializes as the API `Hit`; read back (without `about`) where a
/// reply's knowledge is kept until it is sent (the channel module's conversation turns).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hit {
    pub n: usize,
    #[serde(rename = "type")]
    pub obj_type: String,
    #[serde(rename = "id")]
    pub obj_id: String,
    pub seq: i64,
    pub title: String,
    pub heading: String,
    pub text: String,
    pub label: String,
    pub source_kind: String,
    pub level: Level,
    pub visibility: Visibility,
    pub score: f32,
    pub href: String,
    #[serde(skip)]
    pub about: Vec<ObjRef>,
}

/// `about` objects of an index entry.
fn about_of(c: &mut impl Read, kind: &str, id: &str) -> Result<Vec<ObjRef>> {
    match kind {
        "chunk" => Documents(c).doc_about(id),
        "mem" => Memories(c).subjects(id),
        "memory_entry" => Ok(MemoryEntries(c)
            .get(id)?
            .map(|e| ObjRef::new(&e.subject_type, &e.subject_id))
            .into_iter()
            .collect()),
        "bot_mem" => Ok(BotMemories(c)
            .bot(id)?
            .map(|id| ObjRef::new("bot", &id))
            .into_iter()
            .collect()),
        _ => Ok(Vec::new()),
    }
}

fn href(c: &Candidate, about: &[ObjRef]) -> String {
    match c.obj_type.as_str() {
        "mem" | "memory_entry" => format!("#/knowledge?mem={}", c.obj_id),
        "bot_mem" => match about.iter().find(|a| a.kind == "bot") {
            Some(b) => format!("#/bots/{}", b.id),
            None => "#/bots".into(),
        },
        _ => format!("#/knowledge/doc/{}?seq={}", c.obj_id, c.seq),
    }
}

fn title(c: &Candidate) -> String {
    match c.obj_type.as_str() {
        "chunk" => c.title.clone().unwrap_or_default(),
        "mem" => sync::mem_title(&c.label).to_string(),
        "memory_entry" => c.label.clone(),
        _ => c.label.clone(),
    }
}

/// Boosted score of a candidate.
fn boosted(c: &Candidate, about: &[ObjRef], q: &Query) -> f64 {
    let mut s = c.base;
    if !q.anchors.is_empty() && about.iter().any(|a| q.anchors.contains(a)) {
        s *= ANCHOR_BOOST;
    }
    if c.obj_type == "mem" || c.obj_type == "memory_entry" {
        s *= MEM_BOOST;
    }
    s
}

/// One record result from direct search over work items and main-thread conversations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordHit {
    /// `"item"` | `"chat"`
    pub record_kind: String,
    pub id: String,
    pub title: String,
    pub text: String,
    pub label: String,
    pub href: String,
    pub updated: i64,
}

fn clip_record(s: &str) -> String {
    let trimmed = s.trim();
    if trimmed.chars().count() <= RECORD_SNIPPET_CHARS {
        trimmed.to_string()
    } else {
        let cut: String = trimmed.chars().take(RECORD_SNIPPET_CHARS).collect();
        format!("{cut}…")
    }
}

/// Searches work items and main-thread conversation messages as a separate record category
/// without mixing them into knowledge recall.
pub fn search_records(
    c: &mut impl Read,
    viewer: &Viewer,
    text: &str,
    limit: usize,
) -> Result<Vec<RecordHit>> {
    let terms: Vec<String> = text
        .split_whitespace()
        .map(|t| t.trim().to_lowercase())
        .filter(|t| !t.is_empty())
        .collect();
    if terms.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }
    let limit = limit.clamp(1, MAX_K);
    let zone = crate::sources::chat::workspace_zone(c, &viewer.ws)?;
    let mut out = Vec::new();
    for item in RecordSearch(c).items(&viewer.ws, &terms, limit)? {
        let body = if !item.summary.trim().is_empty() {
            &item.summary
        } else if !item.goal.trim().is_empty() {
            &item.goal
        } else {
            &item.title
        };
        out.push(RecordHit {
            record_kind: "item".into(),
            title: format!("{} {}", item.id, item.title),
            text: clip_record(body),
            label: item.id.clone(),
            href: format!("#/items/{}", item.id),
            id: item.id,
            updated: item.updated,
        });
    }
    for msg in RecordSearch(c).chat(&viewer.ws, crate::sources::chat::SPACE, &terms, limit)? {
        let day = crate::sources::chat::day_of(msg.created, zone);
        let label = steve_i18n::t!("knowledge.chat.day", day = day);
        out.push(RecordHit {
            record_kind: "chat".into(),
            id: msg.id,
            title: label.clone(),
            text: clip_record(&msg.text),
            label,
            href: "#/".into(),
            updated: msg.created,
        });
    }
    out.sort_by(|a, b| b.updated.cmp(&a.updated).then_with(|| a.id.cmp(&b.id)));
    out.truncate(limit);
    Ok(out)
}

/// The note that goes with hits found without the meaning search (the store's semantic pass
/// failed): in contexts, search answers and drafts, so nobody takes a thin answer for the whole.
pub const SEMANTIC_DOWN_NOTE: &str = "（语义检索暂时不可用，这次只按关键词查找，结果可能不全。）";

/// What [`retrieve_all`] found.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Retrieved {
    pub hits: Vec<Hit>,
    /// The semantic pass failed (the store is down or was skipped after a failure): `hits` come
    /// from the keyword passes only.
    pub semantic_down: bool,
}

/// Searches the knowledge base for `q.viewer`. Empty or unsearchable text gives no hits. The hits
/// of [`retrieve_all`], for callers that do not show [`Retrieved::semantic_down`].
pub fn retrieve(c: &mut R<'_>, q: &Query) -> Result<Vec<Hit>> {
    retrieve_all(c, q).map(|r| r.hits)
}

/// [`retrieve`], and whether the semantic pass failed. A failed semantic pass does not fail the
/// search: the keyword passes of the Hub's own index still answer.
pub fn retrieve_all(c: &mut R<'_>, q: &Query) -> Result<Retrieved> {
    let k = q.k.clamp(1, MAX_K);
    let text = q.text.trim();
    if text.is_empty() {
        return Ok(Retrieved::default());
    }
    let scope = Scope::for_viewer(c, q.viewer, q.purpose, q.kinds)?;
    let mut found = |pass| -> Result<Vec<Candidate>> {
        Ok(q.store
            .search(c, &scope, text, pass, CANDIDATES)?
            .into_iter()
            // the Hub's own check of what a store returned
            .filter(|x| scope.admits(x))
            .collect())
    };
    let strict = found(Pass::Strict)?;
    let mut semantic_down = false;
    let semantic = found(Pass::Semantic).unwrap_or_else(|e| {
        tracing::warn!(
            store = q.store.id(),
            "semantic search failed, keywords only: {e}"
        );
        semantic_down = true;
        Vec::new()
    });
    let mut tiers = vec![strict, semantic];
    let docs: HashSet<&str> = tiers.iter().flatten().map(|x| x.obj_id.as_str()).collect();
    if docs.len() < FALLBACK_BELOW {
        tiers.push(found(Pass::Relaxed)?);
    }

    // Rank each tier on its own (bm25 values of different queries do not compare), then concatenate.
    let mut about_cache: HashMap<(String, String), Vec<ObjRef>> = HashMap::new();
    let mut ranked: Vec<(Candidate, f64)> = Vec::new();
    let mut seen_rows: HashSet<(String, String, i64)> = HashSet::new();
    for tier in tiers {
        let mut scored = Vec::new();
        for cd in tier {
            let key = (cd.obj_type.clone(), cd.obj_id.clone(), cd.seq);
            if !seen_rows.insert(key) {
                continue;
            }
            let ak = (cd.obj_type.clone(), cd.obj_id.clone());
            if !about_cache.contains_key(&ak) {
                about_cache.insert(ak.clone(), about_of(c, &cd.obj_type, &cd.obj_id)?);
            }
            let s = boosted(&cd, &about_cache[&ak], q);
            scored.push((cd, s));
        }
        scored.sort_by(|a, b| b.1.total_cmp(&a.1));
        ranked.extend(scored);
    }

    // Dedup, cut, number.
    let mut per_obj: HashMap<(String, String), usize> = HashMap::new();
    let mut texts: HashSet<String> = HashSet::new();
    let mut hits = Vec::new();
    for (cd, s) in ranked {
        let ok = (cd.obj_type.clone(), cd.obj_id.clone());
        let n = per_obj.entry(ok.clone()).or_default();
        if *n >= PER_DOC {
            continue;
        }
        if !cd.text.is_empty() && !texts.insert(cd.text.clone()) {
            continue;
        }
        *n += 1;
        let about = about_cache.get(&ok).cloned().unwrap_or_default();
        hits.push(Hit {
            n: hits.len() + 1,
            title: title(&cd),
            href: href(&cd, &about),
            obj_type: cd.obj_type,
            obj_id: cd.obj_id,
            seq: cd.seq,
            heading: cd.heading,
            text: cd.text,
            label: cd.label,
            source_kind: cd.source_kind,
            level: cd.level,
            visibility: cd.visibility,
            score: s as f32,
            about,
        });
        if hits.len() == k {
            break;
        }
    }
    Ok(Retrieved {
        hits,
        semantic_down,
    })
}

/// Href prefix for memory entries (`#/knowledge?mem=<id>`), shared by hit builders and citation
/// metric accounting.
pub const MEM_HREF_PREFIX: &str = "#/knowledge?mem=";

/// Active confirmed memories about any of `anchors`, ordered by `pinned DESC`, kind priority
/// (`rule` > `preference` > `summary` > `fact` > `event`), `updated DESC`, `id ASC`, and assembled
/// up to `max_chars` (total text characters, each entry capped at [`crate::context::SNIPPET_MAX`]).
/// Same visibility, `never` and paused-source rules as [`retrieve`]; `workspace_only` restricts to
/// `Visibility::Workspace` (for multi-member workspaces).
pub fn anchor_mems(
    c: &mut impl Read,
    viewer: &Viewer,
    anchors: &[ObjRef],
    max_chars: usize,
    workspace_only: bool,
    now: i64,
) -> Result<Vec<Hit>> {
    if anchors.is_empty() || max_chars == 0 {
        return Ok(Vec::new());
    }
    if Documents(c)
        .paused(&viewer.ws)?
        .iter()
        .any(|(_, k)| k == "mems")
    {
        return Ok(Vec::new());
    }
    let query = steve_store::knowledge::AnchorQuery {
        workspace_only,
        max_rows: steve_store::knowledge::ANCHOR_QUERY_MAX_ROWS,
    };
    let mut hits = Vec::new();
    let mut used_chars = 0usize;
    for m in Memories(c).anchors(viewer, anchors, now, query)? {
        let names = about_names(c, &m.about)?;
        let label = sync::mem_label(&m.kind, &names);
        let mut text = if names.is_empty() {
            m.text
        } else {
            format!("{names}：{}", m.text)
        };
        let entry_chars = text.chars().count().min(crate::context::SNIPPET_MAX);
        if !hits.is_empty() && used_chars + entry_chars > max_chars {
            break;
        }
        if hits.is_empty() && entry_chars > max_chars {
            text = if max_chars > 1 {
                let mut t: String = text.chars().take(max_chars - 1).collect();
                t.push('…');
                t
            } else {
                text.chars().take(max_chars).collect()
            };
        }
        used_chars += entry_chars.min(max_chars);
        hits.push(Hit {
            n: hits.len() + 1,
            obj_type: "mem".into(),
            href: format!("{MEM_HREF_PREFIX}{}", m.id),
            obj_id: m.id,
            seq: 0,
            title: sync::mem_title(&label).to_string(),
            heading: String::new(),
            text,
            label,
            source_kind: "mems".into(),
            level: m.level,
            visibility: m.visibility,
            score: 0.0,
            about: m.about,
        });
    }
    let paused_sources: HashSet<String> = Documents(c)
        .paused(&viewer.ws)?
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    let me_party = if !viewer.user.is_empty() {
        steve_store::Contacts::new(c).user_party(&viewer.ws, &viewer.user)?
    } else {
        None
    };
    let mut file_entries = Vec::new();
    for a in anchors {
        let (st, sid) = if a.kind == "party" && me_party.as_deref() == Some(a.id.as_str()) {
            ("me", "me")
        } else {
            (a.kind.as_str(), a.id.as_str())
        };
        for e in MemoryEntries(c).list_by_subject(&viewer.ws, st, sid)? {
            if e.status != EntryStatus::Active
                || e.kind == MemoryKind::Overview
                || e.level == Level::Never
                || paused_sources.contains(&e.source_id)
                || !viewer.sees(&e.visibility, &e.created_by)
                || (workspace_only && e.visibility != Visibility::Workspace)
                || file_entries
                    .iter()
                    .any(|x: &steve_store::knowledge::MemoryEntryRow| x.id == e.id)
            {
                continue;
            }
            file_entries.push(e);
        }
    }
    file_entries.sort_by(|a, b| {
        b.pinned
            .cmp(&a.pinned)
            .then_with(|| a.kind.priority().cmp(&b.kind.priority()))
            .then_with(|| b.updated_at.cmp(&a.updated_at))
            .then_with(|| a.id.cmp(&b.id))
    });
    for e in file_entries {
        let text = match Chunks(c).disclosure(&viewer.ws, "memory_entry", &e.id, 0)? {
            Some((t, _, _)) if !t.is_empty() => t,
            _ => match crate::memory_file::read_entry_text_from_disk(c, &e) {
                Ok(Some(t)) => t,
                _ => continue,
            },
        };
        let entry_chars = text.chars().count().min(crate::context::SNIPPET_MAX);
        if !hits.is_empty() && used_chars + entry_chars > max_chars {
            break;
        }
        let text = if hits.is_empty() && entry_chars > max_chars {
            if max_chars > 1 {
                let mut t: String = text.chars().take(max_chars - 1).collect();
                t.push('…');
                t
            } else {
                text.chars().take(max_chars).collect()
            }
        } else {
            text
        };
        used_chars += entry_chars.min(max_chars);
        let label = format!("{} · {}", e.rel_path, e.heading_path);
        hits.push(Hit {
            n: hits.len() + 1,
            obj_type: "memory_entry".into(),
            href: format!("{MEM_HREF_PREFIX}{}", e.id),
            obj_id: e.id.clone(),
            seq: 0,
            title: e.rel_path.clone(),
            heading: e.heading_path.clone(),
            text,
            label,
            source_kind: "folder".into(),
            level: e.level,
            visibility: e.visibility.clone(),
            score: 0.0,
            about: vec![ObjRef::new(&e.subject_type, &e.subject_id)],
        });
    }
    Ok(hits)
}

pub(crate) fn use_hits(hits: &[Hit]) -> Vec<steve_store::knowledge::UseHit> {
    hits.iter()
        .map(|h| steve_store::knowledge::UseHit {
            n: h.n,
            obj_type: h.obj_type.clone(),
            obj_id: h.obj_id.clone(),
            seq: h.seq,
            score: h.score,
            level: h.level,
            label: h.label.clone(),
        })
        .collect()
}

/// Records which pieces a consumer saw, using the surrounding transaction, updates sidecar
/// `memory_entries.use_count`/`last_used_at`, and emits `steve_memory_uses_total`.
pub fn record_use(
    tx: &mut W<'_>,
    ws: &str,
    consumer: (&str, &str),
    query: &str,
    hits: &[Hit],
) -> Result<String> {
    let id = KnowledgeUses(tx).record(ws, consumer, query, &use_hits(hits))?;
    let now = steve_store::now();
    for h in hits {
        if h.obj_type == "memory_entry" {
            let _ = MemoryEntries(tx).record_use(&h.obj_id, now);
        }
    }
    let mem_hits = hits
        .iter()
        .filter(|h| h.obj_type == "mem" || h.obj_type == "memory_entry")
        .count() as u64;
    crate::uses::record_memory_uses(consumer.0, crate::uses::MemoryUseStage::Injected, mem_hits);
    Ok(id)
}

/// The knowledge a disclosure basis names (`entries` of `reply_basis` / a mail draft's basis), read again
/// for a later check of the same text. For `memory_entry` items, reads the authoritative sidecar
/// `memory_entries.level` & `visibility` so `OutboundCheck` always reflects per-entry disclosure rules.
pub fn knowledge_again(
    c: &mut impl Read,
    ws: &str,
    entries: &[serde_json::Value],
) -> Result<(Vec<steve_modules::outbound::KnowledgeUse>, bool)> {
    use steve_modules::outbound::KnowledgeUse;
    let mut out = Vec::new();
    let mut complete = true;
    for e in entries {
        let kind = e["source_type"].as_str().unwrap_or("");
        if kind == "span" {
            continue;
        }
        let id = e["source_id"].as_str().unwrap_or("");
        let label = e["source_label"].as_str().unwrap_or("").to_string();
        let found =
            if kind == "memory_entry" {
                MemoryEntries(c)
                    .get(id)?
                    .filter(|m| m.workspace_id == ws && m.status == EntryStatus::Active)
                    .and_then(|m| {
                        let text = Chunks(c)
                            .disclosure(ws, "memory_entry", &m.id, 0)
                            .ok()
                            .flatten()
                            .map(|(t, _, _)| t)
                            .or_else(|| {
                                crate::memory_file::read_entry_text_from_disk(c, &m)
                                    .ok()
                                    .flatten()
                            })?;
                        Some(KnowledgeUse {
                            source_type: "memory_entry".into(),
                            source_id: m.id.clone(),
                            source_label: if label.is_empty() {
                                m.rel_path.clone()
                            } else {
                                label.clone()
                            },
                            level: m.level,
                            visibility: m.visibility.code(),
                            text,
                            seq: Some(0),
                        })
                    })
            } else {
                match e["seq"].as_i64() {
                    Some(seq) => {
                        Chunks(c)
                            .disclosure(ws, kind, id, seq)?
                            .map(|(text, level, visibility)| KnowledgeUse {
                                source_type: kind.into(),
                                source_id: id.into(),
                                source_label: label.clone(),
                                level,
                                visibility,
                                text,
                                seq: Some(seq),
                            })
                    }
                    None if kind == "mem" => Memories(c)
                        .get(id)?
                        .filter(|m| m.workspace_id == ws)
                        .map(|m| KnowledgeUse {
                            source_type: "mem".into(),
                            source_id: m.id.clone(),
                            source_label: if label.is_empty() {
                                "记忆".into()
                            } else {
                                label.clone()
                            },
                            level: m.level,
                            visibility: m.visibility.code(),
                            text: m.text.clone(),
                            seq: None,
                        }),
                    None => None,
                }
            };
        match found {
            Some(k) => out.push(k),
            None => complete = false,
        }
    }
    Ok((out, complete))
}
