//! `HelperStore`: the macOS keychain through `steve-keyhelper` (crates/steve-keyhelper).
//!
//! A login-keychain item gets the partition `cdhash:<creator>` when its creator has no Apple Team ID, so an
//! item the Hub itself created is refused to every rebuilt Hub. The helper is installed outside the releases
//! (`$STEVE_HOME/bin/steve-keyhelper`) and rebuilt only when its source changes, so the items it creates stay
//! readable across Hub releases. It only answers a parent signed as the Hub with its own certificate, so the
//! Hub runs it directly (no shell in between).
//!
//! The value travels over pipes only: `set` writes it to the helper's stdin, `get` reads its stdout.

use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, mpsc};
use std::time::{Duration, Instant};

use zeroize::Zeroizing;

use super::{KeyStore, KeyStoreError, decode, encode};
use crate::vault::secret::Kek;

/// Upper bound for a single `steve-keyhelper` invocation (keychain read/write/delete).
const KEYHELPER_TIMEOUT: Duration = Duration::from_secs(15);
/// Grace period after `steve-keyhelper` exits for its pipes to reach EOF before killing its group.
const KEYHELPER_PIPE_DRAIN_GRACE: Duration = Duration::from_millis(200);
/// Maximum wait to reap `steve-keyhelper` or finish pipe EOF after sending `SIGKILL`.
const KEYHELPER_KILL_REAP_TIMEOUT: Duration = Duration::from_secs(2);
/// Polling interval while waiting for non-blocking stdin writes or helper exit.
const KEYHELPER_POLL_INTERVAL: Duration = Duration::from_millis(10);
/// Maximum bytes read from `steve-keyhelper` stdout or stderr.
const MAX_HELPER_STREAM_BYTES: u64 = 4096;

/// One helper at a time: a key rotation must not interleave a delete and an add of two calls.
static SERIAL: Mutex<()> = Mutex::new(());

/// A finished helper run: exit code, stdout (zeroized), stderr.
type Ran = (Option<i32>, Zeroizing<Vec<u8>>, String);

fn kill_helper_group(pid: libc::pid_t) {
    if pid > 1 {
        // SAFETY: `steve-keyhelper` was spawned with `process_group(0)`, so `-pid` targets only
        // its own group and descendants.
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
}

fn kill_and_reap(child: &mut Child, pid: libc::pid_t) {
    kill_helper_group(pid);
    let _ = child.kill();
    let deadline = Instant::now() + KEYHELPER_KILL_REAP_TIMEOUT;
    while Instant::now() < deadline {
        if !matches!(child.try_wait(), Ok(None)) {
            return;
        }
        std::thread::sleep(KEYHELPER_POLL_INTERVAL);
    }
}

pub struct HelperStore {
    bin: PathBuf,
}

impl std::fmt::Debug for HelperStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HelperStore")
            .field("bin", &self.bin)
            .finish()
    }
}

/// The helper exit codes (crates/steve-keyhelper `Fail`); keep both in step.
fn map_exit(code: Option<i32>, stderr: &str) -> KeyStoreError {
    match code {
        Some(10) => KeyStoreError::Locked,
        Some(11) => KeyStoreError::Denied,
        Some(13) => KeyStoreError::Malformed,
        Some(c) => KeyStoreError::Other(format!("steve-keyhelper exited {c}: {}", stderr.trim())),
        None => KeyStoreError::Other("steve-keyhelper was killed".into()),
    }
}

impl HelperStore {
    /// `STEVE_KEYHELPER`, else `<steve_home>/bin/steve-keyhelper`, if it exists and is executable.
    pub fn find(steve_home: &Path, configured: Option<&str>) -> Option<Self> {
        let bin = configured
            .filter(|s| !s.trim().is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| steve_home.join("bin/steve-keyhelper"));
        use std::os::unix::fs::PermissionsExt;
        let meta = std::fs::metadata(&bin).ok()?;
        (meta.is_file() && meta.permissions().mode() & 0o111 != 0).then_some(Self { bin })
    }

    /// Runs `steve-keyhelper <args>`; `Ok((exit code, stdout, stderr))` once it has exited.
    fn run(&self, args: &[&str], input: Option<&[u8]>) -> Result<Ran, KeyStoreError> {
        self.run_with_timeout(args, input, KEYHELPER_TIMEOUT)
    }

    fn run_with_timeout(
        &self,
        args: &[&str],
        input: Option<&[u8]>,
        timeout: Duration,
    ) -> Result<Ran, KeyStoreError> {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let deadline = Instant::now() + timeout;
        let mut child = Command::new(&self.bin)
            .args(args)
            // nothing of the Hub's environment but what the keychain needs to find the login keychain
            .env_clear()
            .envs(
                ["HOME", "USER", "LOGNAME"]
                    .iter()
                    .filter_map(|k| std::env::var_os(k).map(|v| (*k, v))),
            )
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .map_err(|e| KeyStoreError::Other(format!("run {}: {e}", self.bin.display())))?;
        let pid = child.id() as libc::pid_t;

        let stdout_pipe = child
            .stdout
            .take()
            .ok_or_else(|| KeyStoreError::Other("steve-keyhelper stdout missing".into()))?;
        let stderr_pipe = child
            .stderr
            .take()
            .ok_or_else(|| KeyStoreError::Other("steve-keyhelper stderr missing".into()))?;

        let (out_tx, out_rx) = mpsc::sync_channel::<io::Result<Zeroizing<Vec<u8>>>>(1);
        std::thread::spawn(move || {
            let mut out = Zeroizing::new(Vec::new());
            let res = stdout_pipe
                .take(MAX_HELPER_STREAM_BYTES)
                .read_to_end(&mut out)
                .map(|_| out);
            let _ = out_tx.send(res);
        });
        let (err_tx, err_rx) = mpsc::sync_channel::<io::Result<Vec<u8>>>(1);
        std::thread::spawn(move || {
            let mut err = Vec::new();
            let res = stderr_pipe
                .take(MAX_HELPER_STREAM_BYTES)
                .read_to_end(&mut err)
                .map(|_| err);
            let _ = err_tx.send(res);
        });

        if let (Some(v), Some(mut stdin)) = (input, child.stdin.take()) {
            let fd = stdin.as_raw_fd();
            // SAFETY: `stdin` keeps `fd` valid for both flag queries.
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
            if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } != 0
            {
                drop(stdin);
                kill_and_reap(&mut child, pid);
                return Err(KeyStoreError::Other(format!(
                    "configure stdin for steve-keyhelper: {}",
                    io::Error::last_os_error()
                )));
            }
            let mut written = 0;
            while written < v.len() {
                let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                    drop(stdin);
                    kill_and_reap(&mut child, pid);
                    return Err(KeyStoreError::TimedOut(timeout));
                };
                match stdin.write(&v[written..]) {
                    Ok(0) => {
                        drop(stdin);
                        kill_and_reap(&mut child, pid);
                        return Err(KeyStoreError::Other(
                            "write to steve-keyhelper: pipe closed".into(),
                        ));
                    }
                    Ok(n) => written += n,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        std::thread::sleep(KEYHELPER_POLL_INTERVAL.min(remaining));
                    }
                    Err(e) => {
                        drop(stdin);
                        kill_and_reap(&mut child, pid);
                        return Err(KeyStoreError::Other(format!(
                            "write to steve-keyhelper: {e}"
                        )));
                    }
                }
            }
            drop(stdin);
        }

        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {
                    let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                        kill_and_reap(&mut child, pid);
                        return Err(KeyStoreError::TimedOut(timeout));
                    };
                    std::thread::sleep(KEYHELPER_POLL_INTERVAL.min(remaining));
                }
                Err(e) => {
                    kill_and_reap(&mut child, pid);
                    return Err(KeyStoreError::Other(format!(
                        "wait for steve-keyhelper: {e}"
                    )));
                }
            }
        };

        let drain_deadline = Instant::now() + KEYHELPER_PIPE_DRAIN_GRACE;
        let mut out =
            match out_rx.recv_timeout(drain_deadline.saturating_duration_since(Instant::now())) {
                Ok(Ok(bytes)) => Some(bytes),
                Ok(Err(e)) => {
                    kill_helper_group(pid);
                    return Err(KeyStoreError::Other(format!(
                        "read from steve-keyhelper: {e}"
                    )));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    kill_helper_group(pid);
                    return Err(KeyStoreError::Other(
                        "steve-keyhelper stdout reader stopped".into(),
                    ));
                }
            };
        let mut err_bytes =
            match err_rx.recv_timeout(drain_deadline.saturating_duration_since(Instant::now())) {
                Ok(Ok(bytes)) => Some(bytes),
                Ok(Err(_)) => Some(Vec::new()),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(mpsc::RecvTimeoutError::Disconnected) => Some(Vec::new()),
            };

        if out.is_none() || err_bytes.is_none() {
            kill_helper_group(pid);
            let post_kill = Instant::now() + KEYHELPER_KILL_REAP_TIMEOUT;
            if out.is_none() {
                out = match out_rx.recv_timeout(post_kill.saturating_duration_since(Instant::now()))
                {
                    Ok(Ok(bytes)) => Some(bytes),
                    Ok(Err(e)) => {
                        return Err(KeyStoreError::Other(format!(
                            "read from steve-keyhelper: {e}"
                        )));
                    }
                    Err(_) => return Err(KeyStoreError::TimedOut(KEYHELPER_PIPE_DRAIN_GRACE)),
                };
            }
            if err_bytes.is_none() {
                err_bytes = err_rx
                    .recv_timeout(post_kill.saturating_duration_since(Instant::now()))
                    .ok()
                    .and_then(Result::ok);
            }
        }

        let out = out.unwrap_or_else(|| Zeroizing::new(Vec::new()));
        let err = String::from_utf8_lossy(&err_bytes.unwrap_or_default()).into_owned();
        Ok((status.code(), out, err))
    }
}

impl KeyStore for HelperStore {
    fn kind(&self) -> &'static str {
        "keychain"
    }

    fn get(&self, account: &str) -> Result<Option<Kek>, KeyStoreError> {
        match self.run(&["get", account], None)? {
            (Some(0), out, _) => decode(&out).map(Some),
            (Some(12), _, _) => Ok(None),
            (code, _, err) => Err(map_exit(code, &err)),
        }
    }

    fn set(&self, account: &str, key: &Kek) -> Result<(), KeyStoreError> {
        let value = encode(key);
        match self.run(&["set", account], Some(value.as_bytes()))? {
            (Some(0), _, _) => Ok(()),
            (code, _, err) => Err(map_exit(code, &err)),
        }
    }

    fn delete(&self, account: &str) -> Result<(), KeyStoreError> {
        match self.run(&["delete", account], None)? {
            (Some(0), _, _) => Ok(()),
            (code, _, err) => Err(map_exit(code, &err)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_map_to_store_errors() {
        assert_eq!(map_exit(Some(10), ""), KeyStoreError::Locked);
        assert_eq!(
            map_exit(Some(11), "caller pid 1 is not…"),
            KeyStoreError::Denied
        );
        assert_eq!(map_exit(Some(13), ""), KeyStoreError::Malformed);
        assert!(matches!(map_exit(Some(1), "boom"), KeyStoreError::Other(m) if m.contains("boom")));
        assert!(matches!(map_exit(None, ""), KeyStoreError::Other(_)));
    }

    #[test]
    fn finds_the_installed_helper_only_when_executable() {
        let home = tempfile::tempdir().expect("tmp");
        assert!(HelperStore::find(home.path(), None).is_none());
        let bin = home.path().join("bin/steve-keyhelper");
        std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
        std::fs::write(&bin, "#!/bin/sh\n").unwrap();
        assert!(
            HelperStore::find(home.path(), None).is_none(),
            "not executable"
        );
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(HelperStore::find(home.path(), None).unwrap().bin, bin);
        assert!(HelperStore::find(home.path(), Some("/nonexistent/helper")).is_none());
    }

    /// A stand-in helper script speaking the same protocol, to check the pipes and exit codes end to end.
    #[test]
    fn talks_to_a_helper_over_pipes() {
        let home = tempfile::tempdir().expect("tmp");
        let store_file = home.path().join("value");
        let bin = home.path().join("helper.sh");
        std::fs::write(
            &bin,
            format!(
                "#!/bin/sh\nf='{}'\ncase \"$1\" in\n get) [ -f \"$f\" ] || exit 12; cat \"$f\";;\n set) cat > \"$f\";;\n delete) rm -f \"$f\";;\n *) exit 1;;\nesac\n",
                store_file.display()
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let s = HelperStore::find(home.path(), bin.to_str()).expect("helper");
        assert_eq!(s.get("a").unwrap().map(|_| ()), None);
        let k = Kek::from_slice(&[9u8; 32]).unwrap();
        s.set("a", &k).unwrap();
        assert_eq!(
            s.get("a").unwrap().map(|k| k.bytes().to_vec()),
            Some(vec![9u8; 32])
        );
        s.delete("a").unwrap();
        assert!(s.get("a").unwrap().is_none());
    }

    #[test]
    fn hanging_helper_and_pipe_holding_grandchild_are_bounded_and_killed() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().expect("tmp");

        // 1. A helper that blocks past its timeout is killed along with its process group.
        let hang_pid = home.path().join("hang.pid");
        let hang_bin = home.path().join("hang.sh");
        std::fs::write(
            &hang_bin,
            format!(
                "#!/bin/sh\n/bin/sleep 60 & printf '%s' \"$!\" > '{}'\nwait\n",
                hang_pid.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&hang_bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let store = HelperStore::find(home.path(), hang_bin.to_str()).unwrap();
        let timeout = Duration::from_millis(200);
        let started = Instant::now();
        let err = store
            .run_with_timeout(&["get", "a"], None, timeout)
            .expect_err("hanging helper must time out");
        assert_eq!(err, KeyStoreError::TimedOut(timeout));
        assert!(started.elapsed() < Duration::from_secs(3));
        if let Ok(raw) = std::fs::read_to_string(&hang_pid)
            && let Ok(pid) = raw.trim().parse::<libc::pid_t>()
        {
            let deadline = Instant::now() + Duration::from_secs(2);
            while unsafe { libc::kill(pid, 0) } == 0 && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        }

        // 2. A helper whose grandchild holds stdout open does not deadlock waiting for EOF.
        let holder_pid = home.path().join("holder.pid");
        let holder_bin = home.path().join("holder.sh");
        let key = Kek::from_slice(&[7u8; 32]).unwrap();
        let encoded = encode(&key);
        std::fs::write(
            &holder_bin,
            format!(
                "#!/bin/sh\n/bin/sleep 60 & printf '%s' \"$!\" > '{}'\nprintf '%s' '{}'\n",
                holder_pid.display(),
                encoded.as_str()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&holder_bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let store = HelperStore::find(home.path(), holder_bin.to_str()).unwrap();
        let started = Instant::now();
        let got = store.get("a").unwrap().expect("decoded key");
        assert_eq!(got.bytes(), key.bytes());
        assert!(started.elapsed() < Duration::from_secs(3));
        let pid: libc::pid_t = std::fs::read_to_string(&holder_pid)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while unsafe { libc::kill(pid, 0) } == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    }
}
