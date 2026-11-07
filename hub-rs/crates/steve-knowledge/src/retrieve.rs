//! Retrieval: FTS candidates filtered by workspace, visibility, level and
//! paused sources in SQL, then re-ranked, deduplicated, numbered and given citation links.
//!
//! Ranking: `bm25(kb_fts, 1.0, 2.0)` (title weight 2), then ×1.5 when the hit is about one of the
//! anchors, ×1.2 for memories (short and precise), and a 30-day half-life for the main-thread
//! chat source. The strict query (short CJK runs as phrases) goes first; when it finds fewer
//! than 5 documents, the relaxed query (every bigram on its own, so typos still match) fills in
//! after it — K0's evaluation showed this lifts recall@5 without hurting the strict ranking.
//!
//! Visibility and `never` are filtered only here and in the list functions: callers never
//! build their own conditions.

use std::collections::{HashMap, HashSet};

use rusqlite::types::Value as Sql;
use rusqlite::{Connection, params};
use serde::Serialize;
use serde_json::json;
use steve_store::base::mems;
use steve_store::base::{Level, ObjRef, Viewer, Visibility, bind};
use steve_store::{Result, Tx, uid};

use crate::port::{Candidate, KnowledgeStore, Pass, Scope};
use crate::repo;
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
const CHAT_HALF_LIFE_DAYS: f64 = 30.0;
const DAY_MS: f64 = 86_400_000.0;

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

/// One numbered result. Serializes as the API `Hit`.
#[derive(Debug, Clone, PartialEq, Serialize)]
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
fn about_of(c: &Connection, obj_type: &str, obj_id: &str) -> Result<Vec<ObjRef>> {
    match obj_type {
        "chunk" => repo::doc_about(c, obj_id),
        "mem" => {
            let mut st = c.prepare_cached(
                "SELECT object_type, object_id FROM mem_about WHERE mem_id = ? ORDER BY rowid",
            )?;
            let rows = st.query_map([obj_id], |r| {
                Ok(ObjRef {
                    kind: r.get(0)?,
                    id: r.get(1)?,
                })
            })?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        }
        "bot_mem" => {
            let mut st = c.prepare_cached("SELECT bot_id FROM bot_memories WHERE id = ?")?;
            let rows = st.query_map([obj_id], |r| r.get::<_, String>(0))?;
            Ok(rows
                .collect::<rusqlite::Result<Vec<_>>>()?
                .into_iter()
                .map(|b| ObjRef::new("bot", &b))
                .collect())
        }
        _ => Ok(Vec::new()),
    }
}

fn href(c: &Candidate, about: &[ObjRef]) -> String {
    match c.obj_type.as_str() {
        "mem" => format!("#/knowledge?mem={}", c.obj_id),
        "bot_mem" => match about.iter().find(|a| a.kind == "bot") {
            Some(b) => format!("#/bots/{}", b.id),
            None => "#/bots".into(),
        },
        _ if c.source_kind == "items" => {
            format!("#/items/{}", c.doc_ref.as_deref().unwrap_or(&c.obj_id))
        }
        _ => format!("#/knowledge/doc/{}?seq={}", c.obj_id, c.seq),
    }
}

fn title(c: &Candidate) -> String {
    match c.obj_type.as_str() {
        "chunk" => c.title.clone().unwrap_or_default(),
        "mem" => sync::mem_title(&c.label).to_string(),
        _ => c.label.clone(),
    }
}

/// Boosted score of a candidate.
fn boosted(c: &Candidate, about: &[ObjRef], q: &Query) -> f64 {
    let mut s = c.base;
    if !q.anchors.is_empty() && about.iter().any(|a| q.anchors.contains(a)) {
        s *= ANCHOR_BOOST;
    }
    if c.obj_type == "mem" {
        s *= MEM_BOOST;
    }
    if c.source_kind == "chat" {
        let age = ((q.now - c.updated) as f64 / DAY_MS).max(0.0);
        s *= 0.5f64.powf(age / CHAT_HALF_LIFE_DAYS);
    }
    s
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
pub fn retrieve(c: &Connection, q: &Query) -> Result<Vec<Hit>> {
    retrieve_all(c, q).map(|r| r.hits)
}

/// [`retrieve`], and whether the semantic pass failed. A failed semantic pass does not fail the
/// search: the keyword passes of the Hub's own index still answer.
pub fn retrieve_all(c: &Connection, q: &Query) -> Result<Retrieved> {
    let k = q.k.clamp(1, MAX_K);
    let text = q.text.trim();
    if text.is_empty() {
        return Ok(Retrieved::default());
    }
    let scope = Scope::for_viewer(c, q.viewer, q.purpose, q.kinds)?;
    let found = |pass| -> Result<Vec<Candidate>> {
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

/// The confirmed rules and preferences about any of `anchors`, newest first, independent of a
/// query (prompt layer 6). Same visibility, `never` and paused-source rules as [`retrieve`].
pub fn anchor_mems(
    c: &Connection,
    viewer: &Viewer,
    anchors: &[ObjRef],
    limit: usize,
    now: i64,
) -> Result<Vec<Hit>> {
    if anchors.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }
    if repo::paused(c, &viewer.ws)?
        .iter()
        .any(|(_, k)| k == "mems")
    {
        return Ok(Vec::new());
    }
    let scope = viewer.sql("m");
    let ors = vec!["(a.object_type = ? AND a.object_id = ?)"; anchors.len()].join(" OR ");
    let sql = format!(
        "SELECT m.id FROM mems m
          WHERE m.workspace_id = ? AND m.status = 'confirmed' AND m.level <> 'never'
            AND m.kind IN ('rule', 'preference') AND (m.valid_until IS NULL OR m.valid_until > ?)
            AND {} AND EXISTS (SELECT 1 FROM mem_about a WHERE a.mem_id = m.id AND ({ors}))
          ORDER BY m.updated DESC, m.id LIMIT ?",
        scope.sql
    );
    let mut p = vec![Sql::Text(viewer.ws.clone()), Sql::Integer(now)];
    p.extend(scope.params);
    for a in anchors {
        p.push(Sql::Text(a.kind.clone()));
        p.push(Sql::Text(a.id.clone()));
    }
    p.push(Sql::Integer(limit as i64));
    let ids: Vec<String> = {
        let mut st = c.prepare(&sql)?;
        st.query_map(bind(p), |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let mut hits = Vec::new();
    for id in ids {
        let Some(m) = mems::get(c, &id)? else {
            continue;
        };
        let names = about_names(c, &m.about)?;
        let label = sync::mem_label(&m.kind, &names);
        hits.push(Hit {
            n: hits.len() + 1,
            obj_type: "mem".into(),
            href: format!("#/knowledge?mem={}", m.id),
            obj_id: m.id,
            seq: 0,
            title: sync::mem_title(&label).to_string(),
            heading: String::new(),
            text: if names.is_empty() {
                m.text
            } else {
                format!("{names}：{}", m.text)
            },
            label,
            source_kind: "mems".into(),
            level: m.level,
            visibility: m.visibility,
            score: 0.0,
            about: m.about,
        });
    }
    Ok(hits)
}

/// Writes a `kb_uses` row: which hits a consumer (`session`, `chat`, `draft`, `flow`, `search`) got
/// for which query. Ids and numbers only, never the text. Returns the `ku_…` id.
pub fn record_use(
    tx: &Tx,
    ws: &str,
    consumer: (&str, &str),
    query: &str,
    hits: &[Hit],
) -> Result<String> {
    let id = uid("ku");
    let list: Vec<_> = hits
        .iter()
        .map(|h| {
            json!({"n": h.n, "obj_type": h.obj_type, "obj_id": h.obj_id, "seq": h.seq,
                   "score": h.score, "level": h.level, "label": h.label})
        })
        .collect();
    tx.prepare_cached(
        "INSERT INTO kb_uses (id, workspace_id, consumer_type, consumer_id, query, hits, created) VALUES (?, ?, ?, ?, ?, ?, ?)",
    )?
    .execute(params![
        id,
        ws,
        consumer.0,
        consumer.1,
        query,
        serde_json::Value::Array(list).to_string(),
        steve_store::now()
    ])?;
    Ok(id)
}

/// The knowledge a disclosure basis names (`entries` of `reply_basis` / a mail draft's basis), read again
/// for a later check of the same text: a retrieved piece by its index position (`seq`), a memory shown
/// by id by its id. The flag says whether every entry could be read again; when one cannot (removed
/// since, or a basis written before pieces were recorded) the later check has less than the first one
/// had and must not pass on that ([`steve_modules::outbound::R_BASIS_GONE`]).
pub fn knowledge_again(
    c: &Connection,
    ws: &str,
    entries: &[serde_json::Value],
) -> Result<(Vec<steve_modules::outbound::KnowledgeUse>, bool)> {
    use rusqlite::OptionalExtension;
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
        let found = match e["seq"].as_i64() {
            Some(seq) => c
                .query_row(
                    "SELECT text, level, visibility FROM kb_chunks
                      WHERE workspace_id = ? AND obj_type = ? AND obj_id = ? AND seq = ?",
                    params![ws, kind, id, seq],
                    |r| {
                        Ok(KnowledgeUse {
                            source_type: kind.into(),
                            source_id: id.into(),
                            source_label: label.clone(),
                            level: Level::from_db(&r.get::<_, String>(1)?),
                            visibility: r.get(2)?,
                            text: r.get(0)?,
                            seq: Some(seq),
                        })
                    },
                )
                .optional()?,
            None if kind == "mem" => {
                mems::get(c, id)?
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
                    })
            }
            None => None,
        };
        match found {
            Some(k) => out.push(k),
            None => complete = false,
        }
    }
    Ok((out, complete))
}
