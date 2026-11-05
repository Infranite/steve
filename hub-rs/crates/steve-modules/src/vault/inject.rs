//! Injection: Steve runs the command itself, so the agent never holds a
//! value or the path of a file with one.
//!
//! - `cwd` must canonicalize to a directory inside the session's working directory;
//! - environment = the Hub's (which the session's agent process inherits too) plus the injected
//!   variables; `file` values go to `STEVE_HOME/scratch/secrets/<su id>/<VAR>` (directory 0700, file
//!   0600) and the variable holds the path; `stdin` is written and closed;
//! - `/bin/sh -c <command>` in its own process group; on timeout (default 120 s, at most 600 s) the
//!   whole group is killed; when the command ends, whatever it left in its group is killed too and
//!   the temp directory is deleted right away; leftovers are removed at startup ([`cleanup`]);
//! - stdout and stderr keep their first 64 KiB each, redacted before anything else sees them: the
//!   injected values in all their forms, and the temp directory's path.
//!
//! [`Vault::injection_values`] is one of the three decryption sites.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::LazyLock;
use std::time::{Duration, SystemTime};

use regex::Regex;
use steve_vault::redact::Redactor;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

use super::crypto::open_field;
use super::items::repo;
use super::secret::SecretBytes;
use super::{Vault, VaultError, VaultResult};
use crate::ApiError;

/// `400` when `cwd` leaves the session's workspace, verbatim.
pub const CWD_OUTSIDE: &str = "目录不在这个会话的工作区里";
pub const DEFAULT_TIMEOUT_S: u64 = 120;
pub const MAX_TIMEOUT_S: u64 = 600;
/// Bytes kept per stream.
pub const OUTPUT_CAP: usize = 64 * 1024;
/// How long to keep reading pipes after the command ended (a straggler may still hold them).
const DRAIN: Duration = Duration::from_secs(2);
/// The label the temp directory's path is masked with.
const TEMP_LABEL: &str = "临时文件";

static VAR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z_][A-Za-z0-9_]{0,63}$").expect("var pattern"));
static USE_ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^su_[A-Za-z0-9]{1,64}$").expect("use id pattern"));

/// A valid variable name (also a safe file name).
pub fn valid_var(v: &str) -> bool {
    VAR.is_match(v)
}

/// How one field reaches the command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum How {
    Env,
    /// The variable holds the path of a 0600 file with the value.
    File,
    Stdin,
}

impl How {
    /// `secret_uses.mode`.
    pub fn mode(self) -> &'static str {
        match self {
            Self::Env => "env",
            Self::File => "file",
            Self::Stdin => "stdin",
        }
    }
}

/// One injected field. `var` is empty for stdin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Injection {
    pub var: String,
    pub label: String,
    pub how: How,
}

/// What to run. Holds no value.
#[derive(Debug, Clone)]
pub struct RunSpec {
    /// The `secret_uses` id; names the temp directory.
    pub use_id: String,
    /// Already checked by [`resolve_cwd`].
    pub cwd: PathBuf,
    pub command: String,
    pub injections: Vec<Injection>,
    pub timeout: Duration,
}

/// What came back, already redacted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunOutput {
    /// `None` when killed by a signal (timeout) or never started.
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub stdout: String,
    pub stderr: String,
    /// The command could not be started (or a temp file not written): the reason, redacted.
    pub error: Option<String>,
}

impl RunOutput {
    /// The number shown to the agent and the user: the exit code, or -1.
    pub fn code(&self) -> i32 {
        self.exit_code.unwrap_or(-1)
    }
    pub fn ok(&self) -> bool {
        self.exit_code == Some(0)
    }
    /// stdout, then stderr under a marker, then the timeout note.
    pub fn combined(&self) -> String {
        let mut s = self.stdout.clone();
        if !self.stderr.is_empty() {
            if !s.is_empty() && !s.ends_with('\n') {
                s.push('\n');
            }
            s.push_str("[stderr]\n");
            s.push_str(&self.stderr);
        }
        if let Some(e) = &self.error {
            if !s.is_empty() && !s.ends_with('\n') {
                s.push('\n');
            }
            s.push_str(&format!("[steve] {e}"));
        }
        s
    }
}

/// `cwd` (relative to `workdir`, or absolute) canonicalized; it must be `workdir` or below it.
pub fn resolve_cwd(workdir: &Path, cwd: Option<&str>) -> VaultResult<PathBuf> {
    let root = workdir
        .canonicalize()
        .map_err(|_| ApiError::rule(CWD_OUTSIDE))?;
    let want = match cwd.map(str::trim).filter(|c| !c.is_empty()) {
        Some(c) => root.join(c),
        None => root.clone(),
    };
    let real = want
        .canonicalize()
        .map_err(|_| ApiError::rule(CWD_OUTSIDE))?;
    if !real.starts_with(&root) || !real.is_dir() {
        return Err(ApiError::rule(CWD_OUTSIDE).into());
    }
    Ok(real)
}

/// `STEVE_HOME/scratch/secrets`.
pub fn secrets_root(home: &Path) -> PathBuf {
    home.join("scratch").join("secrets")
}

/// Removes temp directories under [`secrets_root`] older than `older_than` (`None` = all, at startup).
/// Returns how many were removed.
pub fn cleanup(home: &Path, older_than: Option<Duration>) -> usize {
    let Ok(rd) = std::fs::read_dir(secrets_root(home)) else {
        return 0;
    };
    let now = SystemTime::now();
    let mut n = 0;
    for e in rd.flatten() {
        let old = match older_than {
            None => true,
            Some(age) => e
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| now.duration_since(t).ok())
                .is_none_or(|d| d >= age),
        };
        if old && std::fs::remove_dir_all(e.path()).is_ok() {
            n += 1;
        }
    }
    if n > 0 {
        tracing::info!(removed = n, "vault: removed leftover secret temp dirs");
    }
    n
}

/// The temp directory of one run; deleted on drop, whatever happens.
struct TempDir(PathBuf);

impl TempDir {
    fn create(home: &Path, use_id: &str) -> std::io::Result<Self> {
        if !USE_ID.is_match(use_id) {
            return Err(std::io::Error::other("bad use id"));
        }
        let root = secrets_root(home);
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&root)?;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
        let dir = root.join(use_id);
        std::fs::DirBuilder::new().mode(0o700).create(&dir)?;
        Ok(Self(dir))
    }

    fn write(&self, var: &str, value: &[u8]) -> std::io::Result<PathBuf> {
        use std::io::Write;
        let p = self.0.join(var);
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&p)?;
        f.write_all(value)?;
        f.sync_all()?;
        Ok(p)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_dir_all(&self.0)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(error = %e, "vault: could not remove a secret temp dir");
        }
    }
}

fn value_of<'a>(values: &'a [(String, SecretBytes)], label: &str) -> Option<&'a SecretBytes> {
    values.iter().find(|(l, _)| l == label).map(|(_, v)| v)
}

/// Reads a pipe to the end, keeping the first `cap` bytes.
async fn read_capped(mut r: impl AsyncRead + Unpin, cap: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match r.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let room = cap.saturating_sub(out.len());
                out.extend_from_slice(&buf[..n.min(room)]);
            }
        }
    }
    out
}

fn kill_group(pid: Option<u32>) {
    if let Some(pid) = pid.and_then(|p| i32::try_from(p).ok()).filter(|p| *p > 1) {
        // SAFETY: plain syscall; a negative pid addresses the process group the child leads
        // (`process_group(0)`). ESRCH (nothing left) is fine.
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
}

/// Redacts, then cuts to [`OUTPUT_CAP`] on a char boundary.
fn finish(r: &Redactor, raw: &[u8]) -> String {
    let red = r.scrub_bytes(raw);
    let s = String::from_utf8_lossy(&red);
    if s.len() <= OUTPUT_CAP {
        return s.into_owned();
    }
    let mut end = OUTPUT_CAP;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// Runs `spec` with `values` (label → value) injected. Never returns a value or the temp path.
pub async fn run(home: &Path, spec: &RunSpec, values: &[(String, SecretBytes)]) -> RunOutput {
    let mut pats: Vec<(&str, &[u8])> = values
        .iter()
        .map(|(l, v)| (l.as_str(), v.expose()))
        .collect();
    let dir_text = secrets_root(home).join(&spec.use_id);
    let dir_text = dir_text.to_string_lossy().into_owned();
    pats.push((TEMP_LABEL, dir_text.as_bytes()));
    let redactor = Redactor::new(pats);
    let fail = |msg: String| RunOutput {
        exit_code: None,
        timed_out: false,
        stdout: String::new(),
        stderr: String::new(),
        error: Some(redactor.scrub_str(&msg).into_owned()),
    };

    let files: Vec<&Injection> = spec
        .injections
        .iter()
        .filter(|i| i.how == How::File)
        .collect();
    let temp = if files.is_empty() {
        None
    } else {
        match TempDir::create(home, &spec.use_id) {
            Ok(t) => Some(t),
            Err(e) => return fail(format!("could not create the temp directory: {e}")),
        }
    };

    let mut cmd = tokio::process::Command::new("/bin/sh");
    for key in [
        "STEVE_UPGRADE_CONTROL_TOKEN",
        "STEVE_SUPERVISOR_PID",
        "STEVE_UPGRADE_ID",
    ] {
        cmd.env_remove(key);
    }
    cmd.arg("-c")
        .arg(&spec.command)
        .current_dir(&spec.cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .kill_on_drop(true);
    let mut stdin_value: Option<&SecretBytes> = None;
    for inj in &spec.injections {
        let Some(v) = value_of(values, &inj.label) else {
            return fail(format!("field {} has no value", inj.label));
        };
        match inj.how {
            How::Env => {
                cmd.env(&inj.var, OsStr::from_bytes(v.expose()));
            }
            How::File => {
                let Some(t) = &temp else { continue };
                match t.write(&inj.var, v.expose()) {
                    Ok(p) => {
                        cmd.env(&inj.var, p);
                    }
                    Err(e) => return fail(format!("could not write the temp file: {e}")),
                }
            }
            How::Stdin => stdin_value = Some(v),
        }
    }
    cmd.stdin(if stdin_value.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return fail(format!("could not start the command: {e}")),
    };
    // the command builder holds copies of env values: drop it as soon as the child runs
    drop(cmd);
    let pid = child.id();
    let feeder = match (child.stdin.take(), stdin_value) {
        (Some(mut w), Some(v)) => {
            let bytes = zeroize::Zeroizing::new(v.expose().to_vec());
            Some(tokio::spawn(async move {
                let _ = w.write_all(&bytes).await;
                let _ = w.shutdown().await;
            }))
        }
        _ => None,
    };
    let keep = OUTPUT_CAP + redactor.longest();
    let out = child
        .stdout
        .take()
        .map(|r| tokio::spawn(read_capped(r, keep)));
    let err = child
        .stderr
        .take()
        .map(|r| tokio::spawn(read_capped(r, keep)));

    let (status, timed_out) = match tokio::time::timeout(spec.timeout, child.wait()).await {
        Ok(s) => (s.ok(), false),
        Err(_) => {
            kill_group(pid);
            (child.wait().await.ok(), true)
        }
    };
    // stragglers in the group would keep the pipes (and maybe a temp file) open
    kill_group(pid);
    drop(temp);
    if let Some(f) = feeder {
        f.abort();
    }
    let collect = |h: Option<tokio::task::JoinHandle<Vec<u8>>>| async move {
        match h {
            Some(h) => tokio::time::timeout(DRAIN, h)
                .await
                .ok()
                .and_then(Result::ok)
                .unwrap_or_default(),
            None => Vec::new(),
        }
    };
    let (o, e) = (collect(out).await, collect(err).await);
    RunOutput {
        exit_code: if timed_out {
            None
        } else {
            status.and_then(|s| s.code())
        },
        timed_out,
        stdout: finish(&redactor, &o),
        stderr: finish(&redactor, &e),
        error: timed_out.then(|| {
            format!(
                "timed out after {} s; the command was stopped",
                spec.timeout.as_secs()
            )
        }),
    }
}

impl Vault {
    /// The values of `labels` of an active item of `ws`, for one injection. Secret fields are
    /// decrypted here; non-secret ones are read as they are.
    pub(crate) fn injection_values(
        &self,
        ws: &str,
        item_id: &str,
        labels: &[String],
    ) -> VaultResult<Vec<(String, SecretBytes)>> {
        let item = self
            .store()
            .reader(|c| repo::item(c, item_id))?
            .filter(|i| i.workspace_id == ws && i.status == "active")
            .ok_or_else(|| ApiError::not_found("条目"))?;
        let rows = self.store().reader(|c| repo::fields(c, item_id))?;
        let mut dek = None;
        let mut out = Vec::new();
        for label in labels {
            if out.iter().any(|(l, _): &(String, SecretBytes)| l == label) {
                continue;
            }
            let f = rows
                .iter()
                .find(|f| &f.label == label)
                .ok_or_else(|| ApiError::not_found("字段"))?;
            let v = if f.secret {
                if dek.is_none() {
                    dek =
                        Some(self.dek_of(&item)?.ok_or_else(|| {
                            VaultError::internal("vault item without a data key")
                        })?);
                }
                let ct = f
                    .value_ct
                    .as_deref()
                    .ok_or_else(|| VaultError::internal("secret field without a value"))?;
                let dek = dek
                    .as_ref()
                    .ok_or_else(|| VaultError::internal("no data key"))?;
                open_field(dek, ws, item_id, &f.id, ct)?
            } else {
                SecretBytes::new(f.value_plain.clone().unwrap_or_default().into_bytes())
            };
            out.push((label.clone(), v));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(dir: &Path, cmd: &str, inj: Vec<Injection>) -> RunSpec {
        RunSpec {
            use_id: steve_store::uid("su"),
            cwd: dir.to_path_buf(),
            command: cmd.into(),
            injections: inj,
            timeout: Duration::from_secs(20),
        }
    }

    fn env(var: &str, label: &str) -> Injection {
        Injection {
            var: var.into(),
            label: label.into(),
            how: How::Env,
        }
    }

    fn vals(xs: &[(&str, &str)]) -> Vec<(String, SecretBytes)> {
        xs.iter()
            .map(|(l, v)| (l.to_string(), SecretBytes::new(v.as_bytes().to_vec())))
            .collect()
    }

    const TOKEN: &str = "inj-T0ken-value-5521";

    #[test]
    fn cwd_must_stay_inside_the_workdir() {
        let t = tempfile::tempdir().unwrap();
        let w = t.path().join("work");
        std::fs::create_dir_all(w.join("sub")).unwrap();
        std::fs::create_dir_all(t.path().join("outside")).unwrap();
        std::os::unix::fs::symlink(t.path().join("outside"), w.join("link")).unwrap();
        let root = w.canonicalize().unwrap();
        assert_eq!(resolve_cwd(&w, None).unwrap(), root);
        assert_eq!(resolve_cwd(&w, Some("sub")).unwrap(), root.join("sub"));
        assert_eq!(resolve_cwd(&w, Some("./sub/..")).unwrap(), root);
        for bad in [
            "..",
            "../outside",
            "link",
            "/",
            "/tmp",
            "missing",
            "sub/../../outside",
        ] {
            let e = resolve_cwd(&w, Some(bad)).unwrap_err();
            assert_eq!(e.to_string(), CWD_OUTSIDE, "{bad}");
            assert_eq!(e.status(), 400);
        }
        let abs = root.join("sub");
        assert!(resolve_cwd(&w, abs.to_str()).is_ok());
    }

    #[tokio::test]
    async fn env_injection_and_output_redaction() {
        let home = tempfile::tempdir().unwrap();
        let w = tempfile::tempdir().unwrap();
        let s = spec(
            w.path(),
            "echo \"t=$TOK\"; printf %s \"$TOK\" | base64; printf %s \"$TOK\" | xxd -p; echo err \"$TOK\" >&2; pwd",
            vec![env("TOK", "令牌")],
        );
        let out = run(home.path(), &s, &vals(&[("令牌", TOKEN)])).await;
        assert_eq!(out.exit_code, Some(0), "{out:?}");
        let all = out.combined();
        assert!(!all.contains(TOKEN), "{all}");
        assert!(!all.contains("aW5qLVQwa2Vu"), "base64 leaked: {all}");
        assert!(!all.contains("696e6a2d54"), "hex leaked: {all}");
        assert!(
            out.stdout.starts_with("t=[已打码：令牌]\n[已打码：令牌]\n"),
            "{all}"
        );
        assert_eq!(out.stderr, "err [已打码：令牌]\n");
        assert!(
            out.stdout
                .contains(&*w.path().canonicalize().unwrap().to_string_lossy())
        );
    }

    #[tokio::test]
    async fn file_injection_is_private_and_deleted() {
        let home = tempfile::tempdir().unwrap();
        let w = tempfile::tempdir().unwrap();
        let stat_mode = if cfg!(target_os = "macos") {
            "stat -f '%Lp'"
        } else {
            "stat -c '%a'"
        };
        let s = spec(
            w.path(),
            &format!(
                "echo \"path=$KEYF\"; {stat_mode} \"$KEYF\"; {stat_mode} \"$(dirname \"$KEYF\")\"; cat \"$KEYF\"; cp \"$KEYF\" copied"
            ),
            vec![Injection {
                var: "KEYF".into(),
                label: "私钥".into(),
                how: How::File,
            }],
        );
        let out = run(home.path(), &s, &vals(&[("私钥", TOKEN)])).await;
        assert_eq!(out.exit_code, Some(0), "{out:?}");
        let lines: Vec<&str> = out.stdout.lines().collect();
        // the agent sees neither the path nor the value
        assert_eq!(lines[0], "path=[已打码：临时文件]/KEYF");
        assert_eq!(lines[1], "600");
        assert_eq!(lines[2], "700");
        assert_eq!(lines[3], "[已打码：私钥]");
        assert!(!secrets_root(home.path()).join(&s.use_id).exists());
        assert_eq!(
            std::fs::metadata(secrets_root(home.path()))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }

    #[tokio::test]
    async fn stdin_injection() {
        let home = tempfile::tempdir().unwrap();
        let w = tempfile::tempdir().unwrap();
        let s = spec(
            w.path(),
            "read -r x; [ \"$x\" = \"$EXPECT\" ] && echo same; echo \"$x\"",
            vec![
                Injection {
                    var: String::new(),
                    label: "密码".into(),
                    how: How::Stdin,
                },
                env("EXPECT", "密码"),
            ],
        );
        let out = run(home.path(), &s, &vals(&[("密码", TOKEN)])).await;
        assert_eq!(out.stdout, "same\n[已打码：密码]\n", "{out:?}");
    }

    #[tokio::test]
    async fn timeout_kills_the_group_and_cleans_up() {
        let home = tempfile::tempdir().unwrap();
        let w = tempfile::tempdir().unwrap();
        let mut s = spec(
            w.path(),
            "echo start; (sleep 30; echo late > late.txt) & sleep 30",
            vec![Injection {
                var: "F".into(),
                label: "k".into(),
                how: How::File,
            }],
        );
        s.timeout = Duration::from_millis(500);
        let t0 = std::time::Instant::now();
        let out = run(home.path(), &s, &vals(&[("k", TOKEN)])).await;
        assert!(t0.elapsed() < Duration::from_secs(5), "{:?}", t0.elapsed());
        assert!(out.timed_out && out.exit_code.is_none() && out.code() == -1);
        assert_eq!(out.stdout, "start\n");
        assert!(out.combined().contains("timed out after 0 s"));
        assert!(!secrets_root(home.path()).join(&s.use_id).exists());
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(!w.path().join("late.txt").exists());
    }

    #[tokio::test]
    async fn exit_codes_and_output_cap() {
        let home = tempfile::tempdir().unwrap();
        let w = tempfile::tempdir().unwrap();
        let s = spec(w.path(), "exit 3", vec![]);
        let out = run(home.path(), &s, &[]).await;
        assert_eq!(out.exit_code, Some(3));
        assert!(!out.ok());
        let s = spec(
            w.path(),
            "head -c 200000 /dev/zero | tr '\\0' a; printf %s \"$T\"",
            vec![env("T", "t")],
        );
        let out = run(home.path(), &s, &vals(&[("t", TOKEN)])).await;
        assert_eq!(out.stdout.len(), OUTPUT_CAP);
        assert!(out.stdout.bytes().all(|b| b == b'a'));
    }

    #[test]
    fn startup_cleanup_removes_leftovers() {
        let home = tempfile::tempdir().unwrap();
        let d = TempDir::create(home.path(), "su_leftover1").unwrap();
        d.write("X", b"v").unwrap();
        std::mem::forget(d);
        let d2 = secrets_root(home.path()).join("su_leftover1");
        assert!(d2.exists());
        assert_eq!(cleanup(home.path(), Some(Duration::from_secs(900))), 0);
        assert!(d2.exists());
        assert_eq!(cleanup(home.path(), None), 1);
        assert!(!d2.exists());
        assert!(TempDir::create(home.path(), "../evil").is_err());
        assert!(TempDir::create(home.path(), "su_a/../../b").is_err());
    }
}
