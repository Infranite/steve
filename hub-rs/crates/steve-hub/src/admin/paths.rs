//! Socket discovery, private directory ownership and inode-safe cleanup.
use anyhow::{Context, Result, bail, ensure};
use sha2::{Digest, Sha256};
use std::{
    ffi::OsString,
    fs, io,
    os::unix::{
        ffi::OsStringExt,
        fs::{FileTypeExt, MetadataExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::{Arc, Mutex, Once, Weak},
};

#[cfg(target_os = "macos")]
const PATH_CAPACITY: usize = 104;
#[cfg(not(target_os = "macos"))]
const PATH_CAPACITY: usize = 108;
fn fits(path: &Path) -> bool {
    path.as_os_str().as_encoded_bytes().len() < PATH_CAPACITY
}
fn root(home: &Path) -> io::Result<PathBuf> {
    Ok(fs::canonicalize(home).unwrap_or(if home.is_absolute() {
        home.to_owned()
    } else {
        std::env::current_dir()?.join(home)
    }))
}
fn fallback_name(home: &Path) -> String {
    let hash = hex::encode(Sha256::digest(home.as_os_str().as_encoded_bytes()));
    format!("steve-{}-{}", super::euid(), &hash[..16])
}
/// Discover the listener without creating directories or modifying the installation.
/// A pointer survives a different TMPDIR in a CLI launched after the Hub.
pub fn socket_path(home: &Path) -> Result<PathBuf> {
    let home = root(home)?;
    let pointer = home.join("run/hub.sock.path");
    match fs::read(&pointer) {
        Ok(bytes) => {
            let path = PathBuf::from(OsString::from_vec(bytes));
            ensure!(
                path.is_absolute()
                    && path.file_name().is_some_and(|s| s == "hub.sock")
                    && path
                        .parent()
                        .and_then(Path::file_name)
                        .is_some_and(|s| s == fallback_name(&home).as_str())
                    && fits(&path),
                "invalid management socket pointer"
            );
            Ok(path)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let normal = home.join("run/hub.sock");
            let path = if fits(&normal) {
                normal
            } else {
                std::env::temp_dir()
                    .join(fallback_name(&home))
                    .join("hub.sock")
            };
            ensure!(
                fits(&path),
                "TMPDIR is too long for a management Unix socket"
            );
            Ok(path)
        }
        Err(e) => Err(e.into()),
    }
}
fn private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    let meta = fs::symlink_metadata(path)?;
    ensure!(
        meta.is_dir() && meta.uid() == super::euid(),
        "management directory must be owned by this user and not a symlink"
    );
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}
fn remove_stale(path: &Path) -> Result<()> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    ensure!(
        meta.file_type().is_socket() && meta.uid() == super::euid(),
        "management path is not this user's socket"
    );
    match UnixStream::connect(path) {
        Ok(_) => bail!(
            "a management socket is already listening at {}",
            path.display()
        ),
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
            ) => {}
        Err(e) => return Err(e).context("cannot prove the management socket is stale"),
    }
    fs::remove_file(path)?;
    Ok(())
}
struct Owned {
    socket: PathBuf,
    pointer: Option<PathBuf>,
    dev: u64,
    ino: u64,
}
impl Owned {
    fn clean(&self) {
        if fs::symlink_metadata(&self.socket)
            .is_ok_and(|m| m.dev() == self.dev && m.ino() == self.ino)
        {
            let _ = fs::remove_file(&self.socket);
            if let Some(pointer) = &self.pointer
                && fs::read(pointer).is_ok_and(|s| s == self.socket.as_os_str().as_encoded_bytes())
            {
                let _ = fs::remove_file(pointer);
                if let Some(dir) = self.socket.parent() {
                    let _ = fs::remove_dir(dir);
                }
            }
        }
    }
}
static OWNED: Mutex<Vec<Weak<Owned>>> = Mutex::new(Vec::new());
static AT_EXIT: Once = Once::new();
extern "C" fn clean_at_exit() {
    for entry in OWNED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .filter_map(Weak::upgrade)
    {
        entry.clean();
    }
}
/// Drop cleans aborted/stopped jobs; atexit also covers the Hub's explicit process::exit.
pub(super) struct Cleanup(Arc<Owned>);
impl Drop for Cleanup {
    fn drop(&mut self) {
        self.0.clean();
    }
}
pub(super) fn bind(home: &Path) -> Result<(UnixListener, Cleanup)> {
    private_dir(&home.join("run"))?;
    let home = root(home)?;
    let path = socket_path(&home)?;
    let pointer = (!path.ends_with("run/hub.sock")).then(|| home.join("run/hub.sock.path"));
    // Validate before binding or adopting cleanup ownership: an error must not unlink
    // an existing pointer (including a symlink) that this server did not create.
    if let Some(pointer) = &pointer {
        // The parent is private. Refuse symlinks before writing a discovery pointer.
        if let Ok(meta) = fs::symlink_metadata(pointer) {
            ensure!(
                meta.is_file() && meta.uid() == super::euid(),
                "invalid management pointer file"
            );
        }
    }
    private_dir(path.parent().context("management socket has no parent")?)?;
    remove_stale(&path)?;
    let listener = UnixListener::bind(&path)?;
    let meta = fs::symlink_metadata(&path)?;
    let cleanup = Cleanup(Arc::new(Owned {
        socket: path.clone(),
        pointer,
        dev: meta.dev(),
        ino: meta.ino(),
    }));
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;
    if let Some(pointer) = &cleanup.0.pointer {
        fs::write(pointer, path.as_os_str().as_encoded_bytes())?;
        fs::set_permissions(pointer, fs::Permissions::from_mode(0o600))?;
    }
    AT_EXIT.call_once(|| {
        // SAFETY: a C ABI callback with static lifetime, no arguments or unwinding.
        unsafe {
            libc::atexit(clean_at_exit);
        }
    });
    let mut owned = OWNED.lock().unwrap_or_else(|e| e.into_inner());
    owned.retain(|e| e.strong_count() > 0);
    owned.push(Arc::downgrade(&cleanup.0));
    Ok((listener, cleanup))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unix_path_capacity_includes_the_terminating_nul_and_counts_bytes() {
        assert!(fits(Path::new(&"a".repeat(PATH_CAPACITY - 1))));
        assert!(!fits(Path::new(&"a".repeat(PATH_CAPACITY))));
        assert!(!fits(Path::new(&"中".repeat(PATH_CAPACITY / 3 + 1))));
    }
    #[test]
    fn a_dead_socket_is_reclaimed_but_a_live_socket_or_regular_file_is_not_removed() {
        let home = tempfile::tempdir().unwrap();
        private_dir(&home.path().join("run")).unwrap();
        let path = socket_path(home.path()).unwrap();
        drop(UnixListener::bind(&path).unwrap());
        let (listener, guard) = bind(home.path()).unwrap();
        assert!(bind(home.path()).is_err());
        assert!(path.exists());
        drop(listener);
        drop(guard);
        assert!(!path.exists());
        fs::write(&path, b"retain this file").unwrap();
        assert!(bind(home.path()).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"retain this file");
    }
    #[test]
    fn an_old_guard_cannot_unlink_a_replacement_listener() {
        let home = tempfile::tempdir().unwrap();
        let (old_listener, old_guard) = bind(home.path()).unwrap();
        let path = socket_path(home.path()).unwrap();
        fs::remove_file(&path).unwrap();
        let (new_listener, new_guard) = bind(home.path()).unwrap();
        drop(old_guard);
        drop(old_listener);
        assert!(path.exists());
        drop(new_listener);
        drop(new_guard);
        assert!(!path.exists());
    }
    #[test]
    fn symlinked_run_directory_is_not_followed_or_chmodded() {
        let home = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let before = fs::metadata(other.path()).unwrap().permissions().mode();
        std::os::unix::fs::symlink(other.path(), home.path().join("run")).unwrap();
        assert!(bind(home.path()).is_err());
        assert_eq!(
            fs::metadata(other.path()).unwrap().permissions().mode(),
            before
        );
    }
    #[test]
    fn a_symlink_pointer_is_rejected_without_unlinking_the_pointer_or_its_target() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("long-home".repeat(10));
        private_dir(&home.join("run")).unwrap();
        let path = socket_path(&home).unwrap();
        let target = temp.path().join("keep-pointer-data");
        fs::write(&target, path.as_os_str().as_encoded_bytes()).unwrap();
        let pointer = home.join("run/hub.sock.path");
        std::os::unix::fs::symlink(&target, &pointer).unwrap();
        assert!(bind(&home).is_err());
        assert!(
            fs::symlink_metadata(&pointer)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            fs::read(&target).unwrap(),
            path.as_os_str().as_encoded_bytes()
        );
        assert!(!path.exists());
    }
}
