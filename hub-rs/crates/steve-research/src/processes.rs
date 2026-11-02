//! Tracks only process groups created by this research service. Admission and spawn registration
//! share a lock with shutdown: a process cannot appear between the final snapshot and close.
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Default)]
struct State {
    closed: bool,
    groups: HashSet<u32>,
}
#[derive(Default)]
pub(crate) struct Processes(Mutex<State>);
pub(crate) struct Group {
    owner: Arc<Processes>,
    pid: u32,
}

fn alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // SAFETY: probe a process group allocated by Command::process_group(0), never a scanned PID.
        (unsafe { libc::kill(-(pid as libc::pid_t), 0) == 0 })
            || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}
fn signal(pid: u32, sig: i32) {
    #[cfg(unix)]
    {
        unsafe {
            libc::kill(-(pid as libc::pid_t), sig);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (pid, sig);
    }
}
impl Processes {
    pub(crate) fn admit<T>(&self, f: impl FnOnce() -> T) -> Option<T> {
        let st = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if st.closed { None } else { Some(f()) }
    }
    pub(crate) fn spawn(
        self: &Arc<Self>,
        cmd: &mut tokio::process::Command,
    ) -> std::io::Result<(tokio::process::Child, Group)> {
        let mut st = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if st.closed {
            return Err(std::io::Error::other("research service is shutting down"));
        }
        let child = cmd.spawn()?;
        let pid = child
            .id()
            .ok_or_else(|| std::io::Error::other("research child has no pid"))?;
        st.groups.insert(pid);
        Ok((
            child,
            Group {
                owner: self.clone(),
                pid,
            },
        ))
    }
    pub(crate) fn close(&self) {
        let mut st = self.0.lock().unwrap_or_else(|e| e.into_inner());
        st.closed = true;
        st.groups.retain(|p| alive(*p));
        for pid in &st.groups {
            signal(*pid, libc::SIGTERM);
        }
    }
    /// Bounded confirmation, including grandchildren after their leader has exited.
    pub(crate) async fn finish_shutdown(&self, grace: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + grace;
        let force_at = tokio::time::Instant::now() + grace / 2;
        loop {
            {
                let mut st = self.0.lock().unwrap_or_else(|e| e.into_inner());
                st.groups.retain(|p| alive(*p));
                if st.groups.is_empty() {
                    return true;
                }
                if tokio::time::Instant::now() >= force_at {
                    for pid in &st.groups {
                        signal(*pid, libc::SIGKILL);
                    }
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep_until(
                (tokio::time::Instant::now() + Duration::from_millis(20)).min(deadline),
            )
            .await;
        }
    }
}
impl Drop for Group {
    fn drop(&mut self) {
        // Also clean up a task cancellation or a leader that exits leaving grandchildren behind.
        signal(self.pid, libc::SIGKILL);
        let owner = self.owner.clone();
        let pid = self.pid;
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            rt.spawn(async move {
                loop {
                    {
                        let mut st = owner.0.lock().unwrap_or_else(|e| e.into_inner());
                        if !st.groups.contains(&pid) {
                            return;
                        }
                        if !alive(pid) {
                            st.groups.remove(&pid);
                            return;
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            });
        }
        // Without a runtime retain the entry, so shutdown never falsely reports the group gone.
    }
}
