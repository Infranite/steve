//! Isolated workflow JS step subprocess (`steve-hub --workflow-step`) and parent Hub runner.
//!
//! The Hub parent never links or invokes `rquickjs` in-process; it spawns itself with
//! `--workflow-step` and exchanges one length-prefixed bounded JSON frame on stdin (`StepInput`)
//! and stdout (`StepFrame`):
//!
//! ```text
//! [4-byte big-endian u32 payload length][UTF-8 JSON payload]
//! ```
//!
//! Explicit length-prefixed framing avoids blocking on pipe EOF (which can deadlock on macOS if
//! concurrent subprocess spawns momentarily inherit a sibling pipe write end before `FD_CLOEXEC`)
//! and lets both sides reject oversized frames before allocating payload memory.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use steve_workflow_api::{
    EffectiveLimits, MAX_STEP_INPUT_BYTES, MAX_STEP_WALL_TIME, StepFrame, StepInput, WorkflowFault,
    WorkflowFaultCode,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

/// Maximum stderr bytes captured from `--workflow-step` for diagnostics on crash (`16 KiB`).
const MAX_STDERR_BYTES: usize = 16 * 1024;

/// Entrypoint for `steve-hub --workflow-step`.
///
/// Reads one length-prefixed bounded JSON [`StepInput`] from stdin, runs the QuickJS sandboxed
/// single-step replay, and writes one length-prefixed bounded JSON [`StepFrame`] to stdout.
/// Runs synchronously with no Tokio runtime.
pub fn main(args: &[String]) -> i32 {
    if !args.is_empty() {
        let frame = failed_frame(
            WorkflowFaultCode::ProtocolError,
            format!("--workflow-step takes no arguments, got {}", args.join(" ")),
        );
        return emit_frame(&frame);
    }

    let raw_input = match read_sync_frame(&mut std::io::stdin().lock(), MAX_STEP_INPUT_BYTES) {
        Ok(bytes) => bytes,
        Err(SyncFrameError::TooLarge { actual, limit }) => {
            let frame = failed_frame(
                WorkflowFaultCode::InputTooLarge,
                format!("StepInput is {actual} bytes, exceeding limit of {limit} bytes"),
            );
            return emit_frame(&frame);
        }
        Err(SyncFrameError::Io(e)) => {
            let frame = failed_frame(
                WorkflowFaultCode::ProtocolError,
                format!("failed to read StepInput frame from stdin: {e}"),
            );
            return emit_frame(&frame);
        }
    };

    let input: StepInput = match serde_json::from_slice(&raw_input) {
        Ok(i) => i,
        Err(e) => {
            let frame = failed_frame(
                WorkflowFaultCode::ProtocolError,
                format!("invalid StepInput JSON: {e}"),
            );
            return emit_frame(&frame);
        }
    };

    let frame = steve_workflow_js::run_step(&input);
    emit_frame(&frame)
}

enum SyncFrameError {
    TooLarge { actual: usize, limit: usize },
    Io(std::io::Error),
}

fn read_sync_frame<R: Read>(reader: &mut R, max_bytes: usize) -> Result<Vec<u8>, SyncFrameError> {
    let mut header = [0u8; 4];
    reader.read_exact(&mut header).map_err(SyncFrameError::Io)?;
    let len = u32::from_be_bytes(header) as usize;
    if len > max_bytes {
        return Err(SyncFrameError::TooLarge {
            actual: len,
            limit: max_bytes,
        });
    }
    let mut payload = vec![0u8; len];
    reader
        .read_exact(&mut payload)
        .map_err(SyncFrameError::Io)?;
    Ok(payload)
}

fn emit_frame(frame: &StepFrame) -> i32 {
    let bytes = match serde_json::to_vec(frame) {
        Ok(b) => b,
        Err(e) => {
            let fallback = failed_frame(
                WorkflowFaultCode::ProtocolError,
                format!("failed to serialize StepFrame: {e}"),
            );
            serde_json::to_vec(&fallback).unwrap_or_default()
        }
    };
    let Ok(len) = u32::try_from(bytes.len()) else {
        return 1;
    };
    let mut out = std::io::stdout().lock();
    if out.write_all(&len.to_be_bytes()).is_err()
        || out.write_all(&bytes).is_err()
        || out.flush().is_err()
    {
        return 1;
    }
    0
}

/// Parent-side subprocess executor that communicates with `steve-hub --workflow-step` strictly via
/// the length-prefixed JSON stdin/stdout frame protocol.
#[derive(Debug, Clone)]
pub struct StepRunner {
    binary: PathBuf,
}

impl StepRunner {
    /// Creates a runner invoking the currently executing `steve-hub` binary.
    pub fn current_exe() -> std::io::Result<Self> {
        Ok(Self {
            binary: std::env::current_exe()?,
        })
    }

    /// Creates a runner targeting an explicit `steve-hub` executable path.
    pub fn new(binary: PathBuf) -> Self {
        Self { binary }
    }

    /// Executes one workflow step under its resolved [`EffectiveLimits`].
    pub async fn execute(&self, input: &StepInput) -> StepFrame {
        let limits = EffectiveLimits::resolve(input.limits.as_ref());
        self.execute_with_parent_limits(input, limits.wall_time, limits.max_frame_bytes)
            .await
    }

    /// Executes one workflow step with explicit parent-enforced wall-clock timeout and stdout
    /// frame byte cap (both clamped to the hard ceilings in `steve-workflow-api`).
    pub async fn execute_with_parent_limits(
        &self,
        input: &StepInput,
        parent_wall_time: Duration,
        parent_max_frame_bytes: usize,
    ) -> StepFrame {
        let limits = EffectiveLimits::resolve(input.limits.as_ref());
        let wall_time = parent_wall_time.min(MAX_STEP_WALL_TIME);
        let max_frame_bytes = parent_max_frame_bytes.min(limits.max_frame_bytes);

        let input_bytes = match serde_json::to_vec(input) {
            Ok(b) => b,
            Err(e) => {
                return failed_frame(
                    WorkflowFaultCode::ProtocolError,
                    format!("failed to serialize StepInput: {e}"),
                );
            }
        };
        if input_bytes.len() > MAX_STEP_INPUT_BYTES {
            return failed_frame(
                WorkflowFaultCode::InputTooLarge,
                format!(
                    "StepInput is {} bytes, exceeding limit of {MAX_STEP_INPUT_BYTES} bytes",
                    input_bytes.len()
                ),
            );
        }
        let Ok(input_len) = u32::try_from(input_bytes.len()) else {
            return failed_frame(
                WorkflowFaultCode::InputTooLarge,
                "StepInput length overflows u32 frame header".into(),
            );
        };

        let mut cmd = Command::new(&self.binary);
        cmd.arg("--workflow-step")
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                return failed_frame(
                    WorkflowFaultCode::SubprocessFailed,
                    format!(
                        "failed to spawn workflow step subprocess {}: {e}",
                        self.binary.display()
                    ),
                );
            }
        };

        let mut stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");

        let run_fut = async {
            let write_fut = async move {
                let _ = stdin.write_all(&input_len.to_be_bytes()).await;
                let _ = stdin.write_all(&input_bytes).await;
                drop(stdin);
            };
            let stdout_fut = read_async_frame(stdout, max_frame_bytes);
            let ((), stdout_res) = tokio::join!(write_fut, stdout_fut);

            let status = if matches!(stdout_res, Err(AsyncFrameError::TooLarge { .. })) {
                // Kill immediately on oversized frame before reaping.
                let _ = child.start_kill();
                reap_child(&mut child).await
            } else {
                reap_child(&mut child).await
            };
            (stdout_res, status)
        };

        let (stdout_res, status) = match tokio::time::timeout(wall_time, run_fut).await {
            Ok(pair) => pair,
            Err(_) => {
                let _ = child.start_kill();
                let _ = reap_child(&mut child).await;
                return failed_frame(
                    WorkflowFaultCode::Timeout,
                    format!(
                        "workflow step subprocess exceeded wall timeout of {}ms and was killed",
                        wall_time.as_millis()
                    ),
                );
            }
        };

        match stdout_res {
            Ok(stdout_bytes) => match serde_json::from_slice::<StepFrame>(&stdout_bytes) {
                Ok(frame) => frame,
                Err(e) => {
                    let stderr_text = read_stderr_briefly(stderr).await;
                    failed_frame(
                        WorkflowFaultCode::ProtocolError,
                        format!(
                            "workflow step subprocess emitted malformed StepFrame ({e}); stderr: {stderr_text}"
                        ),
                    )
                }
            },
            Err(AsyncFrameError::TooLarge { actual, limit }) => failed_frame(
                WorkflowFaultCode::FrameTooLarge,
                format!(
                    "workflow step stdout frame is {actual} bytes, exceeding limit of {limit} bytes"
                ),
            ),
            Err(AsyncFrameError::EmptyEof) => {
                let stderr_text = read_stderr_briefly(stderr).await;
                let status_desc = match status {
                    Ok(s) => format!("{s}"),
                    Err(e) => format!("wait error: {e}"),
                };
                failed_frame(
                    WorkflowFaultCode::SubprocessFailed,
                    format!(
                        "workflow step subprocess exited ({status_desc}) with empty stdout; stderr: {stderr_text}"
                    ),
                )
            }
            Err(AsyncFrameError::Io(e)) => {
                let stderr_text = read_stderr_briefly(stderr).await;
                failed_frame(
                    WorkflowFaultCode::ProtocolError,
                    format!(
                        "failed reading workflow step stdout frame: {e}; stderr: {stderr_text}"
                    ),
                )
            }
        }
    }
}

enum AsyncFrameError {
    TooLarge { actual: usize, limit: usize },
    EmptyEof,
    Io(std::io::Error),
}

async fn read_async_frame<R: AsyncRead + Unpin>(
    mut reader: R,
    max_bytes: usize,
) -> Result<Vec<u8>, AsyncFrameError> {
    let mut header = [0u8; 4];
    let mut read = 0usize;
    while read < 4 {
        let n = reader
            .read(&mut header[read..])
            .await
            .map_err(AsyncFrameError::Io)?;
        if n == 0 {
            return if read == 0 {
                Err(AsyncFrameError::EmptyEof)
            } else {
                Err(AsyncFrameError::Io(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "truncated frame length header",
                )))
            };
        }
        read += n;
    }
    let len = u32::from_be_bytes(header) as usize;
    if len > max_bytes {
        return Err(AsyncFrameError::TooLarge {
            actual: len,
            limit: max_bytes,
        });
    }
    let mut payload = vec![0u8; len];
    reader
        .read_exact(&mut payload)
        .await
        .map_err(AsyncFrameError::Io)?;
    Ok(payload)
}

async fn read_stderr_briefly<R: AsyncRead + Unpin>(mut reader: R) -> String {
    let mut out = Vec::new();
    let mut buf = [0u8; 2048];
    let _ = tokio::time::timeout(Duration::from_millis(50), async {
        while out.len() < MAX_STDERR_BYTES {
            match reader.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let take = (MAX_STDERR_BYTES - out.len()).min(n);
                    out.extend_from_slice(&buf[..take]);
                }
            }
        }
    })
    .await;
    String::from_utf8_lossy(&out).trim().to_owned()
}

/// Reaps a child process without relying exclusively on Tokio's process-global `SIGCHLD` pipe,
/// which can lose wakeups when multiple Tokio runtimes coexist in one process (e.g. parallel
/// `#[tokio::test]` threads). In single-runtime production `try_wait()` or `wait()` completes
/// immediately with zero polling.
async fn reap_child(
    child: &mut tokio::process::Child,
) -> std::io::Result<std::process::ExitStatus> {
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        tokio::select! {
            res = child.wait() => return res,
            () = tokio::time::sleep(Duration::from_millis(5)) => {}
        }
    }
}

fn failed_frame(code: WorkflowFaultCode, detail: String) -> StepFrame {
    StepFrame::Failed {
        fault: WorkflowFault::new(code, detail),
        emitted_calls: Vec::new(),
        logs: Vec::new(),
    }
}
