//! Narrow filesystem and bounded-subprocess primitives for the operator's own installation,
//! never a session workspace.
use anyhow::{Context, Result, bail};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use wait_timeout::ChildExt;

/// Grace period after the direct child exits for stdout/stderr pipes to reach EOF before
/// treating a lingering descendant as a pipe holder and killing the process group.
pub const PIPE_DRAIN_GRACE: Duration = Duration::from_millis(200);
/// Maximum wait to reap the direct child after `SIGKILL` is sent to its process group.
pub const PROCESS_KILL_REAP_TIMEOUT: Duration = Duration::from_secs(5);
/// Maximum wait for pipe-reader threads to observe EOF after the process group is `SIGKILL`ed.
const PIPE_POST_KILL_TIMEOUT: Duration = Duration::from_secs(2);
/// Polling interval while waiting for non-blocking stdin writes to make progress.
const STDIN_WRITE_POLL_INTERVAL: Duration = Duration::from_millis(20);

/// How [`run_bounded`] isolates the child's process group / session so timeouts and pipe-drain
/// cleanup can terminate the entire descendant tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessIsolation {
    /// Start a dedicated process group (`pgid == pid`, via `posix_spawn` on macOS).
    ProcessGroup,
    /// Start a detached session (`setsid`) with no controlling `/dev/tty` and `pgid == pid`.
    /// Used for interactive shell probes so rc scripts cannot stop on `SIGTTIN`/`SIGTTOU`.
    DetachedSession,
}

/// How [`run_bounded`] configures standard input.
#[derive(Clone, Copy, Debug, Default)]
pub enum BoundedStdin<'a> {
    /// Redirect stdin to `/dev/null` so the child cannot block waiting for a terminal or parent.
    #[default]
    Null,
    /// Write these exact bytes over a non-blocking pipe bounded by the overall deadline.
    Bytes(&'a [u8]),
    /// Preserve the stdin already configured on the `Command` (e.g. a private file descriptor).
    Preserve,
}

/// Execution contract for [`run_bounded`].
#[derive(Clone, Copy, Debug)]
pub struct BoundedOptions<'a> {
    pub timeout: Duration,
    pub isolation: ProcessIsolation,
    pub stdin: BoundedStdin<'a>,
    pub capture_output: bool,
    pub max_output_bytes: Option<u64>,
    /// Kill the child's process group after the direct child exits (e.g. for interactive shell
    /// probes whose rc files may launch background helpers).
    pub kill_group_after_exit: bool,
}

impl<'a> BoundedOptions<'a> {
    pub fn capture(timeout: Duration) -> Self {
        Self {
            timeout,
            isolation: ProcessIsolation::ProcessGroup,
            stdin: BoundedStdin::Null,
            capture_output: true,
            max_output_bytes: None,
            kill_group_after_exit: false,
        }
    }

    pub fn silent(timeout: Duration) -> Self {
        Self {
            timeout,
            isolation: ProcessIsolation::ProcessGroup,
            stdin: BoundedStdin::Null,
            capture_output: false,
            max_output_bytes: None,
            kill_group_after_exit: false,
        }
    }
}

/// Strongly typed error returned by [`run_bounded`] at each subprocess boundary.
#[derive(Debug)]
pub enum CommandError {
    Spawn {
        program: String,
        source: io::Error,
    },
    StdinConfigure {
        program: String,
        source: io::Error,
    },
    StdinWrite {
        program: String,
        source: io::Error,
    },
    TimedOut {
        program: String,
        timeout: Duration,
    },
    Wait {
        program: String,
        source: io::Error,
    },
    PipeRead {
        program: String,
        stream: &'static str,
        source: io::Error,
    },
    PipeDrainTimedOut {
        program: String,
        stream: &'static str,
        grace: Duration,
    },
    OutputLimitExceeded {
        program: String,
        stream: &'static str,
        limit: u64,
    },
}

impl fmt::Display for CommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn { program, source } => write!(f, "failed to start {program}: {source}"),
            Self::StdinConfigure { program, source } => {
                write!(f, "failed to configure stdin for {program}: {source}")
            }
            Self::StdinWrite { program, source } => {
                write!(f, "failed to write stdin to {program}: {source}")
            }
            Self::TimedOut { program, timeout } => {
                write!(f, "{program} timed out after {:.1}s", timeout.as_secs_f64())
            }
            Self::Wait { program, source } => write!(f, "failed waiting for {program}: {source}"),
            Self::PipeRead {
                program,
                stream,
                source,
            } => write!(f, "failed reading {stream} from {program}: {source}"),
            Self::PipeDrainTimedOut {
                program,
                stream,
                grace,
            } => write!(
                f,
                "{program} {stream} pipe did not close within {}ms after exit",
                grace.as_millis()
            ),
            Self::OutputLimitExceeded {
                program,
                stream,
                limit,
            } => write!(f, "{program} {stream} exceeded {limit} bytes"),
        }
    }
}

impl std::error::Error for CommandError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Spawn { source, .. }
            | Self::StdinConfigure { source, .. }
            | Self::StdinWrite { source, .. }
            | Self::Wait { source, .. }
            | Self::PipeRead { source, .. } => Some(source),
            Self::TimedOut { .. }
            | Self::PipeDrainTimedOut { .. }
            | Self::OutputLimitExceeded { .. } => None,
        }
    }
}

fn kill_process_group(pid: libc::pid_t) {
    if pid > 1 {
        // SAFETY: the child was started in its own process group (`process_group(0)` or
        // `setsid()`), so `-pid` targets only that group and its descendants.
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
}

fn kill_group_and_reap(child: &mut Child, pid: libc::pid_t) {
    kill_process_group(pid);
    let _ = child.kill();
    let _ = child.wait_timeout(PROCESS_KILL_REAP_TIMEOUT);
}

fn spawn_pipe_reader<R: Read + Send + 'static>(
    reader: R,
    max_output_bytes: Option<u64>,
) -> mpsc::Receiver<io::Result<Vec<u8>>> {
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = match max_output_bytes {
            Some(limit) => reader.take(limit.saturating_add(1)).read_to_end(&mut bytes),
            None => {
                let mut reader = reader;
                reader.read_to_end(&mut bytes)
            }
        }
        .map(|_| bytes);
        let _ = tx.send(result);
    });
    rx
}

fn write_stdin_bounded(
    stdin: &mut std::process::ChildStdin,
    bytes: &[u8],
    deadline: Instant,
    program: &str,
    timeout: Duration,
) -> std::result::Result<(), CommandError> {
    let fd = stdin.as_raw_fd();
    // SAFETY: `stdin` owns `fd` for both flag queries.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } != 0 {
        return Err(CommandError::StdinConfigure {
            program: program.to_owned(),
            source: io::Error::last_os_error(),
        });
    }
    let mut written = 0;
    while written < bytes.len() {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return Err(CommandError::TimedOut {
                program: program.to_owned(),
                timeout,
            });
        };
        match stdin.write(&bytes[written..]) {
            Ok(0) => {
                return Err(CommandError::StdinWrite {
                    program: program.to_owned(),
                    source: io::Error::new(io::ErrorKind::WriteZero, "child closed stdin"),
                });
            }
            Ok(count) => written += count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(STDIN_WRITE_POLL_INTERVAL.min(remaining));
            }
            Err(source) => {
                return Err(CommandError::StdinWrite {
                    program: program.to_owned(),
                    source,
                });
            }
        }
    }
    Ok(())
}

fn recv_pipe_until(
    rx: &mpsc::Receiver<io::Result<Vec<u8>>>,
    deadline: Instant,
    program: &str,
    stream: &'static str,
) -> std::result::Result<Option<Vec<u8>>, CommandError> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    match rx.recv_timeout(remaining) {
        Ok(Ok(bytes)) => Ok(Some(bytes)),
        Ok(Err(source)) => Err(CommandError::PipeRead {
            program: program.to_owned(),
            stream,
            source,
        }),
        Err(mpsc::RecvTimeoutError::Timeout) => Ok(None),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(CommandError::PipeRead {
            program: program.to_owned(),
            stream,
            source: io::Error::new(io::ErrorKind::BrokenPipe, "pipe reader thread terminated"),
        }),
    }
}

/// Runs `command` in its own process group (or detached session) with a hard wall-clock deadline,
/// bounded non-blocking stdin delivery, and bounded pipe draining that kills any grandchild
/// holding stdout/stderr open past [`PIPE_DRAIN_GRACE`].
pub fn run_bounded(
    command: &mut Command,
    program: &str,
    opts: BoundedOptions<'_>,
) -> std::result::Result<Output, CommandError> {
    match opts.isolation {
        ProcessIsolation::ProcessGroup => {
            command.process_group(0);
        }
        ProcessIsolation::DetachedSession => {
            // SAFETY: `libc::setsid` is async-signal-safe per POSIX.
            unsafe {
                command.pre_exec(|| {
                    if libc::setsid() == -1 {
                        return Err(io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
    }
    match opts.stdin {
        BoundedStdin::Null => {
            command.stdin(Stdio::null());
        }
        BoundedStdin::Bytes(_) => {
            command.stdin(Stdio::piped());
        }
        BoundedStdin::Preserve => {}
    }
    if opts.capture_output {
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
    } else {
        command.stdout(Stdio::null()).stderr(Stdio::null());
    }

    let deadline = Instant::now() + opts.timeout;
    let mut child = command.spawn().map_err(|source| CommandError::Spawn {
        program: program.to_owned(),
        source,
    })?;
    let pid = child.id() as libc::pid_t;

    let pipes = if opts.capture_output {
        let stdout = child.stdout.take().expect("piped stdout missing");
        let stderr = child.stderr.take().expect("piped stderr missing");
        Some((
            spawn_pipe_reader(stdout, opts.max_output_bytes),
            spawn_pipe_reader(stderr, opts.max_output_bytes),
        ))
    } else {
        None
    };

    if let BoundedStdin::Bytes(bytes) = opts.stdin {
        let mut stdin = child.stdin.take().expect("piped stdin missing");
        if let Err(error) = write_stdin_bounded(&mut stdin, bytes, deadline, program, opts.timeout)
        {
            drop(stdin);
            kill_group_and_reap(&mut child, pid);
            return Err(error);
        }
        drop(stdin);
    }

    let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
        kill_group_and_reap(&mut child, pid);
        return Err(CommandError::TimedOut {
            program: program.to_owned(),
            timeout: opts.timeout,
        });
    };
    let status = match child.wait_timeout(remaining) {
        Ok(Some(status)) => status,
        Ok(None) => {
            kill_group_and_reap(&mut child, pid);
            return Err(CommandError::TimedOut {
                program: program.to_owned(),
                timeout: opts.timeout,
            });
        }
        Err(source) => {
            kill_group_and_reap(&mut child, pid);
            return Err(CommandError::Wait {
                program: program.to_owned(),
                source,
            });
        }
    };

    let Some((out_rx, err_rx)) = pipes else {
        if opts.kill_group_after_exit {
            kill_process_group(pid);
        }
        return Ok(Output {
            status,
            stdout: Vec::new(),
            stderr: Vec::new(),
        });
    };

    let drain_deadline = Instant::now() + PIPE_DRAIN_GRACE;
    let mut stdout = recv_pipe_until(&out_rx, drain_deadline, program, "stdout")?;
    let mut stderr = recv_pipe_until(&err_rx, drain_deadline, program, "stderr")?;

    if stdout.is_none() || stderr.is_none() || opts.kill_group_after_exit {
        // Kill lingering grandchildren in the child's process group so inherited pipe writers
        // cannot block EOF indefinitely (and background rc helpers do not leak).
        kill_process_group(pid);
    }

    let post_kill_deadline = Instant::now() + PIPE_POST_KILL_TIMEOUT;
    if stdout.is_none() {
        stdout = recv_pipe_until(&out_rx, post_kill_deadline, program, "stdout")?;
    }
    if stderr.is_none() {
        stderr = recv_pipe_until(&err_rx, post_kill_deadline, program, "stderr")?;
    }

    let stdout = stdout.ok_or_else(|| CommandError::PipeDrainTimedOut {
        program: program.to_owned(),
        stream: "stdout",
        grace: PIPE_DRAIN_GRACE,
    })?;
    let stderr = stderr.ok_or_else(|| CommandError::PipeDrainTimedOut {
        program: program.to_owned(),
        stream: "stderr",
        grace: PIPE_DRAIN_GRACE,
    })?;

    if let Some(limit) = opts.max_output_bytes {
        if stdout.len() as u64 > limit {
            return Err(CommandError::OutputLimitExceeded {
                program: program.to_owned(),
                stream: "stdout",
                limit,
            });
        }
        if stderr.len() as u64 > limit {
            return Err(CommandError::OutputLimitExceeded {
                program: program.to_owned(),
                stream: "stderr",
                limit,
            });
        }
    }

    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

pub fn exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

pub fn user_home() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME is unavailable")?;
    let path = PathBuf::from(home);
    if !path.is_absolute() {
        bail!("HOME must be absolute");
    }
    Ok(path)
}

/// The CLI and setup share the Hub/Node home precedence; HOME is only a fallback.
pub fn role_home(role: &str, selected: Option<&Path>) -> Result<PathBuf> {
    let home = selected
        .map(Path::to_path_buf)
        .or_else(|| std::env::var_os("STEVE_HOME").map(PathBuf::from));
    let home = match home {
        Some(home) => home,
        None => user_home()?.join(match role {
            "hub" => ".steve",
            "node" => ".steve-node",
            _ => bail!("role must be hub or node"),
        }),
    };
    canonical(&home)
}

pub fn canonical(path: &Path) -> Result<PathBuf> {
    let expanded = if let Ok(p) = path.strip_prefix("~") {
        user_home()?.join(p)
    } else {
        path.to_path_buf()
    };
    let absolute = if expanded.is_absolute() {
        expanded
    } else {
        std::env::current_dir()?.join(expanded)
    };
    if absolute
        .components()
        .any(|p| matches!(p, Component::ParentDir))
    {
        bail!("path must not contain ..");
    }
    if exists(&absolute) && fs::symlink_metadata(&absolute)?.file_type().is_symlink() {
        bail!("path cannot be a symbolic link: {}", absolute.display());
    }
    let name = absolute
        .file_name()
        .context("installation home must not be a filesystem root")?;
    let parent = absolute.parent().context("path needs a parent")?;
    // Existing ancestors are resolved; a new path may have missing intermediate parents.
    let mut base = parent.to_path_buf();
    let mut suffix = Vec::new();
    while !base.exists() {
        suffix.push(
            base.file_name()
                .context("path has no existing ancestor")?
                .to_os_string(),
        );
        base = base
            .parent()
            .context("path has no existing ancestor")?
            .to_path_buf();
    }
    let mut result = fs::canonicalize(base)?;
    for part in suffix.into_iter().rev() {
        result.push(part);
    }
    result.push(name);
    Ok(result)
}

pub fn owned(path: &Path, directory: bool) -> Result<()> {
    let m = fs::symlink_metadata(path)?;
    let wanted = if directory { 0o700 } else { 0o600 };
    if m.uid() != unsafe { libc::getuid() }
        || m.permissions().mode() & 0o777 != wanted
        || if directory { !m.is_dir() } else { !m.is_file() }
    {
        bail!(
            "path must belong to this user, be {}, and have mode {:04o}: {}",
            if directory {
                "a directory"
            } else {
                "a regular file"
            },
            wanted,
            path.display()
        );
    }
    Ok(())
}

pub fn private_dir(path: &Path) -> Result<()> {
    if exists(path) {
        return owned(path, true);
    }
    if let Some(parent) = path.parent() {
        if !parent.exists() {
            private_dir(parent)?;
        }
        if fs::symlink_metadata(parent)?.file_type().is_symlink() {
            bail!("directory parent cannot be a symlink");
        }
    }
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new().mode(0o700).create(path)?;
    owned(path, true)
}

pub fn read_json(path: &Path) -> Result<Value> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_file() || meta.len() > 1024 * 1024 {
        bail!("invalid metadata file: {}", path.display());
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let value: Value = serde_json::from_reader(file)?;
    if !value.is_object() {
        bail!("metadata must be a JSON object: {}", path.display());
    }
    Ok(value)
}

pub fn private_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    if let Some(parent) = path.parent() {
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}

pub fn atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("metadata needs parent")?;
    private_dir(parent)?;
    if exists(path) {
        owned(path, false)?;
    }
    let temporary = parent.join(format!(".write-{}", uuid::Uuid::new_v4()));
    let result = (|| {
        private_file(&temporary, bytes)?;
        fs::rename(&temporary, path)?;
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if exists(&temporary) {
        let _ = fs::remove_file(temporary);
    }
    result
}

pub fn atomic_json(path: &Path, value: &Value) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    atomic(path, &bytes)
}

pub fn digest(path: &Path) -> Result<String> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    if !file.metadata()?.is_file() {
        bail!("not a regular file: {}", path.display());
    }
    let mut hash = Sha256::new();
    let mut bytes = [0; 64 * 1024];
    loop {
        let n = file.read(&mut bytes)?;
        if n == 0 {
            break;
        }
        hash.update(&bytes[..n]);
    }
    Ok(hex::encode(hash.finalize()))
}

pub fn bytes_digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

pub fn shell_quote(value: &str) -> Result<String> {
    if value.contains(['\n', '\r', '\0']) {
        bail!("value contains a line break or NUL");
    }
    Ok(format!("'{}'", value.replace('\'', "'\\''")))
}

pub fn env_bytes(values: &serde_json::Map<String, Value>) -> Result<Vec<u8>> {
    let mut lines = String::from("# Managed by steve setup; secrets are referenced by file.\n");
    let mut keys: Vec<_> = values.keys().collect();
    keys.sort();
    for key in keys {
        if !key.starts_with("STEVE_")
            || !key
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        {
            bail!("invalid environment key");
        }
        lines.push_str(&format!(
            "{key}={}\n",
            shell_quote(
                values[key]
                    .as_str()
                    .context("environment value must be a string")?
            )?
        ));
    }
    Ok(lines.into_bytes())
}

pub fn clean_command(program: &Path, home: Option<&Path>) -> std::process::Command {
    let mut command = std::process::Command::new(program);
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("STEVE_") {
            command.env_remove(key);
        }
    }
    if let Some(home) = home {
        command.env("STEVE_HOME", home);
    }
    /* what the program prints is shown as this one's: the same language */
    command.env("STEVE_LANG", steve_i18n::current());
    command
}

pub fn regular_tree(root: &Path) -> Result<Vec<PathBuf>> {
    let mut result = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        let m = fs::symlink_metadata(&path)?;
        if m.is_file() {
            result.push(path);
        } else if m.is_dir() {
            for child in fs::read_dir(path)? {
                pending.push(child?.path());
            }
        } else {
            bail!(
                "installation contains a link or special file: {}",
                path.display()
            );
        }
    }
    result.sort();
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn metadata_does_not_follow_links_or_overwrite_unknown_files() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        private_dir(&home).unwrap();
        private_file(&home.join("owned.json"), b"{}").unwrap();
        std::os::unix::fs::symlink(home.join("owned.json"), home.join("link.json")).unwrap();
        assert!(read_json(&home.join("link.json")).is_err());
        assert!(atomic(&home.join("link.json"), b"changed").is_err());
        assert_eq!(fs::read(home.join("owned.json")).unwrap(), b"{}");
    }
    #[test]
    fn shell_values_round_trip_without_interpreting_substitutions() {
        let value = "one ' quote $(touch should-not-exist) `true` $HOME";
        let quoted = shell_quote(value).unwrap();
        assert_eq!(shlex::split(&quoted).unwrap(), vec![value]);
        assert!(shell_quote("one\ntwo").is_err());
    }

    #[test]
    fn run_bounded_kills_process_group_on_timeout() {
        let temp = tempfile::tempdir().unwrap();
        let pid_file = temp.path().join("child.pid");
        let mut command = Command::new("/bin/sh");
        command
            .args([
                "-c",
                "/bin/sleep 60 & printf '%s' \"$!\" > \"$1\"; wait",
                "t",
            ])
            .arg(&pid_file);
        let started = Instant::now();
        let err = run_bounded(
            &mut command,
            "/bin/sh",
            BoundedOptions::capture(Duration::from_millis(200)),
        )
        .expect_err("sleeping command must time out");
        assert!(
            matches!(err, CommandError::TimedOut { .. }),
            "expected TimedOut, got {err:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "timeout must return promptly, took {:?}",
            started.elapsed()
        );
        if let Ok(raw) = fs::read_to_string(&pid_file)
            && let Ok(grandchild) = raw.trim().parse::<libc::pid_t>()
        {
            let deadline = Instant::now() + Duration::from_secs(2);
            while unsafe { libc::kill(grandchild, 0) } == 0 && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            assert_eq!(
                unsafe { libc::kill(grandchild, 0) },
                -1,
                "grandchild {grandchild} survived timeout SIGKILL"
            );
        }
    }

    #[test]
    fn run_bounded_recovers_from_grandchild_inheriting_pipes() {
        let temp = tempfile::tempdir().unwrap();
        let pid_file = temp.path().join("holder.pid");
        let mut command = Command::new("/bin/sh");
        command
            .args([
                "-c",
                "/bin/sleep 60 & printf '%s' \"$!\" > \"$1\"; printf 'payload-ok'",
                "t",
            ])
            .arg(&pid_file);
        let started = Instant::now();
        let output = run_bounded(
            &mut command,
            "/bin/sh",
            BoundedOptions::capture(Duration::from_secs(5)),
        )
        .expect("pipe-holding grandchild must be killed so output finishes");
        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout), "payload-ok");
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "pipe drain recovery took {:?}",
            started.elapsed()
        );
        let grandchild: libc::pid_t = fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while unsafe { libc::kill(grandchild, 0) } == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            unsafe { libc::kill(grandchild, 0) },
            -1,
            "pipe-holding grandchild {grandchild} must not survive"
        );
    }

    #[test]
    fn run_bounded_delivers_stdin_and_enforces_output_limit() {
        let mut echo = Command::new("/bin/cat");
        let out = run_bounded(
            &mut echo,
            "/bin/cat",
            BoundedOptions {
                stdin: BoundedStdin::Bytes(b"hello-bounded\n"),
                ..BoundedOptions::capture(Duration::from_secs(5))
            },
        )
        .unwrap();
        assert!(out.status.success());
        assert_eq!(out.stdout, b"hello-bounded\n");

        let mut big = Command::new("/bin/sh");
        big.args(["-c", "printf '0123456789abcdef'"]);
        let err = run_bounded(
            &mut big,
            "/bin/sh",
            BoundedOptions {
                max_output_bytes: Some(8),
                ..BoundedOptions::capture(Duration::from_secs(5))
            },
        )
        .expect_err("output beyond max_output_bytes must fail");
        assert!(matches!(
            err,
            CommandError::OutputLimitExceeded {
                stream: "stdout",
                limit: 8,
                ..
            }
        ));
    }
}
