//! Small helpers shared by the domain modules: ids, hashes, time, JS-compatible string slicing, and
//! running a command with a deadline.

use base64::Engine;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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
pub fn run_deadline(
    mut cmd: Command,
    deadline: Duration,
) -> Result<(bool, String, String), RunError> {
    use std::os::unix::process::CommandExt;
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(RunError::Spawn)?;
    let end = Instant::now() + deadline;
    let group = child.id() as i32;
    let kill_group = || {
        // SAFETY: a plain kill(2) of the group this child leads; while any member lives the id is
        // the group's and can't be another process's
        unsafe { libc::kill(-group, libc::SIGKILL) };
    };
    /* the pipes are read and the child is waited for on threads of their own, which are left to end by
     * themselves when the deadline passes: none of them is ever joined */
    let (tx, rx) = mpsc::channel();
    let pipes: [Option<Box<dyn Read + Send>>; 2] = [
        child
            .stdout
            .take()
            .map(|o| Box::new(o) as Box<dyn Read + Send>),
        child
            .stderr
            .take()
            .map(|o| Box::new(o) as Box<dyn Read + Send>),
    ];
    for (n, pipe) in pipes.into_iter().enumerate() {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let mut b = vec![];
            if let Some(mut r) = pipe {
                let _ = r.read_to_end(&mut b);
            }
            let _ = tx.send((n, String::from_utf8_lossy(&b).into_owned()));
        });
    }
    drop(tx);
    let (done, exited) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = done.send(child.wait());
    });
    let status = match exited.recv_timeout(deadline) {
        Ok(s) => s.ok(),
        Err(_) => {
            /* the waiting thread reaps it once it is gone */
            kill_group();
            None
        }
    };
    let mut out = [String::new(), String::new()];
    for _ in 0..2 {
        let left = end
            .saturating_duration_since(Instant::now())
            .max(Duration::from_millis(500));
        match rx.recv_timeout(left) {
            Ok((n, s)) => out[n] = s,
            Err(_) => {
                kill_group();
                return Err(RunError::Held);
            }
        }
    }
    let [o, e] = out;
    match status {
        Some(s) => Ok((s.success(), o, e)),
        None => Err(RunError::Timeout(deadline)),
    }
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
        let mark = tempfile::tempdir().unwrap();
        let alive = mark.path().join("alive");
        /* a wrapper that leaves a child holding its output open, and one that hangs itself */
        for script in [
            format!("(sleep 5; touch {}) & echo started", alive.display()),
            format!("sleep 5; touch {}", alive.display()),
        ] {
            let mut cmd = Command::new("/bin/sh");
            cmd.args(["-c", &script]);
            let t = Instant::now();
            assert!(
                run_deadline(cmd, Duration::from_millis(300)).is_err(),
                "{script}"
            );
            assert!(
                t.elapsed() < Duration::from_secs(3),
                "{script}: {:?}",
                t.elapsed()
            );
        }
        std::thread::sleep(Duration::from_secs(6));
        assert!(
            !alive.exists(),
            "what the command started was killed with it"
        );
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
