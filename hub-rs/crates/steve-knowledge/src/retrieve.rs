//! Retrieval (knowledge.md §3.2–3.3): FTS candidates filtered by workspace, visibility, level and
//! paused sources in SQL, then re-ranked, deduplicated, numbered and given citation links.
//!
//! Ranking: `bm25(kb_fts, 1.0, 2.0)` (title weight 2), then ×1.5 when the hit is about one of the
//! anchors, ×1.2 for memories (short and precise), and a 30-day half-life for the main-thread
//! chat source. The strict query (§3.2: short CJK runs as phrases) goes first; when it finds fewer
//! than 5 documents, the relaxed query (every bigram on its own, so typos still match) fills in
//! after it — K0's evaluation showed this lifts recall@5 without hurting the strict ranking.
//!
//! Visibility and `never` are filtered only here and in the list functions (§11): callers never
//! build their own conditions.

use std::collections::{HashMap, HashSet};

use rusqlite::types::Value as Sql;
use rusqlite::{Connection, params};
use serde::Serialize;
use serde_json::json;
use steve_store::base::mems;
use steve_store::base::{Level, ObjRef, Viewer, Visibility, bind};
use steve_store::{Result, Tx, uid};

use crate::repo;
use crate::seg::{fts_query, fts_query_relaxed, is_single_cjk};
use crate::sync::about_names;

/// FTS candidates per pass (§3.3 step 1).
pub const CANDIDATES: usize = 50;
/// Upper bound of `k`.
pub const MAX_K: usize = 20;
/// The strict pass is topped up by the relaxed one below this many distinct documents.
pub const FALLBACK_BELOW: usize = 5;
/// Chunks of one document in one result list (§3.3 step 4).
pub const PER_DOC: usize = 2;

const ANCHOR_BOOST: f64 = 1.5;
const MEM_BOOST: f64 = 1.2;
const CHAT_HALF_LIFE_DAYS: f64 = 30.0;
const DAY_MS: f64 = 86_400_000.0;

/// Who is asking, which decides extra filters (the main-thread rule of §3.3).
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
#[derive(Debug, Clone)]
pub struct Query<'a> {
    /// Workspace and user the results are filtered for.
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
    pub fn new(viewer: &'a Viewer, text: &'a str, purpose: Purpose) -> Self {
        Self {
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

/// One numbered result. Serializes as the API `Hit` of §6.1.
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

/// A candidate row before ranking.
#[derive(Debug, Clone)]
struct Cand {
    obj_type: String,
    obj_id: String,
    seq: i64,
    heading: String,
    text: String,
    source_kind: String,
    label: String,
    level: Level,
    visibility: Visibility,
    updated: i64,
    base: f64,
    title: Option<String>,
    doc_ref: Option<String>,
}

const COLS: &str = "c.obj_type, c.obj_id, c.seq, c.heading, c.text, c.source_kind, c.label, c.level, c.visibility, c.updated";

fn cand(r: &rusqlite::Row, base: Option<f64>) -> rusqlite::Result<Cand> {
    Ok(Cand {
        obj_type: r.get(0)?,
        obj_id: r.get(1)?,
        seq: r.get(2)?,
        heading: r.get(3)?,
        text: r.get(4)?,
        source_kind: r.get(5)?,
        label: r.get(6)?,
        level: Level::from_db(&r.get::<_, String>(7)?),
        visibility: Visibility::from_db(&r.get::<_, String>(8)?),
        updated: r.get(9)?,
        title: r.get(10)?,
        doc_ref: r.get(11)?,
        // bm25 is negative, smaller = better
        base: match base {
            Some(b) => b,
            None => -r.get::<_, f64>(12)?,
        },
    })
}

/// The shared `WHERE` tail on `kb_chunks c`: workspace, never, visibility, purpose, kinds, paused.
fn filters(c: &Connection, q: &Query) -> Result<(String, Vec<Sql>)> {
    let v = q.viewer;
    let scope = v.sql("c");
    let mut sql = format!(
        "c.workspace_id = ? AND c.level <> 'never' AND {}",
        scope.sql
    );
    let mut p = vec![Sql::Text(v.ws.clone())];
    p.extend(scope.params);
    if q.purpose == Purpose::Chat && repo::member_count(c, &v.ws)? > 1 {
        sql.push_str(" AND c.visibility = 'workspace'");
    }
    if let Some(kinds) = q.kinds {
        if kinds.is_empty() {
            sql.push_str(" AND 0");
        } else {
            sql.push_str(&format!(
                " AND c.source_kind IN ({})",
                vec!["?"; kinds.len()].join(", ")
            ));
            p.extend(kinds.iter().map(|k| Sql::Text((*k).into())));
        }
    }
    let paused = repo::paused(c, &v.ws)?;
    let mut docs = Vec::new();
    for (id, kind) in paused {
        match kind.as_str() {
            "mems" => sql.push_str(" AND c.obj_type <> 'mem'"),
            "bot_mems" => sql.push_str(" AND c.obj_type <> 'bot_mem'"),
            _ => docs.push(id),
        }
    }
    if !docs.is_empty() {
        sql.push_str(&format!(
            " AND NOT (c.obj_type = 'chunk' AND c.obj_id IN (SELECT id FROM kb_docs WHERE source_id IN ({})))",
            vec!["?"; docs.len()].join(", ")
        ));
        p.extend(docs.into_iter().map(Sql::Text));
    }
    Ok((sql, p))
}

fn fts(c: &Connection, expr: &str, tail: &(String, Vec<Sql>)) -> Result<Vec<Cand>> {
    let sql = format!(
        "SELECT {COLS}, d.title, d.ref, bm25(kb_fts, 1.0, 2.0) AS r
           FROM kb_fts JOIN kb_chunks c ON c.id = kb_fts.rowid
           LEFT JOIN kb_docs d ON c.obj_type = 'chunk' AND d.id = c.obj_id
          WHERE kb_fts MATCH ? AND {} ORDER BY r LIMIT {CANDIDATES}",
        tail.0
    );
    let mut p = vec![Sql::Text(expr.into())];
    p.extend(tail.1.iter().cloned());
    let mut st = c.prepare(&sql)?;
    let rows = st.query_map(bind(p), |r| cand(r, None))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// A one-character query: `LIKE`-style match on document titles and memory text (§3.2).
fn like(c: &Connection, ch: &str, tail: &(String, Vec<Sql>)) -> Result<Vec<Cand>> {
    let sql = format!(
        "SELECT {COLS}, d.title, d.ref FROM kb_chunks c
           LEFT JOIN kb_docs d ON c.obj_type = 'chunk' AND d.id = c.obj_id
          WHERE ((c.obj_type IN ('mem', 'bot_mem') AND instr(c.text, ?) > 0)
              OR (c.obj_type = 'chunk' AND c.seq = 0 AND instr(d.title, ?) > 0))
            AND {} ORDER BY c.updated DESC LIMIT {CANDIDATES}",
        tail.0
    );
    let mut p = vec![Sql::Text(ch.into()), Sql::Text(ch.into())];
    p.extend(tail.1.iter().cloned());
    let mut st = c.prepare(&sql)?;
    let rows = st.query_map(bind(p), |r| cand(r, Some(1.0)))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
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

fn href(c: &Cand, about: &[ObjRef]) -> String {
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

fn title(c: &Cand) -> String {
    match c.obj_type.as_str() {
        "chunk" => c.title.clone().unwrap_or_default(),
        "mem" => c
            .label
            .strip_prefix("记忆 · ")
            .unwrap_or(&c.label)
            .to_string(),
        _ => c.label.clone(),
    }
}

/// Boosted score of a candidate (§3.3 step 3).
fn boosted(c: &Cand, about: &[ObjRef], q: &Query) -> f64 {
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

/// Searches the knowledge base for `q.viewer` (§3.3). Empty or unsearchable text gives no hits.
pub fn retrieve(c: &Connection, q: &Query) -> Result<Vec<Hit>> {
    let k = q.k.clamp(1, MAX_K);
    let text = q.text.trim();
    if text.is_empty() {
        return Ok(Vec::new());
    }
    let tail = filters(c, q)?;
    let mut tiers: Vec<Vec<Cand>> = Vec::new();
    if is_single_cjk(text) {
        tiers.push(like(c, text, &tail)?);
    } else {
        let Some(strict) = fts_query(text) else {
            return Ok(Vec::new());
        };
        let first = fts(c, &strict, &tail)?;
        let docs: HashSet<&str> = first.iter().map(|x| x.obj_id.as_str()).collect();
        let short = docs.len() < FALLBACK_BELOW;
        tiers.push(first);
        if short && let Some(relaxed) = fts_query_relaxed(text).filter(|r| *r != strict) {
            tiers.push(fts(c, &relaxed, &tail)?);
        }
    }

    // Rank each tier on its own (bm25 values of different queries do not compare), then concatenate.
    let mut about_cache: HashMap<(String, String), Vec<ObjRef>> = HashMap::new();
    let mut ranked: Vec<(Cand, f64)> = Vec::new();
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

    // Dedup (§3.3 step 4), cut, number.
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
    Ok(hits)
}

/// The confirmed rules and preferences about any of `anchors`, newest first, independent of a
/// query (prompt layer 6, §4.1). Same visibility, `never` and paused-source rules as [`retrieve`].
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
        let label = if names.is_empty() {
            "记忆".to_string()
        } else {
            format!("记忆 · {names}")
        };
        hits.push(Hit {
            n: hits.len() + 1,
            obj_type: "mem".into(),
            href: format!("#/knowledge?mem={}", m.id),
            obj_id: m.id,
            seq: 0,
            title: if names.is_empty() {
                "记忆".into()
            } else {
                names.clone()
            },
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
/// for which query. Ids and numbers only, never the text (§11). Returns the `ku_…` id.
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
