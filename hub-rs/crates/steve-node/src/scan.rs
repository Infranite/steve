//! Files on this machine for the Hub: a scan of a tree, the listing of one directory and bounded
//! reads. The scan is the knowledge folder's walk (`steve_knowledge::scan::walk`), told in the
//! contract's words: read-only, never leaving the scanned directory, links resolved and dropped when
//! they point outside it, linked directories not entered, the patterns of [`steve_node_api::glob`].
//! What it could not see is a gap ([`ScanIssue::hides_files`]); nothing it reports is a reason to
//! take a file for gone unless the scan is complete.

use std::fs;
use std::io::{self, Read, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use sha2::Digest as _;
use steve_node_api::glob::{Rules, never_entered};
use steve_node_api::*;

/// A scan's issues take at most this much (encoded); past it the rest is one gap, so a hold that
/// did not fit never lets a file be taken for gone.
const MAX_ISSUE_BYTES: usize = 64 * 1024;
/// A page of a scan or a listing stays well inside a frame of the transport.
const PAGE_BYTES: usize = 160 * 1024;
/// Entries per page at most, whatever the Hub asks.
pub(crate) const MAX_PAGE: u32 = 1000;
/// Bytes of one read at most (hex doubles them on the wire).
pub(crate) const MAX_READ: u32 = 96 * 1024;
/// Wanted files a scan may be asked to list at most.
pub(crate) const MAX_SCAN_FILES: u32 = 100_000;

/// What one scan saw, taken once and read page by page.
#[derive(Debug)]
pub(crate) struct Manifest {
    pub revision: Revision,
    pub entries: Vec<FileInfo>,
    pub issues: Vec<ScanIssue>,
}

/// The version of a file as this Node tells it: its real path and what its metadata says of its
/// content (device, inode, size, modification and change time). The same in a scan, a listing and a
/// read, so a read with a version from a scan answers `conflict` once the file changed.
pub(crate) fn version(real: &Path, m: &fs::Metadata) -> Revision {
    let signature = format!(
        "{}\0{}:{}:{}:{}:{}:{}:{}",
        real.to_string_lossy(),
        m.dev(),
        m.ino(),
        m.len(),
        m.mtime(),
        m.mtime_nsec(),
        m.ctime(),
        m.ctime_nsec()
    );
    let h = hex::encode(sha2::Sha256::digest(signature.as_bytes()));
    Revision::new(format!("v{}", &h[..32])).expect("hex is an id")
}

fn mtime_ms(m: &fs::Metadata) -> i64 {
    m.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_millis() as i64)
}

/// `base/rel` as a contract path, or `None` when it cannot be one (a backslash in a name…).
fn rel_path(base: &str, rel: &str) -> Option<RelPath> {
    if base.is_empty() {
        RelPath::new(rel).ok()
    } else {
        RelPath::new(format!("{base}/{rel}")).ok()
    }
}

struct Walk {
    entries: Vec<FileInfo>,
    wanted: u64,
    over: u64,
    issues: Vec<ScanIssue>,
    issue_bytes: usize,
    issues_cut: bool,
}

impl Walk {
    fn issue(&mut self, i: ScanIssue) {
        if self.issues_cut {
            return;
        }
        let n = serde_json::to_vec(&i).map_or(usize::MAX, |b| b.len());
        if self.issue_bytes.saturating_add(n) > MAX_ISSUE_BYTES {
            self.issues_cut = true;
            self.issues.push(ScanIssue::Unreadable { path: None });
            return;
        }
        self.issue_bytes += n;
        self.issues.push(i);
    }

    /// An entry that may be a directory could not be read.
    fn gap(&mut self, path: Option<RelPath>) {
        self.issue(ScanIssue::Unreadable { path });
    }

    /// A wanted path that is there but is not listed.
    fn hold(&mut self, rules: &Rules, rel: &str, path: Option<RelPath>, why: HeldWhy) {
        if !rules.wants_file(rel) {
            return;
        }
        match path {
            Some(path) => self.issue(ScanIssue::Held { path, why }),
            // it cannot be named: say the scan may hide a file instead
            None => self.gap(None),
        }
    }
}

/// Walks `dir` (canonical, `base` below its root) in name order, as the knowledge folder's walk
/// does. Paths in the manifest are `base/…`, relative to the root. An unreadable `dir` is an error;
/// `stop` is asked before each directory and cuts the scan short (an `interrupted` gap).
pub(crate) fn walk(
    dir: &Path,
    base: &str,
    rules: &Rules,
    limits: ScanLimits,
    stop: &dyn Fn() -> bool,
) -> io::Result<Manifest> {
    let mut out = Walk {
        entries: vec![],
        wanted: 0,
        over: 0,
        issues: vec![],
        issue_bytes: 0,
        issues_cut: false,
    };
    let mut stack: Vec<(PathBuf, String)> = vec![(dir.to_path_buf(), String::new())];
    let mut first = true;
    let mut interrupted = false;
    while let Some((at, prefix)) = stack.pop() {
        if stop() {
            interrupted = true;
            break;
        }
        let rd = match fs::read_dir(&at) {
            Ok(rd) => rd,
            Err(e) if first => return Err(e),
            Err(e) => {
                tracing::debug!(?at, "node.scan: unreadable directory: {e}");
                out.gap(rel_path(base, &prefix));
                continue;
            }
        };
        first = false;
        let mut entries: Vec<fs::DirEntry> = Vec::new();
        for e in rd {
            match e {
                Ok(e) => entries.push(e),
                Err(_) => out.gap(None),
            }
        }
        entries.sort_by_key(|e| e.file_name());
        let mut subdirs = Vec::new();
        for e in entries {
            let Some(name) = e.file_name().to_str().map(str::to_string) else {
                continue; // not UTF-8: no file of a source can have that name
            };
            let rel = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            let path = rel_path(base, &rel);
            let Ok(ft) = e.file_type() else {
                out.gap(path); // could be a directory
                continue;
            };
            let (real, meta) = if ft.is_symlink() {
                match fs::canonicalize(e.path()) {
                    Ok(t) if t.starts_with(dir) => match fs::metadata(&t) {
                        Ok(m) if m.is_file() => (t, m),
                        Ok(_) => continue, // linked directories are not entered
                        Err(_) => {
                            out.hold(rules, &rel, path, HeldWhy::Unreadable);
                            continue;
                        }
                    },
                    Ok(_) => {
                        out.hold(rules, &rel, path, HeldWhy::Outside);
                        continue;
                    }
                    // dangling: the file is gone
                    Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                    Err(_) => {
                        out.hold(rules, &rel, path, HeldWhy::Unreadable);
                        continue;
                    }
                }
            } else if ft.is_dir() {
                if never_entered(&name) || rules.skips_dir(&rel) {
                    continue;
                }
                if path.is_none() {
                    out.gap(None); // a directory that cannot be named hides what is in it
                    continue;
                }
                subdirs.push((e.path(), rel));
                continue;
            } else if ft.is_file() {
                match e.metadata() {
                    Ok(m) => (e.path(), m),
                    Err(_) => {
                        out.hold(rules, &rel, path, HeldWhy::Unreadable);
                        continue;
                    }
                }
            } else {
                continue; // sockets, fifos, devices
            };
            if !rules.wants_file(&rel) {
                continue;
            }
            let Some(path) = path else {
                out.gap(None); // wanted, but it cannot be named
                continue;
            };
            let too_big = meta.len() > limits.max_file_bytes;
            if !too_big && out.wanted >= u64::from(limits.max_files) {
                out.over += 1;
                continue;
            }
            if !too_big {
                out.wanted += 1;
            }
            out.entries.push(FileInfo {
                path,
                is_dir: false,
                size: meta.len(),
                version: version(&real, &meta),
                mtime: Some(Timestamp(mtime_ms(&meta))),
                too_big,
            });
        }
        // reversed, so the stack pops them in name order
        stack.extend(subdirs.into_iter().rev());
    }
    let mut issues = out.issues;
    if out.over > 0 {
        issues.push(ScanIssue::OverLimit { count: out.over });
    }
    if interrupted {
        issues.push(ScanIssue::Interrupted);
    }
    // the same tree gives the same manifest
    let digest = digest_of(&(&out.entries, &issues)).expect("plain data serializes");
    let revision = Revision::new(digest.to_string()).expect("a digest is an id");
    Ok(Manifest {
        revision,
        entries: out.entries,
        issues,
    })
}

fn tag(scan: &OperationId) -> String {
    hex::encode(&sha2::Sha256::digest(scan.as_str().as_bytes())[..4])
}

/// One page of `m`, read under its scan's id. Cursors name their scan: one of another scan is
/// `invalid`. Issues come with the last page, and the last page is the one they fit on.
pub(crate) fn page(m: &Manifest, scan: &OperationId, req: &PageRequest) -> ApiResult<ScanPage> {
    let tag = tag(scan);
    let from = match &req.cursor {
        None => 0,
        Some(c) => c
            .as_str()
            .strip_prefix(&format!("s{tag}."))
            .and_then(|n| n.parse::<usize>().ok())
            .filter(|n| *n <= m.entries.len())
            .ok_or_else(|| ApiError::new(ErrorCode::Invalid, "not a cursor of this scan"))?,
    };
    let (mut to, mut bytes) = fill(&m.entries, from, req.limit);
    let issue_bytes = serde_json::to_vec(&m.issues).map_or(0, |b| b.len());
    if to == m.entries.len() {
        // the issues go on the last page: leave entries for one more page if they do not fit
        // (one entry and every issue always do)
        while to > from + 1 && bytes + issue_bytes > PAGE_BYTES {
            to -= 1;
            bytes -= encoded(&m.entries[to]);
        }
    }
    let last = to == m.entries.len();
    let next = if last {
        None
    } else {
        Some(Cursor::new(format!("s{tag}.{to}")).expect("a cursor"))
    };
    Ok(ScanPage {
        scan: scan.clone(),
        manifest: m.revision.clone(),
        entries: m.entries[from..to].to_vec(),
        next,
        complete: last && !m.issues.iter().any(ScanIssue::hides_files),
        issues: if last { m.issues.clone() } else { vec![] },
    })
}

fn encoded(f: &FileInfo) -> usize {
    serde_json::to_vec(f).map_or(PAGE_BYTES, |b| b.len() + 1)
}

/// From `from`: as many items as `limit` (at most [`MAX_PAGE`]) and the page bytes allow, at
/// least one. Returns the end and the bytes taken.
fn fill(items: &[FileInfo], from: usize, limit: u32) -> (usize, usize) {
    let limit = limit.clamp(1, MAX_PAGE) as usize;
    let (mut to, mut bytes) = (from, 0);
    while to < items.len() && to - from < limit {
        let n = encoded(&items[to]);
        if to > from && bytes + n > PAGE_BYTES {
            break;
        }
        bytes += n;
        to += 1;
    }
    (to, bytes)
}

/// What is right in `dir` (canonical, `base` below the root or the workspace), in name order: its
/// directories and files. Links are listed only as the file inside `inside` they point to; linked
/// directories are not followed (a person browsing gets no way out through them), links out and
/// dangling ones not at all. Names that cannot be a contract path are left out.
pub(crate) fn list(dir: &Path, base: &str, inside: &Path) -> io::Result<Vec<FileInfo>> {
    let mut entries: Vec<fs::DirEntry> = fs::read_dir(dir)?.filter_map(Result::ok).collect();
    entries.sort_by_key(|e| e.file_name());
    let mut out = vec![];
    for e in entries {
        let Some(name) = e.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let Some(path) = rel_path(base, &name) else {
            continue;
        };
        let Ok(ft) = e.file_type() else { continue };
        let (real, meta) = if ft.is_symlink() {
            match fs::canonicalize(e.path()) {
                Ok(t) if t.starts_with(inside) => match fs::metadata(&t) {
                    Ok(m) if m.is_file() => (t, m),
                    _ => continue,
                },
                _ => continue,
            }
        } else if ft.is_dir() || ft.is_file() {
            match e.metadata() {
                Ok(m) => (e.path(), m),
                Err(_) => continue,
            }
        } else {
            continue;
        };
        out.push(FileInfo {
            path,
            is_dir: meta.is_dir(),
            size: if meta.is_dir() { 0 } else { meta.len() },
            version: version(&real, &meta),
            mtime: Some(Timestamp(mtime_ms(&meta))),
            too_big: false,
        });
    }
    Ok(out)
}

/// One page of a listing (cursors are offsets, `l<n>`).
pub(crate) fn list_page(items: Vec<FileInfo>, req: &PageRequest) -> ApiResult<Page<FileInfo>> {
    let from = match &req.cursor {
        None => 0,
        Some(c) => c
            .as_str()
            .strip_prefix('l')
            .and_then(|n| n.parse::<usize>().ok())
            .filter(|n| *n <= items.len())
            .ok_or_else(|| ApiError::new(ErrorCode::Invalid, "not a listing cursor"))?,
    };
    let (to, _) = fill(&items, from, req.limit);
    let next = (to < items.len()).then(|| Cursor::new(format!("l{to}")).expect("a cursor"));
    Ok(Page {
        items: items[from..to].to_vec(),
        next,
    })
}

/// A path inside `base` (canonical): resolved, links included, and still inside it.
pub(crate) fn inside(base: &Path, rel: &RelPath) -> ApiResult<PathBuf> {
    let real = base
        .join(rel.as_str())
        .canonicalize()
        .map_err(|_| ApiError::new(ErrorCode::Invalid, "no such file"))?;
    if !real.starts_with(base) {
        return Err(ApiError::new(ErrorCode::Denied, "outside the root"));
    }
    Ok(real)
}

/// Up to `max` bytes (at most [`MAX_READ`]) of the file at `real` from `offset`. A file that is not
/// at `expected` any more is a `conflict`: read it again from the start.
pub(crate) fn read(
    real: &Path,
    expected: Option<&Revision>,
    offset: u64,
    max: u32,
) -> ApiResult<ReadChunk> {
    let io = |e: io::Error| ApiError::new(ErrorCode::Invalid, format!("cannot read: {e}"));
    let mut f = fs::File::open(real).map_err(io)?;
    let meta = f.metadata().map_err(io)?;
    if !meta.is_file() {
        return Err(ApiError::new(ErrorCode::Invalid, "not a file"));
    }
    if expected.is_some_and(|v| *v != version(real, &meta)) {
        return Err(ApiError::new(ErrorCode::Conflict, "the file changed"));
    }
    let offset = offset.min(meta.len());
    f.seek(SeekFrom::Start(offset)).map_err(io)?;
    let mut bytes = Vec::new();
    f.take(u64::from(max.min(MAX_READ)))
        .read_to_end(&mut bytes)
        .map_err(io)?;
    Ok(ReadChunk {
        offset,
        eof: offset + bytes.len() as u64 >= meta.len(),
        bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pages stay inside a frame even with long paths and the most issues, and every entry comes
    /// through once.
    #[test]
    fn pages_fit_a_frame_and_lose_nothing() {
        let long = "x".repeat(3000);
        let entries: Vec<FileInfo> = (0..300)
            .map(|i| FileInfo {
                path: RelPath::new(format!("{long}/{i:04}.md")).unwrap(),
                is_dir: false,
                size: 1,
                version: Revision::new("v1").unwrap(),
                mtime: Some(Timestamp(1)),
                too_big: false,
            })
            .collect();
        let mut w = Walk {
            entries: vec![],
            wanted: 0,
            over: 0,
            issues: vec![],
            issue_bytes: 0,
            issues_cut: false,
        };
        for i in 0..100 {
            w.issue(ScanIssue::Held {
                path: RelPath::new(format!("{long}/{i}.md")).unwrap(),
                why: HeldWhy::Outside,
            });
        }
        assert!(w.issues_cut, "the issues are cut at their budget");
        assert_eq!(w.issues.last(), Some(&ScanIssue::Unreadable { path: None }));
        let m = Manifest {
            revision: Revision::new("m").unwrap(),
            entries: entries.clone(),
            issues: w.issues,
        };
        let scan = OperationId::new("op_s").unwrap();
        let (mut seen, mut cursor) = (vec![], None);
        loop {
            let p = page(
                &m,
                &scan,
                &PageRequest {
                    cursor,
                    limit: 1000,
                },
            )
            .unwrap();
            assert!(serde_json::to_vec(&p).unwrap().len() < 288 * 1024);
            assert!(p.is_consistent() && !p.complete);
            seen.extend(p.entries);
            cursor = p.next;
            if cursor.is_none() {
                assert!(!p.issues.is_empty());
                break;
            }
            assert!(p.issues.is_empty());
        }
        assert_eq!(seen, entries);
    }
}
