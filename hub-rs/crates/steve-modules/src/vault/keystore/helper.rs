//! `HelperStore`: the macOS keychain through `steve-keyhelper` (crates/steve-keyhelper).
//!
//! A login-keychain item gets the partition `cdhash:<creator>` when its creator has no Apple Team ID, so an
//! item the Hub itself created is refused to every rebuilt Hub. The helper is installed outside the releases
//! (`$STEVE_HOME/bin/steve-keyhelper`) and rebuilt only when its source changes, so the items it creates stay
//! readable across Hub releases. It only answers a parent signed as the Hub with its own certificate, so the
//! Hub runs it directly (no shell in between).
//!
//! The value travels over pipes only: `set` writes it to the helper's stdin, `get` reads its stdout.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::Duration;

use zeroize::Zeroizing;

use super::{KEYHELPER_TIMEOUT, KeyStore, KeyStoreError, decode, encode};
use crate::vault::secret::Kek;

/// Maximum bytes read from `steve-keyhelper` stdout or stderr. A valid base64 32-byte KEK is 44
/// bytes; 4 KiB leaves wide room for diagnostic stderr while capping runaway output and keeping
/// stdout pre-allocated in `Zeroizing<Vec<u8>>` without heap reallocation.
const MAX_HELPER_STREAM_BYTES: usize = 4096;

/// One helper at a time: a key rotation must not interleave a delete and an add of two calls.
static SERIAL: Mutex<()> = Mutex::new(());

/// A finished helper run: exit code, stdout (zeroized), stderr.
type Ran = (Option<i32>, Zeroizing<Vec<u8>>, String);

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
    /// `STEVE_KEYHELPER`, else `<steve_home>/bin/steve-keyhelper`, if it is an absolute path to
    /// an executable regular file (canonicalized so `Command::env_clear()` on macOS never falls
    /// back to multithreaded `fork()`).
    pub fn find(steve_home: &Path, configured: Option<&str>) -> Option<Self> {
        let bin = configured
            .filter(|s| !s.trim().is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| steve_home.join("bin/steve-keyhelper"));
        if !bin.is_absolute() {
            return None;
        }
        let resolved = std::fs::canonicalize(&bin).ok()?;
        use std::os::unix::fs::PermissionsExt;
        let meta = std::fs::metadata(&resolved).ok()?;
        (meta.is_file() && meta.permissions().mode() & 0o111 != 0).then_some(Self { bin: resolved })
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
        let mut command = Command::new(&self.bin);
        command
            .args(args)
            // nothing of the Hub's environment but what the keychain needs to find the login keychain
            .env_clear()
            .envs(
                ["HOME", "USER", "LOGNAME"]
                    .iter()
                    .filter_map(|k| std::env::var_os(k).map(|v| (*k, v))),
            );
        let program = self.bin.to_string_lossy();
        let output = steve_runtime::process::run_bounded_zeroizing(
            &mut command,
            &program,
            steve_runtime::process::BoundedOptions {
                stdin: match input {
                    Some(bytes) => steve_runtime::process::BoundedStdin::Bytes(bytes),
                    None => steve_runtime::process::BoundedStdin::Null,
                },
                // `steve-keyhelper` checks parent code-signing (`Denied`, exit 11) and keychain
                // lock state (`Locked`, exit 10) before reading stdin; ignoring EPIPE ensures
                // `map_exit` surfaces the real exit code instead of `BrokenPipe`.
                ignore_stdin_broken_pipe: true,
                max_output_bytes: MAX_HELPER_STREAM_BYTES,
                ..steve_runtime::process::BoundedOptions::capture("keyhelper", timeout)
            },
        )
        .map_err(|err| match err {
            steve_runtime::process::CommandError::TimedOut { timeout, .. } => {
                KeyStoreError::TimedOut(timeout)
            }
            steve_runtime::process::CommandError::PipeDrainTimedOut { grace, .. } => {
                KeyStoreError::TimedOut(grace)
            }
            steve_runtime::process::CommandError::Spawn { source, .. } => {
                KeyStoreError::Other(format!("run {}: {source}", self.bin.display()))
            }
            other => KeyStoreError::Other(other.to_string()),
        })?;
        let err = String::from_utf8_lossy(&output.stderr).into_owned();
        Ok((output.status.code(), output.stdout, err))
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
        assert_eq!(
            HelperStore::find(home.path(), None).unwrap().bin,
            std::fs::canonicalize(&bin).unwrap()
        );
        assert!(HelperStore::find(home.path(), Some("/nonexistent/helper")).is_none());
        assert!(
            HelperStore::find(home.path(), Some("steve-keyhelper")).is_none(),
            "relative STEVE_KEYHELPER must be rejected"
        );
        assert!(
            HelperStore::find(home.path(), Some("./bin/steve-keyhelper")).is_none(),
            "relative path STEVE_KEYHELPER must be rejected"
        );
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
        let started = std::time::Instant::now();
        let err = store
            .run_with_timeout(&["get", "a"], None, timeout)
            .expect_err("hanging helper must time out");
        assert_eq!(err, KeyStoreError::TimedOut(timeout));
        assert!(started.elapsed() < Duration::from_secs(3));
        if let Ok(raw) = std::fs::read_to_string(&hang_pid)
            && let Ok(pid) = raw.trim().parse::<libc::pid_t>()
        {
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            while unsafe { libc::kill(pid, 0) } == 0 && std::time::Instant::now() < deadline {
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
        let started = std::time::Instant::now();
        let got = store.get("a").unwrap().expect("decoded key");
        assert_eq!(got.bytes(), key.bytes());
        assert!(started.elapsed() < Duration::from_secs(3));
        let pid: libc::pid_t = std::fs::read_to_string(&holder_pid)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while unsafe { libc::kill(pid, 0) } == 0 && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    }

    #[test]
    fn early_denied_or_locked_exit_is_not_masked_by_broken_pipe() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().expect("tmp");
        let key = Kek::from_slice(&[5u8; 32]).unwrap();
        for (code, expected) in [(11, KeyStoreError::Denied), (10, KeyStoreError::Locked)] {
            let bin = home.path().join(format!("exit-{code}.sh"));
            std::fs::write(&bin, format!("#!/bin/sh\nexit {code}\n")).unwrap();
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
            let store = HelperStore::find(home.path(), bin.to_str()).unwrap();
            assert_eq!(
                store.set("ws/kek/v1", &key),
                Err(expected),
                "exit code {code} must surface even if stdin is never read"
            );
        }
    }
}
