//! Copy-on-write file primitive for private, unpublished Node preparation directories.
//!
//! This does not authorize a source, seal a seed, freeze writers or publish a directory.
//! Callers must own both fds and exclusively control the destination directory throughout,
//! including its inherited ACLs. This primitive does not sanitize ACLs.
//! Same-user permissions alone are not that guarantee on an unconfined Node.
//! No callers in the execution path exist yet. Unsupported combinations must start cold.

use std::ffi::{CString, OsStr};
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    InvalidName,
    InvalidSource,
    InvalidDirectory,
    CrossDevice,
    Unsupported,
    AlreadyExists,
    SourceChanged,
    DestinationChanged,
    Io,
}

/// A failed Linux ioctl can leave a newly created, empty destination. A successful clone whose
/// subsequent validation fails can leave a complete clone. Neither is published or auto-deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    Untouched,
    MayContainPartial,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Failure {
    pub reason: Reason,
    pub os_error: Option<i32>,
    pub destination: Effect,
}

impl Failure {
    fn input(reason: Reason) -> Self {
        Self {
            reason,
            os_error: None,
            destination: Effect::Untouched,
        }
    }

    fn io(error: io::Error, destination: Effect) -> Self {
        let os_error = error.raw_os_error();
        let reason = match os_error {
            Some(libc::ENOTSUP | libc::ENOSYS | libc::ENOTTY) => Reason::Unsupported,
            Some(libc::EXDEV) => Reason::CrossDevice,
            Some(libc::EEXIST) => Reason::AlreadyExists,
            _ => Reason::Io,
        };
        Self {
            reason,
            os_error,
            destination,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileId {
    pub device: u64,
    pub inode: u64,
}

/// A handle for inspecting the clone, not necessarily opened for writing on every platform.
#[derive(Debug)]
pub struct ClonedFile {
    pub file: File,
    pub source: FileId,
    pub destination: FileId,
}

#[derive(Debug, PartialEq, Eq)]
struct Stamp {
    id: FileId,
    mode: u32,
    size: u64,
    links: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}

impl Stamp {
    fn get(file: &File, effect: Effect) -> Result<Self, Failure> {
        let m = file.metadata().map_err(|e| Failure::io(e, effect))?;
        Ok(Self {
            id: FileId {
                device: m.dev(),
                inode: m.ino(),
            },
            mode: m.mode(),
            size: m.len(),
            links: m.nlink(),
            modified: (m.mtime(), m.mtime_nsec()),
            changed: (m.ctime(), m.ctime_nsec()),
        })
    }

    // libc mode constants are narrower on macOS than Linux.
    #[allow(clippy::unnecessary_cast)]
    fn regular(&self) -> bool {
        self.mode & libc::S_IFMT as u32 == libc::S_IFREG as u32
    }
}

/// Clone one regular file into an exclusively controlled, unpublished directory on the same
/// volume. `name` is a single entry, not a path. Existing entries are never replaced.
/// The caller must validate inherited ACLs; normalized Unix mode is not ACL sanitization.
///
/// There is no hardlink or full-copy fallback. On failure, inspect `destination` before the
/// caller's separately registered partial-directory cleanup. This function never unlinks.
/// Source metadata checks detect some changes, not concurrent writers or a whole-tree snapshot.
pub fn clone_file_at(source: &File, directory: &File, name: &OsStr) -> Result<ClonedFile, Failure> {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes == b"." || bytes == b".." || bytes.contains(&b'/') {
        return Err(Failure::input(Reason::InvalidName));
    }
    let name = CString::new(bytes).map_err(|_| Failure::input(Reason::InvalidName))?;
    let before = Stamp::get(source, Effect::Untouched)?;
    if !before.regular() {
        return Err(Failure::input(Reason::InvalidSource));
    }
    let parent = directory
        .metadata()
        .map_err(|e| Failure::io(e, Effect::Untouched))?;
    if !parent.is_dir() {
        return Err(Failure::input(Reason::InvalidDirectory));
    }
    if parent.dev() != before.id.device {
        return Err(Failure::input(Reason::CrossDevice));
    }
    let file = platform_clone(source, directory, &name)?;
    let created = Effect::MayContainPartial;
    let after = Stamp::get(source, created)?;
    if before != after {
        return Err(Failure {
            reason: Reason::SourceChanged,
            os_error: None,
            destination: created,
        });
    }
    let target = Stamp::get(&file, created)?;
    if !target.regular()
        || target.links != 1
        || target.size != before.size
        || target.id.device != before.id.device
        || target.id == before.id
    {
        return Err(Failure {
            reason: Reason::DestinationChanged,
            os_error: None,
            destination: created,
        });
    }
    // Normalize Unix mode, retaining the source's owner-executable bit; never setuid/setgid.
    // fchmod does NOT clear macOS ACLs. The private-directory precondition includes inherited
    // ACL validation by the caller; these mode bits alone are not an owner-only guarantee.
    file.set_permissions(std::fs::Permissions::from_mode(
        0o600 | (before.mode & 0o100),
    ))
    .map_err(|e| Failure::io(e, created))?;
    Ok(ClonedFile {
        file,
        source: before.id,
        destination: target.id,
    })
}

#[cfg(target_os = "macos")]
fn platform_clone(source: &File, directory: &File, name: &std::ffi::CStr) -> Result<File, Failure> {
    // SAFETY: both borrowed fds stay live and name is one validated NUL-terminated component.
    // The destination directory MUST be exclusively controlled until the post-clone open.
    if unsafe { libc::fclonefileat(source.as_raw_fd(), directory.as_raw_fd(), name.as_ptr(), 0) }
        != 0
    {
        return Err(Failure::io(io::Error::last_os_error(), Effect::Untouched));
    }
    // O_NONBLOCK avoids blocking if a caller violates exclusivity and replaces it with a FIFO.
    // SAFETY: name and directory are still live; only this relative entry is opened.
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        Err(Failure::io(
            io::Error::last_os_error(),
            Effect::MayContainPartial,
        ))
    } else {
        // SAFETY: this is the sole owner of the successfully opened fd.
        Ok(unsafe { File::from_raw_fd(fd) })
    }
}

#[cfg(target_os = "linux")]
fn platform_clone(source: &File, directory: &File, name: &std::ffi::CStr) -> Result<File, Failure> {
    // SAFETY: both borrowed fds stay live and name is one validated NUL-terminated component.
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if fd < 0 {
        return Err(Failure::io(io::Error::last_os_error(), Effect::Untouched));
    }
    // SAFETY: this is the sole owner of the successfully created fd.
    let file = unsafe { File::from_raw_fd(fd) };
    // SAFETY: valid regular-file fds, the destination was newly created O_EXCL.
    if unsafe { libc::ioctl(file.as_raw_fd(), libc::FICLONE, source.as_raw_fd()) } != 0 {
        return Err(Failure::io(
            io::Error::last_os_error(),
            Effect::MayContainPartial,
        ));
    }
    Ok(file)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn platform_clone(_: &File, _: &File, _: &std::ffi::CStr) -> Result<File, Failure> {
    Err(Failure::input(Reason::Unsupported))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::symlink;

    #[test]
    fn invalid_names_and_nonregular_inputs_do_not_create_outputs() {
        let tmp = tempfile::tempdir().unwrap();
        let root = File::open(tmp.path()).unwrap();
        fs::write(tmp.path().join("source"), b"source").unwrap();
        let source = File::open(tmp.path().join("source")).unwrap();
        for name in ["", ".", "..", "../outside", "/absolute", "a/b", "nul\0byte"] {
            let failure = clone_file_at(&source, &root, OsStr::new(name)).unwrap_err();
            assert_eq!(failure.reason, Reason::InvalidName);
            assert_eq!(failure.destination, Effect::Untouched);
        }
        assert_eq!(
            clone_file_at(&root, &root, OsStr::new("out"))
                .unwrap_err()
                .reason,
            Reason::InvalidSource
        );
        assert_eq!(
            clone_file_at(&source, &source, OsStr::new("out"))
                .unwrap_err()
                .reason,
            Reason::InvalidDirectory
        );
        assert_eq!(fs::read_dir(tmp.path()).unwrap().count(), 1);
    }

    #[test]
    fn existing_files_links_and_directories_are_never_replaced() {
        let tmp = tempfile::tempdir().unwrap();
        let root = File::open(tmp.path()).unwrap();
        fs::write(tmp.path().join("source"), b"source").unwrap();
        let source = File::open(tmp.path().join("source")).unwrap();
        fs::write(tmp.path().join("existing"), b"preserve").unwrap();
        symlink("existing", tmp.path().join("link")).unwrap();
        fs::create_dir(tmp.path().join("dir")).unwrap();
        for name in ["existing", "link", "dir"] {
            let failure = clone_file_at(&source, &root, OsStr::new(name)).unwrap_err();
            assert_eq!(failure.reason, Reason::AlreadyExists);
            assert_eq!(failure.destination, Effect::Untouched);
        }
        assert_eq!(fs::read(tmp.path().join("existing")).unwrap(), b"preserve");
        assert_eq!(
            fs::read_link(tmp.path().join("link")).unwrap(),
            std::path::Path::new("existing")
        );
        assert!(tmp.path().join("dir").is_dir());
    }

    #[test]
    fn actual_clone_has_distinct_inode_and_independent_writes_or_reports_unsupported() {
        let tmp = tempfile::tempdir().unwrap();
        let root = File::open(tmp.path()).unwrap();
        let src = tmp.path().join("source");
        let dst = tmp.path().join("clone=工具");
        fs::write(&src, vec![b's'; 1024 * 1024]).unwrap();
        let source = File::open(&src).unwrap();
        let cloned = match clone_file_at(&source, &root, dst.file_name().unwrap()) {
            Ok(cloned) => cloned,
            Err(failure) if matches!(failure.reason, Reason::Unsupported | Reason::CrossDevice) => {
                eprintln!("actual clone unavailable: {failure:?}");
                assert_eq!(fs::read(&src).unwrap(), vec![b's'; 1024 * 1024]);
                return;
            }
            Err(failure) => panic!("unexpected clone failure: {failure:?}"),
        };
        assert_ne!(cloned.source, cloned.destination);
        assert_eq!(cloned.file.metadata().unwrap().nlink(), 1);
        assert_eq!(fs::read(&dst).unwrap(), vec![b's'; 1024 * 1024]);
        fs::write(&dst, b"destination changed").unwrap();
        assert_eq!(fs::read(&src).unwrap(), vec![b's'; 1024 * 1024]);
        fs::write(&src, b"source changed").unwrap();
        assert_eq!(fs::read(&dst).unwrap(), b"destination changed");
    }
}
