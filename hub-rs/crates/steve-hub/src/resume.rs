//! The harness's own session behind a Steve session, so the user can pick it up in the harness itself
//! (ST-28). The ACP session id Steve keeps is the harness's id. An imported session has no ACP id;
//! its import key names the transcript file, whose name carries the id.
//!
//! Filesystem checks (`cwd_exists`, state-directory env overrides, and transcript lookup) are
//! executed exclusively by the Node via [`steve_node_api::Discovery::resume_probe`]. The Hub renders
//! the resume/fork CLI commands from declared harness metadata and the Node's probe response without
//! reading project or harness directories itself.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::wire::store::Obj;

/// Harness manifest declarations needed to render CLI resume/fork commands.
#[derive(Debug, Clone, Default)]
pub struct Homes {
    /// Manifest declarations (`harness.json`), so custom or overridden `resume_cli` / `fork_cli` /
    /// `state` work on the resume page too.
    pub declared: BTreeMap<String, steve_harness_spec::Declaration>,
}

impl Homes {
    pub fn of(cfg: &crate::config::Config) -> Homes {
        Homes {
            declared: cfg.declared_adapters.clone(),
        }
    }
}

fn text<'a>(s: &'a Obj, k: &str) -> Option<&'a str> {
    s.get(k).and_then(Value::as_str).filter(|v| !v.is_empty())
}

use steve_store::valid_id;

/// The harness's session id from a stored row, before the import key is dropped.
pub fn native_id(s: &Obj) -> Option<String> {
    steve_store::native_id(text(s, "acp_session_id"), text(s, "import_key"))
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
    template: Option<&str>,
    id: &str,
    cwd: &str,
    state_env: &[(String, String)],
) -> Option<String> {
    if !valid_id(id) {
        return None;
    }
    let template = template?;
    let run = template.replace("{id}", &sh(id));
    let env: String = state_env
        .iter()
        .map(|(k, v)| format!("{k}={} ", sh(v)))
        .collect();
    let cd = if cwd.is_empty() {
        String::new()
    } else if cwd.starts_with(['-', '+']) {
        format!("cd -- {} && ", sh(cwd))
    } else {
        format!("cd {} && ", sh(cwd))
    };
    Some(format!("{cd}{env}{run}"))
}

/// `cd <cwd> && <harness command>` with default harness state paths.
pub fn command(h: &Homes, harness: &str, id: &str, cwd: &str) -> Option<String> {
    let props = crate::harnesses::adapter_spec(harness, h.declared.get(harness)).properties;
    render_cli(props.resume_cli.as_deref(), id, cwd, &[])
}

/// The transcript part of the resume response: where it is, when it changed, and what it records.
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

#[allow(clippy::too_many_arguments)]
fn page(
    harness: &str,
    cwd: &str,
    h: &Homes,
    id: Option<&str>,
    cwd_exists: bool,
    state_env: &[(String, String)],
    transcript: Value,
    transcript_checked: bool,
) -> Value {
    let props = crate::harnesses::adapter_spec(harness, h.declared.get(harness)).properties;
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
        "command": render_cli(props.resume_cli.as_deref(), id, cwd, state_env),
        "fork_command": render_cli(props.fork_cli.as_deref(), id, cwd, state_env),
        "transcript": transcript,
        "transcript_checked": transcript_checked,
    })
}

/// Resume info resolved via `node` (`Discovery::resume_probe`), reading no harness or project
/// files from the Hub process.
pub async fn info_on(
    h: &Homes,
    s: &Obj,
    node: Option<&dyn steve_node_api::Discovery>,
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
    node: Option<&dyn steve_node_api::Discovery>,
    state: &SessionState,
) -> Value {
    let harness = text(s, "harness").unwrap_or_default();
    let cwd = text(s, "cwd").unwrap_or_default();
    let id = native_id(s);
    let props = crate::harnesses::adapter_spec(harness, h.declared.get(harness)).properties;
    let (pdir, check, hide_env, probe_node) = match state {
        SessionState::AmbientLocal => (None, props.local_transcript, false, node),
        SessionState::IsolatedLocal {
            harness_dir,
            expose_path: true,
        } => (
            Some(harness_dir.to_string_lossy().into_owned()),
            props.local_transcript,
            false,
            node,
        ),
        SessionState::IsolatedLocal {
            expose_path: false, ..
        } => (None, false, true, node),
        SessionState::RemoteNode => (None, false, true, None),
    };

    let (cwd_exists, state_env, found, transcript_checked) = if let Some(node) = probe_node {
        match node
            .resume_probe(steve_node_api::ResumeProbeRequest {
                harness: harness.to_string(),
                session_id: id.clone(),
                cwd: cwd.to_string(),
                principal_state_dir: pdir,
                check_transcript: check,
            })
            .await
        {
            Ok(resp) => {
                let env = if hide_env { Vec::new() } else { resp.state_env };
                let found = if check {
                    match (&id, resp.transcript) {
                        (Some(id), Some(l)) => transcript(
                            &l.display,
                            l.updated.map(|t| t.0),
                            l.recorded_id,
                            l.recorded_cwd,
                            id,
                            cwd,
                        ),
                        _ => Value::Null,
                    }
                } else {
                    Value::Null
                };
                (resp.cwd_exists, env, found, check)
            }
            Err(_) => (false, Vec::new(), Value::Null, false),
        }
    } else {
        (false, Vec::new(), Value::Null, false)
    };

    page(
        harness,
        cwd,
        h,
        id.as_deref(),
        cwd_exists,
        &state_env,
        found,
        transcript_checked,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::fs;
    use steve_acp::AcpConfig;
    use steve_acp::adapter::claude_code::claude_project;
    use steve_node::{LocalNode, NodeConfig, PolicyCeiling};
    use steve_node_api::NodeId;

    fn obj(v: Value) -> Obj {
        v.as_object().cloned().unwrap()
    }

    fn test_node(root: &Path, codex_home: PathBuf) -> LocalNode {
        let node = LocalNode::open(NodeConfig {
            ceiling: PolicyCeiling::unlimited(),
            node: NodeId::new("local").unwrap(),
            home: root.join("node"),
            acp: AcpConfig {
                declared: HashMap::new(),
                adapters: HashMap::new(),
                node: "node".into(),
                claude_executable: "claude".into(),
                claude_config_dir: Some(root.join(".claude")),
                steve_home: root.join("steve"),
                codex_home: codex_home.clone(),
                user_home: root.to_path_buf(),
                version: "test".into(),
            },
            roots: vec![],
            capacity: steve_node_api::Capacity {
                sessions: steve_node_api::Capacity::SESSIONS_DEFAULT,
                jobs: 0,
                execs: 0,
            },
            storage: Default::default(),
        })
        .unwrap();
        node.set_discovery(steve_node::history::DiscoveryConfig {
            homes: steve_node::history::Homes {
                user_home: root.to_path_buf(),
                steve_home: root.join("steve"),
                state_dirs: BTreeMap::from([
                    ("claude-code".to_string(), root.join(".claude")),
                    ("codex".to_string(), codex_home),
                ]),
            },
            binaries: Default::default(),
        });
        node
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

    #[tokio::test]
    async fn commands_per_harness() {
        let h = Homes::default();
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
        assert_eq!(
            command(&h, "antigravity", "id", "/w").as_deref(),
            Some("cd /w && agy --resume id")
        );
        assert_eq!(command(&h, "unknown-harness", "id", "/w"), None);
        // option injection, tilde expansion, dash-prefixed cwd
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

        // Non-default state_dir env and isolated principal state via Node resume_probe
        let tmp = tempfile::tempdir().unwrap();
        let node = test_node(tmp.path(), PathBuf::from("/srv/codex's"));
        let cx = info_on(
            &h,
            &obj(json!({ "harness": "codex", "cwd": "/w", "acp_session_id": "id" })),
            Some(&node),
            None,
        )
        .await;
        assert_eq!(
            cx["command"],
            r"cd /w && CODEX_HOME='/srv/codex'\''s' codex resume id"
        );

        let iso = info_on(
            &h,
            &obj(json!({ "harness": "opencode", "cwd": "/w", "acp_session_id": "s-1" })),
            Some(&node),
            Some(Path::new("/h/data/node/state/p1/harness")),
        )
        .await;
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

    #[tokio::test]
    async fn finds_and_checks_the_transcript() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let h = Homes::default();
        let cwd = root.join("w/ST-1");
        fs::create_dir_all(&cwd).unwrap();
        let cwd = cwd.to_string_lossy().into_owned();
        let pdir = root.join(".claude/projects").join(claude_project(&cwd));
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
        let day = root.join(".codex/sessions/2026/11/04");
        fs::create_dir_all(&day).unwrap();
        fs::write(
            day.join("rollout-2026-11-04T10-00-00-x-2.jsonl"),
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"x-2\",\"cwd\":\"/elsewhere\"}}\n",
        )
        .unwrap();
        let node = test_node(root, root.join(".codex"));

        let cc = info_on(
            &h,
            &obj(json!({ "harness": "claude-code", "cwd": cwd, "acp_session_id": "c-1" })),
            Some(&node),
            None,
        )
        .await;
        assert_eq!(cc["id"], "c-1");
        assert_eq!(cc["cwd_exists"], true);
        assert_eq!(cc["transcript"]["matches"], true);
        assert_eq!(cc["transcript"]["cwd"], cwd.as_str());

        let cx = info_on(
            &h,
            &obj(json!({ "harness": "codex", "cwd": cwd, "acp_session_id": "x-2" })),
            Some(&node),
            None,
        )
        .await;
        assert_eq!(cx["transcript"]["id"], "x-2");
        assert_eq!(
            cx["transcript"]["matches"], false,
            "recorded in another directory"
        );

        let missing = info_on(
            &h,
            &obj(json!({ "harness": "claude-code", "cwd": cwd, "acp_session_id": "gone" })),
            Some(&node),
            None,
        )
        .await;
        assert_eq!(missing["transcript"], Value::Null);
        assert!(
            missing["command"]
                .as_str()
                .unwrap()
                .ends_with("claude --resume gone")
        );

        let none = info_on(
            &h,
            &obj(json!({ "harness": "codex", "cwd": "/nope", "acp_session_id": null })),
            Some(&node),
            None,
        )
        .await;
        assert_eq!(none["id"], Value::Null);
        assert_eq!(none["command"], Value::Null);
        assert_eq!(none["cwd_exists"], false);
    }
}
