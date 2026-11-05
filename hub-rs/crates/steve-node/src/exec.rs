//! A command run in a workspace ([`steve_node_api::WorkspaceExecutor::exec`]): `/bin/sh -c` in its
//! own process group, stdin from `/dev/null`, stdout and stderr read while it runs and kept up to a
//! bound (the rest is read and dropped, so a chatty command never blocks on a full pipe), and the
//! whole group killed at the deadline and when the command ends (what it started in the background
//! goes with it).
//!
//! The process waits on a pipe until its identity (process group and start time) is in the journal,
//! as the workspace's git does: a Node that dies before recording it closes the pipe and the shell
//! exits without running the command. A Node that dies after leaves a recorded group, which a
//! restart reports as `unknown` (never run again) and [`steve_node_api::ExecutionControl::recover`]
//! stops.
//!
//! Secrets the command was given ([`Run::secret_env`], [`Run::stdin`]) are only in memory: put in
//! its environment, or written to its stdin after the go line. What it prints is masked
//! ([`Run::mask`]) before it leaves here.

use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use steve_node_api::OperationId;
use steve_vault::redact::Redactor;
use zeroize::Zeroizing;

use crate::procs::{self, Stopped};

/// What the Hub's own processes carry and a command must not (as the vault's `inject::run`).
const HUB_ONLY: [&str; 3] = [
    "STEVE_UPGRADE_CONTROL_TOKEN",
    "STEVE_SUPERVISOR_PID",
    "STEVE_UPGRADE_ID",
];

/// How long what is left of the group may take to go once it was killed.
const GONE_WAIT: Duration = Duration::from_secs(3);

pub(crate) struct Run {
    pub dir: PathBuf,
    pub command: String,
    pub env: Vec<(String, String)>,
    /// When the group is killed (ms since the epoch).
    pub deadline: i64,
    /// Bytes kept per stream.
    pub cap: usize,
    /// Secret values for its environment.
    pub secret_env: Vec<(String, Zeroizing<Vec<u8>>)>,
    /// A secret for its stdin (else stdin is `/dev/null`).
    pub stdin: Option<Zeroizing<Vec<u8>>>,
    /// What to mask in its output: the secrets it was given, and their temp files' directory.
    pub mask: Option<Redactor>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ended {
    /// It exited: its code, `None` when a signal ended it.
    Exited(Option<i32>),
    /// Killed at the deadline.
    TimedOut,
    /// Killed because the Node is shutting down.
    Stopped,
}

#[derive(Debug)]
pub(crate) struct Ran {
    pub ended: Ended,
    pub stdout: String,
    pub stderr: String,
    pub truncated: bool,
    /// The whole group is confirmed gone (its record is dropped); else the record stays for recovery.
    pub gone: bool,
}

/// Runs `r` for operation `op`. `Err`: it did not start (nothing of it runs).
pub(crate) fn run(inner: &Arc<crate::Inner>, op: &OperationId, r: &Run) -> Result<Ran, String> {
    let session = crate::workspace::control_session(op);
    if inner.closed.load(Ordering::SeqCst) {
        return Err("node shutting down".into());
    }
    let mut cmd = Command::new("/bin/sh");
    // `read` takes the go line a byte at a time, so a secret written after it is the command's stdin
    let wrapper = if r.stdin.is_some() {
        "IFS= read -r go || exit 125; exec /bin/sh -c \"$1\""
    } else {
        "IFS= read -r go || exit 125; exec /bin/sh -c \"$1\" </dev/null"
    };
    cmd.args(["-c", wrapper, "steve-exec", &r.command])
        .current_dir(&r.dir)
        .process_group(0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in &r.env {
        cmd.env(k, v);
    }
    for (k, v) in &r.secret_env {
        use std::os::unix::ffi::OsStrExt;
        cmd.env(k, std::ffi::OsStr::from_bytes(v));
    }
    for k in HUB_ONLY {
        cmd.env_remove(k);
    }
    let spawned = cmd.spawn().map_err(|e| e.to_string());
    // the builder holds copies of the environment's values: gone as soon as the child runs
    drop(cmd);
    let mut child = spawned?;
    let pid = child.id() as i64;
    let started = procs::start_time(pid);
    if let Err(e) = inner
        .j
        .tx(|t| t.put_proc(&session, pid, 0, inner.incarnation, "exec", started))
    {
        // the shell still waits for its go: closing the pipe ends it before the command runs
        drop(child.stdin.take());
        let _ = child.kill();
        let _ = child.wait();
        return Err(e.to_string());
    }
    let mut out = child.stdout.take().expect("pipe");
    let mut err = child.stderr.take().expect("pipe");
    for fd in [out.as_raw_fd(), err.as_raw_fd()] {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            // not told to go yet: it ends without running the command
            drop(child.stdin.take());
            let _ = child.kill();
            let _ = child.wait();
            let gone = settle(inner, &session, pid, started);
            return if gone {
                Err("cannot read the command's output".into())
            } else {
                Ok(Ran {
                    ended: Ended::Stopped,
                    stdout: String::new(),
                    stderr: String::new(),
                    truncated: false,
                    gone,
                })
            };
        }
    }
    let feeder = {
        use std::io::Write;
        let mut go = child.stdin.take().expect("pipe");
        let _ = go.write_all(b"go\n");
        // a command that never reads its stdin must not hold this up: written on the side
        r.stdin.clone().map(|bytes| {
            std::thread::spawn(move || {
                let _ = go.write_all(&bytes);
            })
        })
    };
    // a secret cut at the bound would not be recognized: keep enough past it to mask, then cut
    let longest = r.mask.as_ref().map_or(0, Redactor::longest);
    let mut stdout = Capped::new(r.cap + longest);
    let mut stderr = Capped::new(r.cap + longest);
    let deadline = Instant::now()
        + Duration::from_millis(r.deadline.saturating_sub(crate::now()).max(0) as u64);
    let ended = loop {
        stdout.drain(&mut out);
        stderr.drain(&mut err);
        if let Some(code) = exited(&child) {
            break Ended::Exited(code);
        }
        if Instant::now() >= deadline {
            break Ended::TimedOut;
        }
        if inner.closed.load(Ordering::SeqCst) {
            break Ended::Stopped;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    // the leader is not reaped yet, so its group id is still this group's: kill all of it
    kill_group(pid);
    let _ = child.wait();
    // what is left in the pipes, until every writer is gone (bounded)
    let until = Instant::now() + GONE_WAIT;
    while Instant::now() < until && !(stdout.eof && stderr.eof) {
        stdout.drain(&mut out);
        stderr.drain(&mut err);
        std::thread::sleep(Duration::from_millis(10));
    }
    let gone = settle(inner, &session, pid, started);
    // the group is gone, so its end of the stdin pipe is: the writer ends
    if let Some(f) = feeder {
        let _ = f.join();
    }
    let (out, out_cut) = stdout.text(r.mask.as_ref(), r.cap);
    let (err, err_cut) = stderr.text(r.mask.as_ref(), r.cap);
    Ok(Ran {
        ended,
        truncated: stdout.truncated || stderr.truncated || out_cut || err_cut,
        stdout: out,
        stderr: err,
        gone,
    })
}

/// Waits (bounded) for the group to be gone, and drops its record once it is.
fn settle(
    inner: &crate::Inner,
    session: &steve_node_api::SessionId,
    pid: i64,
    started: Option<u64>,
) -> bool {
    let until = Instant::now() + GONE_WAIT;
    loop {
        if procs::state(pid, started) == Stopped::Gone {
            return inner.j.tx(|t| t.drop_proc(session, pid)).is_ok();
        }
        if Instant::now() >= until {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Whether the leader exited (its code, `None` for a signal), without reaping it.
fn exited(child: &Child) -> Option<Option<i32>> {
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let r = unsafe {
        libc::waitid(
            libc::P_PID,
            child.id() as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if r != 0 {
        return None;
    }
    #[cfg(target_os = "macos")]
    let pid = info.si_pid;
    #[cfg(not(target_os = "macos"))]
    let pid = unsafe { info.si_pid() };
    if pid == 0 {
        return None;
    }
    #[cfg(target_os = "macos")]
    let status = info.si_status;
    #[cfg(not(target_os = "macos"))]
    let status = unsafe { info.si_status() };
    Some((info.si_code == libc::CLD_EXITED).then_some(status))
}

fn kill_group(pgid: i64) {
    if pgid > 1 {
        unsafe {
            libc::kill(-(pgid as libc::pid_t), libc::SIGKILL);
        }
    }
}

/// One stream: kept up to `cap` bytes, the rest read and dropped.
struct Capped {
    kept: Vec<u8>,
    cap: usize,
    truncated: bool,
    eof: bool,
}

impl Capped {
    fn new(cap: usize) -> Self {
        Capped {
            kept: Vec::new(),
            cap,
            truncated: false,
            eof: false,
        }
    }

    fn drain(&mut self, r: &mut impl Read) {
        let mut buf = [0u8; 8192];
        // a budget per tick: a stream that never stops must not starve the deadline check
        for _ in 0..64 {
            match r.read(&mut buf) {
                Ok(0) => {
                    self.eof = true;
                    return;
                }
                Ok(n) => {
                    let room = self.cap.saturating_sub(self.kept.len());
                    if n > room {
                        self.truncated = true;
                    }
                    self.kept.extend_from_slice(&buf[..n.min(room)]);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => {
                    self.eof = true;
                    return;
                }
            }
        }
    }

    /// As text, masked, cut to `cap` bytes on a character boundary; and whether it was cut.
    fn text(&self, mask: Option<&Redactor>, cap: usize) -> (String, bool) {
        let kept = match mask {
            Some(m) => m.scrub_bytes(&self.kept),
            None => std::borrow::Cow::Borrowed(&self.kept[..]),
        };
        let s = String::from_utf8_lossy(&kept);
        if s.len() <= cap {
            return (s.into_owned(), false);
        }
        let mut end = cap;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        (s[..end].to_string(), true)
    }
}
