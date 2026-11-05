//! `HelperStore`: the macOS keychain through `steve-keyhelper` (crates/steve-keyhelper).
//!
//! A login-keychain item gets the partition `cdhash:<creator>` when its creator has no Apple Team ID, so an
//! item the Hub itself created is refused to every rebuilt Hub. The helper is installed outside the releases
//! (`$STEVE_HOME/bin/steve-keyhelper`) and rebuilt only when its source changes, so the items it creates stay
//! readable across Hub releases. It only answers a parent signed as the Hub with its own certificate, so the
//! Hub runs it directly (no shell in between).
//!
//! The value travels over pipes only: `set` writes it to the helper's stdin, `get` reads its stdout.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;

use zeroize::Zeroizing;

use super::{KeyStore, KeyStoreError, decode, encode};
use crate::vault::secret::Kek;

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

    /// Runs `steve-keyhelper <args>`; `Ok((exit code, stdout))` once it has exited.
    fn run(&self, args: &[&str], input: Option<&[u8]>) -> Result<Ran, KeyStoreError> {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
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
            .spawn()
            .map_err(|e| KeyStoreError::Other(format!("run {}: {e}", self.bin.display())))?;
        if let (Some(v), Some(mut stdin)) = (input, child.stdin.take()) {
            stdin
                .write_all(v)
                .map_err(|e| KeyStoreError::Other(format!("write to steve-keyhelper: {e}")))?;
        }
        let mut out = Zeroizing::new(Vec::new());
        if let Some(stdout) = child.stdout.take() {
            stdout
                .take(4096)
                .read_to_end(&mut out)
                .map_err(|e| KeyStoreError::Other(format!("read from steve-keyhelper: {e}")))?;
        }
        let mut err = String::new();
        if let Some(mut stderr) = child.stderr.take() {
            let _ = stderr.read_to_string(&mut err);
        }
        let status = child
            .wait()
            .map_err(|e| KeyStoreError::Other(format!("wait for steve-keyhelper: {e}")))?;
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
}
