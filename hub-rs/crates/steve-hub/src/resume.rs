//! The harness's own session behind a Steve session, so the user can pick it up in the harness itself
//! (ST-28). The ACP session id Steve keeps is the harness's id: claude-agent-acp hands its id to Claude
//! Code as `--session-id` (`~/.claude/projects/<cwd>/<id>.jsonl`), codex-acp answers with the Codex
//! thread id (`$CODEX_HOME/sessions/…/rollout-<time>-<id>.jsonl`). An imported session has no ACP id;
//! its import key names the transcript file, whose name carries the id.
//!
//! `info` also reads the transcript the id points to, so the page says whether it is really there and
//! belongs to the same directory. Only ids and `cwd` fields are parsed; nothing else is kept or returned.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde_json::{Value, json};

use crate::backend::Obj;

/// Where the harnesses keep their sessions: what the Hub's own environment gives its agents.
#[derive(Debug, Clone, Default)]
pub struct Homes {
    pub home: PathBuf,
    /// `$CLAUDE_CONFIG_DIR`, default `~/.claude`.
    pub claude_dir: PathBuf,
    /// `$CODEX_HOME`, default `~/.codex`.
    pub codex_home: PathBuf,
    /// Manifest declarations (`harness.json`), so custom or overridden `resume_cli` / `fork_cli` /
    /// `state` work on the resume page too.
    pub declared: BTreeMap<String, steve_acp::Declaration>,
}

impl Homes {
    pub fn of(cfg: &crate::config::Config) -> Homes {
        let home = crate::config::home_dir();
        Homes {
            claude_dir: std::env::var_os("CLAUDE_CONFIG_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".claude")),
            codex_home: cfg.codex_home.clone(),
            home,
            declared: cfg.declared_adapters.clone(),
        }
    }
}

fn text<'a>(s: &'a Obj, k: &str) -> Option<&'a str> {
    s.get(k).and_then(Value::as_str).filter(|v| !v.is_empty())
}

/// The id at the end of a transcript file name: `<id>.jsonl` (Claude Code) or `rollout-<time>-<id>.jsonl`
/// (Codex, a UUID of 36 characters).
fn id_of_file(kind: &str, file: &str) -> Option<String> {
    let stem = Path::new(file).file_stem()?.to_str()?;
    let id = match kind {
        "cc" => stem,
        "cx" => stem.get(stem.len().checked_sub(36)?..)?,
        _ => return None,
    };
    (!id.is_empty()).then(|| id.to_owned())
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 256 && !id.starts_with('-') && !id.chars().any(char::is_control)
}

/// The harness's session id of a session row (before `import_key` is dropped from the output).
pub fn native_id(s: &Obj) -> Option<String> {
    if let Some(id) = text(s, "acp_session_id") {
        return valid_id(id).then(|| id.to_owned());
    }
    let (kind, file) = text(s, "import_key")?.split_once(':')?;
    id_of_file(kind, file).filter(|id| valid_id(id))
}

/// Where a session's harness state lives, and whether the caller may inspect an isolated principal
/// directory (`STEVE_PRINCIPAL_STATE=on` is owner-only).
#[derive(Debug, Clone)]
pub enum SessionState {
    AmbientLocal,
    IsolatedLocal {
        harness_dir: PathBuf,
        expose_path: bool,
    },
    RemoteNode,
}

/// A shell word: as is when it is plain and cannot be read as an option, tilde expansion or shell
/// variable assignment, otherwise in single quotes.
fn sh(s: &str) -> String {
    if !s.is_empty()
        && !s.starts_with(['-', '+', '~'])
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "_-./:@%,".contains(c))
    {
        s.to_owned()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

fn render_cli(
    h: &Homes,
    props: &steve_acp::AdapterProperties,
    template: Option<&str>,
    id: &str,
    cwd: &str,
    principal_state_dir: Option<&Path>,
) -> Option<String> {
    if !valid_id(id) {
        return None;
    }
    let template = template?;
    let run = template.replace("{id}", &sh(id));
    let env = match (props.state.as_ref(), principal_state_dir) {
        (Some(state), Some(pdir)) => {
            let mut pairs = Vec::new();
            for (k, v) in state.isolation_env(pdir) {
                let vp = PathBuf::from(v);
                let vp = if vp.is_relative() {
                    std::path::absolute(&vp).unwrap_or(vp)
                } else {
                    vp
                };
                pairs.push(format!(
                    "{}={} ",
                    k.to_string_lossy(),
                    sh(&vp.to_string_lossy())
                ));
            }
            pairs.concat()
        }
        (Some(state), None) if !state.env.is_empty() => {
            let default = if state.home_override {
                h.home.clone()
            } else {
                h.home.join(&state.default)
            };
            let dir = match state.env.as_str() {
                "CLAUDE_CONFIG_DIR" => h.claude_dir.clone(),
                "CODEX_HOME" => h.codex_home.clone(),
                other => std::env::var_os(other)
                    .filter(|v| !v.is_empty())
                    .map(PathBuf::from)
                    .unwrap_or_else(|| default.clone()),
            };
            if dir == default {
                String::new()
            } else {
                let abs = if dir.is_relative() {
                    std::path::absolute(&dir).unwrap_or_else(|_| h.home.join(&dir))
                } else {
                    dir
                };
                format!("{}={} ", state.env, sh(&abs.to_string_lossy()))
            }
        }
        _ => String::new(),
    };
    let cd = if cwd.is_empty() {
        String::new()
    } else if cwd.starts_with(['-', '+']) {
        format!("cd -- {} && ", sh(cwd))
    } else {
        format!("cd {} && ", sh(cwd))
    };
    Some(format!("{cd}{env}{run}"))
}

/// `cd <cwd> && [VAR=dir] <harness command>`; the variable only when the Hub's agents use another one.
pub fn command(h: &Homes, harness: &str, id: &str, cwd: &str) -> Option<String> {
    let props = steve_acp::adapters::spec(harness, h.declared.get(harness)).properties;
    render_cli(h, &props, props.resume_cli.as_deref(), id, cwd, None)
}

fn node_homes(
    h: &Homes,
    props: &steve_acp::AdapterProperties,
    principal_state_dir: Option<&Path>,
) -> steve_node::history::Homes {
    let mut nh = steve_node::history::Homes {
        user_home: h.home.clone(),
        steve_home: PathBuf::new(),
        claude_dir: h.claude_dir.clone(),
        codex_home: h.codex_home.clone(),
    };
    if let (Some(state), Some(pdir)) = (props.state.as_ref(), principal_state_dir) {
        for (k, v) in state.isolation_env(pdir) {
            match k.to_str() {
                Some("HOME") => nh.user_home = PathBuf::from(v),
                Some("CLAUDE_CONFIG_DIR") => nh.claude_dir = PathBuf::from(v),
                Some("CODEX_HOME") => nh.codex_home = PathBuf::from(v),
                _ => {}
            }
        }
    }
    nh
}

/// The transcript part of [`info`]: where it is, when it changed, and what it records.
fn transcript(
    path: &str,
    updated: Option<i64>,
    rid: Option<String>,
    rcwd: Option<String>,
    id: &str,
    cwd: &str,
) -> Value {
    let same = rid.as_deref().is_none_or(|r| r == id)
        && rcwd.as_deref().is_none_or(|c| cwd.is_empty() || c == cwd);
    json!({ "path": path, "updated": updated, "id": rid, "cwd": rcwd, "matches": same })
}

fn page(
    harness: &str,
    cwd: &str,
    h: &Homes,
    id: Option<&str>,
    transcript: Value,
    principal_state_dir: Option<&Path>,
    transcript_checked: bool,
) -> Value {
    let props = steve_acp::adapters::spec(harness, h.declared.get(harness)).properties;
    let cwd_exists = !cwd.is_empty() && Path::new(cwd).is_dir();
    let Some(id) = id else {
        return json!({
            "harness": harness,
            "id": null,
            "cwd": cwd,
            "cwd_exists": cwd_exists,
            "command": null,
            "fork_command": null,
            "transcript": null,
            "transcript_checked": transcript_checked,
        });
    };
    json!({
        "harness": harness,
        "id": id,
        "cwd": cwd,
        "cwd_exists": cwd_exists,
        "command": render_cli(h, &props, props.resume_cli.as_deref(), id, cwd, principal_state_dir),
        "fork_command": render_cli(h, &props, props.fork_cli.as_deref(), id, cwd, principal_state_dir),
        "transcript": transcript,
        "transcript_checked": transcript_checked,
    })
}

/// What the session page shows to resume a session in its harness. Blocking: reads the harness's files.
pub fn info(h: &Homes, s: &Obj) -> Value {
    info_for_state(h, s, &SessionState::AmbientLocal)
}

pub fn info_with_state(h: &Homes, s: &Obj, principal_state_dir: Option<&Path>) -> Value {
    match principal_state_dir {
        Some(dir) => info_for_state(
            h,
            s,
            &SessionState::IsolatedLocal {
                harness_dir: dir.to_path_buf(),
                expose_path: true,
            },
        ),
        None => info_for_state(h, s, &SessionState::AmbientLocal),
    }
}

pub fn info_for_state(h: &Homes, s: &Obj, state: &SessionState) -> Value {
    let harness = text(s, "harness").unwrap_or_default();
    let cwd = text(s, "cwd").unwrap_or_default();
    let id = native_id(s);
    let props = steve_acp::adapters::spec(harness, h.declared.get(harness)).properties;
    let (pdir, check) = match state {
        SessionState::AmbientLocal => (None, props.local_transcript),
        SessionState::IsolatedLocal {
            harness_dir,
            expose_path: true,
        } => (Some(harness_dir.as_path()), props.local_transcript),
        SessionState::IsolatedLocal {
            expose_path: false, ..
        }
        | SessionState::RemoteNode => (None, false),
    };
    let found = if check {
        let nh = node_homes(h, &props, pdir);
        let file = id
            .as_deref()
            .and_then(|id| steve_node::history::find_transcript(&nh, harness, id, cwd));
        match (&id, file) {
            (Some(id), Some(f)) => {
                let (rid, rcwd) = steve_node::history::recorded(&f, harness);
                let updated = std::fs::metadata(&f)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_millis() as i64);
                transcript(&f.to_string_lossy(), updated, rid, rcwd, id, cwd)
            }
            _ => Value::Null,
        }
    } else {
        Value::Null
    };
    page(harness, cwd, h, id.as_deref(), found, pdir, check)
}

/// [`info`] with the transcript found by the local Node ([`Discovery::locate`] of
/// `session:<id>`), the Hub reading none of the harness's files.
pub async fn info_on(
    h: &Homes,
    s: &Obj,
    node: &dyn steve_node_api::Discovery,
    principal_state_dir: Option<&Path>,
) -> Value {
    let state = match principal_state_dir {
        Some(dir) => SessionState::IsolatedLocal {
            harness_dir: dir.to_path_buf(),
            expose_path: true,
        },
        None => SessionState::AmbientLocal,
    };
    info_on_state(h, s, node, &state).await
}

pub async fn info_on_state(
    h: &Homes,
    s: &Obj,
    node: &dyn steve_node_api::Discovery,
    state: &SessionState,
) -> Value {
    match state {
        SessionState::IsolatedLocal { .. } | SessionState::RemoteNode => {
            let h = h.clone();
            let s = s.clone();
            let state = state.clone();
            return tokio::task::spawn_blocking(move || info_for_state(&h, &s, &state))
                .await
                .unwrap_or(Value::Null);
        }
        SessionState::AmbientLocal => {}
    }
    let harness = text(s, "harness").unwrap_or_default();
    let cwd = text(s, "cwd").unwrap_or_default();
    let id = native_id(s);
    let props = steve_acp::adapters::spec(harness, h.declared.get(harness)).properties;
    let mut found = Value::Null;
    if props.local_transcript
        && let Some(id) = &id
    {
        let at = node
            .locate(steve_node_api::HistoryRef {
                harness: harness.into(),
                key: format!("session:{id}"),
            })
            .await;
        if let Ok(l) = at {
            found = transcript(
                &l.display,
                l.updated.map(|t| t.0),
                l.recorded_id,
                l.recorded_cwd,
                id,
                cwd,
            );
        }
    }
    page(
        harness,
        cwd,
        h,
        id.as_deref(),
        found,
        None,
        props.local_transcript,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use steve_node::history::claude_project;

    fn obj(v: Value) -> Obj {
        v.as_object().cloned().unwrap()
    }

    fn homes(root: &Path) -> Homes {
        Homes {
            home: root.into(),
            claude_dir: root.join(".claude"),
            codex_home: root.join(".codex"),
            declared: BTreeMap::new(),
        }
    }

    #[test]
    fn the_acp_id_or_the_imported_file_name() {
        assert_eq!(
            native_id(&obj(json!({ "acp_session_id": "abc", "import_key": null }))),
            Some("abc".into())
        );
        assert_eq!(
            native_id(&obj(
                json!({ "acp_session_id": null, "import_key": "cc:/h/.claude/projects/-x/3b2c458f-84be-4589-a22c-5bfd2579c313.jsonl" })
            )),
            Some("3b2c458f-84be-4589-a22c-5bfd2579c313".into())
        );
        assert_eq!(
            native_id(&obj(
                json!({ "acp_session_id": null, "import_key": "cx:/h/.codex/sessions/2026/11/04/rollout-2026-11-04T15-36-56-01a1a57d-52a0-7111-9c1d-0f8bf59e7c96.jsonl" })
            )),
            Some("01a1a57d-52a0-7111-9c1d-0f8bf59e7c96".into())
        );
        assert_eq!(
            native_id(&obj(json!({ "acp_session_id": "", "import_key": null }))),
            None
        );
    }

    #[test]
    fn commands_per_harness() {
        let h = homes(Path::new("/h"));
        assert_eq!(
            command(&h, "claude-code", "id-1", "/w/ST-1").as_deref(),
            Some("cd /w/ST-1 && claude --resume id-1")
        );
        assert_eq!(
            command(&h, "codex", "id-2", "/w/my dir").as_deref(),
            Some("cd '/w/my dir' && codex resume id-2")
        );
        assert_eq!(
            command(&h, "gemini", "id", "/w").as_deref(),
            Some("cd /w && gemini --resume id")
        );
        assert_eq!(
            command(&h, "goose", "id", "/w").as_deref(),
            Some("cd /w && goose session --resume --session-id id")
        );
        assert_eq!(command(&h, "unknown-harness", "id", "/w"), None);
        let other = Homes {
            codex_home: "/srv/codex's".into(),
            ..h.clone()
        };
        assert_eq!(
            command(&other, "codex", "id", "/w").as_deref(),
            Some(r"cd /w && CODEX_HOME='/srv/codex'\''s' codex resume id")
        );
        // option injection, tilde expansion, dash-prefixed cwd, and principal isolation env
        assert_eq!(
            command(&h, "claude-code", "--dangerously-skip-permissions", "/w"),
            None
        );
        assert_eq!(command(&h, "claude-code", "bad\nid", "/w"), None);
        assert_eq!(
            command(&h, "claude-code", "~/id", "-badcwd").as_deref(),
            Some("cd -- '-badcwd' && claude --resume '~/id'")
        );
        assert_eq!(
            command(&h, "claude-code", "FOO=bar", "+cwd").as_deref(),
            Some("cd -- '+cwd' && claude --resume 'FOO=bar'")
        );
        let iso = info_with_state(
            &h,
            &obj(json!({ "harness": "opencode", "cwd": "/w", "acp_session_id": "s-1" })),
            Some(Path::new("/h/data/node/state/p1/harness")),
        );
        let cmd = iso["command"].as_str().unwrap();
        assert!(
            cmd.contains("XDG_DATA_HOME=/h/data/node/state/p1/harness/xdg/data"),
            "{cmd}"
        );
        assert!(cmd.ends_with("opencode -s s-1"), "{cmd}");
        assert_eq!(iso["transcript_checked"], false);
        assert_eq!(
            iso["fork_command"],
            "cd /w && XDG_DATA_HOME=/h/data/node/state/p1/harness/xdg/data XDG_CONFIG_HOME=/h/data/node/state/p1/harness/xdg/config XDG_STATE_HOME=/h/data/node/state/p1/harness/xdg/state XDG_CACHE_HOME=/h/data/node/state/p1/harness/xdg/cache opencode -s s-1 --fork"
        );
    }

    #[test]
    fn finds_and_checks_the_transcript() {
        let root = std::env::temp_dir().join(format!("steve-resume-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let h = homes(&root);
        let cwd = root.join("w/ST-1");
        fs::create_dir_all(&cwd).unwrap();
        let cwd = cwd.to_string_lossy().into_owned();
        let pdir = h.claude_dir.join("projects").join(claude_project(&cwd));
        fs::create_dir_all(&pdir).unwrap();
        // a pasted screenshot: a line of two megabytes before the first `cwd`
        let image = "A".repeat(2 << 20);
        fs::write(
            pdir.join("c-1.jsonl"),
            format!(
                "{{\"type\":\"queue-operation\",\"sessionId\":\"c-1\"}}\n{{\"type\":\"user\",\"message\":\"{image}\"}}\n{{\"type\":\"user\",\"sessionId\":\"c-1\",\"cwd\":\"{cwd}\"}}\n"
            ),
        )
        .unwrap();
        let day = h.codex_home.join("sessions/2026/11/04");
        fs::create_dir_all(&day).unwrap();
        fs::write(
            day.join("rollout-2026-11-04T10-00-00-x-2.jsonl"),
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"x-2\",\"cwd\":\"/elsewhere\"}}\n",
        )
        .unwrap();

        let cc = info(
            &h,
            &obj(json!({ "harness": "claude-code", "cwd": cwd, "acp_session_id": "c-1" })),
        );
        assert_eq!(cc["id"], "c-1");
        assert_eq!(cc["cwd_exists"], true);
        assert_eq!(cc["transcript"]["matches"], true);
        assert_eq!(cc["transcript"]["cwd"], cwd.as_str());

        let cx = info(
            &h,
            &obj(json!({ "harness": "codex", "cwd": cwd, "acp_session_id": "x-2" })),
        );
        assert_eq!(cx["transcript"]["id"], "x-2");
        assert_eq!(
            cx["transcript"]["matches"], false,
            "recorded in another directory"
        );

        let missing = info(
            &h,
            &obj(json!({ "harness": "claude-code", "cwd": cwd, "acp_session_id": "gone" })),
        );
        assert_eq!(missing["transcript"], Value::Null);
        assert!(
            missing["command"]
                .as_str()
                .unwrap()
                .ends_with("claude --resume gone")
        );

        let none = info(
            &h,
            &obj(json!({ "harness": "codex", "cwd": "/nope", "acp_session_id": null })),
        );
        assert_eq!(none["id"], Value::Null);
        assert_eq!(none["command"], Value::Null);
        assert_eq!(none["cwd_exists"], false);
        let _ = fs::remove_dir_all(&root);
    }
}
