//! Small helpers shared by the domain modules: ids, hashes, time, JS-compatible string slicing, and
//! running a command with a deadline.

use base64::Engine;
use sha2::{Digest, Sha256};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Unix milliseconds, like `Date.now()`.
pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// `p + "_" + 16 hex chars`, the TS `uid()`.
pub fn uid(p: &str) -> String {
    let u = uuid::Uuid::new_v4().simple().to_string();
    format!("{p}_{}", &u[..16])
}

/// sha256 hex, used for session and auth tokens.
pub fn sha(s: &str) -> String {
    hex(&Sha256::digest(s.as_bytes()))
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// 24 random bytes, base64url without padding (MCP session tokens, release drain tokens).
pub fn token() -> String {
    let b: [u8; 24] = rand::random();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

/// Length in UTF-16 code units, like JS `String.length`.
pub fn js_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// `s.slice(0, n)` in UTF-16 code units; never splits a character.
pub fn js_head(s: &str, n: usize) -> &str {
    let mut units = 0;
    for (i, c) in s.char_indices() {
        units += c.len_utf16();
        if units > n {
            return &s[..i];
        }
    }
    s
}

/// `s.slice(-n)` in UTF-16 code units; never splits a character.
pub fn js_tail(s: &str, n: usize) -> &str {
    let mut units = 0;
    for (i, c) in s.char_indices().rev() {
        units += c.len_utf16();
        if units > n {
            return &s[i + c.len_utf8()..];
        }
    }
    s
}

/// JS truthiness of a string: non-empty.
pub fn or<'a>(a: &'a str, b: &'a str) -> &'a str {
    if a.is_empty() { b } else { a }
}

/// Why [`run_deadline`] has no exit status to give.
#[derive(Debug)]
pub enum RunError {
    /// The command could not be started.
    Spawn(std::io::Error),
    /// It was still running at the deadline (the deadline given).
    Timeout(Duration),
    /// It ended, but something it left behind kept its output open past the deadline.
    Held,
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunError::Spawn(e) => write!(f, "启动不了：{e}"),
            RunError::Timeout(d) => write!(f, "超时（{} 秒）", d.as_secs()),
            RunError::Held => f.write_str("超时：输出一直没有结束"),
        }
    }
}

/// Runs `cmd` in a process group of its own and reads its output meanwhile: `(succeeded, stdout,
/// stderr)`, invalid UTF-8 replaced. At `deadline` the whole group is killed (a wrapper's children
/// too: an `ssh` under `git fetch`, a credential helper), and output still held open by something it
/// left behind is not waited for. Blocking: call it where blocking is fine (never on a runtime worker).
pub fn run_deadline(cmd: Command, deadline: Duration) -> Result<(bool, String, String), RunError> {
    run_deadline_with_stdin(cmd, deadline, steve_runtime::process::BoundedStdin::Null)
}

fn run_deadline_with_stdin(
    mut cmd: Command,
    deadline: Duration,
    stdin: steve_runtime::process::BoundedStdin<'_>,
) -> Result<(bool, String, String), RunError> {
    let program = cmd.get_program().to_string_lossy().into_owned();
    let out = steve_runtime::process::run_bounded(
        &mut cmd,
        &program,
        steve_runtime::process::BoundedOptions {
            stdin,
            held_pipe_policy: steve_runtime::process::HeldPipePolicy::FailAfterGroupKill,
            ..steve_runtime::process::BoundedOptions::capture("core_git", deadline)
        },
    )
    .map_err(|err| match err {
        steve_runtime::process::CommandError::Spawn { source, .. } => RunError::Spawn(source),
        steve_runtime::process::CommandError::TimedOut { timeout, .. } => {
            RunError::Timeout(timeout)
        }
        steve_runtime::process::CommandError::PipeDrainTimedOut { .. } => RunError::Held,
        steve_runtime::process::CommandError::Wait { source, .. }
        | steve_runtime::process::CommandError::PipeRead { source, .. }
        | steve_runtime::process::CommandError::StdinConfigure { source, .. }
        | steve_runtime::process::CommandError::StdinWrite { source, .. } => {
            RunError::Spawn(source)
        }
        steve_runtime::process::CommandError::OutputLimitExceeded { .. }
        | steve_runtime::process::CommandError::Cancelled { .. } => RunError::Held,
    })?;
    Ok((
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slicing_counts_utf16_units() {
        assert_eq!(js_head("你好世界", 2), "你好");
        assert_eq!(js_head("ab", 5), "ab");
        assert_eq!(js_head("a😀b", 2), "a");
        assert_eq!(js_tail("你好世界", 2), "世界");
        assert_eq!(js_tail("ab", 5), "ab");
        assert_eq!(js_len("a😀"), 3);
    }

    #[test]
    fn ids_and_tokens() {
        let id = uid("dec");
        assert!(id.starts_with("dec_") && id.len() == 20);
        assert_eq!(token().len(), 32);
        assert_eq!(
            sha("x"),
            "2d711642b726b04401627ca9fbac32f5c8530fb1903cc4db02258717921a4881"
        );
    }

    #[test]
    fn a_command_and_what_it_started_end_at_the_deadline() {
        use std::io::Read as _;
        use std::os::{fd::OwnedFd, unix::net::UnixStream};
        use std::time::Instant;

        // One wrapper leaves a child holding its output open. The other hangs itself.
        for stay in [false, true] {
            let (mut observer, peer) = UnixStream::pair().unwrap();
            observer
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut cmd = Command::new("/bin/sh");
            cmd.args([
                "-c",
                "exec 9<&0; /bin/sleep 30 & printf x >&9; if [ \"$1\" = wait ]; then wait; fi",
                "fixture",
                if stay { "wait" } else { "exit" },
            ])
            .stdin(OwnedFd::from(peer));

            let t = Instant::now();
            let result = run_deadline_with_stdin(
                cmd,
                Duration::from_millis(300),
                steve_runtime::process::BoundedStdin::Preserve,
            );
            assert!(t.elapsed() < Duration::from_secs(3), "{:?}", t.elapsed());
            // The fixture writes 'x' before spawning/waiting, and both the leader and descendant
            // inherit fd 9; reading 'x' then EOF proves the whole process group closed the socket.
            let mut buf = Vec::new();
            observer
                .read_to_end(&mut buf)
                .expect("group must close socket");
            assert_eq!(buf, b"x");
            if stay {
                assert!(matches!(result, Err(RunError::Timeout(_))), "{result:?}");
            } else {
                assert!(matches!(result, Err(RunError::Held)), "{result:?}");
            }
        }
        let mut ok = Command::new("/bin/sh");
        ok.args(["-c", "echo out; echo err >&2; exit 3"]);
        assert_eq!(
            run_deadline(ok, Duration::from_secs(5)).unwrap(),
            (false, "out\n".into(), "err\n".into())
        );
    }

    /// More output than a pipe holds is read while the command runs, so it ends on its own.
    #[test]
    fn a_command_that_writes_a_lot_is_not_stuck_on_its_pipes() {
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "head -c 300000 /dev/zero | tr '\\0' x >&2; echo done"]);
        let (ok, out, err) = run_deadline(cmd, Duration::from_secs(10)).unwrap();
        assert!(ok);
        assert_eq!(out, "done\n");
        assert_eq!(err.len(), 300000);
    }
}
