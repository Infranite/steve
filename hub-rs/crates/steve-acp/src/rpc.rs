//! JSON-RPC 2.0 over NDJSON on an adapter's stdio (acp.ts `startProcess` framing, §5.1–5.2).
//!
//! One [`Conn`] per adapter process. A reader task dispatches stdout lines: responses resolve
//! pending requests, everything else goes to the caller's [`Incoming`] sink in arrival order.
//! A writer task serialises stdin writes. When the process exits every pending request fails
//! with `adapter exited (<code>)`, exactly like the TS `exit` handler.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::{mpsc, oneshot, watch};

use crate::util::js_slice;

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

/// What to spawn. `.js` entry points are run by the caller's Node (see [`crate::AcpConfig::node`]).
pub struct Spawn {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub cwd: PathBuf,
    pub env: Vec<(OsString, OsString)>,
    /// Prefix for stderr log lines (`[acp <sessionId>]`).
    pub log_tag: String,
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, RpcError>>>>>;

/// A live adapter process.
pub struct Conn {
    pid: u32,
    out: mpsc::UnboundedSender<String>,
    pending: Pending,
    next_id: AtomicU64,
    exited: watch::Receiver<bool>,
}

impl Conn {
    /// Spawns the adapter in its own process group, so that killing the group also takes down the
    /// harness it launches (claude under claude-agent-acp) — plan §2, inventory §13.11.
    /// `on_incoming` runs on the reader task, in stdout order; `on_exit` runs once after exit.
    pub fn spawn(
        sp: Spawn,
        on_incoming: impl Fn(&Conn, Incoming) + Send + Sync + 'static,
        on_exit: impl FnOnce() + Send + 'static,
    ) -> Result<Arc<Conn>, RpcError> {
        let mut cmd = Command::new(&sp.program);
        cmd.args(&sp.args)
            .current_dir(&sp.cwd)
            .envs(sp.env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            // last line of defence if the runtime is dropped without shutdown()
            .kill_on_drop(true);
        for key in [
            "STEVE_UPGRADE_CONTROL_TOKEN",
            "STEVE_SUPERVISOR_PID",
            "STEVE_UPGRADE_ID",
        ] {
            cmd.env_remove(key);
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| RpcError::msg(format!("spawn {} {e}", sp.program.display())))?;
        let pid = child.id().unwrap_or(0);
        let (Some(mut stdin), Some(stdout), Some(mut stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            return Err(RpcError::msg("adapter stdio unavailable"));
        };
        let (out, mut out_rx) = mpsc::unbounded_channel::<String>();
        let (exit_tx, exited) = watch::channel(false);
        let conn = Arc::new(Conn {
            pid,
            out,
            pending: Arc::default(),
            next_id: AtomicU64::new(0),
            exited,
        });

        // writer: write errors (closed pipe) are ignored, the exit handler fails pending requests
        tokio::spawn(async move {
            while let Some(line) = out_rx.recv().await {
                if stdin.write_all(line.as_bytes()).await.is_err() || stdin.flush().await.is_err() {
                    break;
                }
            }
        });

        let tag = sp.log_tag.clone();
        tokio::spawn(async move {
            let mut buf = vec![0u8; 8192];
            while let Ok(n) = stderr.read(&mut buf).await {
                if n == 0 {
                    break;
                }
                let t = String::from_utf8_lossy(&buf[..n]);
                let t = t.trim();
                if !t.is_empty() {
                    // an adapter's stderr can echo what a command printed: no injected secret
                    // reaches the log (V4)
                    let t = steve_vault::redact::scrub_any(t);
                    tracing::warn!("{tag} {}", js_slice(&t, 500));
                }
            }
        });

        let weak = Arc::downgrade(&conn);
        let reader = tokio::spawn(async move {
            let mut rd = BufReader::new(stdout);
            let mut line = Vec::new();
            loop {
                line.clear();
                match rd.read_until(b'\n', &mut line).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                let Some(c) = weak.upgrade() else { break };
                c.dispatch(&String::from_utf8_lossy(&line), &on_incoming);
            }
        });

        let pending = conn.pending.clone();
        tokio::spawn(async move {
            let status = child.wait().await;
            // let the reader drain what the process wrote before exiting (bounded: a grandchild may hold stdout open)
            let _ = tokio::time::timeout(Duration::from_millis(500), reader).await;
            let code = match status.ok().and_then(|s| s.code()) {
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
            on_exit();
        });
        Ok(conn)
    }

    fn dispatch(&self, line: &str, sink: &(impl Fn(&Conn, Incoming) + Send + Sync)) {
        if line.trim().is_empty() {
            return;
        }
        let Ok(Value::Object(mut m)) = serde_json::from_str::<Value>(line) else {
            return;
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
            return;
        }
        let Some(method) = method else { return };
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
        let _ = self.out.send(format!("{}\n", Value::Object(o)));
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
        *self.exited.borrow()
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

#[cfg(test)]
mod control_tests {
    use super::*;
    #[tokio::test]
    async fn adapters_never_receive_supervisor_control_environment() {
        let dir = tempfile::tempdir().unwrap();
        let sp = Spawn {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), "test -z \"$STEVE_UPGRADE_CONTROL_TOKEN$STEVE_SUPERVISOR_PID$STEVE_UPGRADE_ID\" && touch clean".into()],
            cwd: dir.path().into(),
            env: ["STEVE_UPGRADE_CONTROL_TOKEN", "STEVE_SUPERVISOR_PID", "STEVE_UPGRADE_ID"].into_iter().map(|k|(k.into(), "synthetic".into())).collect(),
            log_tag: "test".into(),
        };
        let conn = Conn::spawn(sp, |_, _| {}, || {}).unwrap();
        tokio::time::timeout(Duration::from_secs(2), conn.wait_exit())
            .await
            .unwrap();
        assert!(dir.path().join("clean").exists());
    }
}
