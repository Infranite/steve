//! JSON-RPC 2.0 over NDJSON on an adapter's stdio (acp.ts `startProcess` framing).
//!
//! One [`Conn`] per adapter process. A reader task dispatches stdout lines: responses resolve
//! pending requests, everything else goes to the caller's [`Incoming`] sink in arrival order.
//! A bounded sink may hand back a [`Backpressure`]: the reader reads nothing more until it is done,
//! so an adapter that outruns its consumer blocks on its own stdout write instead of being cut off.
//! A writer task serialises stdin writes. When the process exits every pending request fails
//! with `adapter exited (<code>)`, exactly like the TS `exit` handler.

use std::collections::HashMap;
use std::ffi::OsString;
use std::future::Future;
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::pin::Pin;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::{mpsc, oneshot, watch};

use crate::util::js_slice;

struct Outbound {
    line: String,
    written: Option<oneshot::Sender<Result<(), RpcError>>>,
}

async fn write_frames<W: tokio::io::AsyncWrite + Unpin>(
    writer: &mut W,
    queue: &mut mpsc::UnboundedReceiver<Outbound>,
) {
    while let Some(frame) = queue.recv().await {
        let result = async {
            writer.write_all(frame.line.as_bytes()).await?;
            writer.flush().await
        }
        .await
        .map_err(|e: std::io::Error| RpcError::msg(format!("adapter write: {e}")));
        let failed = result.is_err();
        if let Some(tx) = frame.written {
            let _ = tx.send(result);
        }
        if failed {
            break;
        }
    }
}

/// An error answer from the adapter, a timeout, or the process going away.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct RpcError {
    /// The text the TS code would have in `e.message`.
    pub message: String,
    /// The JSON-RPC error object, when the adapter sent one.
    pub data: Option<Value>,
}

impl RpcError {
    fn msg(m: impl Into<String>) -> Self {
        Self {
            message: m.into(),
            data: None,
        }
    }
}

/// Messages from the adapter that are not responses to our requests.
pub enum Incoming {
    /// A notification (`id` absent).
    Notification { method: String, params: Value },
    /// A request the adapter wants answered; `id` is echoed back verbatim.
    Request {
        id: Value,
        method: String,
        params: Value,
    },
}

/// What the reader waits for before it reads the adapter's next line: the sink's consumer is
/// behind. It owns what it needs (never the [`Conn`]), and must end by itself when the consumer
/// goes away or gives up, or the reader (and so the adapter, blocked on its stdout) waits forever.
pub type Backpressure = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// What to spawn. `.js` entry points are run by the caller's Node (see [`crate::AcpConfig::node`]).
pub struct Spawn {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub cwd: PathBuf,
    pub env: Vec<(OsString, OsString)>,
    /// Prefix for stderr log lines (`[acp <sessionId>]`).
    pub log_tag: String,
    /// Start from an empty environment: `env` is all of it (the sandbox's level 2).
    pub clean_env: bool,
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, RpcError>>>>>;

/// Bounded process facts, with no stderr, environment, arguments or request contents.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExitReport {
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub wait_error: bool,
    pub reader_failed: bool,
    pub drain_timed_out: bool,
    pub parse_errors: u64,
}

/// A live adapter process.
pub struct Conn {
    pid: u32,
    reader_failed: AtomicBool,
    parse_errors: AtomicU64,
    out: mpsc::UnboundedSender<Outbound>,
    pending: Pending,
    next_id: AtomicU64,
    exited: watch::Receiver<bool>,
}

/// What an agent's harness never inherits from the Hub: the user's GitHub tokens and SSH agent,
/// with which its shell could push or call GitHub as the user without going through Steve. The
/// adapter outlives a YOLO switch, so they are left out whatever YOLO says; Steve's own pushes take
/// their credential from the vault. `gh`'s own login (its keyring) is not touched.
pub const AGENT_ENV_DROPPED: [&str; 5] = [
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "GH_ENTERPRISE_TOKEN",
    "GITHUB_ENTERPRISE_TOKEN",
    "SSH_AUTH_SOCK",
];

impl Conn {
    /// Spawns the adapter in its own process group, so that killing the group also takes down the
    /// harness it launches (claude under claude-agent-acp).
    /// `on_incoming` runs on the reader task, in stdout order; `on_exit` runs once after exit.
    pub fn spawn(
        sp: Spawn,
        on_incoming: impl Fn(&Conn, Incoming) + Send + Sync + 'static,
        on_exit: impl FnOnce() + Send + 'static,
    ) -> Result<Arc<Conn>, RpcError> {
        Self::spawn_bounded(
            sp,
            usize::MAX,
            |_| true,
            move |c, i| {
                on_incoming(c, i);
                None
            },
            on_exit,
        )
    }

    /// Spawn a waiting shell, record its process identity, then let it exec the adapter. If the
    /// registrar fails (or Node crashes first), EOF closes the gate without executing the adapter.
    /// `registered` is synchronous and must durably record ownership before returning success.
    pub fn spawn_registered(
        sp: Spawn,
        max_frame_bytes: usize,
        admit: impl Fn(usize) -> bool + Send + Sync + 'static,
        registered: impl FnOnce(u32) -> Result<(), RpcError>,
        on_incoming: impl Fn(&Conn, Incoming) -> Option<Backpressure> + Send + Sync + 'static,
        on_exit: impl FnOnce() + Send + 'static,
    ) -> Result<Arc<Conn>, RpcError> {
        Self::spawn_registered_observed(
            sp,
            max_frame_bytes,
            admit,
            registered,
            on_incoming,
            move |_| on_exit(),
        )
    }

    /// The registered path with typed exit evidence. Observation does not change process control.
    pub fn spawn_registered_observed(
        mut sp: Spawn,
        max_frame_bytes: usize,
        admit: impl Fn(usize) -> bool + Send + Sync + 'static,
        registered: impl FnOnce(u32) -> Result<(), RpcError>,
        on_incoming: impl Fn(&Conn, Incoming) -> Option<Backpressure> + Send + Sync + 'static,
        on_exit: impl FnOnce(ExitReport) + Send + 'static,
    ) -> Result<Arc<Conn>, RpcError> {
        let mut args: Vec<OsString> = vec![
            "-c".into(),
            "IFS= read -r go || exit 125; exec \"$@\"".into(),
            "steve-harness".into(),
            sp.program.into_os_string(),
        ];
        args.extend(sp.args);
        sp.program = "/bin/sh".into();
        sp.args = args;
        let conn = Self::spawn_bounded_observed(sp, max_frame_bytes, admit, on_incoming, on_exit)?;
        registered(conn.pid())?;
        conn.out
            .send(Outbound {
                line: "go\n".into(),
                written: None,
            })
            .map_err(|_| RpcError::msg("registered adapter gate closed"))?;
        Ok(conn)
    }

    /// Node driver ingress boundary. Called on the reader before JSON parsing, including replies
    /// and permission requests. Legacy spawn retains its original framing and admission behavior.
    /// The [`Backpressure`] `on_incoming` returns is awaited before the next line is read.
    pub fn spawn_bounded(
        sp: Spawn,
        max_frame_bytes: usize,
        admit: impl Fn(usize) -> bool + Send + Sync + 'static,
        on_incoming: impl Fn(&Conn, Incoming) -> Option<Backpressure> + Send + Sync + 'static,
        on_exit: impl FnOnce() + Send + 'static,
    ) -> Result<Arc<Conn>, RpcError> {
        Self::spawn_bounded_observed(sp, max_frame_bytes, admit, on_incoming, move |_| on_exit())
    }

    pub fn spawn_bounded_observed(
        sp: Spawn,
        max_frame_bytes: usize,
        admit: impl Fn(usize) -> bool + Send + Sync + 'static,
        on_incoming: impl Fn(&Conn, Incoming) -> Option<Backpressure> + Send + Sync + 'static,
        on_exit: impl FnOnce(ExitReport) + Send + 'static,
    ) -> Result<Arc<Conn>, RpcError> {
        let mut cmd = Command::new(&sp.program);
        if sp.clean_env {
            cmd.env_clear();
        }
        cmd.args(&sp.args)
            .current_dir(&sp.cwd)
            .envs(sp.env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            // last line of defence if the runtime is dropped without shutdown()
            .kill_on_drop(true);
        // the upgrade channel, the key store and the secret files are the Hub's alone
        let given = sp
            .env
            .iter()
            .map(|(k, _)| std::ffi::OsString::from(k))
            .filter(|k| {
                k.to_str()
                    .is_some_and(steve_vault::harness::hub_private_env)
            });
        for key in steve_vault::harness::hub_private_vars()
            .into_iter()
            .chain(given)
            .chain(AGENT_ENV_DROPPED.into_iter().map(std::ffi::OsString::from))
        {
            cmd.env_remove(key);
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| RpcError::msg(format!("spawn {} {e}", sp.program.display())))?;
        let pid = child.id().unwrap_or(0);
        let (Some(mut stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            return Err(RpcError::msg("adapter stdio unavailable"));
        };
        let (out, mut out_rx) = mpsc::unbounded_channel::<Outbound>();
        let (exit_tx, exited) = watch::channel(false);
        let conn = Arc::new(Conn {
            pid,
            reader_failed: AtomicBool::new(false),
            parse_errors: AtomicU64::new(0),
            out,
            pending: Arc::default(),
            next_id: AtomicU64::new(0),
            exited,
        });

        tokio::spawn(async move {
            write_frames(&mut stdin, &mut out_rx).await;
        });

        // what the adapter says on its own (timings, progress, its own warnings): kept, not a warning
        // of Steve's. A request that fails says so where it is awaited.
        let tag = sp.log_tag.clone();
        tokio::spawn(stderr_lines(stderr, move |t| tracing::info!("{tag} {t}")));

        let weak = Arc::downgrade(&conn);
        let read_error = Arc::new(AtomicBool::new(false));
        let read_error_in = read_error.clone();
        let reader = tokio::spawn(async move {
            let mut rd = BufReader::new(stdout);
            let mut line = Vec::new();
            loop {
                line.clear();
                let read = if max_frame_bytes == usize::MAX {
                    rd.read_until(b'\n', &mut line).await
                } else {
                    (&mut rd)
                        .take(max_frame_bytes as u64 + 1)
                        .read_until(b'\n', &mut line)
                        .await
                };
                match read {
                    Ok(0) => break,
                    Err(_) => {
                        read_error_in.store(true, Ordering::Relaxed);
                        break;
                    }
                    Ok(_) => {}
                }
                let Some(c) = weak.upgrade() else { break };
                let admitted = admit(line.len());
                if line.len() > max_frame_bytes || !admitted {
                    c.reader_failed.store(true, Ordering::SeqCst);
                    for (_, tx) in c.pending.lock().unwrap().drain() {
                        let _ = tx.send(Err(RpcError::msg("adapter exited: frame limit")));
                    }
                    break;
                }
                let wait = c.dispatch(&String::from_utf8_lossy(&line), &on_incoming);
                // not held while waiting: the Conn goes when its owner lets it go
                drop(c);
                if let Some(wait) = wait {
                    wait.await;
                }
            }
        });

        let pending = conn.pending.clone();
        let observed = Arc::downgrade(&conn);
        tokio::spawn(async move {
            let status = child.wait().await;
            // let the reader drain what the process wrote before exiting (bounded: a grandchild may hold stdout open)
            let drain = tokio::time::timeout(Duration::from_millis(500), reader).await;
            let c = observed.upgrade();
            let report = ExitReport {
                exit_code: status.as_ref().ok().and_then(|s| s.code()),
                signal: status.as_ref().ok().and_then(|s| s.signal()),
                wait_error: status.is_err(),
                reader_failed: read_error.load(Ordering::Relaxed)
                    || matches!(drain, Ok(Err(_)))
                    || c.as_ref()
                        .is_some_and(|c| c.reader_failed.load(Ordering::Relaxed)),
                drain_timed_out: drain.is_err(),
                parse_errors: c
                    .as_ref()
                    .map_or(0, |c| c.parse_errors.load(Ordering::Relaxed)),
            };
            drop(c);
            let code = match report.exit_code {
                Some(c) => c.to_string(),
                None => "null".into(),
            };
            let waiting: Vec<_> = pending
                .lock()
                .map(|mut p| p.drain().collect())
                .unwrap_or_default();
            for (_, tx) in waiting {
                let _ = tx.send(Err(RpcError::msg(format!("adapter exited ({code})"))));
            }
            let _ = exit_tx.send(true);
            on_exit(report);
        });
        Ok(conn)
    }

    fn dispatch(
        &self,
        line: &str,
        sink: &(impl Fn(&Conn, Incoming) -> Option<Backpressure> + Send + Sync),
    ) -> Option<Backpressure> {
        if line.trim().is_empty() {
            return None;
        }
        let Ok(Value::Object(mut m)) = serde_json::from_str::<Value>(line) else {
            self.parse_errors.fetch_add(1, Ordering::Relaxed);
            return None;
        };
        let id = m.remove("id").filter(|v| !v.is_null());
        let method = m.get("method").and_then(Value::as_str).map(str::to_owned);
        if let (Some(id), None) = (&id, &method)
            && let Some(n) = id.as_u64()
            && let Some(tx) = self.pending.lock().ok().and_then(|mut p| p.remove(&n))
        {
            let r = match m.remove("error") {
                Some(err) if !err.is_null() => Err(RpcError {
                    message: err
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    data: Some(err),
                }),
                _ => Ok(m.remove("result").unwrap_or(Value::Null)),
            };
            let _ = tx.send(r);
            return None;
        }
        let method = method?;
        let params = m.remove("params").unwrap_or(Value::Null);
        match id {
            Some(id) => sink(self, Incoming::Request { id, method, params }),
            None => sink(self, Incoming::Notification { method, params }),
        }
    }

    fn send(&self, mut m: serde_json::Map<String, Value>) {
        let mut o = serde_json::Map::new();
        o.insert("jsonrpc".into(), json!("2.0"));
        o.append(&mut m);
        let _ = self.out.send(Outbound {
            line: format!("{}\n", Value::Object(o)),
            written: None,
        });
    }

    /// Sends a request; `timeout` of `None` waits forever (used for `session/prompt`).
    pub async fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Option<Duration>,
    ) -> Result<Value, RpcError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let (tx, rx) = oneshot::channel();
        if let Ok(mut p) = self.pending.lock() {
            if self.reader_failed.load(Ordering::SeqCst) {
                return Err(RpcError::msg("adapter exited: reader failed"));
            }
            p.insert(id, tx);
        }
        let mut m = serde_json::Map::new();
        m.insert("id".into(), json!(id));
        m.insert("method".into(), json!(method));
        m.insert("params".into(), params);
        self.send(m);
        let gone = || RpcError::msg("adapter exited (null)");
        match timeout {
            None => rx.await.unwrap_or_else(|_| Err(gone())),
            Some(d) => match tokio::time::timeout(d, rx).await {
                Ok(r) => r.unwrap_or_else(|_| Err(gone())),
                Err(_) => {
                    // a late response is then ignored
                    if let Ok(mut p) = self.pending.lock() {
                        p.remove(&id);
                    }
                    Err(RpcError::msg(format!("timeout {method}")))
                }
            },
        }
    }

    pub fn notify(&self, method: &str, params: Value) {
        let mut m = serde_json::Map::new();
        m.insert("method".into(), json!(method));
        m.insert("params".into(), params);
        self.send(m);
    }

    pub fn respond(&self, id: Value, result: Value) {
        let mut m = serde_json::Map::new();
        m.insert("id".into(), id);
        m.insert("result".into(), result);
        self.send(m);
    }

    /// Queue a response and report whether all its bytes were flushed to the adapter pipe.
    /// This is transport write evidence, NOT an acknowledgement that the agent applied it.
    /// A dropped receiver/timeout is uncertain: never resend a permission under a new identity.
    pub fn respond_tracked(
        &self,
        id: Value,
        result: Value,
    ) -> Result<oneshot::Receiver<Result<(), RpcError>>, RpcError> {
        let (tx, rx) = oneshot::channel();
        let line = format!("{}\n", json!({"jsonrpc":"2.0", "id":id, "result":result}));
        self.out
            .send(Outbound {
                line,
                written: Some(tx),
            })
            .map_err(|_| RpcError::msg("adapter response pipe is closed"))?;
        Ok(rx)
    }

    pub fn respond_error(&self, id: Value, code: i64, message: &str) {
        let mut m = serde_json::Map::new();
        m.insert("id".into(), id);
        m.insert("error".into(), json!({ "code": code, "message": message }));
        self.send(m);
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    pub fn has_exited(&self) -> bool {
        *self.exited.borrow() || self.reader_failed.load(Ordering::SeqCst)
    }

    /// Signals the adapter's whole process group. Errors (group already gone) are ignored.
    pub fn signal(&self, sig: libc::c_int) {
        if self.pid == 0 {
            return;
        }
        // SAFETY: plain syscall; a negative pid addresses the process group we created at spawn
        unsafe {
            libc::kill(-(self.pid as libc::pid_t), sig);
        }
    }

    /// SIGTERM the group (what the TS `proc.kill()` does, extended to grandchildren).
    pub fn kill(&self) {
        self.signal(libc::SIGTERM);
    }

    /// Waits until the direct child has exited.
    pub async fn wait_exit(&self) {
        let mut rx = self.exited.clone();
        let _ = rx.wait_for(|e| *e).await;
    }
}

/// The longest stderr line read at once; the rest of an overlong line is drained, not logged.
const STDERR_LINE_MAX: u64 = 64 * 1024;

/// Logs an adapter's stderr a line at a time, scrubbed and cut to 500 characters. Whole lines
/// matter: an adapter's stderr can echo what a command printed, and an injected secret split
/// across two reads would slip past the redactor (V4).
async fn stderr_lines(stderr: impl tokio::io::AsyncRead + Unpin, mut log: impl FnMut(&str)) {
    let mut r = BufReader::new(stderr);
    let mut line = Vec::new();
    // inside an overlong line whose head was already logged
    let mut tail = false;
    loop {
        line.clear();
        match (&mut r)
            .take(STDERR_LINE_MAX)
            .read_until(b'\n', &mut line)
            .await
        {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let skip = tail;
        tail = line.last() != Some(&b'\n');
        if skip {
            continue;
        }
        let t = String::from_utf8_lossy(&line);
        let t = t.trim();
        if !t.is_empty() {
            // scrubbed before the cut, so a value straddling the 500th character is caught too
            let t = steve_vault::redact::scrub_any(t);
            log(js_slice(&t, 500));
        }
    }
}

#[cfg(test)]
mod stderr_tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    #[tokio::test]
    async fn a_secret_split_across_reads_is_still_scrubbed() {
        let secret = "sk-test-stderr-split-0123456789abcdef";
        steve_vault::redact::register("rpc-stderr-test", "API_KEY", secret.as_bytes());
        // a tiny pipe, so the reader sees the line in several pieces
        let (mut w, r) = tokio::io::duplex(8);
        let feed = tokio::spawn(async move {
            let text = format!("token is {secret} ok\nnext line\n");
            for piece in text.as_bytes().chunks(5) {
                w.write_all(piece).await.unwrap();
                tokio::task::yield_now().await;
            }
        });
        let mut got = Vec::new();
        stderr_lines(r, |t| got.push(t.to_string())).await;
        feed.await.unwrap();
        steve_vault::redact::forget("rpc-stderr-test");
        assert_eq!(got.len(), 2, "{got:?}");
        assert!(!got[0].contains("sk-test"), "{got:?}");
        assert!(
            got[0].starts_with("token is ") && got[0].ends_with(" ok"),
            "{got:?}"
        );
        assert_eq!(got[1], "next line");
    }

    #[tokio::test]
    async fn an_overlong_line_is_logged_once() {
        let mut text = "x".repeat(STDERR_LINE_MAX as usize * 2);
        text.push_str("\nafter\n");
        let mut got = Vec::new();
        stderr_lines(text.as_bytes(), |t| got.push(t.to_string())).await;
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].len(), 500);
        assert_eq!(got[1], "after");
    }
}

#[cfg(test)]
mod control_tests {
    use super::*;

    #[tokio::test]
    async fn observed_exits_keep_rpc_failure_semantics_and_distinguish_signal_from_exit_code() {
        for (script, code, signal, parse_errors) in [
            ("read line; printf 'not-json\\n'; exit 7", Some(7), None, 1),
            ("read line; kill -TERM $$", None, Some(15), 0),
            ("read line; exit 0", Some(0), None, 0),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let (tx, rx) = oneshot::channel();
            let conn = Conn::spawn_bounded_observed(
                Spawn {
                    program: "/bin/sh".into(),
                    args: vec!["-c".into(), script.into()],
                    cwd: dir.path().into(),
                    env: vec![],
                    log_tag: "exit-observation-test".into(),
                    clean_env: false,
                },
                1024,
                |_| true,
                |_, _| None,
                move |report| {
                    let _ = tx.send(report);
                },
            )
            .unwrap();
            let error = conn
                .request("initialize", json!({}), Some(Duration::from_secs(3)))
                .await
                .unwrap_err();
            let report = tokio::time::timeout(Duration::from_secs(3), rx)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(report.exit_code, code, "{script}");
            assert_eq!(report.signal, signal, "{script}");
            assert_eq!(report.parse_errors, parse_errors, "{script}");
            assert!(!report.wait_error && !report.drain_timed_out && !report.reader_failed);
            let shown = code.map_or("null".into(), |n| n.to_string());
            assert_eq!(error.message, format!("adapter exited ({shown})"));
        }
    }

    #[tokio::test]
    async fn adapters_never_receive_supervisor_control_environment() {
        let dir = tempfile::tempdir().unwrap();
        let sp = Spawn {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), "test -z \"$STEVE_UPGRADE_CONTROL_TOKEN$STEVE_SUPERVISOR_PID$STEVE_UPGRADE_ID\" && touch clean".into()],
            cwd: dir.path().into(),
            env: ["STEVE_UPGRADE_CONTROL_TOKEN", "STEVE_SUPERVISOR_PID", "STEVE_UPGRADE_ID"].into_iter().map(|k|(k.into(), "synthetic".into())).collect(),
            log_tag: "test".into(),
            clean_env: false,
        };
        let conn = Conn::spawn(sp, |_, _| {}, || {}).unwrap();
        tokio::time::timeout(Duration::from_secs(2), conn.wait_exit())
            .await
            .unwrap();
        assert!(dir.path().join("clean").exists());
    }

    #[tokio::test]
    async fn adapters_never_receive_vault_or_secret_file_environment() {
        let dir = tempfile::tempdir().unwrap();
        let sp = Spawn {
            program: "/bin/sh".into(),
            args: vec![
                "-c".into(),
                "test -z \"$STEVE_OIDC_CLIENT_SECRET_FILE$STEVE_VAULT_KEYSTORE$OPENVIKING_KEY_FILE\" && test \"$KEEP\" = 1 && touch clean".into(),
            ],
            cwd: dir.path().into(),
            env: [
                ("STEVE_OIDC_CLIENT_SECRET_FILE", "/x/secrets/oidc"),
                ("STEVE_VAULT_KEYSTORE", "keychain"),
                ("OPENVIKING_KEY_FILE", "/x/secrets/ov"),
                ("KEEP", "1"),
            ]
            .into_iter()
            .map(|(k, v)| (k.into(), v.into()))
            .collect(),
            log_tag: "test".into(),
            clean_env: false,
        };
        let conn = Conn::spawn(sp, |_, _| {}, || {}).unwrap();
        tokio::time::timeout(Duration::from_secs(2), conn.wait_exit())
            .await
            .unwrap();
        assert!(dir.path().join("clean").exists());
    }
}

#[cfg(test)]
mod backpressure_tests {
    use super::*;

    /// The reader reads no further line while the sink's consumer is behind, and goes on once it
    /// has room again: nothing is skipped.
    #[tokio::test]
    async fn the_reader_waits_for_a_sink_that_is_behind() {
        let dir = tempfile::tempdir().unwrap();
        let line = r#"{"jsonrpc":"2.0","method":"n","params":{}}"#;
        let sp = Spawn {
            program: "/bin/sh".into(),
            args: vec![
                "-c".into(),
                format!("for i in 1 2 3; do echo '{line}'; done; sleep 5").into(),
            ],
            cwd: dir.path().into(),
            env: vec![],
            log_tag: "test".into(),
            clean_env: false,
        };
        let seen = Arc::new(AtomicU64::new(0));
        let room = Arc::new(tokio::sync::Semaphore::new(0));
        let (s, r) = (seen.clone(), room.clone());
        let conn = Conn::spawn_bounded(
            sp,
            1 << 20,
            |_| true,
            move |_, _| {
                s.fetch_add(1, Ordering::SeqCst);
                let r = r.clone();
                Some(Box::pin(async move {
                    r.acquire().await.unwrap().forget();
                }))
            },
            || {},
        )
        .unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(seen.load(Ordering::SeqCst), 1, "read on while behind");
        room.add_permits(3);
        let t0 = std::time::Instant::now();
        while seen.load(Ordering::SeqCst) < 3 {
            assert!(t0.elapsed() < Duration::from_secs(3), "lines lost");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        conn.kill();
    }
}

#[cfg(test)]
mod write_evidence_tests {
    use super::*;
    #[tokio::test]
    async fn queued_response_is_not_written_and_broken_pipe_is_not_success() {
        let (mut pipe, peer) = tokio::io::duplex(1);
        let (tx, mut queue) = mpsc::unbounded_channel();
        let (written, mut evidence) = oneshot::channel();
        tx.send(Outbound {
            line: "a response larger than the pipe".into(),
            written: Some(written),
        })
        .unwrap();
        let writer = tokio::spawn(async move {
            write_frames(&mut pipe, &mut queue).await;
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut evidence)
                .await
                .is_err()
        );
        drop(peer);
        assert!(evidence.await.unwrap().is_err());
        writer.await.unwrap();
    }
    #[tokio::test]
    async fn successful_write_has_explicit_evidence() {
        let (mut pipe, mut peer) = tokio::io::duplex(128);
        let (tx, mut queue) = mpsc::unbounded_channel();
        let (written, evidence) = oneshot::channel();
        tx.send(Outbound {
            line: "response\n".into(),
            written: Some(written),
        })
        .unwrap();
        drop(tx);
        write_frames(&mut pipe, &mut queue).await;
        assert!(evidence.await.unwrap().is_ok());
        let mut b = [0; 9];
        peer.read_exact(&mut b).await.unwrap();
        assert_eq!(&b, b"response\n");
    }
}
