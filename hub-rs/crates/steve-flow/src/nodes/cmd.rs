//! `cmd`: `{command, project_id, timeout_s ≤ 600}` runs a shell command in the project's directory.
//! Effectful: every run is approved by you (the engine's gate) unless
//! the flow holds a `grants(subject_type='flow', capability='cmd.run', resource_type='project',
//! resource_id=<project_id> | '*')`. Restarts never replay it; dry runs skip it.
//!
//! Execution is the vault's [`inject::run`] with nothing injected: `/bin/sh -c` in its own process
//! group, the whole group killed on timeout and when the command ends, 64 KiB per stream.
//! `{{…}}` in the command expand to single-quoted shell words, so event text can't inject shell
//! syntax; `{{secrets.…}}` is refused (secrets go only into `http` headers and bodies).
//!
//! With a [`CmdNode`] (the Hub's local Node is on, `STEVE_NODE_MODE=local`), the command runs on
//! the Node that holds the project's files instead, recorded once in the Hub's ledger for the run:
//! same rules, same step results. A Node that is not there fails the step (`设备离线`), and one
//! whose answer is lost (it restarted while the command ran) fails it as unknown: never run again.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use steve_modules::vault::inject::{self, RunSpec};

use super::{Grant, NodeKind, NodeOutcome, RunCx, req_str};

pub const MAX_TIMEOUT_S: u64 = inject::MAX_TIMEOUT_S;
pub const DEFAULT_TIMEOUT_S: u64 = inject::DEFAULT_TIMEOUT_S;
/// Longest command.
const COMMAND_MAX: usize = 4000;
/// The tail of stderr put into the step error.
const ERROR_TAIL: usize = 500;

pub const NO_SECRETS: &str = "机密只能用在 http 节点的 headers 和 body 里";
pub const NO_PROJECT: &str = "项目不存在或不在这台机器上";
/// The project's Node is not there.
pub const OFFLINE: &str = "设备离线";
/// The Node's answer was lost: the command may or may not have run, and is not run again.
pub const UNKNOWN: &str =
    "命令结果未知（运行它的设备中途重启或断开），不会自动重跑，请到项目里确认";

/// One `cmd` step to run on a Node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CmdRequest {
    pub workspace_id: String,
    pub project_id: String,
    /// The run and the step: together they name the one operation (asked again, never run again).
    pub run_id: String,
    pub step: String,
    pub command: String,
    pub timeout: Duration,
}

/// What became of a [`CmdRequest`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CmdRan {
    /// The project is not on a Node here (the local Node is off): the step runs as before.
    Legacy,
    /// It ran to its end: the exit code (`None`: a signal ended it) and the bounded output.
    Ended {
        exit_code: Option<i32>,
        stdout: String,
        stderr: String,
    },
    TimedOut,
    /// It did not start.
    NotRun(String),
    Offline,
    NoProject,
    /// The answer was lost; it is not run again.
    Unknown,
}

/// Runs `cmd` steps on the Node that holds the project (the Hub's workspace router and ledger).
#[async_trait]
pub trait CmdNode: Send + Sync {
    async fn run(&self, req: CmdRequest) -> CmdRan;
}

/// The node; `home` is `STEVE_HOME` (the vault's scratch root, unused without injections).
pub struct Cmd {
    home: PathBuf,
    node: Option<Arc<dyn CmdNode>>,
}

impl Cmd {
    pub fn new(home: impl Into<PathBuf>) -> Self {
        Self {
            home: home.into(),
            node: None,
        }
    }

    /// Runs on the project's Node whenever it says so ([`CmdRan::Legacy`]: as before).
    pub fn with_node(mut self, node: Arc<dyn CmdNode>) -> Self {
        self.node = Some(node);
        self
    }
}

impl Default for Cmd {
    fn default() -> Self {
        Self::new(std::env::temp_dir())
    }
}

fn timeout_of(cfg: &Value) -> Result<u64, String> {
    match cfg.get("timeout_s") {
        None | Some(Value::Null) => Ok(DEFAULT_TIMEOUT_S),
        Some(v) => match v.as_f64() {
            Some(t) if t >= 1.0 && t <= MAX_TIMEOUT_S as f64 => Ok(t.ceil() as u64),
            _ => Err(steve_i18n::t!("flow.node.cmd_timeout", max = MAX_TIMEOUT_S)),
        },
    }
}

/// `'…'` with `'` written as `'\''`.
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Expands `{{path}}` in `tpl`, each value as one shell word.
fn render_command(cx: &RunCx<'_>, tpl: &str, input: &Value) -> String {
    let mut out = String::with_capacity(tpl.len());
    let mut rest = tpl;
    while let Some(i) = rest.find("{{") {
        out.push_str(&rest[..i]);
        let after = &rest[i + 2..];
        let Some(j) = after.find("}}") else {
            out.push_str(&rest[i..]);
            return out;
        };
        let one = &rest[i..i + 2 + j + 2];
        out.push_str(&shell_quote(&cx.render(one, input)));
        rest = &after[j + 2..];
    }
    out.push_str(rest);
    out
}

/// The project's directory, canonicalized: an unarchived project of `ws` on this Hub.
fn project_dir(c: &rusqlite::Connection, ws: &str, id: &str) -> Option<PathBuf> {
    let path: Option<String> = c
        .query_row(
            "SELECT path FROM projects WHERE id = ? AND workspace_id = ? AND archived = 0 AND device_id = 'hub'",
            params![id, ws],
            |r| r.get(0),
        )
        .optional()
        .ok()
        .flatten();
    let p = Path::new(&path?).canonicalize().ok()?;
    p.is_dir().then_some(p)
}

fn tail(s: &str, n: usize) -> &str {
    let mut start = s.len().saturating_sub(n);
    while !s.is_char_boundary(start) {
        start += 1;
    }
    s[start..].trim()
}

#[async_trait]
impl NodeKind for Cmd {
    fn kind(&self) -> &'static str {
        "cmd"
    }

    fn effectful(&self, _: &Value) -> bool {
        true
    }

    fn grant(&self, cfg: &Value) -> Option<Grant> {
        let p = cfg["project_id"].as_str()?.trim();
        (!p.is_empty()).then(|| Grant::new("cmd.run", "project", p))
    }

    fn validate(&self, cfg: &Value) -> Result<(), String> {
        let command = req_str(cfg, "command", super::Required::Command, COMMAND_MAX)?;
        if command.contains("{{secrets.") || command.contains("{{ secrets.") {
            return Err(steve_i18n::t!("flow.node.no_secrets"));
        }
        crate::expr::check_template(command)?;
        req_str(cfg, "project_id", super::Required::Project, 64)?;
        timeout_of(cfg)?;
        Ok(())
    }

    async fn run(&self, cx: &mut RunCx<'_>, cfg: &Value, input: &Value) -> NodeOutcome {
        if let Err(e) = self.validate(cfg) {
            return NodeOutcome::Fail(e);
        }
        let project = cfg["project_id"].as_str().unwrap_or("").trim().to_string();
        let ws = cx.ws().to_string();
        if let Some(node) = &self.node {
            let timeout = timeout_of(cfg).unwrap_or(DEFAULT_TIMEOUT_S);
            let req = CmdRequest {
                workspace_id: ws.clone(),
                project_id: project.clone(),
                run_id: cx.run.id.clone(),
                step: cx.node.id.clone(),
                command: render_command(cx, cfg["command"].as_str().unwrap_or(""), input),
                timeout: Duration::from_secs(timeout),
            };
            match node.run(req).await {
                CmdRan::Legacy => {}
                CmdRan::Ended {
                    exit_code,
                    stdout,
                    stderr,
                } => return ended(exit_code, stdout, stderr),
                CmdRan::TimedOut => return timed_out(timeout),
                CmdRan::NotRun(e) => return not_run(&e),
                CmdRan::Offline => return NodeOutcome::Fail(OFFLINE.into()),
                CmdRan::NoProject => return NodeOutcome::Fail(NO_PROJECT.into()),
                CmdRan::Unknown => return NodeOutcome::Fail(UNKNOWN.into()),
            }
        }
        let Ok(Some(dir)) = cx
            .flow
            .store()
            .reader(|c| Ok::<_, crate::FlowError>(project_dir(c, &ws, &project)))
        else {
            return NodeOutcome::Fail(NO_PROJECT.into());
        };
        let timeout = timeout_of(cfg).unwrap_or(DEFAULT_TIMEOUT_S);
        let command = render_command(cx, cfg["command"].as_str().unwrap_or(""), input);
        let spec = RunSpec {
            use_id: steve_store::uid("su"),
            cwd: dir,
            command,
            injections: Vec::new(),
            timeout: Duration::from_secs(timeout),
        };
        let out = inject::run(&self.home, &spec, &[]).await;
        if out.timed_out {
            return timed_out(timeout);
        }
        if let Some(e) = &out.error {
            return not_run(e);
        }
        ended(out.exit_code, out.stdout, out.stderr)
    }
}

fn timed_out(timeout: u64) -> NodeOutcome {
    NodeOutcome::Fail(format!("命令超过 {timeout} 秒，已停止"))
}

fn not_run(e: &str) -> NodeOutcome {
    NodeOutcome::Fail(format!("命令没有运行：{e}"))
}

/// A command that ran to its end: the next step with its output, or the step failed with its
/// exit code and the tail of its stderr (else stdout). `None` (a signal) is `-1`.
fn ended(exit_code: Option<i32>, stdout: String, stderr: String) -> NodeOutcome {
    let code = exit_code.unwrap_or(-1);
    if exit_code != Some(0) {
        let t = tail(&stderr, ERROR_TAIL);
        let t = if t.is_empty() {
            tail(&stdout, ERROR_TAIL)
        } else {
            t
        };
        return NodeOutcome::Fail(format!("命令退出码 {code}：{t}"));
    }
    NodeOutcome::next(json!({
        "exit_code": code,
        "stdout": stdout,
        "stderr": stderr,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting() {
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("it's; rm -rf /"), r"'it'\''s; rm -rf /'");
    }

    #[test]
    fn validate() {
        let c = Cmd::default();
        assert!(
            c.validate(&json!({"command": "ls", "project_id": "prj_a"}))
                .is_ok()
        );
        assert_eq!(
            c.validate(&json!({"command": "echo {{secrets.vi_1.token}}", "project_id": "prj_a"}))
                .unwrap_err(),
            NO_SECRETS
        );
        assert!(c.validate(&json!({"command": "ls"})).is_err());
        assert!(
            c.validate(&json!({"command": "ls", "project_id": "p", "timeout_s": 601}))
                .is_err()
        );
        assert!(
            c.validate(&json!({"command": "ls", "project_id": "p", "timeout_s": 600}))
                .is_ok()
        );
        assert!(c.effectful(&json!({})));
        assert_eq!(
            c.grant(&json!({"project_id": "prj_a"})),
            Some(Grant::new("cmd.run", "project", "prj_a"))
        );
    }
}
