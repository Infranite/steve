//! Git in a workspace (N0e-E8): the facts the Hub decides on ([`facts`]) and the push
//! ([`GitAction::Push`]).
//!
//! Every git here runs with the system and global config off, no prompts, and nothing in the
//! environment that points it elsewhere (`GIT_DIR`, `GIT_CONFIG_PARAMETERS`…). Reads are bounded
//! in time and output and kill their whole process group when they end.
//!
//! The push, as the Hub's own did it:
//! - the workspace's remote must point at `origin_must_be` (`github.com/<owner>/<name>`, the URL
//!   as configured, not rewritten by `insteadOf`), else it is refused;
//! - the branch must be at `expected_local`, and HEAD and the refs as the Hub saw them;
//! - it runs from a throwaway bare repository under the Node's home (`push/<op>`) that borrows the
//!   workspace's objects (`objects/info/alternates`), so the workspace's own config (hooks,
//!   `http.proxy`, `insteadOf`, credential helpers) plays no part;
//! - the credential, taken from the Hub ([`HubSecrets`]) once, right before, goes in as
//!   `http.<url>.extraheader` through `GIT_CONFIG_COUNT` environment variables: not on the command
//!   line, not in a file, not in the journal, and masked in what git prints;
//! - the target is the canonical `url`, as a fast-forward or under the `expected_remote` lease;
//! - it is a command like [`crate::exec`]: its process group is journaled before git starts, killed
//!   at the deadline, and a Node that died while it ran leaves it `unknown` (never pushed again).
//!
//! With `STEVE_FORBID_REAL_HOSTS=1` (the test guard) a push to anything but a local path or a
//! loopback address is refused here too, whatever the Hub let through.

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use steve_node_api::*;
use steve_vault::redact::Redactor;
use zeroize::Zeroizing;

/// How long one read may take.
const READ_TIMEOUT: Duration = Duration::from_secs(30);
/// What one read may print.
const READ_CAP: usize = 4 << 20;
/// How long a push may take by default ([`crate::LocalNode::set_push_timeout`]). git also gives
/// up by itself on a stalled transfer (`http.lowSpeed*`).
pub(crate) const PUSH_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// What a push's output keeps.
const PUSH_OUTPUT: usize = 16 * 1024;
/// What of git's complaint goes into an error.
const ERROR_CHARS: usize = 300;

/// The detail of a push refused by the test guard (the Hub shows its own text for it).
pub const HOST_FORBIDDEN: &str = "push target forbidden by STEVE_FORBID_REAL_HOSTS";
/// The details of a push whose branch is not where the Hub saw it, or is gone.
pub const BRANCH_MOVED: &str = "the branch moved";
pub const NO_BRANCH: &str = "no such branch";
/// The detail of a push whose workspace's remote points elsewhere.
pub const WRONG_REMOTE: &str = "the workspace's remote points elsewhere";

/// The process environment git must not take from the Node (or the Hub it runs in).
const UNSET: [&str; 14] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_CONFIG",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
    "GIT_NAMESPACE",
    "GIT_SSH",
    "GIT_SSH_COMMAND",
    "GIT_ASKPASS",
    "SSH_ASKPASS",
    "GIT_PROXY_COMMAND",
];

/// The push command: its URL, refspec and lease come in as variables, never as text of it.
const PUSH_SH: &str = "exec git -c core.hooksPath=/dev/null push --quiet --no-verify \
                       ${STEVE_PUSH_LEASE:+\"--force-with-lease=$STEVE_PUSH_LEASE\"} \
                       -- \"$STEVE_PUSH_URL\" \"$STEVE_PUSH_REFSPEC\"";

fn bad(m: impl Into<String>) -> ApiError {
    ApiError::new(ErrorCode::Invalid, m)
}

/// `git -C dir` with the system and global config off and no prompts.
fn git(dir: &Path) -> Command {
    let mut c = Command::new("git");
    for k in UNSET {
        c.env_remove(k);
    }
    for k in [
        "STEVE_UPGRADE_CONTROL_TOKEN",
        "STEVE_SUPERVISOR_PID",
        "STEVE_UPGRADE_ID",
    ] {
        c.env_remove(k);
    }
    c.arg("-C")
        .arg(dir)
        // reads take no lock and run no repository-configured monitor
        .args(["--no-optional-locks", "-c", "core.fsmonitor=false"])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    c
}

struct Out {
    code: Option<i32>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

impl Out {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).trim().to_string()
    }
    fn complaint(&self) -> String {
        String::from_utf8_lossy(&self.stderr)
            .trim()
            .chars()
            .take(ERROR_CHARS)
            .collect()
    }
}

/// Runs a git read in `dir`, bounded; its group is killed when it ends.
fn read(dir: &Path, args: &[&str]) -> ApiResult<Out> {
    let mut c = git(dir);
    c.args(args);
    let mut child = c
        .spawn()
        .map_err(|e| ApiError::new(ErrorCode::Internal, format!("git: {e}")))?;
    let pgid = child.id() as libc::pid_t;
    let drain = |p: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut v = Vec::new();
            if let Some(p) = p {
                let _ = p.take(READ_CAP as u64 + 1).read_to_end(&mut v);
            }
            v
        })
    };
    let out = drain(child.stdout.take().map(|p| Box::new(p) as _));
    let err = drain(child.stderr.take().map(|p| Box::new(p) as _));
    let until = Instant::now() + READ_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if Instant::now() < until => std::thread::sleep(Duration::from_millis(5)),
            _ => break None,
        }
    };
    // what it started goes with it (the group's leader may be reaped: only its group id is used)
    unsafe {
        libc::kill(-pgid, libc::SIGKILL);
    }
    let Some(status) = status else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(ApiError::new(ErrorCode::Expired, "git took too long"));
    };
    let stdout = out.join().unwrap_or_default();
    let stderr = err.join().unwrap_or_default();
    if stdout.len() > READ_CAP {
        return Err(ApiError::new(ErrorCode::Exhausted, "git printed too much"));
    }
    Ok(Out {
        code: status.code(),
        stdout,
        stderr,
    })
}

/// A read that must succeed: its trimmed output.
fn must(dir: &Path, args: &[&str]) -> ApiResult<String> {
    let o = read(dir, args)?;
    if o.code != Some(0) {
        return Err(ApiError::new(
            ErrorCode::Internal,
            format!("git {}: {}", args[0], o.complaint()),
        ));
    }
    Ok(o.text())
}

/// A ref, a branch or a revision expression the Hub may name: no option, no blank or control
/// character, bounded.
fn valid_ref(r: &str) -> bool {
    !r.is_empty()
        && r.len() <= 256
        && !r.starts_with('-')
        && !r.contains("..")
        && r.chars().all(|c| !c.is_whitespace() && !c.is_control())
}

/// Branch names a push takes: `steve/st-12` and the like (as the Hub's own push did).
pub(crate) fn branch_ok(b: &str) -> bool {
    !b.is_empty()
        && b.len() <= 200
        && !b.starts_with('-')
        && !b.starts_with('/')
        && !b.ends_with('/')
        && !b.ends_with(".lock")
        && !b.contains("..")
        && !b.contains("//")
        && b.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'))
}

fn remote_ok(r: &str) -> bool {
    !r.is_empty()
        && r.len() <= 64
        && !r.starts_with('-')
        && r.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

fn sha_ok(s: &str) -> bool {
    (s.len() == 40 || s.len() == 64) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The commit `r` names, `None` when it names none.
fn resolve(dir: &Path, r: &str) -> ApiResult<Option<String>> {
    if !valid_ref(r) {
        return Err(bad(format!(
            "not a ref: {}",
            r.chars().take(40).collect::<String>()
        )));
    }
    let o = read(
        dir,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            "--end-of-options",
            &format!("{r}^{{commit}}"),
        ],
    )?;
    Ok((o.code == Some(0))
        .then(|| o.text())
        .filter(|s| !s.is_empty()))
}

fn resolve_must(dir: &Path, r: &str) -> ApiResult<String> {
    resolve(dir, r)?.ok_or_else(|| bad(format!("no such ref: {r}")))
}

/// HEAD (empty on an unborn branch), its branch, and the revision of HEAD and every ref.
fn state(dir: &Path) -> ApiResult<(String, Option<String>, Revision)> {
    use sha2::Digest as _;
    let head = resolve(dir, "HEAD")?.unwrap_or_default();
    let o = read(dir, &["symbolic-ref", "--quiet", "--short", "HEAD"])?;
    let branch = (o.code == Some(0))
        .then(|| o.text())
        .filter(|b| !b.is_empty());
    let refs = must(
        dir,
        &[
            "for-each-ref",
            "--format=%(refname) %(objectname)",
            "refs/heads",
            "refs/remotes",
            "refs/tags",
        ],
    )?;
    let mut h = sha2::Sha256::new();
    for part in [
        head.as_str(),
        branch.as_deref().unwrap_or(""),
        refs.as_str(),
    ] {
        h.update(part.as_bytes());
        h.update([0]);
    }
    let rev = Revision::new(format!("g{}", &hex::encode(h.finalize())[..32]))
        .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?;
    Ok((head, branch, rev))
}

/// `-z` names, as relative paths (one that is no valid relative path is left out).
fn names(bytes: &[u8]) -> Vec<RelPath> {
    bytes
        .split(|b| *b == 0)
        .filter(|n| !n.is_empty())
        .filter_map(|n| RelPath::new(String::from_utf8_lossy(n).into_owned()).ok())
        .collect()
}

fn range(dir: &Path, from: &str, to: &str) -> ApiResult<RangeFacts> {
    let f = resolve_must(dir, from)?;
    let t = resolve_must(dir, to)?;
    let base = read(dir, &["merge-base", &f, &t])?;
    let merge_base = (base.code == Some(0))
        .then(|| base.text())
        .filter(|s| !s.is_empty());
    let counts = must(
        dir,
        &["rev-list", "--left-right", "--count", &format!("{f}...{t}")],
    )?;
    let mut n = counts
        .split_whitespace()
        .map(|x| x.parse::<u32>().unwrap_or(0));
    let (behind, ahead) = (n.next().unwrap_or(0), n.next().unwrap_or(0));
    let mut overlap = vec![];
    if let Some(mb) = &merge_base {
        let changed = |side: &str| -> ApiResult<Vec<RelPath>> {
            let o = read(
                dir,
                &["diff", "--name-only", "-z", "--no-renames", mb, side],
            )?;
            if o.code != Some(0) {
                return Err(ApiError::new(
                    ErrorCode::Internal,
                    format!("git diff: {}", o.complaint()),
                ));
            }
            Ok(names(&o.stdout))
        };
        let theirs = changed(&t)?;
        overlap = changed(&f)?
            .into_iter()
            .filter(|p| theirs.contains(p))
            .collect();
    }
    // exit 0: it merges cleanly; 1: the conflicted paths follow the tree
    let m = read(
        dir,
        &[
            "merge-tree",
            "--write-tree",
            "--name-only",
            "--no-messages",
            "-z",
            &f,
            &t,
        ],
    )?;
    let conflicts = match m.code {
        Some(0) => vec![],
        Some(1) => {
            let mut parts = m.stdout.splitn(2, |b| *b == 0);
            parts.next();
            names(parts.next().unwrap_or_default())
        }
        _ => {
            return Err(ApiError::new(
                ErrorCode::Internal,
                format!("git merge-tree: {}", m.complaint()),
            ));
        }
    };
    Ok(RangeFacts {
        from: f,
        to: t,
        merge_base,
        ahead,
        behind,
        overlap,
        conflicts,
    })
}

fn status(dir: &Path) -> ApiResult<Vec<StatusEntry>> {
    let o = read(
        dir,
        &["status", "--porcelain=v1", "-z", "--untracked-files=normal"],
    )?;
    if o.code != Some(0) {
        return Err(ApiError::new(
            ErrorCode::Internal,
            format!("git status: {}", o.complaint()),
        ));
    }
    let mut out = vec![];
    let mut parts = o.stdout.split(|b| *b == 0).filter(|p| !p.is_empty());
    while let Some(p) = parts.next() {
        if p.len() < 4 {
            continue;
        }
        let code = String::from_utf8_lossy(&p[..2]).into_owned();
        // a rename or copy names its source next
        if matches!(p[0], b'R' | b'C') {
            parts.next();
        }
        if let Ok(path) = RelPath::new(String::from_utf8_lossy(&p[3..]).into_owned()) {
            out.push(StatusEntry { path, code });
        }
    }
    Ok(out)
}

/// The configured URL of `remote`.
fn remote_url(dir: &Path, remote: &str) -> ApiResult<Option<String>> {
    if !remote_ok(remote) {
        return Err(bad("not a remote name"));
    }
    let o = read(dir, &["config", "--get", &format!("remote.{remote}.url")])?;
    Ok((o.code == Some(0))
        .then(|| o.text())
        .filter(|u| !u.is_empty()))
}

fn answer(dir: &Path, head: &str, r: &GitRead) -> ApiResult<GitReadFact> {
    let (observed, answer) = match r {
        GitRead::IsAncestor { ancestor, of } => {
            let a = resolve_must(dir, ancestor)?;
            let o = resolve_must(dir, of)?;
            let x = read(dir, &["merge-base", "--is-ancestor", &a, &o])?;
            let yes = match x.code {
                Some(0) => true,
                Some(1) => false,
                _ => {
                    return Err(ApiError::new(
                        ErrorCode::Internal,
                        format!("git merge-base: {}", x.complaint()),
                    ));
                }
            };
            (o, GitAnswer::IsAncestor { yes })
        }
        GitRead::Status => (
            head.to_string(),
            GitAnswer::Status {
                entries: status(dir)?,
            },
        ),
        GitRead::Remote { name } => (
            head.to_string(),
            GitAnswer::Remote {
                url: remote_url(dir, name)?,
            },
        ),
        _ => return Err(ApiError::new(ErrorCode::Unsupported, "this git read")),
    };
    Ok(GitReadFact {
        read: r.clone(),
        observed,
        answer,
    })
}

/// What `q` asks, in the workspace directory `dir`.
pub(crate) fn facts(dir: &Path, q: &GitQuery) -> ApiResult<GitFacts> {
    if q.fetch {
        return Err(ApiError::new(
            ErrorCode::Unsupported,
            "this node does not fetch for facts",
        ));
    }
    if q.refs.len() + q.ranges.len() + q.reads.len() > 64 {
        return Err(bad("too many questions at once"));
    }
    let (head, branch, revision) = state(dir)?;
    let dirty = !q.no_status && !status(dir)?.is_empty();
    let mut refs = vec![];
    for r in &q.refs {
        refs.push((r.clone(), resolve(dir, r)?));
    }
    let mut ranges = vec![];
    for (from, to) in &q.ranges {
        ranges.push(range(dir, from, to)?);
    }
    let mut reads = vec![];
    for r in &q.reads {
        reads.push(answer(dir, &head, r)?);
    }
    Ok(GitFacts {
        revision,
        head,
        branch,
        dirty,
        refs,
        ranges,
        reads,
    })
}

/// The test guard's flag, as the Hub reads it: on only for `1`.
fn forbid_real_hosts() -> bool {
    static FLAG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FLAG.get_or_init(|| {
        std::env::var("STEVE_FORBID_REAL_HOSTS")
            .ok()
            .is_some_and(|v| v.trim() == "1")
    })
}

/// Whether a push may go to `url`: a local path or `file://` always; any other URL only on a
/// loopback host while `forbid` (the test guard) is on.
pub(crate) fn target_allowed(forbid: bool, url: &str) -> bool {
    if !forbid || url.starts_with('/') || url.starts_with("file://") {
        return true;
    }
    let Some((_, rest)) = url.split_once("://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = if let Some(v6) = host.strip_prefix('[') {
        v6.split(']').next().unwrap_or("")
    } else {
        host.rsplit_once(':').map_or(host, |(h, _)| h)
    };
    let host = host.trim_end_matches('.');
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.to_canonical().is_loopback())
}

/// A push, checked: where it goes, what it pushes, and from which objects.
pub(crate) struct PushPlan {
    pub url: String,
    pub branch: String,
    pub sha: String,
    pub lease: Option<String>,
    pub objects: PathBuf,
    /// The header's name, when it carries a credential.
    pub header: Option<String>,
}

/// What a push's fields must be, before anything is read.
pub(crate) fn check_push_shape(a: &GitAction) -> ApiResult<()> {
    let GitAction::Push {
        remote,
        branch,
        expected_remote,
        url,
        origin_must_be,
        expected_local,
        auth,
    } = a
    else {
        return Err(ApiError::new(
            ErrorCode::Unsupported,
            "this node applies no rebase or merge yet",
        ));
    };
    if !branch_ok(branch) {
        return Err(bad("not a branch Steve pushes"));
    }
    if !remote_ok(remote) {
        return Err(bad("not a remote name"));
    }
    if !sha_ok(expected_local) || expected_remote.as_deref().is_some_and(|s| !sha_ok(s)) {
        return Err(bad("not a commit id"));
    }
    if url.is_empty()
        || url.len() > 2048
        || url.starts_with('-')
        || url.chars().any(|c| c.is_whitespace() || c.is_control())
        || !(url.starts_with('/') || url.contains("://"))
    {
        return Err(bad("not a push URL"));
    }
    if origin_must_be
        .as_deref()
        .is_some_and(|o| github_repo(&format!("https://github.com/{o}")).is_none())
    {
        return Err(bad("not owner/name"));
    }
    if let Some(a) = auth {
        let SecretDelivery::Header { name } = &a.how else {
            return Err(bad("a push credential goes as a header"));
        };
        if a.id.is_empty()
            || a.id.len() > 256
            || name.is_empty()
            || name.len() > 64
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(bad("a bad push header"));
        }
    }
    if !target_allowed(forbid_real_hosts(), url) {
        return Err(ApiError::new(ErrorCode::Denied, HOST_FORBIDDEN));
    }
    Ok(())
}

/// The push's preconditions in `dir`, now: the remote, the branch, HEAD and the refs.
pub(crate) fn check_push(dir: &Path, apply: &GitApply) -> ApiResult<PushPlan> {
    check_push_shape(&apply.action)?;
    let GitAction::Push {
        remote,
        branch,
        expected_remote,
        url,
        origin_must_be,
        expected_local,
        auth,
    } = &apply.action
    else {
        unreachable!("checked above");
    };
    if let Some(want) = origin_must_be {
        let at = remote_url(dir, remote)?;
        if at.as_deref().and_then(github_repo) != Some(want.to_lowercase()) {
            return Err(ApiError::new(ErrorCode::Denied, WRONG_REMOTE));
        }
    }
    match resolve(dir, &format!("refs/heads/{branch}"))? {
        None => return Err(ApiError::new(ErrorCode::Stale, NO_BRANCH)),
        Some(sha) if sha != *expected_local => {
            return Err(ApiError::new(ErrorCode::Stale, BRANCH_MOVED));
        }
        Some(_) => {}
    }
    let (head, _, revision) = state(dir)?;
    if revision != apply.expected || head != apply.expected_head {
        return Err(ApiError::new(ErrorCode::Stale, "changed since"));
    }
    let common = PathBuf::from(must(dir, &["rev-parse", "--git-common-dir"])?);
    let common = if common.is_absolute() {
        common
    } else {
        dir.join(common)
    };
    Ok(PushPlan {
        url: url.clone(),
        branch: branch.clone(),
        sha: expected_local.clone(),
        lease: expected_remote
            .as_ref()
            .map(|s| format!("refs/heads/{branch}:{s}")),
        objects: common.join("objects"),
        header: auth.as_ref().map(|a| match &a.how {
            SecretDelivery::Header { name } => name.clone(),
            _ => unreachable!("checked above"),
        }),
    })
}

/// `<home>/push`.
fn root(home: &Path) -> PathBuf {
    home.join("push")
}

/// Removes the bare repositories a crash left (at open: nothing runs yet).
pub(crate) fn cleanup(home: &Path) {
    let r = root(home);
    if r.exists()
        && let Err(e) = std::fs::remove_dir_all(&r)
    {
        tracing::warn!("node: could not remove leftover push repositories: {e}");
    }
}

/// The throwaway repository of one push; removed on drop.
pub(crate) struct Bare(PathBuf);

impl Drop for Bare {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_dir_all(&self.0)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!("node: could not remove a push repository: {e}");
        }
    }
}

/// A bare repository for `op` that has the plan's branch at its commit, from the workspace's
/// objects.
pub(crate) fn bare(home: &Path, op: &OperationId, plan: &PushPlan) -> Result<Bare, String> {
    use sha2::Digest as _;
    let name = hex::encode(sha2::Sha256::digest(op.as_str().as_bytes()));
    let dir = root(home).join(&name[..24]);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let b = Bare(dir);
    let run = |args: &[&str]| -> Result<(), String> {
        let o = read(&b.0, args).map_err(|e| e.safe_detail)?;
        if o.code != Some(0) {
            return Err(format!("git {}: {}", args[0], o.complaint()));
        }
        Ok(())
    };
    run(&["init", "--bare", "--quiet"])?;
    std::fs::write(
        b.0.join("objects/info/alternates"),
        format!("{}\n", plan.objects.display()),
    )
    .map_err(|e| e.to_string())?;
    run(&[
        "update-ref",
        &format!("refs/heads/{}", plan.branch),
        &plan.sha,
    ])?;
    Ok(b)
}

/// The command that pushes `plan` from `bare`: environment only, the header's value a secret.
pub(crate) fn push_run(
    bare: &Bare,
    plan: &PushPlan,
    header: Option<Zeroizing<Vec<u8>>>,
    mask: Option<Redactor>,
    deadline: i64,
) -> crate::exec::Run {
    let mut env: Vec<(String, String)> = vec![
        ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
        ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
        ("GIT_TERMINAL_PROMPT".into(), "0".into()),
        ("STEVE_PUSH_URL".into(), plan.url.clone()),
        (
            "STEVE_PUSH_REFSPEC".into(),
            format!("refs/heads/{0}:refs/heads/{0}", plan.branch),
        ),
    ];
    if let Some(l) = &plan.lease {
        env.push(("STEVE_PUSH_LEASE".into(), l.clone()));
    }
    // a transfer slower than 1 byte/s for 60 s is given up by git itself
    let mut config: Vec<(String, String)> = vec![
        ("http.lowSpeedLimit".into(), "1".into()),
        ("http.lowSpeedTime".into(), "60".into()),
    ];
    let mut secret_env = vec![];
    if let Some(h) = header {
        let at = config.len();
        config.push((format!("http.{}.extraheader", plan.url), String::new()));
        secret_env.push((format!("GIT_CONFIG_VALUE_{at}"), h));
    }
    env.push(("GIT_CONFIG_COUNT".into(), config.len().to_string()));
    for (i, (k, v)) in config.into_iter().enumerate() {
        env.push((format!("GIT_CONFIG_KEY_{i}"), k));
        if !secret_env
            .iter()
            .any(|(n, _)| *n == format!("GIT_CONFIG_VALUE_{i}"))
        {
            env.push((format!("GIT_CONFIG_VALUE_{i}"), v));
        }
    }
    let unset = UNSET
        .iter()
        .filter(|k| **k != "GIT_CONFIG_COUNT")
        .copied()
        .collect::<Vec<_>>()
        .join(" ");
    crate::exec::Run {
        dir: bare.0.clone(),
        command: format!("unset {unset}; {PUSH_SH}"),
        env,
        deadline,
        cap: PUSH_OUTPUT,
        secret_env,
        stdin: None,
        mask,
    }
}

/// git's complaint about a push, cut, as an error's detail.
pub(crate) fn complaint(stderr: &str) -> String {
    stderr.trim().chars().take(ERROR_CHARS).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_guard_allows_local_and_loopback_targets_only() {
        for u in [
            "/tmp/remotes/o/r.git",
            "file:///tmp/r.git",
            "http://127.0.0.1:9/o/r.git",
            "http://localhost/o/r.git",
            "https://x@[::1]:8/o/r.git",
        ] {
            assert!(target_allowed(true, u), "{u}");
        }
        for u in [
            "https://github.com/o/r.git",
            "https://127.0.0.1.evil.com/o/r.git",
            "https://localhost@github.com/o/r.git",
            "github.com:o/r",
        ] {
            assert!(!target_allowed(true, u), "{u}");
            assert!(target_allowed(false, u), "{u}");
        }
    }

    #[test]
    fn names_a_push_takes() {
        for b in ["steve/st-12", "main", "fix_1.2"] {
            assert!(branch_ok(b), "{b}");
        }
        for b in [
            "", "-x", "a..b", "a b", "a:b", "+main", "a/", "/a", "x.lock", "a~1",
        ] {
            assert!(!branch_ok(b), "{b}");
        }
        assert!(valid_ref("origin/main") && valid_ref("HEAD"));
        assert!(!valid_ref("--upload-pack=x") && !valid_ref("a b") && !valid_ref("a..b"));
    }
}
