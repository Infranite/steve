//! The harness's own session behind a Steve session, so the user can pick it up in the harness itself
//! (ST-28). The ACP session id Steve keeps is the harness's id: claude-agent-acp hands its id to Claude
//! Code as `--session-id` (`~/.claude/projects/<cwd>/<id>.jsonl`), codex-acp answers with the Codex
//! thread id (`$CODEX_HOME/sessions/…/rollout-<time>-<id>.jsonl`). An imported session has no ACP id;
//! its import key names the transcript file, whose name carries the id.
//!
//! `info` also reads the transcript the id points to, so the page says whether it is really there and
//! belongs to the same directory. Only ids and `cwd` fields are parsed; nothing else is kept or returned.

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde_json::{Value, json};

use crate::backend::Obj;

/// Where the harnesses keep their sessions: what the Hub's own environment gives its agents.
#[derive(Debug, Clone)]
pub struct Homes {
    pub home: PathBuf,
    /// `$CLAUDE_CONFIG_DIR`, default `~/.claude`.
    pub claude_dir: PathBuf,
    /// `$CODEX_HOME`, default `~/.codex`.
    pub codex_home: PathBuf,
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

/// The harness's session id of a session row (before `import_key` is dropped from the output).
pub fn native_id(s: &Obj) -> Option<String> {
    if let Some(id) = text(s, "acp_session_id") {
        return Some(id.to_owned());
    }
    let (kind, file) = text(s, "import_key")?.split_once(':')?;
    id_of_file(kind, file)
}

/// A shell word: as is when it is plain, otherwise in single quotes.
fn sh(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "_-./=:@%+,".contains(c))
    {
        s.to_owned()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

/// `cd <cwd> && [VAR=dir] <harness command>`; the variable only when the Hub's agents use another one.
pub fn command(h: &Homes, harness: &str, id: &str, cwd: &str) -> Option<String> {
    let (var, dir, default, run) = match harness {
        "claude-code" => (
            "CLAUDE_CONFIG_DIR",
            &h.claude_dir,
            h.home.join(".claude"),
            format!("claude --resume {}", sh(id)),
        ),
        "codex" => (
            "CODEX_HOME",
            &h.codex_home,
            h.home.join(".codex"),
            format!("codex resume {}", sh(id)),
        ),
        _ => return None,
    };
    let env = if *dir == default {
        String::new()
    } else {
        format!("{var}={} ", sh(&dir.to_string_lossy()))
    };
    let cd = if cwd.is_empty() {
        String::new()
    } else {
        format!("cd {} && ", sh(cwd))
    };
    Some(format!("{cd}{env}{run}"))
}

/// Claude Code's project directory name for a working directory.
fn claude_project(cwd: &str) -> String {
    cwd.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

fn claude_transcript(dir: &Path, id: &str, cwd: &str) -> Option<PathBuf> {
    let projects = dir.join("projects");
    let name = format!("{id}.jsonl");
    let direct = projects.join(claude_project(cwd)).join(&name);
    if direct.is_file() {
        return Some(direct);
    }
    // a long path gets a shortened directory name: look in every project
    std::fs::read_dir(&projects)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path().join(&name))
        .find(|p| p.is_file())
}

/// The `sessionId`/`id` and `cwd` the transcript itself records, from its first lines.
fn recorded(file: &Path, harness: &str) -> (Option<String>, Option<String>) {
    use std::io::{BufRead, BufReader, Read};
    let Ok(f) = std::fs::File::open(file) else {
        return (None, None);
    };
    let (mut id, mut cwd) = (None, None);
    let field = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
    // the first 8 MB (a pasted screenshot makes a line of a megabyte or more); a line cut there does not parse
    for line in BufReader::new(f.take(8 << 20)).lines().take(50) {
        let Ok(line) = line else { break };
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if harness == "codex" {
            // the first line is `session_meta`
            if v.get("type").and_then(Value::as_str) == Some("session_meta")
                && let Some(p) = v.get("payload")
            {
                return (field(p, "id"), field(p, "cwd"));
            }
            continue;
        }
        id = id.or_else(|| field(&v, "sessionId"));
        cwd = cwd.or_else(|| field(&v, "cwd"));
        if id.is_some() && cwd.is_some() {
            break;
        }
    }
    (id, cwd)
}

/// What the session page shows to resume a session in its harness. Blocking: reads the harness's files.
pub fn info(h: &Homes, s: &Obj) -> Value {
    let harness = text(s, "harness").unwrap_or_default();
    let cwd = text(s, "cwd").unwrap_or_default();
    let id = native_id(s);
    let cwd_exists = !cwd.is_empty() && Path::new(cwd).is_dir();
    let Some(id) = id else {
        return json!({ "harness": harness, "id": null, "cwd": cwd, "cwd_exists": cwd_exists, "command": null, "transcript": null });
    };
    let file = match harness {
        "claude-code" => claude_transcript(&h.claude_dir, &id, cwd),
        "codex" => steve_acp::codex_rollout(&h.codex_home, &id),
        _ => None,
    };
    let transcript = file.map(|f| {
        let (rid, rcwd) = recorded(&f, harness);
        let updated = std::fs::metadata(&f)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64);
        let same = rid.as_deref().is_none_or(|r| r == id)
            && rcwd.as_deref().is_none_or(|c| cwd.is_empty() || c == cwd);
        json!({ "path": f.to_string_lossy(), "updated": updated, "id": rid, "cwd": rcwd, "matches": same })
    });
    json!({
        "harness": harness,
        "id": id,
        "cwd": cwd,
        "cwd_exists": cwd_exists,
        "command": command(h, harness, &id, cwd),
        "transcript": transcript,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn obj(v: Value) -> Obj {
        v.as_object().cloned().unwrap()
    }

    fn homes(root: &Path) -> Homes {
        Homes {
            home: root.into(),
            claude_dir: root.join(".claude"),
            codex_home: root.join(".codex"),
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
        assert_eq!(command(&h, "gemini", "id", "/w"), None);
        let other = Homes {
            codex_home: "/srv/codex's".into(),
            ..h
        };
        assert_eq!(
            command(&other, "codex", "id", "/w").as_deref(),
            Some(r"cd /w && CODEX_HOME='/srv/codex'\''s' codex resume id")
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
