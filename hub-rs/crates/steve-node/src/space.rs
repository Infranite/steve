use std::path::Path;

pub(crate) struct Space {
    pub free: u64,
    pub total: u64,
    pub inodes_free: Option<u64>,
}

/// Filesystem space available to an unprivileged writer; absent if the OS cannot report it.
/// Advisory: another process may take the space before the journal writes.
pub(crate) fn measure(dir: &Path) -> Option<Space> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
    let mut st = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: `path` is NUL-terminated and `st` is written by statvfs before it is read.
    if unsafe { libc::statvfs(path.as_ptr(), st.as_mut_ptr()) } != 0 {
        return None;
    }
    let st = unsafe { st.assume_init() };
    #[allow(clippy::unnecessary_cast)] // the field types differ between platforms
    Some(Space {
        free: (st.f_bavail as u64).saturating_mul(st.f_frsize as u64),
        total: (st.f_blocks as u64).saturating_mul(st.f_frsize as u64),
        inodes_free: Some(st.f_favail as u64),
    })
}

pub(crate) fn free_bytes(dir: &Path) -> Option<u64> {
    measure(dir).map(|space| space.free)
}
