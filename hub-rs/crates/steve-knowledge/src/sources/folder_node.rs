//! A folder source read through a Node (N0e-E4): the Node scans and reads, the Hub keeps the
//! configuration, the schedule, the reconciliation, the index and who sees what.
//!
//! The Hub wires a [`FolderPlaces`] only when its local Node is on (`STEVE_NODE_MODE=local`);
//! without one, or when it answers `Ok(None)`, the folder is walked and read on the Hub as before.
//!
//! The rule of [`super::folder`] holds here too: only a scan that ends complete deletes. A scan the
//! Node cut short, a Node gone offline before the last page, a page that does not belong to the
//! scan: the sync fails with the reason and no document is touched.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use steve_node_api::{
    ApiError, Command, Cursor, ErrorCode, ExecutionControl, FileAddress, FileInfo, FileRead,
    GrantId, HeldWhy, Observed, OperationId, Outcome, PageRequest, RelPath, Revision, RootPath,
    ScanIssue, ScanLimits, ScanSpec, Timestamp, WorkspaceExecutor,
};

use crate::scan::{self, Entry, Gap, Held, MAX_FILE_BYTES};

/// The source's status when its device cannot be reached (stored, zh-CN).
pub const OFFLINE: &str = "设备离线";
/// `kb_docs.error` of a file the Node refuses to read.
pub const DENIED: &str = "设备不允许读取这个文件";
pub(crate) const NO_FOLDER: &str = "这个来源没有设置文件夹";
const UNREADABLE: &str = "文件读不出来";

/// How long a scan may take on the Node before the sync gives up (and deletes nothing).
const SCAN_WAIT: Duration = Duration::from_secs(10 * 60);
/// Entries per page asked for.
const PAGE: u32 = 1000;
/// Bytes per read asked for (the Node may answer fewer).
const CHUNK: u32 = 96 * 1024;

/// Where a folder source's files are: the Node's file faces and the folder as a root path there.
pub struct FolderPlace {
    pub files: Arc<dyn WorkspaceExecutor>,
    pub control: Arc<dyn ExecutionControl>,
    pub root: RootPath,
}

/// Why a folder source has no place now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unplaced {
    /// Its device is not linked now.
    Offline,
    /// The source is gone, or names no folder.
    Nowhere,
    /// The Hub could not tell (its store); the text is logged and stored.
    Failed(String),
}

impl Unplaced {
    fn text(&self) -> String {
        match self {
            Unplaced::Offline => OFFLINE.into(),
            Unplaced::Nowhere => NO_FOLDER.into(),
            Unplaced::Failed(e) => format!("扫描文件夹时出错：{e}"),
        }
    }
}

/// The Hub's router, as the folder source needs it. Knowledge tools never get it: they reach
/// documents through the knowledge store only.
#[async_trait]
pub trait FolderPlaces: Send + Sync {
    /// The place of folder source `source`. `Ok(None)`: no Node holds it here (read on the Hub).
    /// `scanning`: a scan follows, so the Node must already allow the folder (a source just added).
    async fn place(&self, source: &str, scanning: bool) -> Result<Option<FolderPlace>, Unplaced>;
}

/// The Node side of the folder sources, shared by the scan and [`super::folder::FolderLoader`].
#[derive(Default)]
pub(crate) struct NodeFolders {
    places: OnceLock<Arc<dyn FolderPlaces>>,
    /// The version a queued document's file had in the scan that queued it: its read answers
    /// `conflict` once the file changed. In memory only (after a restart a read takes what is there).
    versions: Mutex<HashMap<String, Revision>>,
}

impl NodeFolders {
    pub(crate) fn set(&self, p: Arc<dyn FolderPlaces>) -> bool {
        self.places.set(p).is_ok()
    }

    /// `None`: no router wired (the Hub's local Node is off), the folder is read on the Hub.
    pub(crate) fn places(&self) -> Option<&Arc<dyn FolderPlaces>> {
        self.places.get()
    }

    pub(crate) fn remember(&self, versions: impl IntoIterator<Item = (String, Revision)>) {
        let mut v = self.versions.lock().unwrap_or_else(|e| e.into_inner());
        v.extend(versions);
    }

    fn take_version(&self, doc: &str) -> Option<Revision> {
        let mut v = self.versions.lock().unwrap_or_else(|e| e.into_inner());
        v.remove(doc)
    }

    /// Reads one document's file through its Node.
    pub(crate) async fn load(&self, source: &str, doc: &str, rel: &str) -> NodeRead {
        let Some(places) = self.places() else {
            return NodeRead::Hub;
        };
        let place = match places.place(source, false).await {
            Ok(Some(p)) => p,
            Ok(None) => return NodeRead::Hub,
            Err(Unplaced::Nowhere) => return NodeRead::Refused(NO_FOLDER),
            Err(_) => return NodeRead::Offline,
        };
        let expected = self.take_version(doc);
        read(&place, rel, expected).await
    }
}

/// What reading one file through a Node gave.
#[derive(Debug)]
pub(crate) enum NodeRead {
    Bytes(Vec<u8>),
    Gone,
    TooBig,
    Refused(&'static str),
    /// It changed since the scan: read it again.
    Changed,
    /// Its device could not be reached: leave it for the next scan.
    Offline,
    /// No Node holds it here after all: read it on the Hub.
    Hub,
}

/// `place.root` joined with `rel` (a document's `ref`).
fn under(place: &RootPath, rel: &str) -> Option<RootPath> {
    let joined = match place.rel.as_str() {
        "" => rel.to_string(),
        base => format!("{base}/{rel}"),
    };
    Some(RootPath {
        root: place.root.clone(),
        rel: RelPath::new(joined).ok()?,
    })
}

async fn read(place: &FolderPlace, rel: &str, expected: Option<Revision>) -> NodeRead {
    let Some(at) = under(&place.root, rel).filter(|_| plain(rel)) else {
        return NodeRead::Refused(super::folder::OUTSIDE);
    };
    let mut bytes = Vec::new();
    loop {
        let q = FileRead {
            path: FileAddress::Root(at.clone()),
            expected: expected.clone(),
            offset: bytes.len() as u64,
            max: CHUNK,
        };
        let chunk = match place.files.read_file(q).await {
            Ok(c) => c,
            Err(e) => return refused_read(place, &at, e).await,
        };
        let got = chunk.bytes.len();
        bytes.extend_from_slice(&chunk.bytes);
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return NodeRead::TooBig;
        }
        if chunk.eof {
            return NodeRead::Bytes(bytes);
        }
        if got == 0 {
            // no progress and no end: never loop on it
            return NodeRead::Refused(UNREADABLE);
        }
    }
}

/// Only plain relative names (no `..`, no absolute path), as the Hub's own read allows.
fn plain(rel: &str) -> bool {
    !rel.is_empty()
        && !rel.starts_with('/')
        && rel.split('/').all(|c| !matches!(c, "" | "." | ".."))
}

async fn refused_read(place: &FolderPlace, at: &RootPath, e: ApiError) -> NodeRead {
    match e.code {
        ErrorCode::Conflict => NodeRead::Changed,
        ErrorCode::Offline => NodeRead::Offline,
        ErrorCode::Denied => NodeRead::Refused(super::folder::OUTSIDE),
        // missing, or not a file: only a listing tells which
        ErrorCode::Invalid => match gone(place, at).await {
            Ok(true) => NodeRead::Gone,
            Ok(false) => NodeRead::Refused(UNREADABLE),
            Err(()) => NodeRead::Offline,
        },
        _ => {
            tracing::debug!("kb.folder: read through the Node: {}", e.safe_detail);
            NodeRead::Refused(UNREADABLE)
        }
    }
}

/// Is `at` really not there as a file? Lists the nearest directory above it that can be listed
/// and looks for the next step down. `Ok(false)` when it cannot tell (nothing could be listed);
/// `Err` when the Node went away.
async fn gone(place: &FolderPlace, at: &RootPath) -> Result<bool, ()> {
    let steps: Vec<&str> = at.rel.as_str().split('/').collect();
    for depth in (0..steps.len()).rev() {
        let dir = steps[..depth].join("/");
        let Ok(dir_rel) = RelPath::new(dir) else {
            return Ok(false);
        };
        let listed = list(
            place,
            RootPath {
                root: at.root.clone(),
                rel: dir_rel,
            },
        )
        .await;
        let items = match listed {
            Ok(items) => items,
            Err(e) if e.code == ErrorCode::Offline => return Err(()),
            Err(e) if e.code == ErrorCode::Invalid => continue, // not there either: look higher
            Err(_) => return Ok(false),
        };
        let next = steps[..=depth].join("/");
        return Ok(match items.iter().find(|f| f.path.as_str() == next) {
            None => true,
            // a directory where the file was is no file (the Hub's own read says the same)
            Some(f) => depth + 1 == steps.len() && f.is_dir,
        });
    }
    Ok(false)
}

async fn list(place: &FolderPlace, at: RootPath) -> Result<Vec<FileInfo>, ApiError> {
    let mut out = Vec::new();
    let mut cursor: Option<Cursor> = None;
    loop {
        let page = place
            .files
            .list_root(
                at.clone(),
                PageRequest {
                    cursor: cursor.clone(),
                    limit: PAGE,
                },
            )
            .await?;
        out.extend(page.items);
        match page.next {
            Some(n) if Some(&n) != cursor.as_ref() => cursor = Some(n),
            _ => return Ok(out),
        }
    }
}

/// A scan through the Node, in the words of the Hub's own walk, with each file's version.
pub(crate) struct Scanned {
    pub walk: scan::Walk,
    pub versions: HashMap<String, Revision>,
}

/// The stored reason a scan through the Node failed.
fn scan_failed(e: &ApiError) -> String {
    tracing::info!(
        "kb.folder_scan: through the Node: {:?} {}",
        e.code,
        e.safe_detail
    );
    match e.code {
        ErrorCode::Offline => OFFLINE.into(),
        ErrorCode::Denied => "设备不允许读取这个文件夹".into(),
        ErrorCode::Busy | ErrorCode::Exhausted => "设备正忙，稍后会再扫描".into(),
        ErrorCode::Expired | ErrorCode::OutcomeUnknown | ErrorCode::Stale => {
            "扫描中断了，稍后会再扫描".into()
        }
        ErrorCode::Invalid => "文件夹读不出来".into(),
        _ => "扫描文件夹时出错".into(),
    }
}

fn interrupted() -> String {
    "扫描中断了，稍后会再扫描".into()
}

/// Scans `place` on its Node and reads every page. Any failure before the last page is an
/// error: the caller then touches no document.
pub(crate) async fn scan(
    place: &FolderPlace,
    include: Vec<String>,
    exclude: Vec<String>,
) -> Result<Scanned, String> {
    let op = OperationId::new(steve_store::uid("kbscan")).map_err(|e| e.to_string())?;
    let now = steve_store::now();
    let c = Command {
        id: op.clone(),
        issued_at: Timestamp(now),
        accept_before: Timestamp(now + 60_000),
        grant: GrantId::new("hub").map_err(|e| e.to_string())?,
        body: ScanSpec {
            root: place.root.clone(),
            include,
            exclude,
            limits: ScanLimits {
                max_files: scan::MAX_FILES as u32,
                max_file_bytes: MAX_FILE_BYTES,
            },
        },
    };
    let mut receipt = place.files.scan(c).await.map_err(|e| scan_failed(&e))?;
    let deadline = tokio::time::Instant::now() + SCAN_WAIT;
    let mut wait = Duration::from_millis(20);
    loop {
        match &receipt.state {
            Observed::Finished {
                outcome: Outcome::Succeeded,
                ..
            } => break,
            Observed::Finished { error: Some(e), .. } => return Err(scan_failed(e)),
            Observed::Finished { .. } | Observed::Unknown { .. } => return Err(interrupted()),
            _ => {}
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(interrupted());
        }
        tokio::time::sleep(wait).await;
        wait = (wait * 2).min(Duration::from_secs(1));
        receipt = place
            .control
            .operation(op.clone())
            .await
            .map_err(|e| scan_failed(&e))?;
    }
    let mut pages = Vec::new();
    let mut cursor: Option<Cursor> = None;
    loop {
        let p = place
            .files
            .scan_page(
                op.clone(),
                PageRequest {
                    cursor: cursor.clone(),
                    limit: PAGE,
                },
            )
            .await
            .map_err(|e| scan_failed(&e))?;
        if !p.is_consistent()
            || pages
                .first()
                .is_some_and(|f: &steve_node_api::ScanPage| f.manifest != p.manifest)
        {
            return Err(interrupted());
        }
        let next = p.next.clone();
        pages.push(p);
        match next {
            Some(n) if Some(&n) != cursor.as_ref() => cursor = Some(n),
            Some(_) => return Err(interrupted()),
            None => break,
        }
    }
    Ok(walk_of(&pages))
}

/// A `kb_docs.mtime` for a file whose Node does not tell its modification time: it changes
/// whenever the version does.
fn stamp(v: &Revision) -> i64 {
    let h = steve_store::sha(v.as_str());
    i64::from_str_radix(&h[..15], 16).unwrap_or(0)
}

/// The pages of one scan as the Hub's walk ([`scan::Walk`]) and the versions of its files.
pub(crate) fn walk_of(pages: &[steve_node_api::ScanPage]) -> Scanned {
    let mut w = scan::Walk::default();
    let mut versions = HashMap::new();
    for e in pages.iter().flat_map(|p| &p.entries).filter(|e| !e.is_dir) {
        let entry = Entry {
            rel: e.path.as_str().to_string(),
            path: PathBuf::new(),
            size: e.size,
            mtime: e.mtime.map_or_else(|| stamp(&e.version), |t| t.0),
        };
        versions.insert(entry.rel.clone(), e.version.clone());
        if e.too_big || e.size > MAX_FILE_BYTES {
            w.too_big.push(entry);
        } else {
            w.files.push(entry);
        }
    }
    for i in pages.iter().flat_map(|p| &p.issues) {
        match i {
            ScanIssue::Unreadable { path: Some(_) } => w.gap(Gap::Unreadable),
            ScanIssue::Unreadable { path: None } => w.gap(Gap::Entry),
            ScanIssue::OverLimit { count } => {
                w.over_limit += *count as usize;
                w.gap(Gap::OverLimit);
            }
            ScanIssue::Interrupted => w.gap(Gap::Interrupted),
            ScanIssue::Held { path, why } => {
                let why = match why {
                    HeldWhy::Outside => {
                        w.links_refused += 1;
                        Held::Outside
                    }
                    HeldWhy::Denied => Held::Denied,
                    HeldWhy::Unreadable => Held::Unreadable,
                };
                w.held.push((path.as_str().to_string(), why));
            }
        }
    }
    // only the last page may say complete; one that does not, with no gap named, is cut short
    if !pages.last().is_some_and(|p| p.complete) && w.complete() {
        w.gap(Gap::Interrupted);
    }
    Scanned { walk: w, versions }
}

/// The text of a source with no place now.
pub(crate) fn unplaced(u: &Unplaced) -> String {
    u.text()
}
