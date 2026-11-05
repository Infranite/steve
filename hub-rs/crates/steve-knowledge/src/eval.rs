//! Retrieval evaluation: a fixed corpus, queries with expected sources,
//! and recall@k / MRR. The corpus lives in `tests/eval/`; this module loads it with the same
//! extraction and chunking the index uses, and scores any search function, so the FTS baseline
//! here (K0) and the full `retrieve` (K1) and vector variants (K6) are measured the same way.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

use crate::chunk::{Chunk, TextKind, chunk};
use crate::extract_text::markdown;

/// One evaluation query.
#[derive(Debug, Clone, Deserialize)]
pub struct Case {
    pub id: String,
    pub query: String,
    /// Query style: 两字词, 长句, 英文缩写, 同义说法, 人名, 错别字 …
    pub kind: String,
    /// Corpus refs (`notes/acme.md`, `mem:mm_07`) any of which answers the query.
    pub expect: Vec<String>,
}

/// One corpus document, chunked.
#[derive(Debug, Clone)]
pub struct Doc {
    /// Path relative to the corpus root, or `mem:<id>` for memories.
    pub r#ref: String,
    pub title: String,
    /// Source kind it stands in for: `notes`, `folder`, `items`, `mems`.
    pub source_kind: String,
    pub chunks: Vec<Chunk>,
}

#[derive(Deserialize)]
struct MemLine {
    id: String,
    #[serde(default)]
    about: Option<String>,
    text: String,
}

/// Corpus subdirectory → the source kind it simulates.
const DIRS: [(&str, &str); 3] = [("notes", "notes"), ("wiki", "folder"), ("items", "items")];

/// Loads `<dir>/{notes,wiki,items}/*.md` and `<dir>/mems.jsonl`.
///
/// Titles come from the front matter, else the first `# ` heading, else the file name.
/// A memory becomes one chunk “关于对象：正文”, as the `mems` source indexes it.
pub fn load_corpus(dir: &Path) -> std::io::Result<Vec<Doc>> {
    let mut docs = Vec::new();
    for (sub, kind) in DIRS {
        let path = dir.join(sub);
        if !path.is_dir() {
            continue;
        }
        let mut files: Vec<_> = std::fs::read_dir(&path)?
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "md"))
            .collect();
        files.sort();
        for file in files {
            let src = std::fs::read_to_string(&file)?;
            let e = markdown(&src);
            let name = file
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let title = e
                .front
                .title
                .clone()
                .or_else(|| {
                    e.text
                        .lines()
                        .find_map(|l| l.strip_prefix("# ").map(|t| t.trim().to_string()))
                })
                .unwrap_or(name.clone());
            docs.push(Doc {
                r#ref: format!("{sub}/{name}.md"),
                title,
                source_kind: kind.to_string(),
                chunks: chunk(&e.text, TextKind::Markdown).chunks,
            });
        }
    }
    let mems = dir.join("mems.jsonl");
    if mems.is_file() {
        for (i, line) in std::fs::read_to_string(mems)?.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let m: MemLine = serde_json::from_str(line).map_err(|e| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("mems.jsonl:{}: {e}", i + 1),
                )
            })?;
            let text = match &m.about {
                Some(a) => format!("{a}：{}", m.text),
                None => m.text.clone(),
            };
            let len = text.chars().count();
            docs.push(Doc {
                r#ref: format!("mem:{}", m.id),
                title: String::new(),
                source_kind: "mems".into(),
                chunks: vec![Chunk {
                    seq: 0,
                    heading: String::new(),
                    text,
                    start: 0,
                    end: len,
                }],
            });
        }
    }
    Ok(docs)
}

/// Loads `cases.json` (an array of [`Case`]).
pub fn load_cases(path: &Path) -> std::io::Result<Vec<Case>> {
    let raw = std::fs::read_to_string(path)?;
    serde_json::from_str(&raw).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// A case whose expected sources were not all in the top k.
#[derive(Debug, Clone)]
pub struct Miss {
    pub id: String,
    pub query: String,
    pub expect: Vec<String>,
    pub got: Vec<String>,
}

/// Scores of one run.
#[derive(Debug, Clone)]
pub struct Report {
    pub k: usize,
    pub cases: usize,
    /// Mean over cases of |expected ∩ top k| / |expected|.
    pub recall: f64,
    /// Share of cases with at least one expected ref in the top k.
    pub hit: f64,
    /// Mean reciprocal rank of the first expected ref (0 when absent from the ranking).
    pub mrr: f64,
    /// Per query kind: (cases, recall@k).
    pub by_kind: BTreeMap<String, (usize, f64)>,
    pub misses: Vec<Miss>,
}

impl std::fmt::Display for Report {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(
            f,
            "cases={} recall@{k}={:.3} hit@{k}={:.3} MRR={:.3}",
            self.cases,
            self.recall,
            self.hit,
            self.mrr,
            k = self.k
        )?;
        for (kind, (n, r)) in &self.by_kind {
            writeln!(f, "  {kind}: n={n} recall@{}={r:.3}", self.k)?;
        }
        for m in &self.misses {
            writeln!(
                f,
                "  miss {} 「{}」 expect {:?} got {:?}",
                m.id, m.query, m.expect, m.got
            )?;
        }
        Ok(())
    }
}

/// Runs every case through `search`, which returns doc refs best first (duplicates are
/// ignored after their first position), and scores the top `k`.
pub fn evaluate(cases: &[Case], k: usize, mut search: impl FnMut(&str) -> Vec<String>) -> Report {
    let mut recall_sum = 0.0;
    let mut rr_sum = 0.0;
    let mut hits = 0usize;
    let mut by_kind: BTreeMap<String, (usize, f64)> = BTreeMap::new();
    let mut misses = Vec::new();
    for case in cases {
        let mut ranked: Vec<String> = Vec::new();
        for r in search(&case.query) {
            if !ranked.contains(&r) {
                ranked.push(r);
            }
        }
        let top = &ranked[..ranked.len().min(k)];
        let found = case.expect.iter().filter(|e| top.contains(e)).count();
        let recall = if case.expect.is_empty() {
            1.0
        } else {
            found as f64 / case.expect.len() as f64
        };
        let rr = ranked
            .iter()
            .position(|r| case.expect.contains(r))
            .map_or(0.0, |p| 1.0 / (p + 1) as f64);
        recall_sum += recall;
        hits += usize::from(found > 0 || case.expect.is_empty());
        rr_sum += rr;
        let e = by_kind.entry(case.kind.clone()).or_default();
        e.0 += 1;
        e.1 += recall;
        if found < case.expect.len() {
            misses.push(Miss {
                id: case.id.clone(),
                query: case.query.clone(),
                expect: case.expect.clone(),
                got: top.to_vec(),
            });
        }
    }
    let n = cases.len().max(1) as f64;
    for v in by_kind.values_mut() {
        v.1 /= v.0 as f64;
    }
    Report {
        k,
        cases: cases.len(),
        recall: recall_sum / n,
        hit: hits as f64 / n,
        mrr: rr_sum / n,
        by_kind,
        misses,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn case(id: &str, expect: &[&str]) -> Case {
        Case {
            id: id.into(),
            query: id.into(),
            kind: "t".into(),
            expect: expect.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn scores() {
        let cases = [case("a", &["x"]), case("b", &["y", "z"]), case("c", &["w"])];
        let r = evaluate(&cases, 2, |q| match q {
            "a" => vec!["x".into(), "x".into(), "q".into()],
            "b" => vec!["q".into(), "y".into(), "z".into()],
            _ => vec!["q".into(), "r".into(), "w".into()],
        });
        // a: 1, b: 1/2, c: 0 (w at rank 3 > k).
        assert!((r.recall - 0.5).abs() < 1e-9);
        // a: 1, b: 1/2, c: 1/3.
        assert!((r.mrr - (1.0 + 0.5 + 1.0 / 3.0) / 3.0).abs() < 1e-9);
        assert!((r.hit - 2.0 / 3.0).abs() < 1e-9);
        assert_eq!(r.misses.len(), 2);
    }
}
