//! Artifact bytes never use the knowledge scanner's link-following reads. A grant is captured in
//! the transaction accepting a turn; it keeps that execution's view and workspace generation,
//! even after its command body has been compacted or its session reopened.

use std::ffi::CString;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Component, Path};

use serde::{Deserialize, Serialize};
use steve_node_api::*;

use crate::journal::WorkspaceRow;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Grant {
    pub execution: ExecutionRef,
    pub view_digest: Digest,
    pub workspace: WorkspaceRef,
    pub incarnation: u64,
    pub dir: String,
    pub device: u64,
    pub inode: u64,
}

fn denied() -> ApiError {
    ApiError::new(ErrorCode::Denied, "artifact outside its execution view")
}

fn io_error(e: std::io::Error) -> ApiError {
    match e.raw_os_error() {
        Some(libc::ELOOP | libc::ENOTDIR) => denied(),
        _ => ApiError::new(ErrorCode::Invalid, "artifact unavailable"),
    }
}

/// Open one component relative to an already open directory. Nonblocking avoids waiting forever
/// on a FIFO before the descriptor's type can be checked. CLOEXEC keeps these capabilities out of
/// harness processes started concurrently by another thread.
fn child(parent: &File, name: &std::ffi::OsStr, directory: bool) -> ApiResult<File> {
    let name = CString::new(name.as_bytes()).map_err(|_| denied())?;
    let flags = libc::O_RDONLY
        | libc::O_NOFOLLOW
        | libc::O_CLOEXEC
        | libc::O_NONBLOCK
        | libc::O_NOCTTY
        | if directory { libc::O_DIRECTORY } else { 0 };
    // SAFETY: parent is a live directory descriptor; name is NUL-terminated. A successful openat
    // returns a new owned descriptor, immediately wrapped in File below.
    let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
    if fd < 0 {
        return Err(io_error(std::io::Error::last_os_error()));
    }
    // SAFETY: fd was just returned by openat and has no other owner.
    Ok(unsafe { File::from_raw_fd(fd) })
}

/// The canonical, authoritative workspace path is walked from `/` too: replacing an ancestor
/// with a link must not move the anchor before its descriptor is opened.
fn anchor(path: &Path) -> ApiResult<File> {
    if !path.is_absolute() {
        return Err(denied());
    }
    let mut dir = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open("/")
        .map_err(io_error)?;
    for c in path.components() {
        match c {
            Component::RootDir => {}
            Component::Normal(name) => dir = child(&dir, name, true)?,
            _ => return Err(denied()),
        }
    }
    Ok(dir)
}

impl Grant {
    pub fn capture(
        execution: ExecutionRef,
        view_digest: Digest,
        workspace: WorkspaceRef,
        row: &WorkspaceRow,
    ) -> ApiResult<Self> {
        let dir = anchor(Path::new(&row.path))?;
        let meta = dir.metadata().map_err(io_error)?;
        Ok(Self {
            execution,
            view_digest,
            workspace,
            incarnation: row.incarnation,
            dir: row.path.clone(),
            device: meta.dev(),
            inode: meta.ino(),
        })
    }
}

pub(crate) fn read(grant: &Grant, q: &ArtifactRead) -> ApiResult<ReadChunk> {
    if grant.execution != q.execution || grant.view_digest != q.view_digest {
        return Err(denied());
    }
    let base = Path::new(&grant.dir);
    let mut dir = anchor(base)?;
    let meta = dir.metadata().map_err(io_error)?;
    if (meta.dev(), meta.ino()) != (grant.device, grant.inode) {
        return Err(denied());
    }
    let mut parts = Path::new(q.location.as_str()).components().peekable();
    let mut file = None;
    while let Some(c) = parts.next() {
        let Component::Normal(name) = c else {
            return Err(denied());
        };
        let last = parts.peek().is_none();
        let opened = child(&dir, name, !last)?;
        if last {
            file = Some(opened);
        } else {
            dir = opened;
        }
    }
    let mut file = file.ok_or_else(denied)?;
    let before = file.metadata().map_err(io_error)?;
    if !before.is_file() || before.nlink() != 1 {
        return Err(denied());
    }
    // A directory can be renamed after openat. Check the descriptor's actual path before and
    // after reading, never re-resolve an untrusted pathname.
    let path = base.join(q.location.as_str());
    if crate::scan::opened_path(&file).map_err(io_error)? != path {
        return Err(denied());
    }
    let version = crate::scan::version(&path, &before);
    if q.expected.as_ref().is_some_and(|v| *v != version) {
        return Err(ApiError::new(ErrorCode::Conflict, "the artifact changed"));
    }
    let offset = q.offset.min(before.len());
    file.seek(SeekFrom::Start(offset)).map_err(io_error)?;
    let mut bytes = Vec::new();
    (&mut file)
        .take(u64::from(q.max.min(crate::scan::MAX_READ)))
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    let after = file.metadata().map_err(io_error)?;
    if after.nlink() != 1 || crate::scan::opened_path(&file).map_err(io_error)? != path {
        return Err(denied());
    }
    if crate::scan::version(&path, &after) != version {
        return Err(ApiError::new(ErrorCode::Conflict, "the artifact changed"));
    }
    Ok(ReadChunk {
        offset,
        eof: offset + bytes.len() as u64 >= after.len(),
        bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::symlink;

    fn grant(dir: &Path) -> Grant {
        let dir = dir.canonicalize().unwrap();
        let meta = dir.metadata().unwrap();
        Grant {
            execution: ExecutionRef::Turn {
                session: SessionId::new("s").unwrap(),
                turn: TurnId::new("t").unwrap(),
                attempt: AttemptId::new("a").unwrap(),
            },
            view_digest: Digest::sha256(b"view"),
            workspace: WorkspaceRef {
                node: NodeId::new("n").unwrap(),
                id: WorkspaceId::new("w").unwrap(),
            },
            incarnation: 1,
            dir: dir.to_str().unwrap().into(),
            device: meta.dev(),
            inode: meta.ino(),
        }
    }

    fn query(g: &Grant, location: &str) -> ArtifactRead {
        ArtifactRead {
            execution: g.execution.clone(),
            view_digest: g.view_digest.clone(),
            location: RelPath::new(location).unwrap(),
            expected: None,
            offset: 0,
            max: u32::MAX,
        }
    }

    #[test]
    fn bounded_reads_versions_scope_and_replaced_workspace() {
        let t = tempfile::tempdir().unwrap();
        fs::create_dir(t.path().join("work")).unwrap();
        let g = grant(&t.path().join("work"));
        let p = Path::new(&g.dir).join("a");
        fs::write(&p, b"0123456789").unwrap();
        let mut q = query(&g, "a");
        q.offset = 3;
        q.max = 4;
        let chunk = read(&g, &q).unwrap();
        assert_eq!(chunk.bytes, b"3456");
        assert_eq!(chunk.offset, 3);
        assert!(!chunk.eof);
        q.offset = u64::MAX;
        assert!(read(&g, &q).unwrap().eof);
        q.offset = 0;
        q.expected = Some(crate::scan::version(&p, &p.metadata().unwrap()));
        fs::write(&p, b"changed").unwrap();
        assert_eq!(read(&g, &q).unwrap_err().code, ErrorCode::Conflict);
        q.expected = None;
        q.view_digest = Digest::sha256(b"another view");
        assert_eq!(read(&g, &q).unwrap_err().code, ErrorCode::Denied);
        q = query(&g, "a");
        fs::write(&p, vec![b'x'; crate::scan::MAX_READ as usize + 1]).unwrap();
        let c = read(&g, &q).unwrap();
        assert_eq!(c.bytes.len(), crate::scan::MAX_READ as usize);
        assert!(!c.eof);
        fs::rename(&g.dir, t.path().join("old")).unwrap();
        fs::create_dir(&g.dir).unwrap();
        fs::write(&p, b"new workspace").unwrap();
        assert_eq!(read(&g, &q).unwrap_err().code, ErrorCode::Denied);
    }

    #[test]
    fn links_directories_fifos_and_absolute_paths_are_never_artifacts() {
        let t = tempfile::tempdir().unwrap();
        fs::create_dir_all(t.path().join("work/sub")).unwrap();
        let g = grant(&t.path().join("work"));
        let work = Path::new(&g.dir);
        fs::write(work.join("sub/a"), b"inside").unwrap();
        fs::write(t.path().join("secret"), b"outside").unwrap();
        symlink("sub/a", work.join("file-link")).unwrap();
        symlink("sub", work.join("dir-link")).unwrap();
        symlink(t.path().join("secret"), work.join("out-link")).unwrap();
        fs::hard_link(t.path().join("secret"), work.join("hard")).unwrap();
        fs::hard_link(work.join("sub/a"), work.join("inside-hard")).unwrap();
        let fifo = CString::new(work.join("fifo").as_os_str().as_bytes()).unwrap();
        // SAFETY: fifo is a NUL-terminated path in this test's fresh temporary directory.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        for name in [
            "file-link",
            "dir-link/a",
            "out-link",
            "hard",
            "inside-hard",
            "sub",
            "fifo",
            "",
        ] {
            assert_eq!(
                read(&g, &query(&g, name)).unwrap_err().code,
                ErrorCode::Denied,
                "{name}"
            );
        }
        for name in ["/etc/passwd", "../secret"] {
            assert!(RelPath::new(name).is_err());
        }
        // Even the trusted anchor's ancestors must remain directories, never links.
        fs::rename(work, t.path().join("moved")).unwrap();
        symlink(t.path().join("moved"), work).unwrap();
        assert_eq!(
            read(&g, &query(&g, "sub/a")).unwrap_err().code,
            ErrorCode::Denied
        );
    }

    #[test]
    fn replacing_an_intermediate_directory_with_a_link_never_reads_the_target() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        let t = tempfile::tempdir().unwrap();
        fs::create_dir_all(t.path().join("work/sub")).unwrap();
        fs::create_dir(t.path().join("outside")).unwrap();
        fs::write(t.path().join("work/sub/a"), b"public").unwrap();
        fs::write(t.path().join("outside/a"), b"secret").unwrap();
        let g = grant(&t.path().join("work"));
        let q = query(&g, "sub/a");
        let stop = Arc::new(AtomicBool::new(false));
        let swapped = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let attacker = {
            let stop = stop.clone();
            let swapped = swapped.clone();
            let work = Path::new(&g.dir).to_path_buf();
            let outside = t.path().canonicalize().unwrap().join("outside");
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    fs::rename(work.join("sub"), work.join("parked")).unwrap();
                    symlink(&outside, work.join("sub")).unwrap();
                    swapped.fetch_add(1, Ordering::SeqCst);
                    std::thread::yield_now();
                    fs::remove_file(work.join("sub")).unwrap();
                    fs::rename(work.join("parked"), work.join("sub")).unwrap();
                }
            })
        };
        for _ in 0..1000 {
            if let Ok(chunk) = read(&g, &q) {
                assert_eq!(chunk.bytes, b"public");
            }
        }
        stop.store(true, Ordering::SeqCst);
        attacker.join().unwrap();
        assert!(swapped.load(Ordering::SeqCst) > 0, "race fixture ran");
    }
}
