//! Shared Git domain result types.
//!
//! All live Git execution runs on the Node (`steve-node/src/git.rs` and
//! `steve-node/src/housekeeping.rs`); `steve-core` never spawns `git` in production code.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rebase {
    Ok { moved: bool },
    Dirty(String),
    Conflict(String),
    Error(String),
}

#[cfg(test)]
pub(crate) mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::sync::OnceLock;

    pub fn program() -> &'static Path {
        static GIT: OnceLock<PathBuf> = OnceLock::new();
        GIT.get_or_init(|| {
            std::env::var_os("STEVE_GIT")
                .filter(|g| !g.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| "git".into())
        })
    }

    pub fn command() -> Command {
        let mut c = Command::new(program());
        c.env_remove("STEVE_UPGRADE_CONTROL_TOKEN")
            .env_remove("STEVE_SUPERVISOR_PID")
            .env_remove("STEVE_UPGRADE_ID");
        c
    }

    pub fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
        let out = command()
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_EDITOR", "true")
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(Stdio::null())
            .output()
            .map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
        } else {
            let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
            Err(if err.is_empty() {
                format!("git {} failed", args.join(" "))
            } else {
                err
            })
        }
    }

    pub fn git_read(dir: &Path, args: &[&str]) -> Result<String, String> {
        let out = command()
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GIT_NO_LAZY_FETCH", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(Stdio::null())
            .output()
            .map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
    }

    pub fn git_test(dir: &Path, args: &[&str]) -> Result<bool, String> {
        let out = command()
            .arg("-C")
            .arg(dir)
            .args(args)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| e.to_string())?;
        match out.status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(String::from_utf8_lossy(&out.stderr).trim().to_string()),
        }
    }

    pub fn g(dir: &Path, args: &[&str]) -> String {
        git(dir, args).unwrap_or_else(|e| panic!("git {args:?}: {e}"))
    }

    /// A repo on main with one commit.
    pub fn repo(root: &Path) -> PathBuf {
        let r = root.join("repo");
        fs::create_dir_all(&r).unwrap();
        g(&r, &["init", "-q", "-b", "main"]);
        g(&r, &["config", "user.email", "t@t"]);
        g(&r, &["config", "user.name", "t"]);
        g(&r, &["config", "commit.gpgsign", "false"]);
        g(
            &r,
            &["config", "--local", "maintenance.autoDetach", "false"],
        );
        g(&r, &["config", "--local", "gc.autoDetach", "false"]);
        fs::write(r.join("a.txt"), "a\n").unwrap();
        g(&r, &["add", "."]);
        g(&r, &["commit", "-q", "-m", "a"]);
        r
    }

    pub fn branch_with(
        repo: &Path,
        wts: &Path,
        dir: &str,
        file: &str,
        text: &str,
    ) -> (String, PathBuf) {
        let name = format!("steve/{}", dir.to_lowercase());
        let wt = wts.join(dir);
        g(
            repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                &name,
                &wt.to_string_lossy(),
                "main",
            ],
        );
        fs::write(wt.join(file), text).unwrap();
        g(&wt, &["add", "."]);
        g(&wt, &["commit", "-q", "-m", file]);
        (name, wt)
    }
}
