//! Harness process groups: whether one is gone, and stopping it with proof.
//!
//! Only the system's own answer that no process of the group exists (ESRCH) counts as gone. A group
//! the Node may not signal (EPERM) or a leader whose start time is not the one recorded (its pid was
//! reused) is uncertain: nothing is freed on it and nothing of it is killed.

use std::time::Duration;

/// What became of a process group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stopped {
    Gone,
    /// Still runs after TERM and KILL.
    Alive,
    /// Cannot be told (not ours to signal, or the leader is another process now).
    Uncertain,
}

impl Stopped {
    pub fn gone(self) -> bool {
        self == Stopped::Gone
    }
}

enum Probe {
    Gone,
    Alive,
    Uncertain,
}

fn probe(pgid: i64) -> Probe {
    if pgid <= 1 {
        return Probe::Uncertain;
    }
    let r = unsafe { libc::kill(-(pgid as libc::pid_t), 0) };
    if r == 0 {
        return Probe::Alive;
    }
    match std::io::Error::last_os_error().raw_os_error() {
        Some(libc::ESRCH) => Probe::Gone,
        _ => Probe::Uncertain,
    }
}

/// The start time of process `pid` (seconds since the epoch, with sub-second precision folded in),
/// `None` when it does not exist or cannot be read.
pub(crate) fn start_time(pid: i64) -> Option<u64> {
    #[cfg(target_os = "macos")]
    {
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
        let n = unsafe {
            libc::proc_pidinfo(
                pid as libc::c_int,
                libc::PROC_PIDTBSDINFO,
                0,
                &mut info as *mut _ as *mut libc::c_void,
                size,
            )
        };
        if n != size {
            return None;
        }
        Some(info.pbi_start_tvsec * 1_000_000 + info.pbi_start_tvusec)
    }
    #[cfg(not(target_os = "macos"))]
    {
        // field 22 of /proc/<pid>/stat, after the command in parentheses
        let s = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let rest = &s[s.rfind(')')? + 2..];
        let ticks: u64 = rest.split_whitespace().nth(19)?.parse().ok()?;
        let boot: u64 = std::fs::read_to_string("/proc/stat")
            .ok()?
            .lines()
            .find_map(|l| l.strip_prefix("btime "))?
            .parse()
            .ok()?;
        let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        if hz <= 0 {
            return None;
        }
        boot.checked_mul(hz as u64)?.checked_add(ticks)
    }
}

/// The recorded group is still the one the Node started: its leader, if alive, began when recorded.
/// (While any member of a group lives, its id is not given to another group; a dead leader with
/// living members is still ours.)
fn ours(pgid: i64, started: Option<u64>) -> bool {
    match (start_time(pgid), started) {
        (Some(now), Some(then)) => now == then,
        // A missing or unreadable leader is not proof of group ownership. Without a
        // durable group handle (e.g. a cgroup) leave it uncertain for reconciliation.
        (None, _) => false,
        (Some(_), None) => false,
    }
}

/// Is the group gone (without touching it)?
pub(crate) fn state(pgid: i64, started: Option<u64>) -> Stopped {
    match probe(pgid) {
        Probe::Gone => Stopped::Gone,
        Probe::Uncertain => Stopped::Uncertain,
        Probe::Alive if !ours(pgid, started) => Stopped::Uncertain,
        Probe::Alive => Stopped::Alive,
    }
}

/// The processes the group's leader started (its descendants plus the other members of its group,
/// the leader excluded, pid > 1), only while the recorded leader is provably still the one we
/// started, checked before and after reading the table. Anything uncertain, or `ps` failing, is
/// empty: the Hub then asks.
pub(crate) fn started_by(pgid: i64, started: Option<u64>) -> Vec<u32> {
    if pgid <= 1 || pgid > u32::MAX as i64 || state(pgid, started) != Stopped::Alive {
        return Vec::new();
    }
    let out = match std::process::Command::new("ps")
        .args(["-A", "-o", "pid=,ppid=,pgid="])
        .output()
    {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
        _ => return Vec::new(),
    };
    let v = started_in(&parse_ps(&out), pgid as u32);
    if state(pgid, started) != Stopped::Alive {
        return Vec::new();
    }
    v
}

/// `(pid, ppid, pgid)` rows of `ps -A -o pid=,ppid=,pgid=` (bad lines skipped).
fn parse_ps(out: &str) -> Vec<(u32, u32, u32)> {
    out.lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace().map(|x| x.parse::<u32>().ok());
            Some((it.next()??, it.next()??, it.next()??))
        })
        .collect()
}

/// The same rule as the Hub's legacy YOLO (`steve-core` yolo/procs.rs), kept here so the Node does
/// not depend on steve-core.
fn started_in(table: &[(u32, u32, u32)], root: u32) -> Vec<u32> {
    use std::collections::{HashMap, HashSet};
    if root <= 1 {
        return Vec::new();
    }
    let mut kids: HashMap<u32, Vec<u32>> = HashMap::new();
    for &(pid, ppid, _) in table {
        kids.entry(ppid).or_default().push(pid);
    }
    let mut seen: HashSet<u32> = HashSet::new();
    let mut stack = vec![root];
    while let Some(p) = stack.pop() {
        for &c in kids.get(&p).into_iter().flatten() {
            if c != root && seen.insert(c) {
                stack.push(c);
            }
        }
    }
    for &(pid, _, pgid) in table {
        if pgid == root && pid != root {
            seen.insert(pid);
        }
    }
    let mut v: Vec<u32> = seen.into_iter().filter(|p| *p > 1).collect();
    v.sort_unstable();
    v
}

/// Stops the group (TERM, then KILL) and says what became of it.
pub(crate) async fn stop(pgid: i64, started: Option<u64>) -> Stopped {
    stop_checked(
        || state(pgid, started),
        |signal| unsafe { libc::kill(-(pgid as libc::pid_t), signal) == 0 },
    )
    .await
}

async fn stop_checked(
    mut state: impl FnMut() -> Stopped,
    mut signal: impl FnMut(i32) -> bool,
) -> Stopped {
    for sig in [libc::SIGTERM, libc::SIGKILL] {
        match state() {
            Stopped::Alive => {}
            other => return other,
        }
        if !signal(sig) {
            return if state().gone() {
                Stopped::Gone
            } else {
                Stopped::Uncertain
            };
        }
        for _ in 0..30 {
            match state() {
                Stopped::Alive => {}
                other => return other,
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    state()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn started_in_is_descendants_and_the_group() {
        let t = parse_ps(
            "  1 0 1\n 100 1 100\n 101 100 100\n 102 101 100\n 103 1 100\n 200 1 200\n 201 200 200\n 104 102 104\n bad line\n",
        );
        // 101, 102 by parent; 103 re-parented but still in the group; 104 its own group but a child
        assert_eq!(started_in(&t, 100), vec![101, 102, 103, 104]);
        assert!(!started_in(&t, 100).contains(&201));
        assert!(started_in(&t, 1).is_empty());
    }

    #[test]
    fn started_by_needs_the_recorded_leader() {
        use std::os::unix::process::CommandExt;
        let mut leader = std::process::Command::new("sh")
            .args(["-c", "sleep 5 & wait"])
            .process_group(0)
            .spawn()
            .unwrap();
        let pgid = leader.id() as i64;
        let started = start_time(pgid);
        let mut kids = vec![];
        for _ in 0..100 {
            kids = started_by(pgid, started);
            if !kids.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        // a wrong start time is a reused pid: nothing is the harness's
        let reused = started_by(pgid, started.map(|s| s + 1));
        unsafe { libc::kill(-(pgid as libc::pid_t), libc::SIGKILL) };
        let _ = leader.wait();
        assert!(!kids.is_empty(), "the sleep is the leader's");
        assert!(
            !kids.contains(&(pgid as u32)),
            "the leader itself is excluded"
        );
        assert!(reused.is_empty());
        // a dead group yields nothing
        for _ in 0..100 {
            if state(pgid, started) == Stopped::Gone {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(started_by(pgid, started).is_empty());
    }

    #[tokio::test]
    async fn lost_identity_prevents_escalation() {
        let mut probes = 0;
        let mut signals = vec![];
        let stopped = stop_checked(
            || {
                probes += 1;
                if probes == 1 {
                    Stopped::Alive
                } else {
                    Stopped::Uncertain
                }
            },
            |s| {
                signals.push(s);
                true
            },
        )
        .await;
        assert_eq!(stopped, Stopped::Uncertain);
        assert_eq!(signals, vec![libc::SIGTERM]);
        assert!(!ours(std::process::id() as i64, None));
    }

    #[tokio::test]
    async fn a_group_is_stopped_with_proof() {
        use std::os::unix::process::CommandExt;
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .unwrap();
        let pid = child.id() as i64;
        // reaped as it exits (a zombie still counts as a process of the group)
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        let started = start_time(pid);
        assert!(started.is_some());
        assert_eq!(state(pid, started), Stopped::Alive);
        // a recorded start time that is not this process's: not ours to kill
        assert_eq!(state(pid, started.map(|s| s + 1)), Stopped::Uncertain);
        assert_eq!(stop(pid, started).await, Stopped::Gone);
        assert_eq!(state(pid, started), Stopped::Gone);
    }
}
