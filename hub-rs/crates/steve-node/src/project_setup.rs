//! Owner-initiated setup on the Hub's own Node, before the destination can be a Node root.
//! No wire operation: the Hub and its built-in Node use the same machine and OS identity.

use std::collections::HashMap;
use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use steve_node_api::*;
use tempfile::TempDir;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::Semaphore;

pub use steve_node_api::ProjectSetupError as SetupError;

/// Maximum directories returned by [`browse_dir`].
const BROWSE_MAX: usize = 500;
/// README files inspected by [`describe_dir`] in order.
const README_FILES: [&str; 4] = ["README.md", "readme.md", "AGENTS.md", "package.json"];
/// Prefix of each README returned to the Hub (in UTF-16 code units, matching JS `slice`).
const README_CHARS: usize = 2500;
/// Maximum concurrent project clone operations on one host.
const MAX_CONCURRENT_CLONES: usize = 4;
/// Maximum retained finished project-setup operations in memory.
const MAX_SETUP_STATUSES: usize = 128;

fn user_home_dir() -> String {
    std::env::var("HOME").unwrap_or_default()
}

/// Folds `.`/`..` and repeated slashes while preserving a leading/trailing slash.
pub fn normalize(p: &str) -> String {
    if p.is_empty() {
        return ".".into();
    }
    let abs = p.starts_with('/');
    let trailing = p.ends_with('/');
    let mut parts: Vec<&str> = vec![];
    for seg in p.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                if parts.last().is_some_and(|l| *l != "..") {
                    parts.pop();
                } else if !abs {
                    parts.push("..");
                }
            }
            s => parts.push(s),
        }
    }
    let mut out = parts.join("/");
    if abs {
        out.insert(0, '/');
    }
    if out.is_empty() {
        return if abs { "/".into() } else { ".".into() };
    }
    if trailing && !out.ends_with('/') {
        out.push('/');
    }
    out
}

fn join_path(a: &str, b: &str) -> String {
    let joined = [a, b]
        .iter()
        .filter(|s| !s.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join("/");
    normalize(&joined)
}

/// `~` and `~/x` → `$HOME` and `$HOME/x`; anything else unchanged.
pub fn expand(p: &str) -> String {
    let home = user_home_dir();
    match p.strip_prefix("~/") {
        Some(rest) if !home.is_empty() => join_path(&home, rest),
        None if p == "~" && !home.is_empty() => home,
        _ => p.to_string(),
    }
}

fn shown(path: &str, home: &str) -> String {
    if home.is_empty() {
        return path.to_string();
    }
    match path.strip_prefix(home) {
        Some("") => "~".into(),
        Some(rest) if rest.starts_with('/') => format!("~{rest}"),
        _ => path.to_string(),
    }
}

fn utf16_slice(s: &str, max_units: usize) -> String {
    let mut units = 0;
    let mut end = 0;
    for (i, ch) in s.char_indices() {
        let len = ch.len_utf16();
        if units + len > max_units {
            break;
        }
        units += len;
        end = i + ch.len_utf8();
    }
    s[..end].to_string()
}

fn fs_error_detail(e: &io::Error, syscall: &str, path: &str) -> String {
    let (code, what) = match e.raw_os_error() {
        Some(20) => ("ENOTDIR", "not a directory"),
        Some(13) => ("EACCES", "permission denied"),
        Some(2) => ("ENOENT", "no such file or directory"),
        Some(21) => ("EISDIR", "illegal operation on a directory"),
        Some(1) => ("EPERM", "operation not permitted"),
        _ => return format!("{e}, {syscall} '{path}'"),
    };
    format!("{code}: {what}, {syscall} '{path}'")
}

/// Browse any directory on this machine for owner-initiated project creation.
pub fn browse_dir(raw: &str) -> BrowseDirResponse {
    let home = user_home_dir();
    let asked = match raw.trim() {
        "" => home.clone(),
        p => expand(p),
    };
    let mut path = normalize(&asked);
    if path.len() > 1 && path.ends_with('/') {
        path.pop();
    }
    let parent = Path::new(&path)
        .parent()
        .map(|p| shown(&p.to_string_lossy(), &home));
    let mut out = BrowseDirResponse {
        path: shown(&path, &home),
        parent,
        exists: false,
        denied: false,
        git: false,
        dirs: vec![],
        more: false,
    };
    if !path.starts_with('/') || !Path::new(&path).is_dir() {
        return out;
    }
    out.exists = true;
    out.git = Path::new(&join_path(&path, ".git")).exists();
    let Ok(read) = std::fs::read_dir(&path) else {
        out.denied = true;
        return out;
    };
    let mut names: Vec<String> = read
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !n.starts_with('.'))
        .collect();
    names.sort_by_key(|n| n.to_lowercase());
    out.more = names.len() > BROWSE_MAX;
    out.dirs = names
        .into_iter()
        .take(BROWSE_MAX)
        .map(|n| {
            let p = join_path(&path, &n);
            BrowseDirEntry {
                name: n,
                path: shown(&p, &home),
                git: Path::new(&join_path(&p, ".git")).exists(),
            }
        })
        .collect();
    out
}

/// Inspect a host directory for project registration or AI description drafting.
pub fn describe_dir(raw: &str, with_readme: bool) -> ApiResult<DescribeDirResponse> {
    let resolved = normalize(&expand(raw));
    let p = Path::new(&resolved);
    let exists = p.exists();
    let is_dir = p.is_dir();
    if !exists {
        return Ok(DescribeDirResponse {
            resolved_path: resolved,
            exists: false,
            is_dir: false,
            git: false,
            files: vec![],
            readmes: vec![],
        });
    }
    let git = is_dir && Path::new(&join_path(&resolved, ".git")).exists();
    if !with_readme {
        return Ok(DescribeDirResponse {
            resolved_path: resolved,
            exists: true,
            is_dir,
            git,
            files: vec![],
            readmes: vec![],
        });
    }
    let mut names: Vec<String> = std::fs::read_dir(p)
        .map_err(|e| Error::new(Code::Internal, fs_error_detail(&e, "scandir", &resolved)))?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    let files: Vec<String> = names
        .into_iter()
        .filter(|f| !f.starts_with('.'))
        .take(60)
        .collect();
    let mut readmes = vec![];
    for f in README_FILES {
        let fp = join_path(&resolved, f);
        if !Path::new(&fp).exists() {
            continue;
        }
        let text = std::fs::read(&fp)
            .map_err(|e| Error::new(Code::Internal, fs_error_detail(&e, "open", &fp)))?;
        readmes.push((
            f.to_string(),
            utf16_slice(&String::from_utf8_lossy(&text), README_CHARS),
        ));
    }
    Ok(DescribeDirResponse {
        resolved_path: resolved,
        exists: true,
        is_dir: true,
        git,
        files,
        readmes,
    })
}

/// Check whether `raw` exists and is a directory on this machine, returning its canonical path.
pub fn path_check(raw: &str) -> PathCheckResponse {
    let p = PathBuf::from(expand(raw));
    match std::fs::canonicalize(&p) {
        Ok(canon) => {
            let is_dir = canon.is_dir();
            let writable = is_dir && crate::workspace::usable(&canon);
            PathCheckResponse {
                exists: true,
                is_dir,
                canonical: Some(canon.to_string_lossy().into_owned()),
                writable,
            }
        }
        Err(_) => PathCheckResponse {
            exists: p.exists(),
            is_dir: p.is_dir(),
            canonical: None,
            writable: false,
        },
    }
}

/// Tracks owner-initiated project creation and git clone operations on the colocated Node.
pub struct SetupManager {
    statuses: Mutex<HashMap<OperationId, ProjectSetupStatus>>,
    clones: Arc<Semaphore>,
}

impl Default for SetupManager {
    fn default() -> Self {
        Self::new()
    }
}

impl SetupManager {
    pub fn new() -> Self {
        Self {
            statuses: Mutex::new(HashMap::new()),
            clones: Arc::new(Semaphore::new(MAX_CONCURRENT_CLONES)),
        }
    }

    pub fn preview(&self, req: &ProjectSetupPreviewRequest) -> ProjectSetupPreview {
        let expanded = expand(&req.parent);
        match destination(Path::new(&expanded), &req.directory) {
            Ok(path) => ProjectSetupPreview::Ready {
                path: path
                    .into_os_string()
                    .into_string()
                    .expect("UTF-8 destination"),
            },
            Err(error) => ProjectSetupPreview::Failed { error },
        }
    }

    fn record(&self, status: ProjectSetupStatus) {
        let mut map = self.statuses.lock().unwrap_or_else(|e| e.into_inner());
        if map.len() >= MAX_SETUP_STATUSES {
            map.retain(|_, s| !s.is_terminal());
        }
        map.insert(status.operation.clone(), status);
    }

    pub fn status(&self, op: &OperationId) -> ApiResult<ProjectSetupStatus> {
        self.statuses
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(op)
            .cloned()
            .ok_or_else(|| Error::new(Code::Invalid, "no such project setup operation"))
    }

    pub async fn start(
        self: &Arc<Self>,
        op: OperationId,
        req: ProjectSetupRequest,
    ) -> ApiResult<ProjectSetupStatus> {
        if let Some(existing) = self
            .statuses
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&op)
            .cloned()
        {
            return Ok(existing);
        }
        let parent_expanded = expand(&req.parent);
        let directory = req.directory.clone();
        let target = tokio::task::spawn_blocking(move || {
            destination(Path::new(&parent_expanded), &directory)
        })
        .await
        .map_err(|e| Error::new(Code::Internal, e.to_string()))?;
        let target = match target {
            Ok(p) => p,
            Err(error) => {
                let st = ProjectSetupStatus {
                    operation: op,
                    phase: "failed".into(),
                    percent: None,
                    path: None,
                    error: Some(error),
                };
                self.record(st.clone());
                return Ok(st);
            }
        };
        let target_str = target
            .clone()
            .into_os_string()
            .into_string()
            .expect("UTF-8 destination");
        if target_str != req.preview {
            return Err(Error::new(
                Code::Conflict,
                "project setup preview no longer matches destination",
            ));
        }

        match req.mode {
            ProjectSetupMode::Empty => {
                let target_for_create = target.clone();
                let res = tokio::task::spawn_blocking(move || create_empty(&target_for_create))
                    .await
                    .map_err(|e| Error::new(Code::Internal, e.to_string()))?;
                let st = match res {
                    Ok(()) => ProjectSetupStatus {
                        operation: op,
                        phase: "done".into(),
                        percent: Some(100),
                        path: Some(target_str),
                        error: None,
                    },
                    Err(error) => ProjectSetupStatus {
                        operation: op,
                        phase: "failed".into(),
                        percent: None,
                        path: None,
                        error: Some(error),
                    },
                };
                self.record(st.clone());
                Ok(st)
            }
            ProjectSetupMode::Clone { remote, branch } => {
                let remote = match Remote::new(remote) {
                    Ok(r) => r,
                    Err(error) => {
                        let st = ProjectSetupStatus {
                            operation: op,
                            phase: "failed".into(),
                            percent: None,
                            path: None,
                            error: Some(error),
                        };
                        self.record(st.clone());
                        return Ok(st);
                    }
                };
                let branch = match branch
                    .filter(|b| !b.is_empty())
                    .map(Branch::new)
                    .transpose()
                {
                    Ok(b) => b,
                    Err(error) => {
                        let st = ProjectSetupStatus {
                            operation: op,
                            phase: "failed".into(),
                            percent: None,
                            path: None,
                            error: Some(error),
                        };
                        self.record(st.clone());
                        return Ok(st);
                    }
                };
                let permit = self
                    .clones
                    .clone()
                    .try_acquire_owned()
                    .map_err(|_| Error::later(Code::Busy, "too many concurrent clones"))?;
                let target_for_stage = target.clone();
                let staging = tokio::task::spawn_blocking(move || Staging::new(&target_for_stage))
                    .await
                    .map_err(|e| Error::new(Code::Internal, e.to_string()))?;
                let staging = match staging {
                    Ok(s) => s,
                    Err(error) => {
                        let st = ProjectSetupStatus {
                            operation: op,
                            phase: "failed".into(),
                            percent: None,
                            path: None,
                            error: Some(error),
                        };
                        self.record(st.clone());
                        return Ok(st);
                    }
                };
                let initial = ProjectSetupStatus {
                    operation: op.clone(),
                    phase: "connecting".into(),
                    percent: None,
                    path: None,
                    error: None,
                };
                self.record(initial.clone());
                let mgr = self.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    let clone_res = staging
                        .clone_repo(remote, branch, |phase, percent| {
                            mgr.record(ProjectSetupStatus {
                                operation: op.clone(),
                                phase: phase.into(),
                                percent: Some(percent),
                                path: None,
                                error: None,
                            });
                        })
                        .await;
                    let final_res = match clone_res {
                        Ok(()) => {
                            let target_pub = target.clone();
                            tokio::task::spawn_blocking(move || staging.publish(&target_pub))
                                .await
                                .map_err(|_| SetupError::Filesystem)
                                .and_then(|r| r)
                        }
                        Err(e) => Err(e),
                    };
                    let terminal = match final_res {
                        Ok(()) => ProjectSetupStatus {
                            operation: op,
                            phase: "done".into(),
                            percent: Some(100),
                            path: Some(target_str),
                            error: None,
                        },
                        Err(error) => ProjectSetupStatus {
                            operation: op,
                            phase: "failed".into(),
                            percent: None,
                            path: None,
                            error: Some(error),
                        },
                    };
                    mgr.record(terminal);
                });
                Ok(initial)
            }
        }
    }
}

pub fn destination(parent: &Path, name: &str) -> Result<PathBuf, SetupError> {
    if name.is_empty()
        || matches!(name, "." | "..")
        || name.eq_ignore_ascii_case(".git")
        || name.len() > 255
        || name
            .chars()
            .any(|c| c.is_control() || matches!(c, '/' | '\\'))
    {
        return Err(SetupError::DirectoryName);
    }
    if !parent.is_absolute() {
        return Err(SetupError::Parent);
    }
    let parent = parent.canonicalize().map_err(|_| SetupError::Parent)?;
    if !parent.is_dir() {
        return Err(SetupError::Parent);
    }
    let path = parent.join(name);
    if path.to_str().is_none() {
        return Err(SetupError::Parent);
    }
    match path.symlink_metadata() {
        Ok(_) => Err(SetupError::Conflict), // includes files and dangling symlinks
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(path),
        Err(_) => Err(SetupError::Filesystem),
    }
}

pub fn create_empty(path: &Path) -> Result<(), SetupError> {
    std::fs::create_dir(path).map_err(fs_error)
}

fn fs_error(e: io::Error) -> SetupError {
    if e.kind() == io::ErrorKind::AlreadyExists || e.raw_os_error() == Some(libc::ENOTEMPTY) {
        SetupError::Conflict
    } else {
        SetupError::Filesystem
    }
}

pub struct Remote(String);

impl Remote {
    pub fn new(raw: String) -> Result<Self, SetupError> {
        // No userinfo for HTTPS, no password for SSH, no URL token parameters. SSH usernames
        // are routing information, not credentials. Keep Git's SCP syntax for SSH aliases.
        if raw.is_empty()
            || raw.starts_with('-')
            || raw.contains(['\\', '?', '#'])
            || raw.chars().any(|c| c.is_control() || c.is_whitespace())
        {
            return Err(SetupError::Remote);
        }
        let atom = |s: &str| {
            !s.starts_with('-')
                && !s.is_empty()
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b".-_".contains(&b))
        };
        let valid = if let Some((scheme, rest)) = raw.split_once("://") {
            let authority = rest.split('/').next().unwrap_or_default();
            !authority.is_empty()
                && url::Url::parse(&raw).is_ok_and(|u| {
                    u.has_host()
                        && u.path().len() > 1
                        && match scheme {
                            "https" => !authority.contains('@'),
                            "ssh" => {
                                u.password().is_none()
                                    && !authority.starts_with('@')
                                    && (u.username().is_empty() || atom(u.username()))
                                    && u.host_str().is_some_and(|h| !h.starts_with('-'))
                            }
                            _ => false,
                        }
                })
        } else {
            raw.split_once(':').is_some_and(|(addr, path)| {
                !path.is_empty()
                    && !path.starts_with(':')
                    && !path.contains('@')
                    && match addr.split_once('@') {
                        Some((user, host)) => atom(user) && atom(host),
                        None => atom(addr),
                    }
            })
        };
        valid.then_some(Self(raw)).ok_or(SetupError::Remote)
    }
}

pub struct Branch(String);

impl Branch {
    pub fn new(raw: String) -> Result<Self, SetupError> {
        // A literal branch name, not a revision, option, tag, or wildcard.
        if raw.is_empty()
            || raw.starts_with('-')
            || raw == "HEAD"
            || raw.len() > 255
            || raw.contains("..")
            || raw.contains("@{")
            || raw.ends_with('.')
            || raw
                .chars()
                .any(|c| c.is_control() || c.is_whitespace() || "~^:?*[\\".contains(c))
            || raw
                .split('/')
                .any(|p| p.is_empty() || p.starts_with('.') || p.ends_with(".lock"))
        {
            return Err(SetupError::Branch);
        }
        Ok(Self(raw))
    }
}

pub struct Staging {
    tmp: TempDir,
    repo: PathBuf,
}

impl Staging {
    pub fn new(target: &Path) -> Result<Self, SetupError> {
        let tmp = tempfile::Builder::new()
            .prefix(".steve-clone-")
            .tempdir_in(target.parent().ok_or(SetupError::Filesystem)?)
            .map_err(|_| SetupError::Filesystem)?;
        let repo = tmp.path().join("repo");
        Ok(Self { tmp, repo })
    }

    /// Publish a complete checkout. Ordinary rename can replace an empty directory on Unix;
    /// never fall back to it, even on a filesystem that does not support exclusive rename.
    pub fn publish(self, target: &Path) -> Result<(), SetupError> {
        let src =
            CString::new(self.repo.as_os_str().as_bytes()).map_err(|_| SetupError::Filesystem)?;
        let dst =
            CString::new(target.as_os_str().as_bytes()).map_err(|_| SetupError::Filesystem)?;
        #[cfg(target_os = "macos")]
        let result = unsafe {
            libc::renameatx_np(
                libc::AT_FDCWD,
                src.as_ptr(),
                libc::AT_FDCWD,
                dst.as_ptr(),
                libc::RENAME_EXCL,
            )
        };
        #[cfg(target_os = "linux")]
        let result = unsafe {
            libc::renameat2(
                libc::AT_FDCWD,
                src.as_ptr(),
                libc::AT_FDCWD,
                dst.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        let result = -1;
        if result == 0 {
            Ok(())
        } else {
            Err(fs_error(io::Error::last_os_error()))
        }
    }

    /// Keep system/user Git config, helper programs, keychain and SSH agent in the Node's OS
    /// context. Do not run hooks, repository-local config, recursive submodule clones, or external
    /// transport helpers. No command output is returned or logged, even on failure.
    pub async fn clone_repo(
        &self,
        remote: Remote,
        branch: Option<Branch>,
        progress: impl FnMut(&'static str, u8),
    ) -> Result<(), SetupError> {
        let mut cmd = self.clone_command(&remote, branch.as_ref());
        let mut child = cmd.spawn().map_err(|_| SetupError::Git)?;
        let mut group = CloneGroup(child.id());
        let stderr = child.stderr.take().ok_or(SetupError::Git)?;
        let run = async {
            // Read before wait so the leader cannot be reaped while we may still need to
            // kill its process group (e.g. a hung credential helper or SSH subprocess).
            let observed = observe(stderr, progress).await?;
            let status = child.wait().await.map_err(|_| SetupError::Git)?;
            group.0 = None;
            if status.success() {
                if observed.broken_head && branch.is_none() {
                    Err(SetupError::DefaultBranch)
                } else {
                    Ok(())
                }
            } else {
                Err(observed.failure)
            }
        };
        match tokio::time::timeout(Duration::from_secs(600), run).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                if group.0.is_some() {
                    drop(group);
                    let _ = child.wait().await;
                }
                return Err(e);
            }
            Err(_) => {
                drop(group);
                let _ = child.wait().await;
                return Err(SetupError::Timeout);
            }
        }
        if let Some(b) = branch {
            // Git's --branch also takes a tag. Detached HEAD is not a requested branch.
            let head = tokio::fs::read(self.repo.join(".git/HEAD"))
                .await
                .map_err(|_| SetupError::Git)?;
            if head != format!("ref: refs/heads/{}\n", b.0).as_bytes() {
                return Err(SetupError::Branch);
            }
        }
        Ok(())
    }

    /// Assemble the trusted clone command while retaining owner authentication context.
    fn clone_command(&self, remote: &Remote, branch: Option<&Branch>) -> Command {
        let mut cmd = Command::new("git");
        cmd.current_dir(self.tmp.path())
            // An explicit, nonexistent GIT_DIR prevents discovery of a repository above
            // the staging directory. Clone still creates the new checkout's own .git.
            .env("GIT_DIR", self.tmp.path().join("unused-git-dir"))
            .env("GIT_CONFIG_PARAMETERS", "")
            .env("GIT_CONFIG_COUNT", "0")
            .env("GIT_ALLOW_PROTOCOL", "https:ssh")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_ASKPASS", "")
            .env("SSH_ASKPASS_REQUIRE", "never")
            .env("LC_ALL", "C")
            .env("GIT_LFS_SKIP_SMUDGE", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .process_group(0)
            .kill_on_drop(true);
        for (name, _) in std::env::vars_os() {
            let key = name.to_string_lossy();
            if key.starts_with("GIT_TRACE") || key == "GIT_CURL_VERBOSE" {
                cmd.env_remove(name);
            }
        }
        for key in ["GIT_TRACE2", "GIT_TRACE2_EVENT", "GIT_TRACE2_PERF"] {
            cmd.env(key, "0");
        }
        for key in [
            "GIT_WORK_TREE",
            "GIT_COMMON_DIR",
            "GIT_IMPLICIT_WORK_TREE",
            "GIT_GRAFT_FILE",
            "GIT_SHALLOW_FILE",
            "GIT_REPLACE_REF_BASE",
            "GIT_INDEX_FILE",
            "GIT_OBJECT_DIRECTORY",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            "GIT_NAMESPACE",
            "GIT_QUARANTINE_PATH",
            "GIT_CONFIG",
            "GIT_PREFIX",
            "GIT_PROTOCOL_FROM_USER",
            "GIT_TEMPLATE_DIR",
        ] {
            cmd.env_remove(key);
        }
        cmd.args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "http.followRedirects=false",
            "clone",
            "--progress",
            "--no-recurse-submodules",
            "--template=",
        ]);
        if let Some(b) = branch {
            cmd.args(["--branch", &b.0]);
        }
        cmd.arg("--").arg(&remote.0).arg(&self.repo);
        cmd
    }
}

struct CloneGroup(Option<u32>);

impl Drop for CloneGroup {
    fn drop(&mut self) {
        if let Some(pid) = self.0.filter(|p| *p > 1) {
            // This child is in a fresh process group and has not been waited on.
            unsafe { libc::kill(-(pid as libc::pid_t), libc::SIGKILL) };
        }
    }
}

fn line_status(line: &str) -> Option<SetupError> {
    if !line.starts_with("fatal: ") && !line.starts_with("error: ") && !line.starts_with("ssh: ") {
        return None;
    }
    let text = line.to_ascii_lowercase();
    if text.contains("remote branch") && text.contains("not found") {
        Some(SetupError::Branch)
    } else if [
        "authentication failed",
        "could not read username",
        "could not read password",
        "permission denied",
        "error: 401",
        "error: 403",
        "error: 404",
        "repository not found",
        "host key verification failed",
    ]
    .iter()
    .any(|s| text.contains(s))
    {
        Some(SetupError::Authentication)
    } else if [
        "could not resolve",
        "failed to connect",
        "connection refused",
        "connection timed out",
        "connection reset",
        "connection closed",
        "network is unreachable",
        "ssl",
        "tls",
        "early eof",
        "remote end hung up",
        "timed out",
    ]
    .iter()
    .any(|s| text.contains(s))
    {
        Some(SetupError::Network)
    } else {
        None
    }
}

struct Observed {
    failure: SetupError,
    broken_head: bool,
}

async fn observe(
    mut input: impl tokio::io::AsyncRead + Unpin,
    mut emit: impl FnMut(&'static str, u8),
) -> Result<Observed, SetupError> {
    let mut chunk = [0; 4096];
    let mut line = Vec::with_capacity(1024);
    let mut last = None;
    let mut observed = Observed {
        failure: SetupError::Clone,
        broken_head: false,
    };
    loop {
        let n = input.read(&mut chunk).await.map_err(|_| SetupError::Git)?;
        if n == 0 {
            return Ok(observed);
        }
        for &b in &chunk[..n] {
            if b == b'\r' || b == b'\n' {
                if let Ok(s) = std::str::from_utf8(&line) {
                    if let Some(e) = line_status(s) {
                        observed.failure = e;
                    }
                    if s.starts_with("warning: remote HEAD refers to nonexistent ref") {
                        observed.broken_head = true;
                    }
                    for (prefix, phase) in [
                        ("Receiving objects:", "receiving"),
                        ("Resolving deltas:", "resolving"),
                        ("Updating files:", "checkout"),
                    ] {
                        if let Some((percent, _)) = s
                            .strip_prefix(prefix)
                            .and_then(|s| s.trim_start().split_once('%'))
                            && let Ok(p) = percent.parse::<u8>()
                            && p <= 100
                            && last != Some((phase, p))
                        {
                            emit(phase, p);
                            last = Some((phase, p));
                        }
                    }
                }
                line.clear();
            } else if line.len() < 1024 {
                line.push(b);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt, symlink};

    #[test]
    fn destination_canonicalizes_only_the_parent_and_refuses_any_existing_entry() {
        let d = tempfile::tempdir().unwrap();
        let alias = d.path().join("parent");
        symlink(d.path(), &alias).unwrap();
        assert_eq!(
            destination(&alias, "a b").unwrap(),
            d.path().canonicalize().unwrap().join("a b")
        );
        for name in ["", ".", "..", "child/path", "child\\path", ".GIT", "bad\n"] {
            assert_eq!(destination(d.path(), name), Err(SetupError::DirectoryName));
        }
        assert_eq!(
            destination(Path::new("relative"), "ok"),
            Err(SetupError::Parent)
        );
        std::fs::write(d.path().join("file"), "do not touch").unwrap();
        symlink("no-such-target", d.path().join("dangling")).unwrap();
        for name in ["file", "dangling", "parent"] {
            assert_eq!(destination(d.path(), name), Err(SetupError::Conflict));
        }
    }

    #[test]
    fn publication_and_empty_creation_do_not_replace_a_late_directory() {
        let d = tempfile::tempdir().unwrap();
        let target = destination(d.path(), "project").unwrap();
        let staged = Staging::new(&target).unwrap();
        let tmp = staged.tmp.path().to_path_buf();
        std::fs::create_dir(&staged.repo).unwrap();
        std::fs::write(staged.repo.join("README"), "clone").unwrap();
        assert!(!target.exists());
        std::fs::create_dir(&target).unwrap();
        let inode = target.metadata().unwrap().ino();
        assert_eq!(staged.publish(&target), Err(SetupError::Conflict));
        assert!(!tmp.exists());
        assert_eq!(create_empty(&target), Err(SetupError::Conflict));
        assert_eq!(target.metadata().unwrap().ino(), inode);
        assert_eq!(target.read_dir().unwrap().count(), 0);

        std::fs::remove_dir(&target).unwrap();
        let staged = Staging::new(&target).unwrap();
        let tmp = staged.tmp.path().to_path_buf();
        std::fs::create_dir(&staged.repo).unwrap();
        std::fs::write(staged.repo.join("README"), "clone").unwrap();
        staged.publish(&target).unwrap();
        assert!(!tmp.exists());
        assert_eq!(
            std::fs::read_to_string(target.join("README")).unwrap(),
            "clone"
        );
    }

    #[test]
    fn only_credential_free_network_urls_and_literal_branches() {
        for url in [
            "https://example.org/team/repo.git",
            "ssh://git@example.org:2222/team/repo.git",
            "ssh://git@[::1]/repo.git",
            "git@ssh-alias:repo.git",
        ] {
            assert!(Remote::new(url.into()).is_ok(), "{url}");
        }
        for url in [
            "--help",
            "ext::command",
            "../repo",
            "file:///repo",
            "http://example.org/repo.git",
            "https:///repo",
            "https://:password@example.org/repo.git",
            "https://user@example.org/repo.git",
            "https://@example.org/repo.git",
            "ssh://git:password@example.org/repo.git",
            "git:password@host:repo",
            "https://example.org/repo.git?token=secret",
            "https://example.org/repo.git#fragment",
        ] {
            assert!(
                matches!(Remote::new(url.into()), Err(SetupError::Remote)),
                "{url}"
            );
        }
        assert!(Branch::new("release/v1".into()).is_ok());
        for name in [
            "HEAD", "--mirror", "@{-1}", "a..b", "a/b.lock", "foo//bar", "x^", "a b",
        ] {
            assert!(
                matches!(Branch::new(name.into()), Err(SetupError::Branch)),
                "{name}"
            );
        }
    }

    #[tokio::test]
    async fn only_known_git_progress_is_emitted_and_stderr_is_classified_not_forwarded() {
        let input = b"Cloning into 'folder'...\nremote: Receiving objects: 7% secret\rReceiving objects: 32% (32/100)\rReceiving objects: 32% (32/100)\rResolving deltas: 100%\nfatal: Authentication failed for 'https://private.example/secret'\n";
        let mut progress = Vec::new();
        let result = observe(&input[..], |phase, pct| progress.push((phase, pct)))
            .await
            .unwrap();
        assert_eq!(progress, [("receiving", 32), ("resolving", 100)]);
        assert_eq!(result.failure, SetupError::Authentication);
    }
}
