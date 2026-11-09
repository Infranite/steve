//! Layer 2 of read-only enforcement: every tool call the adapter reports is checked by what it is
//! (its ACP kind, its locations and the search it describes, never its harness's tool name) and the
//! run stops at the first one that is not a read inside the run's directories. Pure functions, so
//! the rules are table-tested without a harness.

use serde_json::Value;
use std::fmt;
use std::path::{Component, Path, PathBuf};

/// Steve's read-only MCP tools (on the `steve` server).
pub const MCP_TOOLS: [&str; 6] = [
    "steve_knowledge_search",
    "steve_knowledge_read",
    "steve_items_search",
    "steve_item_read",
    "steve_sessions_list",
    "steve_debug",
];

/// Directories under the home directory that hold secrets. Relative to `Scope::home`.
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

/// The search a read-only tool call describes in its input (a `path` to search, a `glob` that
/// narrows which files are read, a file `pattern`), checked by what it reaches whatever the tool is
/// called: Ok when it stays inside `scope` and away from secrets. `tool` names the call in a violation.
pub fn check_search(tool: &str, input: &Value, scope: &Scope) -> Result<(), Violation> {
    if let Some(p) = str_arg(input, "path") {
        check_path(tool, p, scope)?;
    }
    // a glob aimed at .env files reads secrets
    if let Some(g) = str_arg(input, "glob")
        && g.split(['/', '{', ',']).any(is_env_name)
    {
        return Err(Violation::Sensitive {
            tool: tool.to_string(),
            path: g.to_string(),
        });
    }
    // an absolute or parent-relative pattern reaches outside `path`: check its literal prefix
    let pat = str_arg(input, "pattern").unwrap_or("");
    if pat.starts_with('/') || pat.starts_with('~') || pat.split('/').any(|c| c == "..") {
        let base = str_arg(input, "path").unwrap_or(".");
        let prefix = literal_prefix(pat);
        let joined = if prefix.starts_with('/') || prefix.starts_with('~') {
            prefix
        } else {
            format!("{}/{}", base.trim_end_matches('/'), prefix)
        };
        check_path(tool, &joined, scope)?;
    }
    Ok(())
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
            check_path("shell", &p, scope)?;
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
            check_shell("cat ~/.aws/credentials", &s),
            Err(Violation::Sensitive {
                tool: "shell".into(),
                path: "~/.aws/credentials".into()
            })
        );
    }

    /// Reads by their path, searches by what they reach: whatever the tool is called.
    #[test]
    fn table() {
        let (t, s) = scope();
        let home = t.path().join("home");
        let h = home.display().to_string();
        let read = |p: &str| check_path("read", p, &s);
        let kind = |r: Result<(), Violation>| {
            r.err().map(|v| match v {
                Violation::Tool { .. } => "tool",
                Violation::OutOfScope { .. } => "scope",
                Violation::Sensitive { .. } => "sensitive",
                Violation::BadInput { .. } => "input",
            })
        };
        let reads: Vec<(String, Option<&str>)> = vec![
            ("src/lib.rs".into(), None),
            (format!("{h}/proj/src/lib.rs"), None),
            (format!("{h}/other/x.md"), None),
            (format!("{h}/.ssh/id_rsa"), Some("sensitive")),
            ("~/.ssh/id_rsa".into(), Some("sensitive")),
            ("~/.steve/data/steve.sqlite3".into(), Some("sensitive")),
            ("~/.aws/credentials".into(), Some("sensitive")),
            ("~/.config/gh/hosts.yml".into(), Some("sensitive")),
            (
                "~/Library/Keychains/login.keychain-db".into(),
                Some("sensitive"),
            ),
            ("keys/id_rsa".into(), Some("sensitive")),
            ("src/../../.ssh/id_rsa".into(), Some("sensitive")),
            (".env".into(), Some("sensitive")),
            ("src/.env.local".into(), Some("sensitive")),
            ("/etc/hosts".into(), Some("scope")),
            ("../secret.txt".into(), Some("scope")),
        ];
        for (p, want) in reads {
            assert_eq!(kind(read(&p)), want, "{p}");
        }
        let searches: Vec<(Value, Option<&str>)> = vec![
            (json!({"pattern": "fn"}), None),
            (json!({"pattern": "fn", "path": "src"}), None),
            (json!({"pattern": "KEY", "path": "/etc"}), Some("scope")),
            (
                json!({"pattern": "KEY", "glob": "**/.env*"}),
                Some("sensitive"),
            ),
            (
                json!({"pattern": "x", "path": "~/.steve"}),
                Some("sensitive"),
            ),
            (json!({"pattern": "**/*.rs"}), None),
            (json!({"pattern": "/etc/*"}), Some("scope")),
            (json!({"pattern": "../../*"}), Some("scope")),
            (json!({"pattern": "~/.ssh/*"}), Some("sensitive")),
            (json!({"pattern": "*", "path": "/"}), Some("scope")),
        ];
        for (input, want) in searches {
            assert_eq!(kind(check_search("search", &input, &s)), want, "{input}");
        }
    }
}
