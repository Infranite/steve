//! Lexical baseline on the evaluation corpus: `seg` bigrams in the
//! `kb_fts` table, `fts_query`, `bm25(kb_fts, 1.0, 2.0)`, best chunk per document. No anchor,
//! memory or recency weighting — `retrieve` must score at least this.
//!
//! `cargo test -p steve-knowledge --test eval_fts -- --nocapture` prints recall@k and misses.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

/// Minimum chunks expected from the synthetic corpus (80 docs; currently 156 chunks after
/// replacing the oversized internal wiki proposals with compact neutral fixtures).
const MIN_CORPUS_CHUNKS: usize = 140;
const MIN_EVAL_CASES: usize = 40;
const MIN_RECALL_AT_5: f64 = 0.8;

use steve_knowledge::eval::{evaluate, load_cases, load_corpus};
use steve_knowledge::seg::{fts_query, fts_query_relaxed, seg};
use steve_store::testkit::knowledge::FtsFixture;

fn eval_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/eval")
}

#[test]
fn recall_on_eval_corpus() {
    let docs = load_corpus(&eval_dir().join("corpus")).unwrap();
    let cases = load_cases(&eval_dir().join("cases.json")).unwrap();

    let fts = FtsFixture::new().unwrap();
    let mut owner: HashMap<i64, String> = HashMap::new();
    let mut rowid = 0i64;
    for d in &docs {
        for c in &d.chunks {
            rowid += 1;
            fts.put(rowid, &seg(&c.index_text()), &seg(&d.title))
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
        chunks >= MIN_CORPUS_CHUNKS,
        "corpus should have at least {MIN_CORPUS_CHUNKS} chunks, got {chunks}"
    );
    assert!(cases.len() >= MIN_EVAL_CASES);
    for c in &cases {
        for e in &c.expect {
            assert!(
                docs.iter().any(|d| &d.r#ref == e),
                "{}: unknown ref {e}",
                c.id
            );
        }
    }

    let run = |expr: Option<String>| -> Vec<String> {
        let Some(expr) = expr else {
            return Vec::new();
        };
        fts.search(&expr)
            .unwrap()
            .into_iter()
            .map(|id| owner[&id].clone())
            .collect()
    };
    // Strict first; when it yields fewer than 5 documents, append a relaxed pass
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
    eprintln!("strict: {r5}");
    let f5 = evaluate(&cases, 5, with_fallback);
    eprintln!("strict + relaxed fallback: {f5}");
    assert!(
        r5.recall >= MIN_RECALL_AT_5,
        "recall@5 {:.3} below the bar {MIN_RECALL_AT_5}",
        r5.recall
    );
    assert!(f5.recall >= r5.recall);
}
