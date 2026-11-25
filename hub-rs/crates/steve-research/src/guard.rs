//! Path scope and violation types shared across research runs and the Node wire protocol.
//! Read-only enforcement relies on the OS sandbox (`steve-confine`), `PermissionMode::Plan`,
//! and unconditional permission refusal in one-shot ACP calls.

use std::fmt;
use std::path::{Component, Path, PathBuf};

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
}

/// Why a tool call ended the run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Violation {
    /// A tool permission request refused by one-shot read-only policy.
    Tool { tool: String },
    /// A path outside the run's readable directories.
    OutOfScope { tool: String, path: String },
    /// A secret path rejected by confinement.
    Sensitive { tool: String, path: String },
    /// A tool invocation with unusable input.
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
