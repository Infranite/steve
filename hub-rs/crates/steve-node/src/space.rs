use std::path::Path;

/// Free bytes on the filesystem of `dir` for an unprivileged writer; `None` if it cannot be read.
/// Advisory: another process may take the space before the journal writes.
pub(crate) fn free_bytes(dir: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
    let mut st = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: `path` is NUL-terminated and `st` is written by statvfs before it is read.
    if unsafe { libc::statvfs(path.as_ptr(), st.as_mut_ptr()) } != 0 {
        return None;
    }
    let st = unsafe { st.assume_init() };
    #[allow(clippy::unnecessary_cast)] // the field types differ between platforms
    Some((st.f_bavail as u64).saturating_mul(st.f_frsize as u64))
}
