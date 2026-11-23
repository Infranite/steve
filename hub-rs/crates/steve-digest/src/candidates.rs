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

use serde::Serialize;
use serde_json::{Value, json};
use steve_store::base::{ObjRef, Viewer, links::Links};
use steve_store::digest::{Matters, Segments};
use steve_store::{ChannelSources, ItemReads, MailSources, MemoryIndex, Read, StoreError};

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

fn matter(c: &mut impl Read, ws: &str, id: &str) -> Result<Option<(String, String, String)>> {
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
    c: &mut impl Read,
    ws: &str,
    conv: &ConvRef,
    b: &Built,
    now: i64,
) -> Result<Vec<Candidate>> {
    let mut acc = Acc {
        out: BTreeMap::new(),
    };
    let in_window: Vec<&crate::facts::Line> = b.lines.iter().filter(|l| l.pos > 0).collect();
    let first_at = in_window.first().map(|l| l.at).unwrap_or(0);
    add_thread_candidates(c, ws, conv, b, &mut acc)?;
    add_referenced_candidates(c, ws, b, &mut acc)?;
    add_people_candidates(c, ws, b, first_at, &mut acc)?;
    add_fulltext_candidates(c, ws, &in_window, now, &mut acc)?;
    Ok(rank_candidates(acc))
}

fn add_thread_candidates(
    c: &mut impl Read,
    ws: &str,
    conv: &ConvRef,
    b: &Built,
    acc: &mut Acc,
) -> Result<()> {
    let key = conv.key();
    for chain in b.chains.keys() {
        for id in Segments::new(c).matters_by_chains(ws, &key, [chain.as_str()])? {
            if let Some((id, t, s)) = matter(c, ws, &id)? {
                acc.add("matter", &id, &t, Some(s), "same_thread", 3.0);
            }
        }
    }
    let replied: HashSet<String> = b.facts["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| m["reply_to"].as_str().map(str::to_string))
        .collect();
    let replied: Vec<_> = replied.into_iter().collect();
    let locations = match conv.kind.as_str() {
        "space" => ChannelSources(c).message_locations(ws, &conv.id, &replied)?,
        "mail_thread" => MailSources(c).message_locations(ws, &conv.id, &replied)?,
        _ => Vec::new(),
    };
    for (message, at) in locations {
        for id in Segments::new(c).matters_at(ws, &key, at, &message)? {
            if let Some((id, t, s)) = matter(c, ws, &id)? {
                acc.add("matter", &id, &t, Some(s), "same_thread", 2.0);
            }
        }
    }
    Ok(())
}

fn add_referenced_candidates(c: &mut impl Read, ws: &str, b: &Built, acc: &mut Acc) -> Result<()> {
    // referenced items and the matters linked to them
    for no in &b.items {
        let title = ItemReads(c).title(ws, no)?.unwrap_or_default();
        acc.add("item", no, &title, None, "referenced", 3.0);
        for link in Links(c).to(ws, &ObjRef::new("item", no), None)? {
            if link.subject_type != "matter" {
                continue;
            }
            let id = link.subject_id;
            if let Some((id, t, s)) = matter(c, ws, &id)? {
                acc.add("matter", &id, &t, Some(s), "referenced", 2.0);
            }
        }
    }
    Ok(())
}

fn add_people_candidates(
    c: &mut impl Read,
    ws: &str,
    b: &Built,
    first_at: i64,
    acc: &mut Acc,
) -> Result<()> {
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
        let overlap = Matters(c).recent_overlap(
            ws,
            &people,
            first_at.saturating_sub(NEAR_MS),
            first_at.saturating_add(NEAR_MS),
        )?;
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
    Ok(())
}

fn add_fulltext_candidates(
    c: &mut impl Read,
    ws: &str,
    in_window: &[&crate::facts::Line],
    now: i64,
    acc: &mut Acc,
) -> Result<()> {
    // full text: matters and memories
    let text: String = in_window
        .iter()
        .map(|l| l.body.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    if let Some(expr) = match_expr(&salient(&text)) {
        for (id, s) in Matters(c).search(ws, &expr, &["open", "done"], MAX_EACH)? {
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
        add_memory_candidates(c, ws, expr, now, acc)?;
    }
    Ok(())
}

fn rank_candidates(acc: Acc) -> Vec<Candidate> {
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
    all
}

fn add_memory_candidates(
    c: &mut impl Read,
    ws: &str,
    expr: String,
    now: i64,
    acc: &mut Acc,
) -> Result<()> {
    let viewer = crate::conv::owner_viewer(c, ws)?.unwrap_or_else(|| Viewer::bare(ws, ""));
    for (memory, score) in MemoryIndex::new(c).search(&viewer, &expr, now, MAX_EACH as u64)? {
        acc.add(
            "mem",
            &memory.id,
            &crate::cut(&memory.text, 120),
            Some(memory.status),
            "fulltext",
            (-score).clamp(0.0, 10.0) / 10.0,
        );
    }
    Ok(())
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
