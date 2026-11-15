//! What a window may belong to, by deterministic signals only. Each candidate says why it was found:
//!
//! - `same_thread`: a matter already holds a segment of the same reply chain or topic, or the message a
//!   window message replies to;
//! - `referenced`: an item the window names (and the matters linked to it);
//! - `same_people`: a matter with the same people (two or more, or the one other person of a DM), active in
//!   the last [`NEAR_MS`];
//! - `fulltext`: matters and memories the window's salient words find in the full-text index.
//!
//! The digest Bot decides; the candidates only save it the search, and every id it writes back must be one of
//! these or come from a tool's answer.

use std::collections::{BTreeMap, HashMap, HashSet};

use rusqlite::{Connection, params};
use serde::Serialize;
use serde_json::{Value, json};
use steve_store::StoreError;

use crate::conv::ConvRef;
use crate::facts::Built;

type Result<T> = std::result::Result<T, StoreError>;

/// How recent a matter must be to count as close in time.
pub const NEAR_MS: i64 = 14 * 24 * 3600 * 1000;
/// The most candidates of each type.
pub const MAX_EACH: usize = 6;
/// The most salient words a full-text search uses.
const MAX_TERMS: usize = 12;

/// Common words that say nothing about a matter.
const STOP: [&str; 48] = [
    "我们", "你们", "他们", "一下", "这个", "那个", "今天", "明天", "可以", "没有", "就是", "什么",
    "大家", "一个", "现在", "还是", "已经", "不是", "然后", "因为", "如果", "所以", "这样", "需要",
    "时候", "我的", "的话", "知道", "看看", "问题", "好的", "收到", "谢谢", "一起", "这边", "那边",
    "是不", "不要", "要不", "下午", "上午", "晚上", "http", "https", "com", "www", "the", "and",
];

#[derive(Debug, Clone, Serialize)]
pub struct Candidate {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub id: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    pub reasons: Vec<&'static str>,
    pub score: f64,
}

struct Acc {
    out: BTreeMap<(&'static str, String), Candidate>,
}

impl Acc {
    fn add(
        &mut self,
        kind: &'static str,
        id: &str,
        title: &str,
        state: Option<String>,
        why: &'static str,
        score: f64,
    ) {
        let e = self
            .out
            .entry((kind, id.to_string()))
            .or_insert_with(|| Candidate {
                kind,
                id: id.to_string(),
                title: title.to_string(),
                state,
                reasons: Vec::new(),
                score: 0.0,
            });
        if !e.reasons.contains(&why) {
            e.reasons.push(why);
        }
        e.score += score;
    }
}

fn matter(c: &Connection, ws: &str, id: &str) -> Result<Option<(String, String, String)>> {
    crate::matter::resolve(c, ws, id)
}

/// Salient words of a text: CJK bigrams and words of 3+ characters that occur at least twice, most frequent
/// first (ties by the word).
pub fn salient(text: &str) -> Vec<String> {
    let mut freq: HashMap<String, usize> = HashMap::new();
    let cleaned: String = text
        .split_whitespace()
        .filter(|w| !w.starts_with("http"))
        .collect::<Vec<_>>()
        .join(" ");
    for t in steve_knowledge::seg::seg(&cleaned).split(|c: char| !c.is_alphanumeric()) {
        let n = t.chars().count();
        let cjk = t.chars().all(steve_knowledge::seg::is_cjk);
        if (cjk && n == 2) || (!cjk && n >= 3 && !t.chars().all(|c| c.is_ascii_digit())) {
            if STOP.contains(&t) {
                continue;
            }
            *freq.entry(t.to_string()).or_default() += 1;
        }
    }
    let mut v: Vec<(String, usize)> = freq.into_iter().filter(|(_, n)| *n >= 2).collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    v.into_iter().take(MAX_TERMS).map(|x| x.0).collect()
}

fn match_expr(terms: &[String]) -> Option<String> {
    (!terms.is_empty()).then(|| {
        terms
            .iter()
            .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" OR ")
    })
}

/// The candidates of a built window of `conv`.
pub fn find(
    c: &Connection,
    ws: &str,
    conv: &ConvRef,
    b: &Built,
    now: i64,
) -> Result<Vec<Candidate>> {
    let mut acc = Acc {
        out: BTreeMap::new(),
    };
    let key = conv.key();
    let in_window: Vec<&crate::facts::Line> = b.lines.iter().filter(|l| l.pos > 0).collect();
    let first_at = in_window.first().map(|l| l.at).unwrap_or(0);

    // same reply chain or topic: segments of this conversation sharing a chain
    if !b.chains.is_empty() {
        let mut st = c.prepare_cached(
            "SELECT DISTINCT s.matter_id FROM digest_segments s, json_each(s.chains) j
              WHERE s.workspace_id = ? AND s.conv = ? AND s.matter_id IS NOT NULL AND j.value = ?",
        )?;
        for ch in b.chains.keys() {
            let ids: Vec<String> = st
                .query_map(params![ws, key, ch], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            for id in ids {
                if let Some((id, t, s)) = matter(c, ws, &id)? {
                    acc.add("matter", &id, &t, Some(s), "same_thread", 3.0);
                }
            }
        }
    }
    // the messages replied to outside the window: the segments holding them
    let replied: HashSet<String> = b.facts["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| m["reply_to"].as_str().map(str::to_string))
        .collect();
    if !replied.is_empty() {
        let mut at_of = c.prepare_cached(match conv.kind.as_str() {
            "mail_thread" => "SELECT date FROM mail_messages WHERE id = ?",
            _ => "SELECT created FROM chan_messages WHERE id = ?",
        })?;
        let mut seg = c.prepare_cached(
            "SELECT matter_id FROM digest_segments WHERE workspace_id = ? AND conv = ? AND matter_id IS NOT NULL
               AND (first_at, first_id) <= (?3, ?4) AND (last_at, last_id) >= (?3, ?4)",
        )?;
        for r in &replied {
            let at: Option<i64> = at_of.query_row([r], |x| x.get(0)).ok();
            if let Some(at) = at {
                let ids: Vec<String> = seg
                    .query_map(params![ws, key, at, r], |x| x.get(0))?
                    .collect::<rusqlite::Result<_>>()?;
                for id in ids {
                    if let Some((id, t, s)) = matter(c, ws, &id)? {
                        acc.add("matter", &id, &t, Some(s), "same_thread", 2.0);
                    }
                }
            }
        }
    }

    // referenced items and the matters linked to them
    for no in &b.items {
        let title: String = c
            .prepare_cached("SELECT title FROM items WHERE workspace_id = ? AND id = ?")?
            .query_row(params![ws, no], |r| r.get(0))
            .unwrap_or_default();
        acc.add("item", no, &title, None, "referenced", 3.0);
        let ids: Vec<String> = c
            .prepare_cached(
                "SELECT subject_id FROM links WHERE workspace_id = ? AND subject_type = 'matter' AND object_type = 'item' AND object_id = ?",
            )?
            .query_map(params![ws, no], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        for id in ids {
            if let Some((id, t, s)) = matter(c, ws, &id)? {
                acc.add("matter", &id, &t, Some(s), "referenced", 2.0);
            }
        }
    }

    // the same people, close in time (bots and you do not count)
    let people: Vec<&str> = b
        .parties
        .iter()
        .filter(|p| !p.bot && !p.mine)
        .map(|p| p.id.as_str())
        .collect();
    if !people.is_empty() {
        let need = if people.len() == 1 && b.facts["conv"]["kind"] == "dm" {
            1
        } else {
            2
        };
        let mut overlap: HashMap<String, usize> = HashMap::new();
        let mut st = c.prepare_cached(
            "SELECT mp.matter_id FROM matter_parties mp JOIN matters m ON m.id = mp.matter_id
              WHERE m.workspace_id = ? AND mp.party_id = ? AND m.state = 'open' AND coalesce(m.last_at, m.updated) BETWEEN ? AND ?",
        )?;
        for p in &people {
            let ids: Vec<String> = st
                .query_map(
                    params![
                        ws,
                        p,
                        first_at.saturating_sub(NEAR_MS),
                        first_at.saturating_add(NEAR_MS)
                    ],
                    |r| r.get(0),
                )?
                .collect::<rusqlite::Result<_>>()?;
            for id in ids {
                *overlap.entry(id).or_default() += 1;
            }
        }
        for (id, n) in overlap {
            if n >= need
                && let Some((id, t, s)) = matter(c, ws, &id)?
            {
                acc.add(
                    "matter",
                    &id,
                    &t,
                    Some(s),
                    "same_people",
                    n as f64 / people.len() as f64,
                );
            }
        }
    }

    // full text: matters and memories
    let text: String = in_window
        .iter()
        .map(|l| l.body.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    if let Some(expr) = match_expr(&salient(&text)) {
        let rows: Vec<(String, f64)> = c
            .prepare_cached(
                "SELECT r.matter_id, bm25(matters_fts) AS s FROM matters_fts JOIN matters_fts_rows r ON r.row = matters_fts.rowid
                  JOIN matters m ON m.id = r.matter_id
                  WHERE matters_fts MATCH ? AND m.workspace_id = ? AND m.state IN ('open', 'done') ORDER BY s LIMIT 6",
            )?
            .query_map(params![expr, ws], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        for (id, s) in rows {
            if let Some((id, t, st)) = matter(c, ws, &id)? {
                acc.add(
                    "matter",
                    &id,
                    &t,
                    Some(st),
                    "fulltext",
                    (-s).clamp(0.0, 10.0) / 10.0,
                );
            }
        }
        let rows: Vec<(String, String, String, f64)> = c
            .prepare_cached(
                "SELECT m.id, m.text, m.status, bm25(kb_fts) AS s FROM kb_fts JOIN kb_chunks k ON k.id = kb_fts.rowid
                   JOIN mems m ON m.id = k.obj_id
                  WHERE kb_fts MATCH ? AND k.obj_type = 'mem' AND m.workspace_id = ? AND m.status IN ('confirmed', 'candidate') AND m.level <> 'never' AND (m.valid_until IS NULL OR m.valid_until > ?3) AND (m.visibility = 'workspace' OR m.created_by = ?4 OR m.visibility IN (SELECT value FROM json_each(?5)))
                  ORDER BY s LIMIT 6",
            )?
            .query_map(params![expr, ws, now, crate::conv::owner(c, ws)?.unwrap_or_default(), serde_json::to_string(&crate::conv::owner_viewer(c, ws)?.map(|v| v.scopes).unwrap_or_default()).unwrap_or_default()], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<rusqlite::Result<_>>()?;
        for (id, text, status, s) in rows {
            acc.add(
                "mem",
                &id,
                &crate::cut(&text, 120),
                Some(status),
                "fulltext",
                (-s).clamp(0.0, 10.0) / 10.0,
            );
        }
    }

    let mut all: Vec<Candidate> = acc.out.into_values().collect();
    all.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.id.cmp(&b.id))
    });
    let mut per: HashMap<&'static str, usize> = HashMap::new();
    all.retain(|x| {
        let n = per.entry(x.kind).or_default();
        *n += 1;
        *n <= MAX_EACH
    });
    for x in &mut all {
        x.score = (x.score * 100.0).round() / 100.0;
    }
    Ok(all)
}

/// Candidates as stored in a packet.
pub fn json(xs: &[Candidate]) -> Value {
    json!(xs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn salient_words_repeat() {
        let t = "部署脚本今天要改。部署脚本的回滚也要改。deploy deploy ok";
        let s = salient(t);
        assert!(s.contains(&"部署".to_string()), "{s:?}");
        assert!(s.contains(&"脚本".to_string()));
        assert!(s.contains(&"deploy".to_string()));
        assert!(!s.contains(&"ok".to_string()));
        assert!(!s.contains(&"今天".to_string()));
    }
}
