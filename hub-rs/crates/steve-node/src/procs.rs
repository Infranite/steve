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
        rest.split_whitespace().nth(19)?.parse().ok()
    }
}

/// The recorded group is still the one the Node started: its leader, if alive, began when recorded.
/// (While any member of a group lives, its id is not given to another group; a dead leader with
/// living members is still ours.)
fn ours(pgid: i64, started: Option<u64>) -> bool {
    match (start_time(pgid), started) {
        (Some(now), Some(then)) => now == then,
        (None, _) => true,
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

/// Stops the group (TERM, then KILL) and says what became of it.
pub(crate) async fn stop(pgid: i64, started: Option<u64>) -> Stopped {
    match state(pgid, started) {
        Stopped::Alive => {}
        other => return other,
    }
    unsafe { libc::kill(-(pgid as libc::pid_t), libc::SIGTERM) };
    for _ in 0..30 {
        if let Probe::Gone = probe(pgid) {
            return Stopped::Gone;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    unsafe { libc::kill(-(pgid as libc::pid_t), libc::SIGKILL) };
    for _ in 0..30 {
        match probe(pgid) {
            Probe::Gone => return Stopped::Gone,
            Probe::Uncertain => return Stopped::Uncertain,
            Probe::Alive => tokio::time::sleep(Duration::from_millis(100)).await,
        }
    }
    Stopped::Alive
}

#[cfg(test)]
mod tests {
    use super::*;

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
