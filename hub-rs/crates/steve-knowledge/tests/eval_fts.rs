//! Lexical baseline on the evaluation corpus (knowledge.md §3.5): `seg` bigrams in the spec's
//! `kb_fts` table, `fts_query`, `bm25(kb_fts, 1.0, 2.0)`, best chunk per document. No anchor,
//! memory or recency weighting — that is `retrieve` (K1), which must score at least this.
//!
//! `cargo test -p steve-knowledge --test eval_fts -- --nocapture` prints recall@k and misses.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use rusqlite::{Connection, params};
use steve_knowledge::eval::{evaluate, load_cases, load_corpus};
use steve_knowledge::seg::{fts_query, fts_query_relaxed, seg};

const KB_FTS: &str = "CREATE VIRTUAL TABLE IF NOT EXISTS kb_fts USING fts5(
  seg, title, tokenize = 'unicode61 remove_diacritics 2', content = '', contentless_delete = 1);";

fn eval_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/eval")
}

#[test]
fn recall_on_eval_corpus() {
    let docs = load_corpus(&eval_dir().join("corpus")).unwrap();
    let cases = load_cases(&eval_dir().join("cases.json")).unwrap();

    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(KB_FTS).unwrap();
    let mut owner: HashMap<i64, String> = HashMap::new();
    let mut rowid = 0i64;
    for d in &docs {
        for c in &d.chunks {
            rowid += 1;
            conn.execute(
                "INSERT INTO kb_fts(rowid, seg, title) VALUES (?1, ?2, ?3)",
                params![rowid, seg(&c.index_text()), seg(&d.title)],
            )
            .unwrap();
            owner.insert(rowid, d.r#ref.clone());
        }
    }
    let chunks = rowid as usize;
    eprintln!(
        "corpus: {} docs, {chunks} chunks; {} cases",
        docs.len(),
        cases.len()
    );
    assert!(
        chunks >= 180,
        "corpus should be about 200 chunks, got {chunks}"
    );
    assert!(cases.len() >= 40);
    for c in &cases {
        for e in &c.expect {
            assert!(
                docs.iter().any(|d| &d.r#ref == e),
                "{}: unknown ref {e}",
                c.id
            );
        }
    }

    let mut st = conn
        .prepare("SELECT rowid FROM kb_fts WHERE kb_fts MATCH ?1 ORDER BY bm25(kb_fts, 1.0, 2.0) LIMIT 50")
        .unwrap();
    let mut run = |expr: Option<String>| -> Vec<String> {
        let Some(expr) = expr else {
            return Vec::new();
        };
        st.query_map([expr], |r| r.get::<_, i64>(0))
            .unwrap()
            .map(|id| owner[&id.unwrap()].clone())
            .collect()
    };
    // Strict (§3.2) first; when it yields fewer than 5 documents, append a relaxed pass
    // (bigrams without phrases) so short queries with a typo still find something.
    let mut ranked: HashMap<String, (Vec<String>, Vec<String>)> = HashMap::new();
    for c in &cases {
        ranked.insert(
            c.query.clone(),
            (run(fts_query(&c.query)), run(fts_query_relaxed(&c.query))),
        );
    }
    let strict = |q: &str| ranked[q].0.clone();
    let with_fallback = |q: &str| {
        let (s, r) = &ranked[q];
        let mut out = s.clone();
        if s.iter().collect::<HashSet<_>>().len() < 5 {
            out.extend(r.iter().cloned());
        }
        out
    };

    for k in [1, 10] {
        let r = evaluate(&cases, k, strict);
        eprintln!("strict: recall@{k}={:.3} hit@{k}={:.3}", r.recall, r.hit);
    }
    let r5 = evaluate(&cases, 5, strict);
    eprintln!("strict (§3.2): {r5}");
    let f5 = evaluate(&cases, 5, with_fallback);
    eprintln!("strict + relaxed fallback: {f5}");
    assert!(
        r5.recall >= 0.8,
        "recall@5 {:.3} below the phase-1 bar 0.8",
        r5.recall
    );
    assert!(f5.recall >= r5.recall);
}
