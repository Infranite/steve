//! What a process, or a process group, uses of the machine, as the platform accounts it (Linux
//! `/proc`, macOS `libproc`). A figure the platform does not give is `None`, never a guess. Sampling
//! is a few system calls: callers sample on demand (a scrape, a vitals round), never on a hot path.

/// One process's use, or the sum over a process group's members.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Usage {
    /// User plus system CPU time so far.
    pub cpu_seconds: f64,
    pub resident_bytes: u64,
    pub open_fds: Option<u64>,
    pub threads: Option<u64>,
    pub disk_read_bytes: Option<u64>,
    pub disk_written_bytes: Option<u64>,
    /// Processes summed (1 for one process).
    pub processes: u32,
}

impl Usage {
    fn add(mut self, o: Usage) -> Usage {
        let sum = |a: Option<u64>, b: Option<u64>| match (a, b) {
            (None, None) => None,
            (a, b) => Some(a.unwrap_or(0) + b.unwrap_or(0)),
        };
        self.cpu_seconds += o.cpu_seconds;
        self.resident_bytes += o.resident_bytes;
        self.open_fds = sum(self.open_fds, o.open_fds);
        self.threads = sum(self.threads, o.threads);
        self.disk_read_bytes = sum(self.disk_read_bytes, o.disk_read_bytes);
        self.disk_written_bytes = sum(self.disk_written_bytes, o.disk_written_bytes);
        self.processes += o.processes;
        self
    }
}

/// This process.
pub fn this() -> Option<Usage> {
    pid(std::process::id() as i32)
}

/// Every live member of process group `pgid`, summed; `None` when it has none (or cannot be read).
pub fn group(pgid: i32) -> Option<Usage> {
    if pgid <= 1 {
        return None;
    }
    members(pgid).into_iter().filter_map(pid).reduce(Usage::add)
}

#[cfg(target_os = "linux")]
fn pid(pid: i32) -> Option<Usage> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let f = stat_fields(&stat)?;
    // SAFETY: sysconf has no pointers or side effects.
    let (tick, page) = unsafe {
        (
            libc::sysconf(libc::_SC_CLK_TCK),
            libc::sysconf(libc::_SC_PAGESIZE),
        )
    };
    let n = |i: usize| f.get(i).and_then(|v| v.parse::<u64>().ok());
    // after the command: state is field 3, so field k is f[k - 3]
    let cpu = (n(14 - 3)? + n(15 - 3)?) as f64 / tick.max(1) as f64;
    let io = std::fs::read_to_string(format!("/proc/{pid}/io")).ok();
    let io_field = |key: &str| {
        io.as_deref()?
            .lines()
            .find_map(|l| l.strip_prefix(key)?.trim().parse().ok())
    };
    Some(Usage {
        cpu_seconds: cpu,
        resident_bytes: n(24 - 3)? * page.max(1) as u64,
        open_fds: std::fs::read_dir(format!("/proc/{pid}/fd"))
            .ok()
            .map(|d| d.count() as u64),
        threads: n(20 - 3),
        disk_read_bytes: io_field("read_bytes:"),
        disk_written_bytes: io_field("write_bytes:"),
        processes: 1,
    })
}

/// The fields of `/proc/<pid>/stat` after the parenthesised command (which may hold spaces).
#[cfg(any(target_os = "linux", test))]
fn stat_fields(stat: &str) -> Option<Vec<&str>> {
    Some(stat.rsplit_once(')')?.1.split_whitespace().collect())
}

#[cfg(target_os = "linux")]
fn members(pgid: i32) -> Vec<i32> {
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return vec![];
    };
    dir.filter_map(|e| e.ok()?.file_name().to_str()?.parse::<i32>().ok())
        .filter(|p| {
            std::fs::read_to_string(format!("/proc/{p}/stat"))
                .ok()
                .and_then(|s| stat_fields(&s)?.get(5 - 3)?.parse::<i32>().ok())
                == Some(pgid)
        })
        .collect()
}

#[cfg(target_os = "macos")]
fn pid(pid: i32) -> Option<Usage> {
    let mut ru = std::mem::MaybeUninit::<libc::rusage_info_v2>::zeroed();
    // SAFETY: RUSAGE_INFO_V2 writes exactly this struct; nothing is read on failure.
    if unsafe { libc::proc_pid_rusage(pid, libc::RUSAGE_INFO_V2, ru.as_mut_ptr().cast()) } != 0 {
        return None;
    }
    // SAFETY: the call succeeded and filled the struct.
    let ru = unsafe { ru.assume_init() };
    let mut task = std::mem::MaybeUninit::<libc::proc_taskinfo>::zeroed();
    let size = std::mem::size_of::<libc::proc_taskinfo>() as libc::c_int;
    // SAFETY: the buffer is a proc_taskinfo of the size passed.
    let threads = (unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTASKINFO,
            0,
            task.as_mut_ptr().cast(),
            size,
        )
    } == size)
        // SAFETY: the whole struct was written.
        .then(|| unsafe { task.assume_init() }.pti_threadnum.max(0) as u64);
    // SAFETY: a null buffer asks for the size the descriptor list needs.
    let fds = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDLISTFDS, 0, std::ptr::null_mut(), 0) };
    Some(Usage {
        cpu_seconds: mach_seconds(ru.ri_user_time + ru.ri_system_time),
        resident_bytes: ru.ri_resident_size,
        open_fds: (fds >= 0).then(|| fds as u64 / std::mem::size_of::<libc::proc_fdinfo>() as u64),
        threads,
        disk_read_bytes: Some(ru.ri_diskio_bytesread),
        disk_written_bytes: Some(ru.ri_diskio_byteswritten),
        processes: 1,
    })
}

/// `rusage_info` times are Mach absolute time units (nanoseconds only where the timebase is 1/1).
#[cfg(target_os = "macos")]
fn mach_seconds(units: u64) -> f64 {
    #[repr(C)]
    struct Timebase {
        numer: u32,
        denom: u32,
    }
    unsafe extern "C" {
        fn mach_timebase_info(info: *mut Timebase) -> libc::c_int;
    }
    static RATIO: std::sync::OnceLock<f64> = std::sync::OnceLock::new();
    let ratio = *RATIO.get_or_init(|| {
        let mut tb = Timebase { numer: 0, denom: 0 };
        // SAFETY: writes the two fields of the struct it is given.
        if unsafe { mach_timebase_info(&mut tb) } == 0 && tb.denom != 0 {
            f64::from(tb.numer) / f64::from(tb.denom)
        } else {
            1.0
        }
    });
    units as f64 * ratio / 1e9
}

#[cfg(target_os = "macos")]
fn members(pgid: i32) -> Vec<i32> {
    let mut pids = vec![0i32; 256];
    loop {
        let bytes = (pids.len() * std::mem::size_of::<i32>()) as libc::c_int;
        // SAFETY: the buffer holds `bytes` bytes of pids. Unlike proc_listpids, this answers a
        // count of pids, not of bytes.
        let n = unsafe { libc::proc_listpgrppids(pgid, pids.as_mut_ptr().cast(), bytes) };
        if n <= 0 {
            return vec![];
        }
        let count = n as usize;
        if count < pids.len() {
            pids.truncate(count);
            pids.retain(|p| *p > 0);
            return pids;
        }
        pids.resize(pids.len() * 2, 0);
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn pid(_: i32) -> Option<Usage> {
    None
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn members(_: i32) -> Vec<i32> {
    vec![]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_process_and_its_group_are_measured() {
        let me = this().expect("this platform accounts for its processes");
        assert!(me.resident_bytes > 0 && me.processes == 1);
        assert!(me.threads.is_some_and(|t| t >= 1) && me.open_fds.is_some_and(|n| n >= 1));
        // SAFETY: getpgrp has no pointers or side effects.
        let group = group(unsafe { libc::getpgrp() }).expect("this process's own group");
        assert!(group.processes >= 1 && group.resident_bytes >= me.resident_bytes / 2);
        assert!(super::group(1).is_none());
    }

    #[test]
    fn a_command_with_parentheses_does_not_shift_the_fields() {
        let f = stat_fields("42 (a) b) S 1 7 7 0 -1").unwrap();
        assert_eq!((f[0], f[2]), ("S", "7"));
    }
}
