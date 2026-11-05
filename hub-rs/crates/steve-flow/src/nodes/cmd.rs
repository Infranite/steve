//! `cmd`: `{command, project_id, timeout_s ≤ 600}` runs a shell command in the project's directory.
//! Effectful: every run is approved by you (the engine's gate) unless
//! the flow holds a `grants(subject_type='flow', capability='cmd.run', resource_type='project',
//! resource_id=<project_id> | '*')`. Restarts never replay it; dry runs skip it.
//!
//! Execution is the vault's [`inject::run`] with nothing injected: `/bin/sh -c` in its own process
//! group, the whole group killed on timeout and when the command ends, 64 KiB per stream.
//! `{{…}}` in the command expand to single-quoted shell words, so event text can't inject shell
//! syntax; `{{secrets.…}}` is refused (secrets go only into `http` headers and bodies).

use std::path::{Path, PathBuf};
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

/// The node; `home` is `STEVE_HOME` (the vault's scratch root, unused without injections).
pub struct Cmd {
    home: PathBuf,
}

impl Cmd {
    pub fn new(home: impl Into<PathBuf>) -> Self {
        Self { home: home.into() }
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
            _ => Err(format!("超时要在 1 到 {MAX_TIMEOUT_S} 秒之间")),
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
        let command = req_str(cfg, "command", "命令", COMMAND_MAX)?;
        if command.contains("{{secrets.") || command.contains("{{ secrets.") {
            return Err(NO_SECRETS.into());
        }
        crate::expr::check_template(command)?;
        req_str(cfg, "project_id", "项目", 64)?;
        timeout_of(cfg)?;
        Ok(())
    }

    async fn run(&self, cx: &mut RunCx<'_>, cfg: &Value, input: &Value) -> NodeOutcome {
        if let Err(e) = self.validate(cfg) {
            return NodeOutcome::Fail(e);
        }
        let project = cfg["project_id"].as_str().unwrap_or("").trim().to_string();
        let ws = cx.ws().to_string();
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
            return NodeOutcome::Fail(format!("命令超过 {timeout} 秒，已停止"));
        }
        if let Some(e) = &out.error {
            return NodeOutcome::Fail(format!("命令没有运行：{e}"));
        }
        if !out.ok() {
            let t = tail(&out.stderr, ERROR_TAIL);
            let t = if t.is_empty() {
                tail(&out.stdout, ERROR_TAIL)
            } else {
                t
            };
            return NodeOutcome::Fail(format!("命令退出码 {}：{t}", out.code()));
        }
        NodeOutcome::next(json!({
            "exit_code": out.code(),
            "stdout": out.stdout,
            "stderr": out.stderr,
        }))
    }
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
