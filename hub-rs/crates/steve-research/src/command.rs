//! Layer 1 of read-only enforcement: the harness command line and the files it reads. Only Read/Grep/Glob
//! exist (`--tools`), writes, shells and the web are denied (`--disallowedTools` plus `permissions.deny`,
//! which win over any user allow rule), project settings and hooks are not loaded, nothing outside the
//! readable directories is pre-approved (`dontAsk` denies it), and MCP is only Steve's server.

use crate::guard::{DENIED_TOOLS, MCP_TOOLS, READ_TOOLS, Scope};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// A process to start: program, arguments, working directory and extra environment.
#[derive(Debug, Clone, PartialEq)]
pub struct Invocation {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
}

/// Everything the command line depends on, so it can be snapshot-tested.
#[derive(Debug, Clone)]
pub struct CommandPlan<'a> {
    pub executable: &'a Path,
    pub prompt: &'a str,
    /// The prompt (with images) goes to stdin as one stream-json user message instead of `-p <prompt>`.
    pub stdin_input: bool,
    pub model: &'a str,
    /// `--max-turns`, only when a limit is set.
    pub max_turns: Option<u32>,
    pub cwd: &'a Path,
    pub add_dirs: &'a [PathBuf],
    pub run_dir: &'a Path,
    pub with_mcp: bool,
    pub resume: Option<&'a str>,
    /// The sandbox profile file when the run is sandboxed.
    pub sandbox_profile: Option<&'a Path>,
    /// `--effort`, when the settings ask for one.
    pub effort: Option<&'a str>,
}

/// `settings.json` of a run: `permissions.deny` for secrets and every writing tool, hooks off.
pub fn settings(scope: &Scope) -> Value {
    let mut deny: Vec<String> = Vec::new();
    for d in crate::guard::SENSITIVE_HOME_DIRS {
        deny.push(format!("Read(~/{d}/**)"));
    }
    // `//` makes the pattern absolute; this also covers a home that isn't $HOME (tests) and symlinks.
    let lexical_home = scope.home.display().to_string();
    for d in crate::guard::SENSITIVE_HOME_DIRS {
        deny.push(format!(
            "Read(/{}/{d}/**)",
            lexical_home.trim_end_matches('/')
        ));
    }
    for d in scope.sensitive_dirs() {
        let r = format!("Read(/{}/**)", d.display());
        if !deny.contains(&r) {
            deny.push(r);
        }
    }
    deny.push("Read(**/.env*)".into());
    deny.extend(DENIED_TOOLS.iter().map(|t| t.to_string()));
    json!({
        "permissions": { "deny": deny, "defaultMode": "dontAsk" },
        "disableAllHooks": true,
    })
}

/// `mcp.json`: only Steve's `/mcp`, with the run's own bearer token.
pub fn mcp_config(url: &str, token: &str) -> Value {
    json!({"mcpServers": {"steve": {
        "type": "http",
        "url": url,
        "headers": {"Authorization": format!("Bearer {token}")},
    }}})
}

/// The harness invocation of one attempt.
pub fn invocation(p: &CommandPlan<'_>) -> Invocation {
    let mut args: Vec<String> = vec!["-p".into()];
    if p.stdin_input {
        args.extend(["--input-format".into(), "stream-json".into()]);
    } else {
        args.push(p.prompt.into());
    }
    args.extend([
        "--output-format".into(),
        "stream-json".into(),
        "--verbose".into(),
        "--include-partial-messages".into(),
        "--model".into(),
        p.model.into(),
    ]);
    if let Some(n) = p.max_turns {
        args.extend(["--max-turns".into(), n.to_string()]);
    }
    args.extend([
        // user settings carry the gateway login; project/local ones (an untrusted repo's hooks or allow
        // rules) are not loaded.
        "--setting-sources".into(),
        "user".into(),
        "--permission-mode".into(),
        "dontAsk".into(),
        "--permission-prompts".into(),
        "none".into(),
        format!("--tools={}", READ_TOOLS.join(",")),
        format!("--disallowedTools={}", DENIED_TOOLS.join(",")),
    ]);
    if p.with_mcp {
        args.push(format!("--allowedTools={}", MCP_TOOLS.join(",")));
    }
    args.push(format!(
        "--settings={}",
        p.run_dir.join("settings.json").display()
    ));
    args.push("--strict-mcp-config".into());
    if p.with_mcp {
        args.push(format!(
            "--mcp-config={}",
            p.run_dir.join("mcp.json").display()
        ));
    }
    if let Some(e) = p.effort {
        args.extend(["--effort".into(), e.into()]);
    }
    args.push("--disable-slash-commands".into());
    for d in p.add_dirs {
        args.push(format!("--add-dir={}", d.display()));
    }
    if let Some(s) = p.resume {
        args.push("--resume".into());
        args.push(s.into());
    }
    let mut env = Vec::new();
    let (program, args) = match p.sandbox_profile {
        Some(prof) => {
            // the harness's temp files go to the run directory, the only writable place besides ~/.claude.
            env.push((
                "TMPDIR".to_string(),
                p.run_dir.join("tmp").display().to_string(),
            ));
            let mut a = vec![
                "-f".to_string(),
                prof.display().to_string(),
                p.executable.display().to_string(),
            ];
            a.extend(args);
            (PathBuf::from(crate::sandbox::SANDBOX_EXEC), a)
        }
        None => (p.executable.to_path_buf(), args),
    };
    Invocation {
        program,
        args,
        cwd: p.cwd.to_path_buf(),
        env,
    }
}

/// Codex features a research run never uses (hooks, plugins, sub-agents, the browser and computer
/// use, image generation, goals, apps). `unified_exec` is off so every shell command arrives as a
/// `command_execution` event the guard can see.
pub const CODEX_OFF: [&str; 9] = [
    "hooks",
    "plugins",
    "multi_agent",
    "computer_use",
    "browser_use",
    "image_generation",
    "goals",
    "apps",
    "unified_exec",
];

/// The env var Codex reads the run's MCP bearer token from (never on the command line).
pub const CODEX_MCP_TOKEN_VAR: &str = "STEVE_RESEARCH_MCP_TOKEN";

/// Everything a Codex research command line depends on.
#[derive(Debug, Clone)]
pub struct CodexPlan<'a> {
    pub executable: &'a Path,
    pub prompt: &'a str,
    /// Empty: Codex's configured model (`-m` left out).
    pub model: &'a str,
    pub cwd: &'a Path,
    pub run_dir: &'a Path,
    /// Steve's `/mcp` and the run's token.
    pub mcp: Option<(&'a str, &'a str)>,
    pub effort: Option<&'a str>,
    /// Images of the question, given to the model as image input (`-i`).
    pub images: &'a [PathBuf],
    /// No way to read files ([`CODEX_NO_FILES`] off as well): someone besides the owner reads the answer.
    pub no_files: bool,
}

/// What reads files in Codex besides the shell `unified_exec` already turns off: the shell tool itself and
/// the image viewer (it opens any path). Off when someone besides the owner reads the answer.
pub const CODEX_NO_FILES: [&str; 2] = ["shell_tool", "view_image"];

/// `codex exec` for one research attempt. Layer 1 is Codex's own read-only sandbox: commands cannot
/// write anywhere or reach the network. No web search, no MCP server but Steve's, nothing persisted.
/// There is no layer 3 (`sandbox-exec` around Codex): Codex sandboxes its commands with Seatbelt itself,
/// and Seatbelt cannot be nested.
pub fn codex_invocation(p: &CodexPlan<'_>) -> Invocation {
    let mut args: Vec<String> = [
        "exec",
        "--json",
        "--ephemeral",
        "--skip-git-repo-check",
        "-s",
        "read-only",
        "-C",
    ]
    .map(String::from)
    .into();
    args.push(p.cwd.display().to_string());
    if !p.model.is_empty() {
        args.extend(["-m".into(), p.model.into()]);
    }
    args.extend([
        "-c".into(),
        "web_search=\"disabled\"".into(),
        "-c".into(),
        "mcp_servers={}".into(),
    ]);
    for f in CODEX_OFF {
        args.extend(["--disable".into(), f.into()]);
    }
    if p.no_files {
        for f in CODEX_NO_FILES {
            args.extend(["--disable".into(), f.into()]);
        }
    }
    let mut env = vec![(
        "TMPDIR".to_string(),
        p.run_dir.join("tmp").display().to_string(),
    )];
    if let Some((url, token)) = p.mcp {
        args.extend([
            "-c".into(),
            format!(
                "mcp_servers.steve.url={}",
                serde_json::to_string(url).unwrap_or_default()
            ),
            "-c".into(),
            format!("mcp_servers.steve.bearer_token_env_var=\"{CODEX_MCP_TOKEN_VAR}\""),
            "-c".into(),
            format!(
                "mcp_servers.steve.enabled_tools={}",
                serde_json::to_string(
                    &MCP_TOOLS
                        .iter()
                        .filter_map(|t| t.strip_prefix("mcp__steve__"))
                        .collect::<Vec<_>>()
                )
                .unwrap_or_default()
            ),
        ]);
        env.push((CODEX_MCP_TOKEN_VAR.into(), token.into()));
    }
    if let Some(e) = p.effort {
        args.extend(["-c".into(), format!("model_reasoning_effort=\"{e}\"")]);
    }
    for i in p.images {
        args.extend(["-i".into(), i.display().to_string()]);
    }
    args.push("--".into());
    args.push(p.prompt.into());
    Invocation {
        program: p.executable.to_path_buf(),
        args,
        cwd: p.cwd.to_path_buf(),
        env,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_command_line() {
        let inv = codex_invocation(&CodexPlan {
            executable: Path::new("/bin/codex"),
            prompt: "Q",
            model: "gpt-6-luna",
            cwd: Path::new("/w/proj"),
            run_dir: Path::new("/s/research/tr_1"),
            mcp: Some(("http://127.0.0.1:1/mcp", "sekrit-123")),
            effort: Some("low"),
            images: &[PathBuf::from("/f/a.png")],
            no_files: false,
        });
        assert_eq!(inv.program, PathBuf::from("/bin/codex"));
        let a = inv.args.join(" ");
        assert!(a.starts_with("exec --json --ephemeral --skip-git-repo-check -s read-only -C /w/proj -m gpt-6-luna -c web_search=\"disabled\" -c mcp_servers={} --disable hooks"), "{a}");
        assert!(a.contains("--disable unified_exec"));
        assert!(a.contains("-c mcp_servers.steve.url=\"http://127.0.0.1:1/mcp\""));
        assert!(a.contains("mcp_servers.steve.enabled_tools=[\"steve_knowledge_search\",\"steve_knowledge_read\",\"steve_items_search\",\"steve_item_read\",\"steve_sessions_list\"]"));
        assert!(
            a.ends_with("-c model_reasoning_effort=\"low\" -i /f/a.png -- Q"),
            "{a}"
        );
        // the token goes through the environment only
        assert!(!a.contains("sekrit-123"));
        assert!(
            inv.env
                .contains(&(CODEX_MCP_TOKEN_VAR.into(), "sekrit-123".into()))
        );
        let bare = codex_invocation(&CodexPlan {
            executable: Path::new("/bin/codex"),
            prompt: "Q",
            model: "",
            cwd: Path::new("/w/proj"),
            run_dir: Path::new("/s/r"),
            mcp: None,
            effort: None,
            images: &[],
            no_files: false,
        });
        assert!(
            !bare
                .args
                .iter()
                .any(|x| x == "-m" || x.contains("mcp_servers.steve"))
        );
        // the owner reads it: Codex keeps its shell (the guard checks each command)
        assert!(!a.contains("--disable shell_tool"));
    }

    /// #36: someone besides the owner reads the answer: no shell and no image viewer, so nothing reads a
    /// file; Steve's tools stay.
    #[test]
    fn a_shared_codex_run_cannot_read_files() {
        let inv = codex_invocation(&CodexPlan {
            executable: Path::new("/bin/codex"),
            prompt: "Q",
            model: "",
            cwd: Path::new("/s/r/empty"),
            run_dir: Path::new("/s/r"),
            mcp: Some(("http://127.0.0.1:1/mcp", "t")),
            effort: None,
            images: &[],
            no_files: true,
        });
        let a = inv.args.join(" ");
        assert!(
            a.contains("--disable shell_tool --disable view_image"),
            "{a}"
        );
        assert!(a.contains("--disable unified_exec"), "{a}");
        assert!(a.contains("-C /s/r/empty"), "{a}");
        assert!(a.contains("mcp_servers.steve.url"), "{a}");
    }

    #[test]
    fn command_line_snapshot() {
        let dirs = vec![PathBuf::from("/w/docs")];
        let plan = CommandPlan {
            executable: Path::new("/bin/claude"),
            prompt: "Q",
            stdin_input: false,
            model: "claude-sonnet-5-5",
            max_turns: Some(24),
            cwd: Path::new("/w/proj"),
            add_dirs: &dirs,
            run_dir: Path::new("/s/research/tr_1"),
            with_mcp: true,
            resume: Some("sess-1"),
            sandbox_profile: Some(Path::new("/s/research/tr_1/sandbox.sb")),
            effort: None,
        };
        let inv = invocation(&plan);
        assert_eq!(inv.program, PathBuf::from("/usr/bin/sandbox-exec"));
        assert_eq!(inv.cwd, PathBuf::from("/w/proj"));
        assert_eq!(
            inv.env,
            vec![("TMPDIR".into(), "/s/research/tr_1/tmp".into())]
        );
        let want = [
            "-f",
            "/s/research/tr_1/sandbox.sb",
            "/bin/claude",
            "-p",
            "Q",
            "--output-format",
            "stream-json",
            "--verbose",
            "--include-partial-messages",
            "--model",
            "claude-sonnet-5-5",
            "--max-turns",
            "24",
            "--setting-sources",
            "user",
            "--permission-mode",
            "dontAsk",
            "--permission-prompts",
            "none",
            "--tools=Read,Grep,Glob",
            "--disallowedTools=Bash,Edit,Write,MultiEdit,NotebookEdit,WebFetch,WebSearch,Task,Agent,KillShell,TodoWrite,PowerShell",
            "--allowedTools=mcp__steve__steve_knowledge_search,mcp__steve__steve_knowledge_read,mcp__steve__steve_items_search,mcp__steve__steve_item_read,mcp__steve__steve_sessions_list",
            "--settings=/s/research/tr_1/settings.json",
            "--strict-mcp-config",
            "--mcp-config=/s/research/tr_1/mcp.json",
            "--disable-slash-commands",
            "--add-dir=/w/docs",
            "--resume",
            "sess-1",
        ];
        assert_eq!(inv.args, want);

        let bare = invocation(&CommandPlan {
            with_mcp: false,
            resume: None,
            sandbox_profile: None,
            add_dirs: &[],
            ..plan
        });
        assert_eq!(bare.program, PathBuf::from("/bin/claude"));
        assert!(bare.args.iter().any(|a| a == "--strict-mcp-config"));
        assert!(
            !bare
                .args
                .iter()
                .any(|a| a.starts_with("--mcp-config") || a.starts_with("--allowedTools"))
        );
        assert!(bare.env.is_empty());
        assert!(!bare.args.iter().any(|a| a == "--effort"));
        // no turn limit set: no --max-turns at all
        let open = invocation(&CommandPlan {
            max_turns: None,
            ..plan
        });
        assert!(!open.args.iter().any(|a| a == "--max-turns"));

        let piped = invocation(&CommandPlan {
            stdin_input: true,
            ..plan
        });
        assert_eq!(
            &piped.args[3..7],
            ["-p", "--input-format", "stream-json", "--output-format"]
        );
        assert!(
            !piped.args.iter().any(|a| a == "Q"),
            "the prompt goes to stdin"
        );

        let deep = invocation(&CommandPlan {
            effort: Some("high"),
            ..plan
        });
        let at = deep.args.iter().position(|a| a == "--effort").unwrap();
        assert_eq!(deep.args[at + 1], "high");
    }

    #[test]
    fn settings_deny_secrets_and_writes() {
        let t = tempfile::tempdir().unwrap();
        let s = Scope::new(t.path(), &[], Path::new("/Users/user"));
        let v = settings(&s);
        let deny: Vec<&str> = v["permissions"]["deny"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|x| x.as_str())
            .collect();
        for r in [
            "Read(~/.ssh/**)",
            "Read(~/.steve/**)",
            "Read(~/.aws/**)",
            "Read(~/.config/gh/**)",
            "Read(~/Library/Keychains/**)",
            "Read(//Users/user/.ssh/**)",
            "Read(**/.env*)",
            "Bash",
            "Edit",
            "Write",
            "WebFetch",
            "WebSearch",
        ] {
            assert!(deny.contains(&r), "missing {r}: {deny:?}");
        }
        assert_eq!(v["permissions"]["defaultMode"], "dontAsk");
        assert_eq!(v["disableAllHooks"], true);
    }
}
