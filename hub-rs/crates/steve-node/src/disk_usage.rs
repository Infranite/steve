//! Metadata-only, no-follow disk observations on this Node. Callers authorize the roots.
//!
//! `Complete` means there were no known gaps, not an atomic filesystem snapshot or exclusive
//! physical allocation: reflinks can share blocks, and writers may change files after sampling.
//! Incomplete observations must never become zero-byte estimates or historical p90 samples.

use std::collections::HashMap;
use std::ffi::{CStr, CString, OsStr};
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// Traversal work limits, not disk quotas. Exhaustion is an incomplete observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Budget {
    pub entries: u64,
    pub depth: usize,
    pub elapsed: Duration,
}

/// Both logical bytes and du-style allocation, with each inode counted once per measurement.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub apparent_bytes: u64,
    pub allocated_bytes: u64,
    pub inodes: u64,
}

impl Usage {
    fn include(&mut self, stamp: Stamp) -> Result<(), Incomplete> {
        let size = u64::try_from(stamp.size).map_err(|_| Incomplete::Overflow)?;
        let blocks = u64::try_from(stamp.blocks).map_err(|_| Incomplete::Overflow)?;
        let next = Self {
            apparent_bytes: self
                .apparent_bytes
                .checked_add(size)
                .ok_or(Incomplete::Overflow)?,
            allocated_bytes: blocks
                .checked_mul(512)
                .and_then(|n| self.allocated_bytes.checked_add(n))
                .ok_or(Incomplete::Overflow)?,
            inodes: self.inodes.checked_add(1).ok_or(Incomplete::Overflow)?,
        };
        *self = next;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Incomplete {
    NoRoots,
    InvalidPath,
    Io,
    Changed,
    CrossDevice,
    EntryBudget,
    DepthBudget,
    TimeBudget,
    Overflow,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Complete(Usage),
    Incomplete { observed: Usage, reason: Incomplete },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Measurement {
    pub outcome: Outcome,
    /// Roots proven absent during this observation, distinct from an empty input or IO error.
    pub missing_roots: u64,
    pub entries: u64,
    pub elapsed: Duration,
    /// All complete observations belong to one device; roots on different volumes are incomplete.
    pub device: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    device: u64,
    inode: u64,
    mode: u32,
    size: i64,
    blocks: i64,
    links: u64,
    mtime: (i64, i64),
    ctime: (i64, i64),
}

impl Stamp {
    // libc's stat integer widths differ between macOS and Linux.
    #[allow(clippy::unnecessary_cast)]
    fn from_stat(s: libc::stat) -> Self {
        Self {
            device: s.st_dev as u64,
            inode: s.st_ino as u64,
            mode: s.st_mode as u32,
            size: s.st_size as i64,
            blocks: s.st_blocks as i64,
            links: s.st_nlink as u64,
            mtime: (s.st_mtime as i64, s.st_mtime_nsec as i64),
            ctime: (s.st_ctime as i64, s.st_ctime_nsec as i64),
        }
    }

    #[allow(clippy::unnecessary_cast)]
    fn directory(self) -> bool {
        self.mode & libc::S_IFMT as u32 == libc::S_IFDIR as u32
    }
}

fn fstat(fd: RawFd) -> io::Result<Stamp> {
    let mut s = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: fd is borrowed from a live File; fstat initializes stat on success.
    if unsafe { libc::fstat(fd, s.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fstat succeeded.
    Ok(Stamp::from_stat(unsafe { s.assume_init() }))
}

fn lstat_at(fd: RawFd, name: &CStr) -> io::Result<Stamp> {
    let mut s = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: name is NUL terminated and fd belongs to the live directory frame.
    if unsafe { libc::fstatat(fd, name.as_ptr(), s.as_mut_ptr(), libc::AT_SYMLINK_NOFOLLOW) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fstatat succeeded.
    Ok(Stamp::from_stat(unsafe { s.assume_init() }))
}

fn open_dir_at(fd: RawFd, name: &CStr) -> io::Result<File> {
    // SAFETY: fd is AT_FDCWD with an absolute root or a live directory; name is NUL terminated.
    let opened = unsafe {
        libc::openat(
            fd,
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if opened < 0 {
        Err(io::Error::last_os_error())
    } else {
        // SAFETY: opened was returned by openat and has no other owner.
        Ok(unsafe { File::from_raw_fd(opened) })
    }
}

fn anchor(
    path: &Path,
    check: &dyn Fn() -> Result<(), Incomplete>,
) -> Result<Option<File>, Incomplete> {
    if !path.is_absolute()
        || path
            .components()
            .any(|c| !matches!(c, Component::RootDir | Component::Normal(_)))
    {
        return Err(Incomplete::InvalidPath);
    }
    check()?;
    let mut dir = open_dir_at(libc::AT_FDCWD, c"/").map_err(|_| Incomplete::Io)?;
    let mut at = PathBuf::from("/");
    for c in path.components() {
        if let Component::Normal(name) = c {
            check()?;
            if crate::scan::opened_path(&dir).map_err(|_| Incomplete::Io)? != at {
                return Err(Incomplete::Changed);
            }
            let before = fstat(dir.as_raw_fd()).map_err(|_| Incomplete::Io)?;
            let name = CString::new(name.as_bytes()).map_err(|_| Incomplete::InvalidPath)?;
            dir = match open_dir_at(dir.as_raw_fd(), &name) {
                Ok(dir) => dir,
                Err(e) if e.raw_os_error() == Some(libc::ENOENT) => {
                    if crate::scan::opened_path(&dir).map_err(|_| Incomplete::Io)? != at
                        || fstat(dir.as_raw_fd()).map_err(|_| Incomplete::Io)? != before
                    {
                        return Err(Incomplete::Changed);
                    }
                    check()?;
                    return Ok(None);
                }
                Err(_) => return Err(Incomplete::Io),
            };
            at.push(OsStr::from_bytes(name.as_bytes()));
        }
    }
    check()?;
    Ok(Some(dir))
}

struct Directory {
    file: File,
    stream: *mut libc::DIR,
    path: PathBuf,
    before: Stamp,
    depth: usize,
}

impl Drop for Directory {
    fn drop(&mut self) {
        // SAFETY: stream is exclusively owned by this Directory after fdopendir succeeds.
        unsafe { libc::closedir(self.stream) };
    }
}

impl Directory {
    fn new(file: File, path: PathBuf, before: Stamp, depth: usize) -> Result<Self, Incomplete> {
        if fstat(file.as_raw_fd()).map_err(|_| Incomplete::Io)? != before
            || crate::scan::opened_path(&file).map_err(|_| Incomplete::Io)? != path
        {
            return Err(Incomplete::Changed);
        }
        // SAFETY: file is live. F_DUPFD_CLOEXEC returns a distinct owned descriptor.
        let copy = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
        if copy < 0 {
            return Err(Incomplete::Io);
        }
        // SAFETY: copy is a directory descriptor. On success fdopendir takes its ownership.
        let stream = unsafe { libc::fdopendir(copy) };
        if stream.is_null() {
            // SAFETY: fdopendir failed, leaving copy owned here.
            unsafe { libc::close(copy) };
            return Err(Incomplete::Io);
        }
        Ok(Self {
            file,
            stream,
            path,
            before,
            depth,
        })
    }

    fn unchanged(&self) -> Result<(), Incomplete> {
        if fstat(self.file.as_raw_fd()).map_err(|_| Incomplete::Io)? != self.before
            || crate::scan::opened_path(&self.file).map_err(|_| Incomplete::Io)? != self.path
        {
            Err(Incomplete::Changed)
        } else {
            Ok(())
        }
    }

    fn next(
        &mut self,
        check: &dyn Fn() -> Result<(), Incomplete>,
    ) -> Result<Option<CString>, Incomplete> {
        loop {
            check()?;
            self.unchanged()?;
            // SAFETY: this synchronous traversal owns the stream. errno is thread-local and is
            // checked immediately after readdir to distinguish end-of-directory from failure.
            let entry = unsafe {
                *errno() = 0;
                let entry = libc::readdir(self.stream);
                if entry.is_null() && *errno() != 0 {
                    return Err(Incomplete::Io);
                }
                entry
            };
            if entry.is_null() {
                self.unchanged()?;
                return Ok(None);
            }
            // SAFETY: readdir's NUL-terminated d_name is valid until its next call. Copy it now.
            let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_owned();
            if name.as_bytes() != b"." && name.as_bytes() != b".." {
                return Ok(Some(name));
            }
        }
    }
}

#[cfg(target_os = "macos")]
unsafe fn errno() -> *mut libc::c_int {
    // SAFETY: libc returns this thread's errno address.
    unsafe { libc::__error() }
}

#[cfg(not(target_os = "macos"))]
unsafe fn errno() -> *mut libc::c_int {
    // SAFETY: libc returns this thread's errno address (Linux Nodes).
    unsafe { libc::__errno_location() }
}

struct Walk<'a> {
    usage: Usage,
    seen: HashMap<(u64, u64), Stamp>,
    budget: Budget,
    started: Instant,
    entries: u64,
    missing: u64,
    device: Option<u64>,
    cancelled: &'a dyn Fn() -> bool,
}

impl Walk<'_> {
    fn check(&self) -> Result<(), Incomplete> {
        if (self.cancelled)() {
            Err(Incomplete::Cancelled)
        } else if self.started.elapsed() >= self.budget.elapsed {
            Err(Incomplete::TimeBudget)
        } else {
            Ok(())
        }
    }

    fn visit(&mut self, stamp: Stamp) -> Result<bool, Incomplete> {
        self.check()?;
        if self.entries >= self.budget.entries {
            return Err(Incomplete::EntryBudget);
        }
        self.entries += 1;
        let key = (stamp.device, stamp.inode);
        if let Some(old) = self.seen.get(&key) {
            return if *old == stamp {
                Ok(false)
            } else {
                Err(Incomplete::Changed)
            };
        }
        self.usage.include(stamp)?;
        self.seen.insert(key, stamp);
        Ok(true)
    }

    fn root(&mut self, path: &Path) -> Result<(), Incomplete> {
        self.check()?;
        let Some(file) = anchor(path, &|| self.check())? else {
            self.missing = self.missing.checked_add(1).ok_or(Incomplete::Overflow)?;
            return Ok(());
        };
        let before = fstat(file.as_raw_fd()).map_err(|_| Incomplete::Io)?;
        if crate::scan::opened_path(&file).map_err(|_| Incomplete::Io)? != path {
            return Err(Incomplete::Changed);
        }
        if self.device.is_some_and(|device| device != before.device) {
            return Err(Incomplete::CrossDevice);
        }
        self.device = Some(before.device);
        if !self.visit(before)? {
            return Ok(());
        }
        let device = before.device;
        let mut stack = vec![Directory::new(file, path.to_path_buf(), before, 0)?];
        while let Some(dir) = stack.last_mut() {
            self.check()?;
            let Some(name) = dir.next(&|| self.check())? else {
                stack.pop();
                continue;
            };
            let stamp = lstat_at(dir.file.as_raw_fd(), &name).map_err(|_| Incomplete::Io)?;
            if stamp.device != device {
                return Err(Incomplete::CrossDevice);
            }
            let fresh = self.visit(stamp)?;
            if stamp.directory() && fresh {
                let depth = dir.depth.checked_add(1).ok_or(Incomplete::Overflow)?;
                if depth > self.budget.depth {
                    return Err(Incomplete::DepthBudget);
                }
                let child = open_dir_at(dir.file.as_raw_fd(), &name).map_err(|_| Incomplete::Io)?;
                let path = dir.path.join(OsStr::from_bytes(name.as_bytes()));
                dir.unchanged()?;
                stack.push(Directory::new(child, path, stamp, depth)?);
            } else if lstat_at(dir.file.as_raw_fd(), &name).map_err(|_| Incomplete::Io)? != stamp {
                return Err(Incomplete::Changed);
            }
        }
        Ok(())
    }

    fn finish(self, result: Result<(), Incomplete>) -> Measurement {
        let elapsed = self.started.elapsed();
        // A final readdir/EOF or missing-root check can cross the deadline. Do not report a
        // complete sample after its budget expires. This is cooperative: it cannot interrupt a
        // single blocked filesystem syscall, only stop at the next operation boundary.
        let result = result.and_then(|()| self.check());
        Measurement {
            outcome: match result {
                Ok(()) => Outcome::Complete(self.usage),
                Err(reason) => Outcome::Incomplete {
                    observed: self.usage,
                    reason,
                },
            },
            missing_roots: self.missing,
            entries: self.entries,
            elapsed,
            device: self.device,
        }
    }
}

/// Observe the union of these authorized directories without opening file contents or following
/// links. Root paths must be absolute with no link ancestors, including `/tmp` aliases on macOS.
/// A missing root is recorded explicitly. Empty input, IO errors and budget limits are incomplete.
pub fn measure(paths: &[PathBuf], budget: Budget) -> Measurement {
    measure_cancellable(paths, budget, &|| false)
}

/// Cooperative cancellation at traversal boundaries, including "."/".." entries. A single
/// filesystem syscall cannot be interrupted; cancellation never authorizes replacement work.
pub(crate) fn measure_cancellable(
    paths: &[PathBuf],
    budget: Budget,
    cancelled: &dyn Fn() -> bool,
) -> Measurement {
    let mut walk = Walk {
        usage: Usage::default(),
        seen: HashMap::new(),
        budget,
        started: Instant::now(),
        entries: 0,
        missing: 0,
        device: None,
        cancelled,
    };
    let result = if paths.is_empty() {
        Err(Incomplete::NoRoots)
    } else {
        paths.iter().try_for_each(|path| walk.root(path))
    };
    walk.finish(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::{MetadataExt, symlink};

    fn budget() -> Budget {
        Budget {
            entries: 100_000,
            depth: 64,
            elapsed: Duration::from_secs(10),
        }
    }

    fn complete(paths: &[PathBuf]) -> Usage {
        match measure(paths, budget()).outcome {
            Outcome::Complete(usage) => usage,
            other => panic!("{other:?}"),
        }
    }

    fn reason(paths: &[PathBuf], limits: Budget) -> Incomplete {
        match measure(paths, limits).outcome {
            Outcome::Incomplete { reason, .. } => reason,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn empty_discovery_missing_directory_and_empty_directory_are_distinct() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        assert_eq!(reason(&[], budget()), Incomplete::NoRoots);
        let missing = measure(&[root.join("absent/also-absent")], budget());
        assert_eq!(missing.missing_roots, 1);
        assert_eq!(missing.outcome, Outcome::Complete(Usage::default()));
        assert!(missing.device.is_none());
        let empty = measure(std::slice::from_ref(&root), budget());
        assert_eq!(empty.missing_roots, 0);
        assert!(empty.device.is_some());
        let usage = complete(std::slice::from_ref(&root));
        assert_eq!(usage.inodes, 1);
        assert_eq!(usage.apparent_bytes, root.metadata().unwrap().len());
        assert_eq!(
            usage.allocated_bytes,
            root.metadata().unwrap().blocks() * 512
        );
    }

    #[test]
    fn hardlinks_overlapping_roots_and_repeated_roots_are_deduplicated() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let sub = root.join("sub");
        fs::create_dir(&sub).unwrap();
        fs::write(sub.join("data"), b"the same data").unwrap();
        fs::hard_link(sub.join("data"), root.join("alias")).unwrap();
        let usage = complete(std::slice::from_ref(&root));
        assert_eq!(usage.inodes, 3);
        assert_eq!(usage, complete(&[root.clone(), sub.clone(), root.clone()]));
        assert_eq!(usage, complete(&[sub, root]));
    }

    #[test]
    fn sparse_files_report_allocation_separately_from_logical_size() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let path = root.join("sparse");
        let file = File::create(&path).unwrap();
        file.set_len(1024 * 1024 * 1024).unwrap();
        let usage = complete(std::slice::from_ref(&root));
        assert_eq!(
            usage.apparent_bytes,
            root.metadata().unwrap().len() + file.metadata().unwrap().len()
        );
        assert_eq!(
            usage.allocated_bytes,
            (root.metadata().unwrap().blocks() + file.metadata().unwrap().blocks()) * 512
        );
        assert!(usage.apparent_bytes > usage.allocated_bytes);
    }

    #[test]
    fn links_and_fifos_are_counted_without_opening_or_following_them() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let work = root.join("work");
        fs::create_dir(&work).unwrap();
        fs::create_dir(root.join("outside")).unwrap();
        fs::write(root.join("outside/large"), vec![0; 1_048_576]).unwrap();
        symlink(root.join("outside"), work.join("outside-link")).unwrap();
        symlink(root.join("nonexistent"), work.join("broken")).unwrap();
        symlink(&work, work.join("loop")).unwrap();
        let fifo = CString::new(work.join("fifo").as_os_str().as_bytes()).unwrap();
        // SAFETY: this is a fresh path inside the test-owned directory.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        let usage = complete(&[work]);
        assert_eq!(usage.inodes, 5);
        assert!(usage.apparent_bytes < 1_048_576);
        assert!(usage.allocated_bytes < 1_048_576);
    }

    #[test]
    fn link_roots_link_ancestors_and_invalid_roots_are_incomplete() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        fs::create_dir_all(root.join("real/sub")).unwrap();
        symlink(root.join("real"), root.join("link")).unwrap();
        for path in [root.join("link"), root.join("link/sub")] {
            assert_eq!(reason(&[path], budget()), Incomplete::Io);
        }
        for path in [PathBuf::from("relative"), root.join("../elsewhere")] {
            assert_eq!(reason(&[path], budget()), Incomplete::InvalidPath);
        }
        fs::write(root.join("file"), b"").unwrap();
        assert_eq!(reason(&[root.join("file")], budget()), Incomplete::Io);
    }

    #[test]
    fn traversal_budgets_produce_explicit_incomplete_observations() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        fs::create_dir(root.join("sub")).unwrap();
        let paths = [root];
        assert_eq!(
            reason(
                &paths,
                Budget {
                    entries: 1,
                    ..budget()
                }
            ),
            Incomplete::EntryBudget
        );
        assert_eq!(
            reason(
                &paths,
                Budget {
                    depth: 0,
                    ..budget()
                }
            ),
            Incomplete::DepthBudget
        );
        assert_eq!(
            reason(
                &paths,
                Budget {
                    elapsed: Duration::ZERO,
                    ..budget()
                }
            ),
            Incomplete::TimeBudget
        );
    }

    #[test]
    fn non_utf8_names_are_sampled_without_lossy_conversion_when_supported() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let name = OsStr::from_bytes(b"raw-\xff");
        match fs::write(root.join(name), b"non-UTF8 data") {
            Ok(()) => {}
            Err(e) if e.raw_os_error() == Some(libc::EILSEQ) => {
                eprintln!("filesystem cannot create the non-UTF8 fixture: {e}");
                return;
            }
            Err(e) => panic!("{e}"),
        }
        assert_eq!(complete(&[root]).inodes, 2);
    }

    #[test]
    fn directory_replacements_and_renames_are_detected_before_enumeration() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let work = root.join("work");
        fs::create_dir(&work).unwrap();
        let file = anchor(&work, &|| Ok(())).unwrap().unwrap();
        let before = fstat(file.as_raw_fd()).unwrap();
        let mut dir = Directory::new(file, work.clone(), before, 0).unwrap();
        fs::rename(&work, root.join("parked")).unwrap();
        fs::create_dir(&work).unwrap();
        assert!(matches!(dir.next(&|| Ok(())), Err(Incomplete::Changed)));
        let replacement = anchor(&work, &|| Ok(())).unwrap().unwrap();
        assert!(matches!(
            Directory::new(replacement, work, before, 0),
            Err(Incomplete::Changed)
        ));
    }

    #[test]
    fn observed_directory_mutation_after_open_is_incomplete() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let file = anchor(&root, &|| Ok(())).unwrap().unwrap();
        // A new directory and its first file can share one timestamp tick on Linux, while
        // size, blocks and link count stay unchanged. Give this fixture an old mtime so the
        // real directory mutation below has an observable stamp change on either platform.
        file.set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1))
            .unwrap();
        let before = fstat(file.as_raw_fd()).unwrap();
        let mut dir = Directory::new(file, root.clone(), before, 0).unwrap();
        fs::write(root.join("new"), b"changes directory metadata").unwrap();
        assert_ne!(before, fstat(dir.file.as_raw_fd()).unwrap());
        assert!(matches!(dir.next(&|| Ok(())), Err(Incomplete::Changed)));
    }

    #[test]
    fn allocation_overflow_is_not_saturated_or_partially_committed() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let stamp = fstat(anchor(&root, &|| Ok(())).unwrap().unwrap().as_raw_fd()).unwrap();
        let mut usage = Usage {
            allocated_bytes: u64::MAX,
            ..Usage::default()
        };
        let before = usage;
        assert_eq!(
            usage.include(Stamp { blocks: 1, ..stamp }),
            Err(Incomplete::Overflow)
        );
        assert_eq!(usage, before);
        assert_eq!(
            Usage::default().include(Stamp {
                blocks: i64::MAX,
                ..stamp
            }),
            Err(Incomplete::Overflow)
        );
        assert_eq!(
            Usage::default().include(Stamp { size: -1, ..stamp }),
            Err(Incomplete::Overflow)
        );
    }

    #[test]
    fn deadline_crossed_in_the_final_operation_never_becomes_a_complete_sample() {
        let walk = Walk {
            usage: Usage {
                apparent_bytes: 123,
                allocated_bytes: 512,
                inodes: 1,
            },
            seen: HashMap::new(),
            budget: Budget {
                elapsed: Duration::from_secs(1),
                ..budget()
            },
            started: Instant::now() - Duration::from_secs(2),
            entries: 1,
            missing: 1,
            device: Some(1),
            cancelled: &|| false,
        };
        let observed = walk.usage;
        assert_eq!(
            walk.finish(Ok(())).outcome,
            Outcome::Incomplete {
                observed,
                reason: Incomplete::TimeBudget
            }
        );
    }

    #[test]
    fn a_previously_selected_volume_cannot_receive_bytes_from_another_device() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let device = root.metadata().unwrap().dev();
        let mut walk = Walk {
            usage: Usage::default(),
            seen: HashMap::new(),
            budget: budget(),
            started: Instant::now(),
            entries: 0,
            missing: 0,
            device: Some(device.wrapping_add(1)),
            cancelled: &|| false,
        };
        assert_eq!(walk.root(&root), Err(Incomplete::CrossDevice));
        assert_eq!(walk.usage, Usage::default());
    }

    #[test]
    fn cancelled_traversals_stop_before_opening_and_during_enumeration() {
        use std::cell::Cell;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        for i in 0..100 {
            fs::write(root.join(format!("file{i}")), b"x").unwrap();
        }
        let stopped = measure_cancellable(std::slice::from_ref(&root), budget(), &|| true);
        assert!(matches!(
            stopped.outcome,
            Outcome::Incomplete {
                reason: Incomplete::Cancelled,
                ..
            }
        ));
        assert_eq!(stopped.entries, 0);
        let checks = Cell::new(0);
        let stopped = measure_cancellable(&[root], budget(), &|| {
            checks.set(checks.get() + 1);
            checks.get() > 30
        });
        assert!(matches!(
            stopped.outcome,
            Outcome::Incomplete {
                reason: Incomplete::Cancelled,
                ..
            }
        ));
        assert!(stopped.entries > 0 && stopped.entries < 101);
    }

    #[test]
    fn swapping_a_subdirectory_for_an_external_link_never_counts_target_contents() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let work = root.join("work");
        let outside = root.join("outside");
        fs::create_dir_all(work.join("sub")).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::write(work.join("sub/small"), b"safe").unwrap();
        File::create(outside.join("large"))
            .unwrap()
            .set_len(1 << 30)
            .unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let swaps = Arc::new(AtomicU64::new(0));
        let (ready, started) = std::sync::mpsc::channel();
        let worker = {
            let (stop, swaps) = (stop.clone(), swaps.clone());
            let work = work.clone();
            std::thread::spawn(move || {
                let mut ready = Some(ready);
                while !stop.load(Ordering::SeqCst) {
                    fs::rename(work.join("sub"), work.join("parked")).unwrap();
                    symlink(&outside, work.join("sub")).unwrap();
                    swaps.fetch_add(1, Ordering::SeqCst);
                    if let Some(ready) = ready.take() {
                        ready.send(()).unwrap();
                    }
                    std::thread::yield_now();
                    fs::remove_file(work.join("sub")).unwrap();
                    fs::rename(work.join("parked"), work.join("sub")).unwrap();
                }
            })
        };
        started.recv().unwrap();
        let mut largest = 0;
        for _ in 0..200 {
            let usage = match measure(std::slice::from_ref(&work), budget()).outcome {
                Outcome::Complete(usage)
                | Outcome::Incomplete {
                    observed: usage, ..
                } => usage,
            };
            largest = largest.max(usage.apparent_bytes);
        }
        stop.store(true, Ordering::SeqCst);
        worker.join().unwrap();
        assert!(swaps.load(Ordering::SeqCst) > 0);
        assert!(
            largest < 1 << 30,
            "never include the link target, even in a partial result"
        );
    }
}
