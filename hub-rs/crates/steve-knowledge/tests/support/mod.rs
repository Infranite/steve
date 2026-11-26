//! Disk-backed test Node implementing [`FolderPlaces`], [`WorkspaceExecutor`], and
//! [`ExecutionControl`] so knowledge integration tests exercise the Node scan/read/path_check path.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use steve_knowledge::scan;
use steve_knowledge::sources::folder;
use steve_knowledge::sources::folder_node::{FolderPlace, FolderPlaces, Unplaced};
use steve_node_api::*;

struct TestDiskPlaces {
    store: steve_store::Store,
    roots: Mutex<HashMap<String, PathBuf>>,
    scans: Mutex<HashMap<String, ScanPage>>,
}

impl TestDiskPlaces {
    fn new(store: steve_store::Store) -> Arc<Self> {
        Arc::new(Self {
            store,
            roots: Mutex::new(HashMap::new()),
            scans: Mutex::new(HashMap::new()),
        })
    }

    fn root_dir(&self, id: &RootId) -> ApiResult<PathBuf> {
        self.roots
            .lock()
            .unwrap()
            .get(id.as_str())
            .cloned()
            .ok_or_else(|| Error::new(Code::Forbidden, "unknown root"))
    }
}

pub struct TestDiskRouter(Arc<TestDiskPlaces>);

impl TestDiskRouter {
    pub fn new(store: steve_store::Store) -> Arc<Self> {
        Arc::new(Self(TestDiskPlaces::new(store)))
    }
}

#[async_trait]
impl FolderPlaces for TestDiskRouter {
    async fn place(&self, source: &str, _scanning: bool) -> Result<Option<FolderPlace>, Unplaced> {
        let src = self
            .0
            .store
            .read(|c| steve_store::knowledge::Documents(c).source(source))
            .map_err(|e| Unplaced::Failed(e.to_string()))?;
        let Some(src) = src else {
            return Err(Unplaced::Nowhere);
        };
        let Some(path) = folder::folder_root(&src) else {
            return Err(Unplaced::Nowhere);
        };
        let rid = format!("src-{}", source.replace('_', "-"));
        let root_id = RootId::new(&rid).map_err(|e| Unplaced::Failed(e.to_string()))?;
        self.0.roots.lock().unwrap().insert(rid, path);
        Ok(Some(FolderPlace {
            files: self.0.clone(),
            control: self.0.clone(),
            root: RootPath {
                root: root_id,
                rel: RelPath::new("").unwrap(),
            },
        }))
    }

    async fn check_path(&self, path: &str) -> Result<Option<PathCheckResponse>, Unplaced> {
        let p = Path::new(path.trim());
        match fs::metadata(p) {
            Ok(meta) => Ok(Some(PathCheckResponse {
                exists: true,
                is_dir: meta.is_dir(),
                canonical: fs::canonicalize(p)
                    .ok()
                    .map(|c| c.to_string_lossy().into_owned()),
            })),
            Err(_) => Ok(Some(PathCheckResponse {
                exists: false,
                is_dir: false,
                canonical: None,
            })),
        }
    }
}

#[async_trait]
impl WorkspaceExecutor for TestDiskPlaces {
    async fn prepare_workspace(&self, _: Command<Prepare>) -> ApiResult<Receipt> {
        Err(Error::new(Code::Unsupported, "unused"))
    }
    async fn workspace(&self, _: WorkspaceRef) -> ApiResult<WorkspaceView> {
        Err(Error::new(Code::Unsupported, "unused"))
    }
    async fn tidy_workspace(&self, _: Command<Tidy>) -> ApiResult<Receipt> {
        Err(Error::new(Code::Unsupported, "unused"))
    }
    async fn git_facts(&self, _: GitQuery) -> ApiResult<GitFacts> {
        Err(Error::new(Code::Unsupported, "unused"))
    }
    async fn git_apply(&self, _: Command<GitApply>) -> ApiResult<Receipt> {
        Err(Error::new(Code::Unsupported, "unused"))
    }
    async fn exec(&self, _: Command<ExecSpec>) -> ApiResult<Receipt> {
        Err(Error::new(Code::Unsupported, "unused"))
    }
    async fn list_dir(&self, _: PathRef, _: PageRequest) -> ApiResult<Page<FileInfo>> {
        Err(Error::new(Code::Unsupported, "unused"))
    }
    async fn list_root(&self, at: RootPath, _: PageRequest) -> ApiResult<Page<FileInfo>> {
        let root = self.root_dir(&at.root)?;
        let dir = if at.rel.as_str().is_empty() {
            root.clone()
        } else {
            root.join(at.rel.as_str())
        };
        let rd = fs::read_dir(&dir).map_err(|_| Error::new(Code::Invalid, "not a dir"))?;
        let mut items = Vec::new();
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let rel = if at.rel.as_str().is_empty() {
                name
            } else {
                format!("{}/{}", at.rel.as_str(), name)
            };
            let Ok(meta) = e.metadata() else { continue };
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as i64);
            items.push(FileInfo {
                path: RelPath::new(rel).unwrap(),
                is_dir: meta.is_dir(),
                size: meta.len(),
                mtime: mtime.map(Timestamp),
                version: Revision::new(format!("{}-{}", meta.len(), mtime.unwrap_or(0))).unwrap(),
                too_big: false,
            });
        }
        Ok(Page { items, next: None })
    }
    async fn read_file(&self, q: FileRead) -> ApiResult<ReadChunk> {
        let FileAddress::Root(at) = q.path else {
            return Err(Error::new(Code::Invalid, "root required"));
        };
        let root = self.root_dir(&at.root)?;
        let root_canon = fs::canonicalize(&root).map_err(|_| Error::new(Code::Invalid, "gone"))?;
        let full = root.join(at.rel.as_str());
        let canon = match fs::canonicalize(&full) {
            Ok(c) => c,
            Err(_) => return Err(Error::new(Code::Invalid, "gone")),
        };
        if !canon.starts_with(&root_canon) {
            return Err(Error::new(Code::Forbidden, "outside root"));
        }
        let bytes = fs::read(&canon).map_err(|_| Error::new(Code::Invalid, "unreadable"))?;
        let start = (q.offset as usize).min(bytes.len());
        let end = start.saturating_add(q.max as usize).min(bytes.len());
        Ok(ReadChunk {
            bytes: bytes[start..end].to_vec(),
            eof: end >= bytes.len(),
            offset: q.offset,
        })
    }
    async fn scan(&self, c: Command<ScanSpec>) -> ApiResult<Receipt> {
        let root = self.root_dir(&c.body.root.root)?;
        let rules = scan::Rules::new(c.body.include, c.body.exclude);
        let walk =
            scan::walk(&root, &rules).map_err(|e| Error::new(Code::Internal, e.to_string()))?;
        let complete = walk.complete();
        let mut entries = Vec::new();
        for f in walk.files {
            entries.push(FileInfo {
                path: RelPath::new(f.rel).unwrap(),
                is_dir: false,
                size: f.size,
                mtime: Some(Timestamp(f.mtime)),
                version: Revision::new(format!("{}-{}", f.size, f.mtime)).unwrap(),
                too_big: false,
            });
        }
        for f in walk.too_big {
            entries.push(FileInfo {
                path: RelPath::new(f.rel).unwrap(),
                is_dir: false,
                size: f.size,
                mtime: Some(Timestamp(f.mtime)),
                version: Revision::new(format!("{}-{}", f.size, f.mtime)).unwrap(),
                too_big: true,
            });
        }
        let mut issues = Vec::new();
        for g in &walk.gaps {
            match g {
                scan::Gap::Unreadable => issues.push(ScanIssue::Unreadable {
                    path: Some(RelPath::new("").unwrap()),
                }),
                scan::Gap::Entry => issues.push(ScanIssue::Unreadable { path: None }),
                scan::Gap::OverLimit => issues.push(ScanIssue::OverLimit {
                    count: walk.over_limit as u64,
                }),
                scan::Gap::Interrupted => issues.push(ScanIssue::Interrupted),
            }
        }
        for (rel, why) in &walk.held {
            let why = match why {
                scan::Held::Outside => HeldWhy::Outside,
                scan::Held::Denied => HeldWhy::Denied,
                scan::Held::Unreadable => HeldWhy::Unreadable,
            };
            issues.push(ScanIssue::Held {
                path: RelPath::new(rel.clone()).unwrap(),
                why,
            });
        }
        let page = ScanPage {
            scan: c.id.clone(),
            manifest: Revision::new("m1").unwrap(),
            entries,
            issues,
            next: None,
            complete,
        };
        self.scans
            .lock()
            .unwrap()
            .insert(c.id.as_str().to_string(), page);
        Ok(Receipt {
            operation: c.id,
            state: Observed::Finished {
                outcome: Outcome::Succeeded,
                error: None,
                result: None,
            },
        })
    }
    async fn scan_page(&self, scan: OperationId, _: PageRequest) -> ApiResult<ScanPage> {
        self.scans
            .lock()
            .unwrap()
            .get(scan.as_str())
            .cloned()
            .ok_or_else(|| Error::new(Code::Invalid, "no scan"))
    }
}

#[async_trait]
impl ExecutionControl for TestDiskPlaces {
    async fn operation(&self, op: OperationId) -> ApiResult<Receipt> {
        Ok(Receipt {
            operation: op,
            state: Observed::Finished {
                outcome: Outcome::Succeeded,
                error: None,
                result: None,
            },
        })
    }
    async fn cancel(&self, _: Command<Cancel>) -> ApiResult<Receipt> {
        Err(Error::new(Code::Unsupported, "unused"))
    }
    async fn abandon(&self, _: Command<Abandon>) -> ApiResult<Receipt> {
        Err(Error::new(Code::Unsupported, "unused"))
    }
    async fn recover(&self, _: Command<Recover>) -> ApiResult<Receipt> {
        Err(Error::new(Code::Unsupported, "unused"))
    }
    async fn resolve_permission(&self, _: Command<PermissionResolution>) -> ApiResult<Receipt> {
        Err(Error::new(Code::Unsupported, "unused"))
    }
    async fn snapshot(&self, _: PageRequest) -> ApiResult<Snapshot> {
        Err(Error::new(Code::Unsupported, "unused"))
    }
    async fn streams(&self) -> ApiResult<Vec<StreamHead>> {
        Err(Error::new(Code::Unsupported, "unused"))
    }
    async fn replay(&self, _: Watermark, _: u32) -> ApiResult<EventBatch> {
        Err(Error::new(Code::Unsupported, "unused"))
    }
    async fn acknowledge(&self, _: EventAck) -> ApiResult<()> {
        Err(Error::new(Code::Unsupported, "unused"))
    }
}
