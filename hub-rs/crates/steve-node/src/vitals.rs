//! How this machine is doing (B3): load, memory and the space left under each root, measured when
//! the Hub asks. Each figure comes from the platform's own account of it; one it cannot give is
//! left out.

use std::path::Path;

use steve_node_api::{NodeVitals, RootVitals, Timestamp};

use crate::workspace::Root;

pub(crate) fn measure(roots: &[Root], sessions: u32) -> NodeVitals {
    let (mem_total, mem_available) = memory();
    NodeVitals {
        at: Timestamp(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0),
        ),
        cpus: std::thread::available_parallelism()
            .ok()
            .map(|n| n.get() as u32),
        load: load(),
        mem_total,
        mem_available,
        roots: roots
            .iter()
            .filter_map(|r| {
                let (free, total, inodes_free) = space(&r.path)?;
                Some(RootVitals {
                    root: r.id.clone(),
                    display: r.display.clone(),
                    free,
                    total,
                    inodes_free,
                })
            })
            .collect(),
        sessions,
    }
}

fn load() -> Option<[f64; 3]> {
    let mut l = [0f64; 3];
    // SAFETY: getloadavg writes at most 3 values into the array it is given
    let n = unsafe { libc::getloadavg(l.as_mut_ptr(), 3) };
    (n == 3).then_some(l)
}

/// Free and total bytes, and free inodes, of the file system `dir` is on.
fn space(dir: &Path) -> Option<(u64, u64, Option<u64>)> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
    let mut st = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: `path` is NUL-terminated and `st` is written by statvfs before it is read.
    if unsafe { libc::statvfs(path.as_ptr(), st.as_mut_ptr()) } != 0 {
        return None;
    }
    let st = unsafe { st.assume_init() };
    #[allow(clippy::unnecessary_cast)] // the field types differ between platforms
    let (frsize, blocks, avail, favail) = (
        st.f_frsize as u64,
        st.f_blocks as u64,
        st.f_bavail as u64,
        st.f_favail as u64,
    );
    Some((
        avail.saturating_mul(frsize),
        blocks.saturating_mul(frsize),
        Some(favail),
    ))
}

/// (total, available) bytes of memory.
#[cfg(target_os = "linux")]
fn memory() -> (Option<u64>, Option<u64>) {
    let Ok(m) = std::fs::read_to_string("/proc/meminfo") else {
        return (None, None);
    };
    (meminfo(&m, "MemTotal:"), meminfo(&m, "MemAvailable:"))
}

#[cfg(any(target_os = "linux", test))]
fn meminfo(m: &str, key: &str) -> Option<u64> {
    let kb: u64 = m
        .lines()
        .find_map(|l| l.strip_prefix(key))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    Some(kb * 1024)
}

/// (total, available) bytes of memory: `hw.memsize`, and free, inactive, speculative and purgeable
/// pages as `vm_stat` counts them (what macOS gives back without swapping).
#[cfg(target_os = "macos")]
fn memory() -> (Option<u64>, Option<u64>) {
    let mut total: u64 = 0;
    let mut len = std::mem::size_of::<u64>();
    // SAFETY: the name is NUL-terminated and `total` is a u64 of the size passed in `len`
    let ok = unsafe {
        libc::sysctlbyname(
            c"hw.memsize".as_ptr(),
            (&mut total as *mut u64).cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    } == 0;
    let available = std::process::Command::new("/usr/bin/vm_stat")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| vm_stat(&String::from_utf8_lossy(&o.stdout)));
    (ok.then_some(total), available)
}

#[cfg(any(target_os = "macos", test))]
fn vm_stat(out: &str) -> Option<u64> {
    let page: u64 = out
        .lines()
        .next()?
        .split("page size of ")
        .nth(1)?
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    let pages = |key: &str| -> u64 {
        out.lines()
            .find_map(|l| l.strip_prefix(key))
            .and_then(|v| v.trim().trim_end_matches('.').parse().ok())
            .unwrap_or(0)
    };
    let free = [
        "Pages free:",
        "Pages inactive:",
        "Pages speculative:",
        "Pages purgeable:",
    ]
    .iter()
    .map(|k| pages(k))
    .sum::<u64>();
    Some(free * page)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn memory() -> (Option<u64>, Option<u64>) {
    (None, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_accounts_read_as_bytes() {
        let vm = "Mach Virtual Memory Statistics: (page size of 16384 bytes)\nPages free:      10.\nPages active:    99.\nPages inactive:   5.\nPages speculative: 1.\nPages purgeable:  4.\n";
        assert_eq!(vm_stat(vm), Some(20 * 16384));
        let mi = "MemTotal:       16000000 kB\nMemFree:  100 kB\nMemAvailable:    8000000 kB\n";
        assert_eq!(meminfo(mi, "MemTotal:"), Some(16_000_000 * 1024));
        assert_eq!(meminfo(mi, "MemAvailable:"), Some(8_000_000 * 1024));
    }

    #[test]
    fn this_machine_has_figures() {
        let dir = std::env::temp_dir();
        let v = measure(
            &[Root {
                id: steve_node_api::RootId::new("tmp").unwrap(),
                path: dir.clone(),
                display: "tmp".into(),
                access: Default::default(),
            }],
            2,
        );
        assert!(v.cpus.is_some_and(|n| n > 0));
        assert!(v.load.is_some());
        assert!(v.mem_total.is_some_and(|m| m > 0));
        assert_eq!(v.roots.len(), 1);
        assert!(v.roots[0].total >= v.roots[0].free && v.roots[0].total > 0);
        assert_eq!(v.sessions, 2);
    }
}
