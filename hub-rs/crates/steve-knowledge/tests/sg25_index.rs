//! SG25: ordered concurrent loading, transactional indexing, retries and re-read boundaries.
use std::collections::HashMap;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;
use steve_knowledge::{
    KbEvent, Knowledge,
    chunk::TextKind,
    extract_text::{Extracted, FrontMatter},
    index::{self, BoxFuture, DocLoader, Loaded},
    repo::{self, DocRow, NewDoc, SourceRow},
};
use steve_modules::{
    Modules,
    testkit::{ME, TestEnv, WS},
};
use steve_store::base::{Level, Visibility};
use tokio::sync::{Notify, Semaphore};
const NOW: i64 = 2_000_000_000_000;
fn extracted(text: &str) -> Extracted {
    Extracted {
        text: text.into(),
        kind: TextKind::Plain,
        front: FrontMatter::default(),
    }
}

#[derive(Default)]
struct Loader {
    calls: Mutex<HashMap<String, usize>>,
}
impl DocLoader for Loader {
    fn kinds(&self) -> &[&str] {
        &["sg25", "files"]
    }
    fn load<'a>(
        &'a self,
        m: &'a Modules,
        d: &'a DocRow,
        _: &'a SourceRow,
    ) -> BoxFuture<'a, Loaded> {
        Box::pin(async move {
            let n = {
                let mut counts = self.calls.lock().unwrap();
                let n = counts.entry(d.r#ref.clone()).or_default();
                *n += 1;
                *n
            };
            match d.r#ref.as_str() {
                "skip" => Loaded::Skipped("title only".into()),
                "failed" => Loaded::Failed("extraction failed".into()),
                "gone" => Loaded::Gone,
                "retry" => Loaded::Retry { now: false },
                "retry_now" if n == 1 => Loaded::Retry { now: true },
                "front" => Loaded::Text(Extracted {
                    text: "front matter body".into(),
                    kind: TextKind::Plain,
                    front: FrontMatter {
                        title: Some("Front title".into()),
                        tags: vec!["tag".into()],
                        level: Some("never".into()),
                        visibility: Some("workspace".into()),
                    },
                }),
                "vanish" => {
                    m.store
                        .transaction(|tx| index::delete_doc(tx, &d.id))
                        .unwrap();
                    Loaded::Text(extracted("must not resurrect"))
                }
                "rescope" => {
                    m.store
                        .transaction(|tx| {
                            repo::set_doc_scope(
                                tx,
                                &d.id,
                                Some(&Visibility::Workspace),
                                Some(Level::Never),
                                NOW - 1,
                            )?;
                            repo::set_doc_text(
                                tx,
                                &d.id,
                                "Changed while loading",
                                "changed body",
                                NOW - 1,
                            )
                        })
                        .unwrap();
                    Loaded::Text(extracted("loaded body"))
                }
                _ => Loaded::Text(extracted(&format!("loaded {}", d.r#ref))),
            }
        })
    }
}
struct H {
    env: TestEnv,
    kb: Knowledge,
    events: Arc<Mutex<Vec<KbEvent>>>,
    committed: Arc<Mutex<Vec<i64>>>,
    wake: Arc<Notify>,
}
fn setup(loader: Arc<dyn DocLoader>, check_commit: bool) -> H {
    let env = TestEnv::new(|_| {});
    env.clock.set(NOW);
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    let committed = Arc::new(Mutex::new(Vec::new()));
    let seen = committed.clone();
    let store = env.store().clone();
    let wake = Arc::new(Notify::new());
    let signal = wake.clone();
    let kb = Knowledge::with_loaders(
        move |e| {
            if check_commit {
                let count = store
                    .reader(|c| {
                        Ok::<_, steve_store::StoreError>(c.query_row(
                            "SELECT count(*) FROM kb_docs WHERE indexed=?",
                            [NOW],
                            |r| r.get(0),
                        )?)
                    })
                    .unwrap();
                seen.lock().unwrap().push(count);
            }
            sink.lock().unwrap().push(e);
            signal.notify_one();
        },
        vec![loader],
    );
    H {
        env,
        kb,
        events,
        committed,
        wake,
    }
}
impl H {
    fn seed(&self, source: &str, kind: &str, refs: &[&str]) -> Vec<String> {
        self.env
            .store()
            .transaction(|tx| {
                repo::insert_source(
                    tx,
                    source,
                    WS,
                    kind,
                    "SG25 source",
                    &serde_json::json!({}),
                    &Visibility::Private,
                    Level::Conclusion,
                    ME,
                    100,
                )?;
                let src = repo::source(tx, source)?.unwrap();
                let mut ids = Vec::new();
                for name in refs {
                    let id = format!("kd_{name}");
                    repo::insert_doc(
                        tx,
                        WS,
                        &NewDoc {
                            id: id.clone(),
                            source_id: source.into(),
                            r#ref: (*name).into(),
                            title: format!("title-{name}"),
                            body: Some("old text".into()),
                            size: 8,
                            mtime: Some(100),
                            created_by: ME.into(),
                            ..Default::default()
                        },
                        100,
                    )?;
                    let d = repo::doc(tx, &id)?.unwrap();
                    index::index_doc(tx, &d, &src, &extracted("old text"), 100, false)?;
                    ids.push(id);
                }
                Ok::<_, steve_store::StoreError>(ids)
            })
            .unwrap()
    }
    fn doc(&self, id: &str) -> DocRow {
        self.env
            .store()
            .reader(|c| repo::doc(c, id))
            .unwrap()
            .unwrap()
    }
    fn rows(&self, id: &str) -> i64 {
        self.env
            .store()
            .reader(|c| {
                Ok::<_, steve_store::StoreError>(c.query_row(
                    "SELECT count(*) FROM kb_chunks WHERE obj_type='chunk' AND obj_id=?",
                    [id],
                    |r| r.get(0),
                )?)
            })
            .unwrap()
    }
    fn notified(&self) -> Vec<String> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .map(|e| {
                assert_eq!(e.ws, WS);
                e.id.clone()
            })
            .collect()
    }
}

#[tokio::test]
async fn sg25_mixed_load_outcomes_commit_independently_and_retries_keep_the_previous_index() {
    let loader = Arc::new(Loader::default());
    let h = setup(loader.clone(), false);
    let mut ids = h.seed(
        "ks_sg25",
        "sg25",
        &[
            "ok",
            "skip",
            "failed",
            "gone",
            "retry",
            "retry_now",
            "already_deleted",
        ],
    );
    let front = h.seed("ks_files", "files", &["front"]).remove(0);
    let unsupported = h.seed("ks_unknown", "sg25_unknown", &["unknown"]).remove(0);
    let deleted = h.doc("kd_already_deleted");
    h.env
        .store()
        .transaction(|tx| index::fail_doc(tx, &deleted, "deleted", "old deletion", 100))
        .unwrap();
    let deleted = h.doc("kd_already_deleted");
    let old_retry = h.doc("kd_retry");
    let old_now = h.doc("kd_retry_now");
    // Prepare the private queue without running it; now=true must not affect this batch's notifications.
    let _prepared = index::worker(h.env.modules.clone(), h.kb.clone()).unwrap();
    ids.extend([front.clone(), unsupported.clone(), "kd_missing".into()]);
    index::run_batch(&h.env.modules, &h.kb, &ids).await.unwrap();
    assert_eq!(
        h.notified(),
        [
            "kd_ok",
            "kd_skip",
            "kd_failed",
            "kd_gone",
            "kd_front",
            "kd_unknown"
        ]
    );
    for (id, status, error, rows, chunks) in [
        ("kd_ok", "active", None, 1, 1),
        ("kd_skip", "skipped", Some("title only"), 1, 0),
        ("kd_failed", "failed", Some("extraction failed"), 0, 0),
        ("kd_gone", "deleted", None, 0, 0),
        ("kd_unknown", "failed", Some("这种来源还不能入库"), 0, 0),
    ] {
        let d = h.doc(id);
        assert_eq!(d.status, status);
        assert_eq!(d.error.as_deref(), error);
        assert_eq!(h.rows(id), rows);
        assert_eq!(d.chunks, chunks);
        assert_eq!(d.indexed, Some(NOW));
    }
    for (id, mut before) in [("kd_retry", old_retry), ("kd_retry_now", old_now)] {
        before.mtime = None;
        assert_eq!(h.doc(id), before);
        assert_eq!(h.rows(id), 1);
    }
    assert_eq!(h.doc("kd_already_deleted"), deleted);
    assert!(!loader.calls.lock().unwrap().contains_key("already_deleted"));
    let d = h.doc(&front);
    assert_eq!(
        d.title, "title-front",
        "uploaded files keep their assigned title"
    );
    assert_eq!(d.tags, ["tag"]);
    assert_eq!(
        d.visibility, None,
        "front matter cannot widen a private source to workspace"
    );
    assert_eq!(d.level, Some(Level::Never));
    let dangling:i64=h.env.store().reader(|c|Ok::<_,steve_store::StoreError>(c.query_row("SELECT count(*) FROM kb_fts f LEFT JOIN kb_chunks c ON c.id=f.rowid WHERE c.id IS NULL",[],|r|r.get(0))?)).unwrap();
    assert_eq!(dangling, 0);
}

#[tokio::test]
async fn sg25_retry_now_is_queued_after_commit_but_retry_later_is_not() {
    let loader = Arc::new(Loader::default());
    let h = setup(loader.clone(), false);
    let ids = h.seed("ks_sg25", "sg25", &["retry", "retry_now"]);
    let worker = index::worker(h.env.modules.clone(), h.kb.clone()).unwrap();
    index::run_batch(&h.env.modules, &h.kb, &ids).await.unwrap();
    assert!(h.notified().is_empty());
    let job = tokio::spawn(worker);
    tokio::time::timeout(Duration::from_secs(30), h.wake.notified())
        .await
        .unwrap();
    assert_eq!(h.notified(), ["kd_retry_now"]);
    assert_eq!(loader.calls.lock().unwrap().get("retry_now"), Some(&2));
    assert_eq!(loader.calls.lock().unwrap().get("retry"), Some(&1));
    assert_eq!(h.doc("kd_retry_now").indexed, Some(NOW));
    assert_eq!(h.doc("kd_retry").indexed, Some(100));
    job.abort();
    assert!(job.await.unwrap_err().is_cancelled());
}

#[tokio::test]
async fn sg25_real_write_failure_rolls_back_the_whole_batch_and_emits_no_partial_notifications() {
    let h = setup(Arc::new(Loader::default()), false);
    let ids = h.seed("ks_sg25", "sg25", &["first", "second"]);
    let before = ids.iter().map(|id| h.doc(id)).collect::<Vec<_>>();
    h.env.store().with_conn(|c|{c.execute_batch("CREATE TEMP TRIGGER sg25_fail_batch BEFORE UPDATE ON kb_docs WHEN NEW.id='kd_second' BEGIN SELECT RAISE(ABORT,'sg25 batch rollback'); END;")?;Ok(())}).unwrap();
    let error = index::run_batch(&h.env.modules, &h.kb, &ids)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("sg25 batch rollback"), "{error}");
    assert_eq!(ids.iter().map(|id| h.doc(id)).collect::<Vec<_>>(), before);
    assert!(h.notified().is_empty());
    for id in &ids {
        let text: String = h
            .env
            .store()
            .reader(|c| {
                Ok::<_, steve_store::StoreError>(c.query_row(
                    "SELECT text FROM kb_chunks WHERE obj_id=?",
                    [id],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(text, "old text");
    }
    h.env
        .store()
        .with_conn(|c| {
            c.execute_batch("DROP TRIGGER sg25_fail_batch")?;
            Ok(())
        })
        .unwrap();
    index::run_batch(&h.env.modules, &h.kb, &ids).await.unwrap();
    assert_eq!(h.notified(), ids);
}

#[tokio::test]
async fn sg25_loaded_documents_are_reread_before_writes_and_deleted_rows_are_not_resurrected() {
    let h = setup(Arc::new(Loader::default()), false);
    let ids = h.seed("ks_sg25", "sg25", &["vanish", "rescope"]);
    index::run_batch(&h.env.modules, &h.kb, &ids).await.unwrap();
    assert!(
        h.env
            .store()
            .reader(|c| repo::doc(c, "kd_vanish"))
            .unwrap()
            .is_none()
    );
    assert_eq!(h.rows("kd_vanish"), 0);
    let d = h.doc("kd_rescope");
    assert_eq!(d.title, "Changed while loading");
    assert_eq!(d.visibility, Some(Visibility::Workspace));
    assert_eq!(d.level, Some(Level::Never));
    let (title, vis, level): (String, String, String) = h
        .env
        .store()
        .reader(|c| {
            Ok::<_, steve_store::StoreError>(c.query_row(
                "SELECT label,visibility,level FROM kb_chunks WHERE obj_id='kd_rescope'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?)
        })
        .unwrap();
    assert!(title.ends_with("Changed while loading"));
    assert_eq!(vis, "workspace");
    assert_eq!(level, "never");
    assert_eq!(h.notified(), ["kd_rescope"]);
}

struct Gated {
    entered: Semaphore,
    finished: Semaphore,
    gates: Vec<Semaphore>,
    active: AtomicUsize,
    max: AtomicUsize,
    started: Mutex<Vec<usize>>,
    completed: Mutex<Vec<usize>>,
}
impl Gated {
    fn new() -> Self {
        Self {
            entered: Semaphore::new(0),
            finished: Semaphore::new(0),
            gates: (0..6).map(|_| Semaphore::new(0)).collect(),
            active: AtomicUsize::new(0),
            max: AtomicUsize::new(0),
            started: Mutex::new(vec![]),
            completed: Mutex::new(vec![]),
        }
    }
}
impl DocLoader for Gated {
    fn kinds(&self) -> &[&str] {
        &["sg25"]
    }
    fn load<'a>(
        &'a self,
        _: &'a Modules,
        d: &'a DocRow,
        _: &'a SourceRow,
    ) -> BoxFuture<'a, Loaded> {
        Box::pin(async move {
            let i = d.r#ref.parse::<usize>().unwrap();
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max.fetch_max(active, Ordering::SeqCst);
            self.started.lock().unwrap().push(i);
            self.entered.add_permits(1);
            self.gates[i].acquire().await.unwrap().forget();
            self.active.fetch_sub(1, Ordering::SeqCst);
            self.completed.lock().unwrap().push(i);
            self.finished.add_permits(1);
            Loaded::Text(extracted("concurrent indexed body"))
        })
    }
}

#[tokio::test]
async fn sg25_loading_is_bounded_to_four_and_completion_and_notifications_keep_input_order() {
    let loader = Arc::new(Gated::new());
    let h = setup(loader.clone(), true);
    let ids = h.seed("ks_sg25", "sg25", &["0", "1", "2", "3", "4", "5"]);
    let (m, kb, work) = (h.env.modules.clone(), h.kb.clone(), ids.clone());
    let job = tokio::spawn(async move { index::run_batch(&m, &kb, &work).await });
    tokio::time::timeout(Duration::from_secs(30), loader.entered.acquire_many(4))
        .await
        .unwrap()
        .unwrap()
        .forget();
    let mut started = loader.started.lock().unwrap().clone();
    started.sort_unstable();
    assert_eq!(started, [0, 1, 2, 3]);
    assert_eq!(loader.max.load(Ordering::SeqCst), 4);
    for i in [3, 2, 1] {
        loader.gates[i].add_permits(1);
        tokio::time::timeout(Duration::from_secs(30), loader.finished.acquire())
            .await
            .unwrap()
            .unwrap()
            .forget();
    }
    assert_eq!(*loader.completed.lock().unwrap(), [3, 2, 1]);
    assert_eq!(loader.started.lock().unwrap().len(), 4);
    assert!(h.notified().is_empty());
    for i in [4, 5, 0] {
        loader.gates[i].add_permits(1);
    }
    tokio::time::timeout(Duration::from_secs(30), job)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(loader.max.load(Ordering::SeqCst), 4);
    assert_eq!(loader.active.load(Ordering::SeqCst), 0);
    assert_eq!(h.notified(), ids);
    assert_eq!(*h.committed.lock().unwrap(), vec![6; 6]);
}
