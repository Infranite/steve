//! N0e-E4: a folder source scanned and read through a Node (here `FakeNode`). The Hub keeps the
//! documents, the index and who sees them; only a scan that ends complete deletes anything.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use steve_knowledge::repo::{self, DocRow};
use steve_knowledge::sources::folder::{self, FolderPolicy, SourceBody};
use steve_knowledge::sources::folder_node::{DENIED, FolderPlace, FolderPlaces, OFFLINE, Unplaced};
use steve_knowledge::{Knowledge, TokenGuard, index};
use steve_modules::testkit::{ME, TestEnv, WS, caller};
use steve_node_api::testkit::FakeNode;
use steve_node_api::*;

/// The FakeNode's files face, with a few faults: pages forced small, the link lost after some
/// pages, a file rewritten just before its first read, every call refused as offline.
struct Faulty {
    fake: Arc<FakeNode>,
    small_pages: AtomicBool,
    /// Pages served before the link goes (`usize::MAX`: never).
    pages_left: AtomicUsize,
    offline: AtomicBool,
    /// Only reads fail as offline.
    reads_offline: AtomicBool,
    /// Rewritten with these bytes right before their first read.
    /// (`None`: removed instead.)
    rewrite: Mutex<Option<(String, Option<Vec<u8>>)>>,
    reads: AtomicUsize,
}

fn offline() -> ApiError {
    ApiError::new(ErrorCode::Offline, "gone")
}

impl Faulty {
    fn up(&self) -> ApiResult<()> {
        if self.offline.load(Ordering::SeqCst) {
            Err(offline())
        } else {
            Ok(())
        }
    }
}

#[async_trait]
impl WorkspaceExecutor for Faulty {
    async fn prepare_workspace(&self, c: Command<Prepare>) -> ApiResult<Receipt> {
        self.fake.prepare_workspace(c).await
    }
    async fn workspace(&self, w: WorkspaceRef) -> ApiResult<WorkspaceView> {
        self.fake.workspace(w).await
    }
    async fn tidy_workspace(&self, c: Command<Tidy>) -> ApiResult<Receipt> {
        self.fake.tidy_workspace(c).await
    }
    async fn git_facts(&self, q: GitQuery) -> ApiResult<GitFacts> {
        self.fake.git_facts(q).await
    }
    async fn git_apply(&self, c: Command<GitApply>) -> ApiResult<Receipt> {
        self.fake.git_apply(c).await
    }
    async fn exec(&self, c: Command<ExecSpec>) -> ApiResult<Receipt> {
        self.fake.exec(c).await
    }
    async fn list_dir(&self, p: PathRef, page: PageRequest) -> ApiResult<Page<FileInfo>> {
        self.fake.list_dir(p, page).await
    }
    async fn list_root(&self, at: RootPath, page: PageRequest) -> ApiResult<Page<FileInfo>> {
        self.up()?;
        self.fake.list_root(at, page).await
    }
    async fn read_file(&self, q: FileRead) -> ApiResult<ReadChunk> {
        self.up()?;
        if self.reads_offline.load(Ordering::SeqCst) {
            return Err(offline());
        }
        self.reads.fetch_add(1, Ordering::SeqCst);
        let FileAddress::Root(at) = &q.path else {
            unreachable!()
        };
        let take = {
            let mut r = self.rewrite.lock().unwrap();
            match r.as_ref() {
                Some((rel, _)) if rel == at.rel.as_str() => r.take(),
                _ => None,
            }
        };
        match take {
            Some((rel, Some(bytes))) => self.fake.put_file("kb", &rel, &bytes),
            Some((rel, None)) => self.fake.remove_file("kb", &rel),
            None => {}
        }
        self.fake.read_file(q).await
    }
    async fn scan(&self, c: Command<ScanSpec>) -> ApiResult<Receipt> {
        self.up()?;
        self.fake.scan(c).await
    }
    async fn scan_page(&self, scan: OperationId, mut page: PageRequest) -> ApiResult<ScanPage> {
        self.up()?;
        let left = self.pages_left.load(Ordering::SeqCst);
        if left == 0 {
            return Err(offline());
        }
        if left != usize::MAX {
            self.pages_left.store(left - 1, Ordering::SeqCst);
        }
        if self.small_pages.load(Ordering::SeqCst) {
            page.limit = 2;
        }
        self.fake.scan_page(scan, page).await
    }
}

struct Places {
    files: Arc<Faulty>,
    fake: Arc<FakeNode>,
    /// The router says the device is not linked.
    unlinked: AtomicBool,
}

#[async_trait]
impl FolderPlaces for Places {
    async fn place(&self, _source: &str, _scanning: bool) -> Result<Option<FolderPlace>, Unplaced> {
        if self.unlinked.load(Ordering::SeqCst) {
            return Err(Unplaced::Offline);
        }
        Ok(Some(FolderPlace {
            files: self.files.clone(),
            control: self.fake.clone(),
            root: RootPath {
                root: RootId::new("kb").unwrap(),
                rel: RelPath::new("").unwrap(),
            },
        }))
    }
}

struct Env {
    env: TestEnv,
    kb: Knowledge,
    fake: Arc<FakeNode>,
    files: Arc<Faulty>,
    places: Arc<Places>,
    wiki: PathBuf,
    _home: tempfile::TempDir,
}

fn setup() -> Env {
    let env = TestEnv::new(|_| {});
    let home_dir = tempfile::tempdir().unwrap();
    let home = fs::canonicalize(home_dir.path()).unwrap();
    // the Hub still checks the folder is one it may add; its files are the Node's
    let wiki = home.join("wiki");
    fs::create_dir_all(home.join(".steve")).unwrap();
    fs::create_dir_all(&wiki).unwrap();
    let kb = Knowledge::with_loaders(Arc::new(TokenGuard), |_| {}, vec![]);
    assert!(kb.set_folder_policy(FolderPolicy::new(&home, home.join(".steve"))));
    let fake = FakeNode::new("n_kb");
    fake.set_roots(vec![RootInfo {
        id: RootId::new("kb").unwrap(),
        display: "~/wiki".into(),
        access: RootAccess::ReadOnly,
    }]);
    let files = Arc::new(Faulty {
        fake: fake.clone(),
        small_pages: AtomicBool::new(false),
        pages_left: AtomicUsize::new(usize::MAX),
        offline: AtomicBool::new(false),
        reads_offline: AtomicBool::new(false),
        rewrite: Mutex::new(None),
        reads: AtomicUsize::new(0),
    });
    let places = Arc::new(Places {
        files: files.clone(),
        fake: fake.clone(),
        unlinked: AtomicBool::new(false),
    });
    assert!(kb.set_folder_places(places.clone()));
    Env {
        env,
        kb,
        fake,
        files,
        places,
        wiki,
        _home: home_dir,
    }
}

impl Env {
    fn add(&self) -> String {
        let s = folder::create(
            &self.env.modules,
            &self.kb,
            &caller(WS, ME),
            SourceBody {
                kind: "folder".into(),
                name: "Wiki".into(),
                path: self.wiki.to_string_lossy().into(),
                include: None,
                exclude: None,
                visibility: None,
                level: None,
            },
        )
        .unwrap();
        s.id
    }
    async fn sync(&self, id: &str) -> Result<folder::SyncReport, String> {
        folder::sync_now(&self.env.modules, &self.kb, id)
            .await
            .map(|r| r.expect("claimed"))
            .map_err(|e| e.to_string())
    }
    fn docs(&self, id: &str) -> Vec<DocRow> {
        let mut d = self
            .env
            .store()
            .reader(|c| repo::docs_of_source(c, id))
            .unwrap();
        d.sort_by(|a, b| a.r#ref.cmp(&b.r#ref));
        d
    }
    fn doc(&self, id: &str, r: &str) -> DocRow {
        self.docs(id)
            .into_iter()
            .find(|d| d.r#ref == r)
            .unwrap_or_else(|| panic!("no doc {r}"))
    }
    fn source(&self, id: &str) -> repo::SourceRow {
        self.env
            .store()
            .reader(|c| repo::source(c, id))
            .unwrap()
            .unwrap()
    }
    fn counts(&self, id: &str) -> (i64, i64) {
        let s = self.source(id);
        self.env
            .store()
            .reader(|c| repo::source_counts(c, &s))
            .unwrap()
    }
    fn texts(&self, doc: &str) -> String {
        self.env
            .store()
            .reader(|c| repo::doc_chunks(c, doc, 100))
            .unwrap()
            .into_iter()
            .map(|(_, _, t)| t)
            .collect::<Vec<_>>()
            .join("\n")
    }
    /// Five indexed files.
    async fn indexed(&self) -> String {
        for (rel, text) in [
            ("a.md", "# 甲\n苹果的笔记"),
            ("b.md", "# 乙\n香蕉的笔记"),
            ("c.md", "# 丙\n樱桃的笔记"),
            ("sub/d.md", "# 丁\n葡萄的笔记"),
            ("sub/e.txt", "西瓜的笔记"),
        ] {
            self.fake.put_file("kb", rel, text.as_bytes());
        }
        let id = self.add();
        let r = self.sync(&id).await.unwrap();
        assert_eq!((r.files, r.changed, r.deleted), (5, 5, 0), "{r:?}");
        assert!(self.docs(&id).iter().all(|d| d.status == "active"));
        id
    }
}

fn statuses(docs: &[DocRow]) -> Vec<(&str, &str)> {
    docs.iter()
        .map(|d| (d.r#ref.as_str(), d.status.as_str()))
        .collect()
}

#[tokio::test]
async fn a_complete_scan_indexes_and_deletes_what_is_gone() {
    let e = setup();
    let id = e.indexed().await;
    assert!(e.texts(&e.doc(&id, "a.md").id).contains("苹果"));
    assert_eq!(e.source(&id).status, "ok");

    e.fake.remove_file("kb", "b.md");
    e.fake
        .put_file("kb", "a.md", "# 甲\n苹果和梨的笔记".as_bytes());
    let r = e.sync(&id).await.unwrap();
    assert_eq!((r.changed, r.deleted, r.kept), (1, 1, 0), "{r:?}");
    assert_eq!(e.doc(&id, "b.md").status, "deleted");
    assert!(e.texts(&e.doc(&id, "a.md").id).contains("梨"));
}

#[tokio::test]
async fn offline_after_two_pages_deletes_nothing() {
    let e = setup();
    let id = e.indexed().await;
    let before = (statuses(&e.docs(&id)).len(), e.counts(&id));
    e.fake.remove_file("kb", "a.md");
    e.fake.remove_file("kb", "sub/e.txt");
    // five files again: three pages of two, the link lost after the second
    e.fake.put_file("kb", "f.md", "新的".as_bytes());
    e.fake.put_file("kb", "g.md", "也是新的".as_bytes());
    e.files.small_pages.store(true, Ordering::SeqCst);
    e.files.pages_left.store(2, Ordering::SeqCst);

    let err = e.sync(&id).await.unwrap_err();
    assert!(err.contains(OFFLINE), "{err}");
    let s = e.source(&id);
    assert_eq!(
        (s.status.as_str(), s.error.as_deref()),
        ("error", Some(OFFLINE))
    );
    let docs = e.docs(&id);
    assert!(
        docs.iter().all(|d| d.status == "active"),
        "{:?}",
        statuses(&docs)
    );
    assert_eq!((docs.len(), e.counts(&id)), before, "the index is intact");

    // the device not linked at all: the same, nothing scanned
    e.places.unlinked.store(true, Ordering::SeqCst);
    assert!(e.sync(&id).await.unwrap_err().contains(OFFLINE));
    assert_eq!(e.source(&id).error.as_deref(), Some(OFFLINE));
    assert!(e.docs(&id).iter().all(|d| d.status == "active"));

    // back: the complete scan deletes them
    e.places.unlinked.store(false, Ordering::SeqCst);
    e.files.pages_left.store(usize::MAX, Ordering::SeqCst);
    let r = e.sync(&id).await.unwrap();
    assert_eq!((r.changed, r.deleted), (2, 2), "{r:?}");
    assert_eq!(e.doc(&id, "a.md").status, "deleted");
    assert_eq!(e.doc(&id, "sub/e.txt").status, "deleted");
}

#[tokio::test]
async fn held_and_unreadable_issues_keep_documents() {
    let e = setup();
    let id = e.indexed().await;
    // a.md is there but refused by the Node; sub/ could not be listed
    e.fake.remove_file("kb", "a.md");
    e.fake.scan_issue(
        "kb",
        ScanIssue::Held {
            path: RelPath::new("a.md").unwrap(),
            why: HeldWhy::Denied,
        },
    );
    let r = e.sync(&id).await.unwrap();
    assert_eq!((r.deleted, r.kept), (0, 1), "{r:?}");
    assert!(
        r.incomplete.is_empty(),
        "a hold leaves the scan whole: {r:?}"
    );
    let a = e.doc(&id, "a.md");
    assert_eq!(
        (a.status.as_str(), a.error.as_deref()),
        ("failed", Some(DENIED))
    );

    e.fake.remove_file("kb", "sub/d.md");
    e.fake.scan_issue(
        "kb",
        ScanIssue::Unreadable {
            path: Some(RelPath::new("sub").unwrap()),
        },
    );
    let r = e.sync(&id).await.unwrap();
    assert_eq!(r.deleted, 0, "{r:?}");
    assert_eq!(r.incomplete, ["unreadable"]);
    assert_eq!(e.doc(&id, "sub/d.md").status, "active");
    assert!(e.texts(&e.doc(&id, "sub/d.md").id).contains("葡萄"));
}

#[tokio::test]
async fn a_file_changed_before_its_read_is_queued_again() {
    let e = setup();
    let worker = index::spawn_worker(e.env.modules.clone(), e.kb.clone()).unwrap();
    let id = e.indexed().await;
    e.fake
        .put_file("kb", "c.md", "# 丙\n樱桃和桃子的笔记".as_bytes());
    // written again between the scan and the read: the scan's version is stale
    *e.files.rewrite.lock().unwrap() = Some((
        "c.md".into(),
        Some("# 丙\n最后是李子的笔记".as_bytes().to_vec()),
    ));
    e.sync(&id).await.unwrap();
    let c = e.doc(&id, "c.md");
    assert_ne!(c.status, "failed", "{c:?}");
    assert_eq!(
        c.mtime, None,
        "the conflict was not taken as the file's state"
    );
    for _ in 0..200 {
        if e.texts(&c.id).contains("李子") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(e.texts(&c.id).contains("李子"), "{}", e.texts(&c.id));
    assert_eq!(e.doc(&id, "c.md").status, "active");
    // the next scan reads it once more (its mtime was forgotten), and settles
    let r = e.sync(&id).await.unwrap();
    assert_eq!(r.changed, 1, "{r:?}");
    assert_eq!(e.sync(&id).await.unwrap().changed, 0);
    worker.abort();
}

#[tokio::test]
async fn offline_while_reading_keeps_the_old_text_for_the_next_scan() {
    let e = setup();
    let id = e.indexed().await;
    e.fake
        .put_file("kb", "b.md", "# 乙\n香蕉换成了芒果".as_bytes());
    // the scan sees the change; its read finds the device gone
    e.files.reads_offline.store(true, Ordering::SeqCst);
    e.sync(&id).await.unwrap();
    let b = e.doc(&id, "b.md");
    assert_eq!((b.status.as_str(), b.mtime), ("active", None), "{b:?}");
    assert!(e.texts(&b.id).contains("香蕉"), "the old text stays");

    e.files.reads_offline.store(false, Ordering::SeqCst);
    let r = e.sync(&id).await.unwrap();
    assert_eq!(r.changed, 1, "{r:?}");
    assert!(e.texts(&b.id).contains("芒果"));
}

#[tokio::test]
async fn a_file_gone_before_its_read_is_deleted() {
    let e = setup();
    let id = e.indexed().await;
    e.fake
        .put_file("kb", "sub/deeper/h.md", "很快就没了".as_bytes());
    *e.files.rewrite.lock().unwrap() = Some(("sub/deeper/h.md".into(), None));
    e.sync(&id).await.unwrap();
    assert_eq!(e.doc(&id, "sub/deeper/h.md").status, "deleted");
    assert_eq!(e.doc(&id, "sub/d.md").status, "active");
}
