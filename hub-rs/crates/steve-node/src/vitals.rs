//! How this machine is doing (B3): load, memory and the space left under each root, measured when
//! the Hub asks. Each figure comes from the platform's own account of it; one it cannot give is
//! left out.

use std::collections::BTreeMap;
use steve_node_api::{
    MAX_REPORTED_SESSION_VITALS, NetworkBytes, NodeMeters, NodeVitals, ProcessMeters, RootVitals,
    SessionId, SessionVitals, Timestamp,
};

use crate::journal::Journal;
use crate::workspace::Root;

/// Node-process meters plus per-session harness process-group resource breakdowns.
pub(crate) struct SampledMeters {
    pub(crate) meters: NodeMeters,
    pub(crate) adapter_disk_written_bytes: u64,
    pub(crate) session_vitals: Vec<SessionVitals>,
}

/// Aggregates per-session process-group counters, sums total adapter disk writes across all
/// measured sessions, sorts descending by `disk_written_bytes + resident_bytes`, and truncates to
/// [`MAX_REPORTED_SESSION_VITALS`].
pub(crate) fn aggregate_session_vitals(
    samples: impl IntoIterator<Item = (SessionId, f64, u64, u64)>,
) -> (u64, Vec<SessionVitals>) {
    let mut by_session: BTreeMap<SessionId, (f64, u64, u64)> = BTreeMap::new();
    let mut total_disk_written = 0_u64;
    for (session_id, cpu_seconds, resident_bytes, disk_written_bytes) in samples {
        total_disk_written = total_disk_written.saturating_add(disk_written_bytes);
        let entry = by_session.entry(session_id).or_insert((0.0, 0, 0));
        entry.0 += cpu_seconds;
        entry.1 = entry.1.saturating_add(resident_bytes);
        entry.2 = entry.2.saturating_add(disk_written_bytes);
    }
    let mut sessions: Vec<SessionVitals> = by_session
        .into_iter()
        .map(
            |(session_id, (cpu_seconds, resident_bytes, disk_written_bytes))| SessionVitals {
                session_id,
                cpu_seconds,
                resident_bytes,
                disk_written_bytes,
            },
        )
        .collect();
    sessions.sort_by(|a, b| {
        let weight_b = b.disk_written_bytes.saturating_add(b.resident_bytes);
        let weight_a = a.disk_written_bytes.saturating_add(a.resident_bytes);
        weight_b
            .cmp(&weight_a)
            .then_with(|| a.session_id.as_str().cmp(b.session_id.as_str()))
    });
    sessions.truncate(MAX_REPORTED_SESSION_VITALS);
    (total_disk_written, sessions)
}

/// The Node process's meters (S1): its journal, what it has not delivered yet, the harness process
/// groups it runs (including per-session CPU, memory and disk writes) and the process itself.
/// A journal read that fails leaves its figures at zero.
pub(crate) fn meters(j: &Journal) -> SampledMeters {
    let (journal_bytes, journal_file_bytes, journal_commits) = j.meters();
    let (event_backlog, event_backlog_bytes) = j.read(|t| t.backlog()).unwrap_or_default();
    let groups = j.read(|t| t.procs()).unwrap_or_default();
    let mut adapter_cpu_seconds = 0.0_f64;
    let mut adapter_resident_bytes = 0_u64;
    let mut per_session = Vec::with_capacity(groups.len());
    for p in &groups {
        let Some(pgid) = i32::try_from(p.pgid).ok() else {
            continue;
        };
        let Some(u) = steve_runtime::process::group(pgid) else {
            continue;
        };
        adapter_cpu_seconds += u.cpu_seconds;
        adapter_resident_bytes = adapter_resident_bytes.saturating_add(u.resident_bytes);
        per_session.push((
            p.session.clone(),
            u.cpu_seconds,
            u.resident_bytes,
            u.disk_written_bytes.unwrap_or(0),
        ));
    }
    let (adapter_disk_written_bytes, session_vitals) = aggregate_session_vitals(per_session);
    SampledMeters {
        meters: NodeMeters {
            journal_bytes,
            journal_file_bytes,
            journal_commits,
            event_backlog,
            event_backlog_bytes,
            adapters: groups.len() as u32,
            adapter_cpu_seconds,
            adapter_resident_bytes,
            network: network().unwrap_or_default(),
            process: steve_runtime::process::this().map(|u| ProcessMeters {
                cpu_seconds: u.cpu_seconds,
                resident_bytes: u.resident_bytes,
                open_fds: u.open_fds,
                threads: u.threads,
                disk_read_bytes: u.disk_read_bytes,
                disk_written_bytes: u.disk_written_bytes,
            }),
        },
        adapter_disk_written_bytes,
        session_vitals,
    }
}

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
                let space = crate::space::measure(&r.path)?;
                Some(RootVitals {
                    root: r.id.clone(),
                    display: r.display.clone(),
                    free: space.free,
                    total: space.total,
                    inodes_free: space.inodes_free,
                })
            })
            .collect(),
        sessions,
        adapter_disk_written_bytes: 0,
        session_vitals: Vec::new(),
        meters: None,
    }
}

#[cfg(target_os = "linux")]
fn network() -> Option<BTreeMap<String, NetworkBytes>> {
    proc_network(&std::fs::read_to_string("/proc/net/dev").ok()?)
}

#[cfg(any(target_os = "linux", test))]
fn proc_network(data: &str) -> Option<BTreeMap<String, NetworkBytes>> {
    data.lines()
        .skip(2)
        .filter_map(|line| line.split_once(':'))
        .filter(|(name, _)| name.trim() != "lo")
        .map(|(name, counts)| {
            let mut fields = counts.split_whitespace();
            let received = fields.next()?.parse().ok()?;
            let sent = fields.nth(7)?.parse().ok()?;
            Some((name.trim().to_string(), NetworkBytes { received, sent }))
        })
        .collect()
}

#[cfg(target_os = "macos")]
fn network() -> Option<BTreeMap<String, NetworkBytes>> {
    let mib = [libc::CTL_NET, libc::PF_ROUTE, 0, 0, libc::NET_RT_IFLIST2, 0];
    let mut size = 0;
    let read = |out, size| {
        // SAFETY: mib is a valid read-only route query. out is null for sizing or points to
        // a buffer with the capacity given by size. No write to kernel state is requested.
        unsafe {
            libc::sysctl(
                mib.as_ptr().cast_mut(),
                mib.len() as _,
                out,
                size,
                std::ptr::null_mut(),
                0,
            ) == 0
        }
    };
    if !read(std::ptr::null_mut(), &mut size) {
        return None;
    }
    let mut data = vec![0_u8; size];
    if !read(data.as_mut_ptr().cast(), &mut size) {
        return None;
    }
    let mut data = data.get(..size)?;
    let mut counters = BTreeMap::new();
    while !data.is_empty() {
        let [low, high, _, kind, ..] = *data else {
            return None;
        };
        let length = u16::from_ne_bytes([low, high]) as usize;
        if length < 4 {
            return None;
        }
        let (message, rest) = data.split_at_checked(length)?;
        data = rest;
        if kind == libc::RTM_IFINFO2 as u8 {
            if message.len() < std::mem::size_of::<libc::if_msghdr2>() {
                return None;
            }
            // SAFETY: this is a complete Darwin routing message of the matching type.
            // The integer-only C header may be unaligned in the byte buffer.
            let header = unsafe { message.as_ptr().cast::<libc::if_msghdr2>().read_unaligned() };
            if header.ifm_flags & libc::IFF_LOOPBACK == 0 {
                let index = header.ifm_index;
                counters.insert(
                    index.to_string(),
                    NetworkBytes {
                        received: header.ifm_data.ifi_ibytes,
                        sent: header.ifm_data.ifi_obytes,
                    },
                );
            }
        }
    }
    Some(counters)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn network() -> Option<BTreeMap<String, NetworkBytes>> {
    None
}

fn load() -> Option<[f64; 3]> {
    let mut l = [0f64; 3];
    // SAFETY: getloadavg writes at most 3 values into the array it is given
    let n = unsafe { libc::getloadavg(l.as_mut_ptr(), 3) };
    (n == 3).then_some(l)
}

/// (total, available) bytes of memory.
#[cfg(target_os = "linux")]
pub(crate) fn memory() -> (Option<u64>, Option<u64>) {
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
pub(crate) fn memory() -> (Option<u64>, Option<u64>) {
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
pub(crate) fn memory() -> (Option<u64>, Option<u64>) {
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
    fn network_counts_use_receive_and_transmit_columns_and_skip_loopback() {
        let data = "header\nheader\n  lo: 9 0 0 0 0 0 0 0 8\n eth0: 42 0 0 0 0 0 0 0 81\n";
        let counters = proc_network(data).unwrap();
        assert_eq!(counters.len(), 1);
        assert_eq!(
            counters["eth0"],
            NetworkBytes {
                received: 42,
                sent: 81
            }
        );
        assert!(proc_network("header\nheader\neth0: unreadable").is_none());
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

    #[test]
    fn aggregate_session_vitals_sums_sorts_and_caps_at_limit() {
        let sid = |n: usize| SessionId::new(format!("sess_{n:03}")).unwrap();
        let mut samples = Vec::new();
        // 70 distinct sessions (> MAX_REPORTED_SESSION_VITALS = 64), plus a duplicate entry for sess_000.
        samples.push((sid(0), 1.5, 1_000, 2_000));
        samples.push((sid(0), 0.5, 500, 3_000));
        for i in 1..70 {
            let disk = (i as u64) * 10_000;
            let rss = (i as u64) * 1_000;
            samples.push((sid(i), i as f64 * 0.1, rss, disk));
        }
        let expected_total: u64 = 5_000 + (1..70_u64).map(|i| i * 10_000).sum::<u64>();
        let (total_disk, out) = aggregate_session_vitals(samples);
        assert_eq!(total_disk, expected_total);
        assert_eq!(out.len(), MAX_REPORTED_SESSION_VITALS);
        // Highest weight (disk + resident) comes first: sess_069.
        assert_eq!(out[0].session_id, sid(69));
        assert_eq!(out[0].disk_written_bytes, 690_000);
        assert_eq!(out[0].resident_bytes, 69_000);
        // Verify strictly non-increasing weight ordering across the returned slice.
        for pair in out.windows(2) {
            let w0 = pair[0].disk_written_bytes + pair[0].resident_bytes;
            let w1 = pair[1].disk_written_bytes + pair[1].resident_bytes;
            assert!(w0 >= w1);
        }
        // sess_000 aggregated to 5_000 disk + 1_500 rss = 6_500, which is below sess_006..069 (64 items).
        assert!(!out.iter().any(|s| s.session_id == sid(0)));
    }
}
