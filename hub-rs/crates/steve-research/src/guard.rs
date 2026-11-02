//! Layer 2 of read-only enforcement: the Hub looks at every tool call the harness reports and stops the
//! run at the first one that is not a read inside the run's directories. Pure functions, so the rules are
//! table-tested without a harness.

use serde_json::Value;
use std::fmt;
use std::path::{Component, Path, PathBuf};

/// Built-in harness tools a research run may use (`--tools`). All of them only read.
pub const READ_TOOLS: [&str; 3] = ["Read", "Grep", "Glob"];

/// Steve's read-only MCP tools, as the harness names them (`mcp__<server>__<tool>`, server `steve`).
pub const MCP_TOOLS: [&str; 4] = [
    "mcp__steve__steve_knowledge_search",
    "mcp__steve__steve_knowledge_read",
    "mcp__steve__steve_items_search",
    "mcp__steve__steve_item_read",
];

/// Harness-internal tools that neither read files nor act: `ToolSearch` only loads the schemas of
/// deferred (MCP) tools, which the harness may need before it can call the knowledge tools.
pub const INERT_TOOLS: [&str; 1] = ["ToolSearch"];

/// Tools named on `--disallowedTools` and denied in settings, so the harness refuses them even if a user
/// or project setting allows them.
pub const DENIED_TOOLS: [&str; 12] = [
    "Bash",
    "Edit",
    "Write",
    "MultiEdit",
    "NotebookEdit",
    "WebFetch",
    "WebSearch",
    "Task",
    "Agent",
    "KillShell",
    "TodoWrite",
    "PowerShell",
];

/// Directories under the home directory that hold secrets (§4.3). Relative to `Scope::home`.
pub const SENSITIVE_HOME_DIRS: [&str; 5] =
    [".ssh", ".steve", ".aws", ".config/gh", "Library/Keychains"];

/// Where a run may read: its cwd, the extra directories (`--add-dir`) and the run's scratch directory.
/// Paths are canonicalized once here so symlinks and `/tmp` vs `/private/tmp` compare correctly.
#[derive(Debug, Clone)]
pub struct Scope {
    pub cwd: PathBuf,
    pub readable: Vec<PathBuf>,
    pub home: PathBuf,
}

impl Scope {
    /// `cwd` is readable too; `home` is whose secret directories are off limits.
    pub fn new(cwd: &Path, add_dirs: &[PathBuf], home: &Path) -> Self {
        let cwd = resolve(cwd);
        let mut readable = vec![cwd.clone()];
        for d in add_dirs {
            let d = resolve(d);
            if !readable.contains(&d) {
                readable.push(d);
            }
        }
        Scope {
            cwd,
            readable,
            home: resolve(home),
        }
    }

    /// The sensitive directories of this scope's home, canonical.
    pub fn sensitive_dirs(&self) -> Vec<PathBuf> {
        SENSITIVE_HOME_DIRS
            .iter()
            .map(|d| resolve(&self.home.join(d)))
            .collect()
    }
}

/// Why a tool call ended the run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Violation {
    /// A tool that is not on the read-only allowlist (Bash, Write, Edit, WebFetch, …).
    Tool { tool: String },
    /// A path outside the run's readable directories.
    OutOfScope { tool: String, path: String },
    /// A secret: `~/.ssh`, `~/.steve`, `~/.aws`, `~/.config/gh`, `~/Library/Keychains`, `.env*`.
    Sensitive { tool: String, path: String },
    /// A file tool without a usable path argument.
    BadInput { tool: String },
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Violation::Tool { tool } => write!(f, "tool {tool} is not allowed in a research run"),
            Violation::OutOfScope { tool, path } => {
                write!(f, "{tool} outside the readable directories: {path}")
            }
            Violation::Sensitive { tool, path } => write!(f, "{tool} on a sensitive path: {path}"),
            Violation::BadInput { tool } => write!(f, "{tool} without a usable path"),
        }
    }
}

impl Violation {
    /// The tool that broke the rule.
    pub fn tool(&self) -> &str {
        match self {
            Violation::Tool { tool }
            | Violation::OutOfScope { tool, .. }
            | Violation::Sensitive { tool, .. }
            | Violation::BadInput { tool } => tool,
        }
    }
}

/// `research_guard(name, input)` of §4.3: Ok when the call is a read inside `scope`.
pub fn research_guard(name: &str, input: &Value, scope: &Scope) -> Result<(), Violation> {
    let tool = || name.to_string();
    match name {
        "Read" => {
            let p =
                str_arg(input, "file_path").ok_or_else(|| Violation::BadInput { tool: tool() })?;
            check_path(name, p, scope)
        }
        "Grep" => {
            if let Some(p) = str_arg(input, "path") {
                check_path(name, p, scope)?;
            }
            // `glob` narrows which files Grep reads; one aimed at .env files reads secrets.
            if let Some(g) = str_arg(input, "glob")
                && g.split(['/', '{', ',']).any(is_env_name)
            {
                return Err(Violation::Sensitive {
                    tool: tool(),
                    path: g.to_string(),
                });
            }
            Ok(())
        }
        "Glob" => {
            if let Some(p) = str_arg(input, "path") {
                check_path(name, p, scope)?;
            }
            // An absolute or parent-relative pattern reaches outside `path`: check its literal prefix.
            let pat = str_arg(input, "pattern").unwrap_or("");
            if pat.starts_with('/') || pat.starts_with('~') || pat.split('/').any(|c| c == "..") {
                let base = str_arg(input, "path").unwrap_or(".");
                let prefix = literal_prefix(pat);
                let joined = if prefix.starts_with('/') || prefix.starts_with('~') {
                    prefix
                } else {
                    format!("{}/{}", base.trim_end_matches('/'), prefix)
                };
                check_path(name, &joined, scope)?;
            }
            Ok(())
        }
        // Codex: its read-only sandbox stops writes and the network; the paths a command names must
        // still be readable ones and no secret
        "Shell" => {
            let c =
                str_arg(input, "command").ok_or_else(|| Violation::BadInput { tool: tool() })?;
            check_shell(c, scope)
        }
        n if MCP_TOOLS.contains(&n) || INERT_TOOLS.contains(&n) => Ok(()),
        _ => Err(Violation::Tool { tool: tool() }),
    }
}

/// The command a shell wrapper runs (`/bin/zsh -lc 'cat a'` → `cat a`); others as they are.
pub fn shell_inner(cmd: &str) -> String {
    let c = cmd.trim();
    let mut parts = c.splitn(3, ' ');
    let (sh, flag, rest) = (parts.next(), parts.next(), parts.next());
    let is_shell = sh.is_some_and(|s| {
        let b = s.rsplit('/').next().unwrap_or(s);
        matches!(b, "sh" | "bash" | "zsh")
    });
    match (is_shell, flag, rest) {
        (true, Some(f), Some(r)) if f.starts_with('-') && f.contains('c') => {
            let r = r.trim();
            if r.len() >= 2 && r.starts_with('\'') && r.ends_with('\'') {
                r[1..r.len() - 1].replace("'\\''", "'")
            } else if r.len() >= 2 && r.starts_with('"') && r.ends_with('"') {
                r[1..r.len() - 1].replace("\\\"", "\"")
            } else {
                r.to_string()
            }
        }
        _ => c.to_string(),
    }
}

/// The words of a command line: quotes removed, split at blanks and at `; | & < > ( )` and backquotes.
fn shell_words(cmd: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for ch in cmd.chars() {
        match (quote, ch) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => cur.push(c),
            (None, '\'' | '"') => quote = Some(ch),
            (None, c) if c.is_whitespace() || ";|&<>()`".contains(c) => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            (None, c) => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// The path a word names, if it looks like one: with a slash, from home, or a `.env` file. Regular
/// expressions (`^/x`, `a\/b`, `(x|y)`) are not paths.
fn shell_path(word: &str) -> Option<String> {
    let w = match word.split_once('=') {
        Some((k, v)) if k.starts_with('-') => v,
        _ => word,
    };
    let w = w
        .strip_prefix("${HOME}")
        .or_else(|| w.strip_prefix("$HOME"))
        .map_or_else(|| w.to_string(), |r| format!("~{r}"));
    if w.contains(['\\', '^', '|', '+', '(', ')']) || w.starts_with('-') {
        return None;
    }
    let w = if w.contains(['*', '?', '[', '{']) {
        literal_prefix(&w)
    } else {
        w
    };
    let looks = w.contains('/') || w == "~" || w.split('/').any(is_env_name);
    (looks && !w.is_empty()).then_some(w)
}

/// A Codex shell command: every path it names must be readable and not a secret. Best effort: a path
/// built while the command runs is not seen; the sandbox still keeps the command from writing or
/// reaching the network.
pub fn check_shell(command: &str, scope: &Scope) -> Result<(), Violation> {
    let inner = shell_inner(command);
    for w in shell_words(&inner) {
        if let Some(p) = shell_path(&w) {
            check_path("Shell", &p, scope)?;
        }
    }
    Ok(())
}

/// The file a reading command (`cat`, `sed -n`, `head`, `tail`, `nl`, `less`, `bat`) reads, for
/// citations.
pub fn shell_read(command: &str) -> Option<String> {
    let inner = shell_inner(command);
    let words = shell_words(&inner);
    let first = words.first()?;
    if !matches!(
        first.as_str(),
        "cat" | "sed" | "head" | "tail" | "nl" | "less" | "bat"
    ) {
        return None;
    }
    words[1..]
        .iter()
        .rev()
        .find(|w| !w.starts_with('-') && !w.contains(',') && (w.contains('.') || w.contains('/')))
        .cloned()
}

fn str_arg<'a>(input: &'a Value, key: &str) -> Option<&'a str> {
    input
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
}

/// The directories of a glob pattern before its first wildcard (`/a/b/*.rs` → `/a/b`).
fn literal_prefix(pat: &str) -> String {
    let mut out = Vec::new();
    for c in pat.split('/') {
        if c.contains(['*', '?', '[', '{']) {
            break;
        }
        out.push(c);
    }
    let s = out.join("/");
    if s.is_empty() && pat.starts_with('/') {
        "/".to_string()
    } else {
        s
    }
}

fn is_env_name(c: &str) -> bool {
    let c = c.trim_start_matches("**").trim_start_matches('*');
    c.starts_with(".env")
}

/// Checks one path argument: secrets first (a secret inside a project is still a secret), then scope.
pub fn check_path(tool: &str, raw: &str, scope: &Scope) -> Result<(), Violation> {
    let abs = absolute(raw, scope);
    let real = resolve(&abs);
    let err_path = raw.to_string();
    for p in [&abs, &real] {
        if p.components().any(|c| match c {
            Component::Normal(s) => s.to_str().is_some_and(is_env_name),
            _ => false,
        }) {
            return Err(Violation::Sensitive {
                tool: tool.into(),
                path: err_path,
            });
        }
    }
    let lexical_home = normalize(&scope.home);
    for d in SENSITIVE_HOME_DIRS {
        let lex = lexical_home.join(d);
        let canon = resolve(&scope.home.join(d));
        if abs.starts_with(&lex) || real.starts_with(&canon) || real.starts_with(&lex) {
            return Err(Violation::Sensitive {
                tool: tool.into(),
                path: err_path,
            });
        }
    }
    if scope.readable.iter().any(|d| real.starts_with(d)) {
        Ok(())
    } else {
        Err(Violation::OutOfScope {
            tool: tool.into(),
            path: err_path,
        })
    }
}

/// `~` expanded, relative paths joined to the cwd, `.`/`..` folded (without touching the disk).
fn absolute(raw: &str, scope: &Scope) -> PathBuf {
    let p = if raw == "~" {
        scope.home.clone()
    } else if let Some(rest) = raw.strip_prefix("~/") {
        scope.home.join(rest)
    } else {
        let p = Path::new(raw);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            scope.cwd.join(p)
        }
    };
    normalize(&p)
}

fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            c => out.push(c.as_os_str()),
        }
    }
    out
}

/// The real location of `p`: symlinks resolved through its deepest existing ancestor, the rest appended.
pub fn resolve(p: &Path) -> PathBuf {
    let p = normalize(p);
    let mut base = p.clone();
    let mut rest = Vec::new();
    loop {
        if let Ok(c) = base.canonicalize() {
            let mut out = c;
            for r in rest.iter().rev() {
                out.push(r);
            }
            return out;
        }
        match (base.file_name().map(|s| s.to_os_string()), base.parent()) {
            (Some(name), Some(parent)) => {
                rest.push(name);
                base = parent.to_path_buf();
            }
            _ => return p,
        }
    }
}

/// The short label of a tool call shown in the thread (`steve.tool.brief`): Read → relative path with line
/// range, Grep → pattern and directory, Glob → pattern, knowledge → query, items → id.
pub fn brief(name: &str, input: &Value, cwd: &Path) -> String {
    let rel = |p: &str| -> String {
        let cwd = resolve(cwd);
        let abs = resolve(Path::new(p));
        match abs.strip_prefix(&cwd) {
            Ok(r) if !r.as_os_str().is_empty() => r.display().to_string(),
            _ => p.to_string(),
        }
    };
    match name {
        "Read" => {
            let p = rel(str_arg(input, "file_path").unwrap_or(""));
            let off = input.get("offset").and_then(Value::as_u64);
            let lim = input.get("limit").and_then(Value::as_u64);
            match (off, lim) {
                (Some(o), Some(l)) => format!("{p}:{o}-{}", o + l.saturating_sub(1)),
                (Some(o), None) => format!("{p}:{o}-"),
                (None, Some(l)) => format!("{p}:1-{l}"),
                (None, None) => p,
            }
        }
        "Grep" => {
            let pat = str_arg(input, "pattern").unwrap_or("");
            match str_arg(input, "path") {
                Some(d) => format!("{pat} · {}", rel(d)),
                None => pat.to_string(),
            }
        }
        "Glob" => str_arg(input, "pattern").unwrap_or("").to_string(),
        "Shell" => {
            let c = shell_inner(str_arg(input, "command").unwrap_or(""));
            match c.char_indices().nth(120) {
                Some((i, _)) => format!("{}…", &c[..i]),
                None => c,
            }
        }
        _ => ["query", "id", "item", "source"]
            .iter()
            .find_map(|k| match input.get(*k) {
                Some(Value::String(s)) => Some(s.clone()),
                Some(Value::Number(n)) => Some(n.to_string()),
                _ => None,
            })
            .unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn scope() -> (tempfile::TempDir, Scope) {
        let t = tempfile::tempdir().unwrap();
        let home = t.path().join("home");
        let proj = home.join("proj");
        let other = home.join("other");
        std::fs::create_dir_all(proj.join("src")).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        std::fs::create_dir_all(home.join(".ssh")).unwrap();
        std::fs::write(home.join(".ssh/id_rsa"), "k").unwrap();
        std::fs::write(proj.join("src/lib.rs"), "fn a(){}").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(home.join(".ssh"), proj.join("keys")).unwrap();
        let s = Scope::new(&proj, &[other], &home);
        (t, s)
    }

    #[test]
    fn codex_shell_commands() {
        let (t, s) = scope();
        let h = t.path().join("home").display().to_string();
        let ok = [
            "/bin/zsh -lc 'cat src/lib.rs'",
            "/bin/zsh -lc \"sed -n '1,80p' src/lib.rs\"",
            "rg -n 'fn a' src",
            "ls",
            "/bin/zsh -lc 'rg \"^/usr\" .'",
        ];
        for c in ok {
            assert_eq!(check_shell(c, &s), Ok(()), "{c}");
        }
        assert_eq!(check_shell(&format!("cat {h}/other/x.md"), &s), Ok(()));
        let bad = [
            "/bin/zsh -lc 'cat ~/.ssh/id_rsa'".to_string(),
            "cat $HOME/.ssh/id_rsa".into(),
            "/bin/zsh -lc 'cat keys/id_rsa'".into(),
            "cat ../../.steve/steve.env".into(),
            "cat .env".into(),
            "find / -name id_rsa".into(),
            "cat /etc/hosts".into(),
            format!("cat {h}/.ssh/id_rsa"),
        ];
        for c in &bad {
            assert!(check_shell(c, &s).is_err(), "{c}");
        }
        assert_eq!(shell_inner("/bin/zsh -lc 'echo '\\''x'\\'''"), "echo 'x'");
        assert_eq!(
            shell_read("/bin/zsh -lc \"sed -n '1,80p' src/lib.rs\"").as_deref(),
            Some("src/lib.rs")
        );
        assert_eq!(shell_read("rg x src"), None);
        assert_eq!(
            research_guard("Shell", &json!({"command": "cat ~/.aws/credentials"}), &s),
            Err(Violation::Sensitive {
                tool: "Shell".into(),
                path: "~/.aws/credentials".into()
            })
        );
    }

    #[test]
    fn table() {
        let (t, s) = scope();
        let home = t.path().join("home");
        let h = home.display().to_string();
        type Case = (&'static str, Value, Option<&'static str>);
        let cases: Vec<Case> = vec![
            ("Read", json!({"file_path": "src/lib.rs"}), None),
            (
                "Read",
                json!({"file_path": format!("{h}/proj/src/lib.rs")}),
                None,
            ),
            (
                "Read",
                json!({"file_path": format!("{h}/other/x.md")}),
                None,
            ),
            (
                "Read",
                json!({"file_path": format!("{h}/.ssh/id_rsa")}),
                Some("sensitive"),
            ),
            (
                "Read",
                json!({"file_path": "~/.ssh/id_rsa"}),
                Some("sensitive"),
            ),
            (
                "Read",
                json!({"file_path": "~/.steve/data/steve.sqlite3"}),
                Some("sensitive"),
            ),
            (
                "Read",
                json!({"file_path": "~/.aws/credentials"}),
                Some("sensitive"),
            ),
            (
                "Read",
                json!({"file_path": "~/.config/gh/hosts.yml"}),
                Some("sensitive"),
            ),
            (
                "Read",
                json!({"file_path": "~/Library/Keychains/login.keychain-db"}),
                Some("sensitive"),
            ),
            (
                "Read",
                json!({"file_path": "keys/id_rsa"}),
                Some("sensitive"),
            ),
            (
                "Read",
                json!({"file_path": "src/../../.ssh/id_rsa"}),
                Some("sensitive"),
            ),
            ("Read", json!({"file_path": ".env"}), Some("sensitive")),
            (
                "Read",
                json!({"file_path": "src/.env.local"}),
                Some("sensitive"),
            ),
            ("Read", json!({"file_path": "/etc/hosts"}), Some("scope")),
            ("Read", json!({"file_path": "../secret.txt"}), Some("scope")),
            ("Read", json!({}), Some("input")),
            ("Grep", json!({"pattern": "fn"}), None),
            ("Grep", json!({"pattern": "fn", "path": "src"}), None),
            (
                "Grep",
                json!({"pattern": "KEY", "path": "/etc"}),
                Some("scope"),
            ),
            (
                "Grep",
                json!({"pattern": "KEY", "glob": "**/.env*"}),
                Some("sensitive"),
            ),
            (
                "Grep",
                json!({"pattern": "x", "path": "~/.steve"}),
                Some("sensitive"),
            ),
            ("Glob", json!({"pattern": "**/*.rs"}), None),
            ("Glob", json!({"pattern": "/etc/*"}), Some("scope")),
            ("Glob", json!({"pattern": "../../*"}), Some("scope")),
            ("Glob", json!({"pattern": "~/.ssh/*"}), Some("sensitive")),
            ("Glob", json!({"pattern": "*", "path": "/"}), Some("scope")),
            (
                "mcp__steve__steve_knowledge_search",
                json!({"query": "x"}),
                None,
            ),
            ("mcp__steve__steve_item_read", json!({"id": "ST-1"}), None),
            ("ToolSearch", json!({"query": "select:x"}), None),
            ("Bash", json!({"command": "rm -rf src"}), Some("tool")),
            (
                "Write",
                json!({"file_path": "src/x.rs", "content": ""}),
                Some("tool"),
            ),
            ("Edit", json!({"file_path": "src/lib.rs"}), Some("tool")),
            ("MultiEdit", json!({}), Some("tool")),
            ("NotebookEdit", json!({}), Some("tool")),
            ("WebFetch", json!({"url": "https://x"}), Some("tool")),
            ("WebSearch", json!({"query": "x"}), Some("tool")),
            ("Task", json!({}), Some("tool")),
            ("mcp__steve__steve_deliver", json!({}), Some("tool")),
            ("mcp__other__read", json!({}), Some("tool")),
        ];
        for (name, input, want) in cases {
            let got = research_guard(name, &input, &s);
            let kind = got.as_ref().err().map(|v| match v {
                Violation::Tool { .. } => "tool",
                Violation::OutOfScope { .. } => "scope",
                Violation::Sensitive { .. } => "sensitive",
                Violation::BadInput { .. } => "input",
            });
            assert_eq!(kind, want, "{name} {input} → {got:?}");
        }
    }

    #[test]
    fn briefs() {
        let (_t, s) = scope();
        let lib = s.cwd.join("src/lib.rs").display().to_string();
        assert_eq!(
            brief("Read", &json!({"file_path": lib}), &s.cwd),
            "src/lib.rs"
        );
        assert_eq!(
            brief(
                "Read",
                &json!({"file_path": "src/lib.rs", "offset": 10, "limit": 20}),
                &s.cwd
            ),
            "src/lib.rs:10-29"
        );
        assert_eq!(
            brief(
                "Grep",
                &json!({"pattern": "retrieve", "path": "src"}),
                &s.cwd
            ),
            "retrieve · src"
        );
        assert_eq!(
            brief("Glob", &json!({"pattern": "**/*.rs"}), &s.cwd),
            "**/*.rs"
        );
        assert_eq!(
            brief(
                "mcp__steve__steve_knowledge_search",
                &json!({"query": "知识库"}),
                &s.cwd
            ),
            "知识库"
        );
        assert_eq!(
            brief(
                "mcp__steve__steve_item_read",
                &json!({"id": "ST-20"}),
                &s.cwd
            ),
            "ST-20"
        );
    }
}
