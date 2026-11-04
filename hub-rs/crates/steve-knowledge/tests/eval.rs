//! The evaluation corpus (knowledge.md §3.5) through the real index and [`retrieve`]: documents
//! written with `index::replace`, memories through `mems` + `sync_mem`, queries as `Purpose::Search`
//! with ranking boosts and the relaxed fallback. Phase-1 bar: recall@5 ≥ 0.8.
//!
//! `cargo test -p steve-knowledge --test eval -- --nocapture` prints recall@k, MRR and misses.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use serde_json::json;
use steve_knowledge::eval::{evaluate, load_cases, load_corpus};
use steve_knowledge::index::{self, ChunkMeta};
use steve_knowledge::repo::{self, NewDoc};
use steve_knowledge::retrieve::{Purpose, Query, retrieve};
use steve_knowledge::{sources, sync};
use steve_modules::testkit::{ME, TestEnv, WS};
use steve_store::base::mems::{self, NewMem};
use steve_store::base::{Level, Viewer, Visibility};
use steve_store::{StoreError, uid};

fn eval_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/eval")
}

#[test]
fn retrieve_on_eval_corpus() {
    let docs = load_corpus(&eval_dir().join("corpus")).unwrap();
    let cases = load_cases(&eval_dir().join("cases.json")).unwrap();
    let env = TestEnv::new(|_| {});
    let now = steve_store::now();

    // mm id → corpus ref (`mem:<id>`)
    let mut mem_refs: HashMap<String, String> = HashMap::new();
    let chunks = env
        .store()
        .transaction(|tx| {
            sources::ensure_builtin(tx, WS, ME, now)?;
            let folder = uid("ks");
            repo::insert_source(
                tx,
                &folder,
                WS,
                "folder",
                "我的 Wiki",
                &json!({"path": "/tmp/wiki"}),
                &Visibility::Private,
                Level::Conclusion,
                ME,
                now,
            )?;
            let mut n = 0;
            for d in &docs {
                if d.source_kind == "mems" {
                    let m = mems::create(
                        tx,
                        WS,
                        &NewMem {
                            text: d.chunks[0].text.clone(),
                            created_by: ME.into(),
                            ..Default::default()
                        },
                    )?;
                    sync::sync_mem(tx, &m.id, now)?;
                    mem_refs.insert(m.id, d.r#ref.clone());
                    n += 1;
                    continue;
                }
                let src = match d.source_kind.as_str() {
                    "folder" => repo::source(tx, &folder)?,
                    k => repo::source_of_kind(tx, WS, k)?,
                }
                .expect("source");
                let id = uid("kd");
                repo::insert_doc(
                    tx,
                    WS,
                    &NewDoc {
                        id: id.clone(),
                        source_id: src.id.clone(),
                        r#ref: d.r#ref.clone(),
                        title: d.title.clone(),
                        created_by: ME.into(),
                        ..Default::default()
                    },
                    now,
                )?;
                let label = index::doc_label(&src, &d.title);
                n += index::replace(
                    tx,
                    &ChunkMeta {
                        ws: WS,
                        obj_type: "chunk",
                        obj_id: &id,
                        title: &d.title,
                        visibility: &src.visibility,
                        level: src.level,
                        created_by: ME,
                        source_kind: &src.kind,
                        label: &label,
                        updated: now,
                    },
                    &d.chunks,
                )?;
            }
            Ok::<_, StoreError>(n)
        })
        .unwrap();
    eprintln!(
        "corpus: {} docs, {chunks} index rows; {} cases",
        docs.len(),
        cases.len()
    );
    assert!(chunks >= 180 && cases.len() >= 40);

    let refs: HashMap<String, String> = env
        .store()
        .reader(|c| {
            let mut st = c.prepare("SELECT id, ref FROM kb_docs")?;
            let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            Ok::<_, StoreError>(rows.collect::<rusqlite::Result<HashMap<_, _>>>()?)
        })
        .unwrap();
    let viewer = Viewer::bare(WS, ME);
    let mut took: Vec<u128> = Vec::new();
    let mut search = |q: &str| -> Vec<String> {
        let t = Instant::now();
        let hits = env
            .store()
            .reader(|c| {
                let mut query = Query::new(
                    &steve_knowledge::port::SqliteStore,
                    &viewer,
                    q,
                    Purpose::Search,
                );
                query.k = 20;
                retrieve(c, &query)
            })
            .unwrap();
        took.push(t.elapsed().as_micros());
        hits.iter()
            .filter_map(|h| match h.obj_type.as_str() {
                "mem" => mem_refs.get(&h.obj_id).cloned(),
                _ => refs.get(&h.obj_id).cloned(),
            })
            .collect()
    };
    let r1 = evaluate(&cases, 1, &mut search);
    let r10 = evaluate(&cases, 10, &mut search);
    let r5 = evaluate(&cases, 5, &mut search);
    eprintln!(
        "retrieve: recall@1={:.3} recall@10={:.3}",
        r1.recall, r10.recall
    );
    eprintln!("retrieve: {r5}");
    took.sort_unstable();
    let p95 = took[(took.len() * 95 / 100).min(took.len() - 1)];
    eprintln!("latency: p50={}µs p95={p95}µs", took[took.len() / 2]);
    assert!(
        r5.recall >= 0.8,
        "recall@5 {:.3} below the phase-1 bar 0.8",
        r5.recall
    );
}
