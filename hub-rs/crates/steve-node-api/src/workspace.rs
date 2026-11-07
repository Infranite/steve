//! Working directories, git, commands and files on a Node. The Hub decides when to prepare, tidy,
//! merge or publish; the Node returns facts and applies an action only when its preconditions still
//! hold. No arbitrary git argv, no decision (merge or rebase, resolve a conflict, delete a branch).

use serde::{Deserialize, Serialize};

use crate::ids::*;
use crate::run::{ExecutionPolicy, Limits, PathRef, WorkspaceRef};

/// A place under one of the Node's allowed roots.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RootPath {
    pub root: RootId,
    pub rel: RelPath,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PrepareKind {
    /// Work in the directory itself.
    Existing { dir: RootPath },
    /// A fresh scratch directory owned by the Node.
    Scratch,
    /// A worktree of `repository` on `branch` (created from `base` when the branch is new), at
    /// `at` when given (a directory that need not exist yet, under a read-write root; one that is
    /// there already is adopted only if it is that worktree), else where the Node keeps its own.
    Worktree {
        repository: RootPath,
        branch: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        at: Option<RootPath>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Prepare {
    /// The Hub's stable id of what the workspace is for, namespaced (`item:it_3f2a…`, not the visible
    /// `ST-12`). Preparing again with the same resource and the same kind gives the same workspace;
    /// with another kind, repository or branch it is a conflict.
    pub resource: String,
    pub kind: PrepareKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceView {
    pub workspace: WorkspaceRef,
    pub revision: Revision,
    /// Exists and is writable.
    pub usable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// What to show a person ("~/code/steve/.worktrees/ST-12"); never an address.
    pub display: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tidy {
    pub workspace: WorkspaceRef,
    pub expected: Revision,
    pub keep_branch: bool,
}

/// Which facts to collect, in one round trip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitQuery {
    pub workspace: WorkspaceRef,
    /// Refs to resolve (`HEAD`, `main`, `origin/main`).
    #[serde(default)]
    pub refs: Vec<String>,
    /// Pairs to compute the merge base of and the commits between.
    #[serde(default)]
    pub ranges: Vec<(String, String)>,
    /// Whether to fetch first (a network call on the Node).
    #[serde(default)]
    pub fetch: bool,
    /// Further reads, answered in [`GitFacts::reads`] in the same order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reads: Vec<GitRead>,
    /// Leave the working tree out: no status scan, `dirty` is false and the revision is HEAD and
    /// the branch only (never equal to one with the tree in it). For reads that never look at the
    /// tree, such as a preview of a commit or a branch. A Node that does not know it scans as
    /// before (a fuller revision, never a wrong one). A full `git status` is the slow part of a query
    /// that only asks for refs or reads.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub heads_only: bool,
}

/// A commit range: `from..to`, or everything reachable from `to` when `from` is `None`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevRange {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    pub to: String,
}

/// One read-only git question. Each kind is a fixed command on the Node; there is no free argv.
/// `max` bounds what comes back (bytes of content, or entries of a log).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GitRead {
    /// A file at `rev` (`rev:path`), or with no path the commit itself (message, stat and patch).
    /// A `rev` that names no commit answers a `Blob` with `found: false`, with or without a path.
    Show {
        rev: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<RelPath>,
        max: u32,
    },
    /// Commits of `range`, newest first, at most `max`.
    Log { range: RevRange, max: u32 },
    /// Stat and patch of `range`, the patch cut at `max` bytes.
    Diff { range: RevRange, max: u32 },
    /// Whether `ancestor` is an ancestor of (or equal to) `of`.
    IsAncestor { ancestor: String, of: String },
    /// The working tree's changes (what `status --porcelain` lists).
    Status,
    /// The tree the working directory would commit as, untracked files included, without
    /// touching the index (it changes whenever the content does).
    Fingerprint,
    /// Whether `rel` is ignored, and whether anything under it is tracked.
    TrackedIgnored { rel: RelPath },
    /// The paths that differ between two trees or commits (`diff --name-only`), at most `max`.
    ChangedPaths { from: String, to: String, max: u32 },
    /// The commit on `onto` that brought `commit`'s change in: `commit` itself when it is on
    /// `onto`, the rebased copy of its last commit, or a squash of all of its own. What the Node
    /// has locally (fetch first).
    Landed { commit: String, onto: String },
    /// The tags `remote` has and the commits they point at (annotated tags peeled), as the remote
    /// says (`ls-remote`): a network call, under the fetch deadline.
    RemoteTags { remote: String },
    /// The URL `remote` is configured with (`insteadOf` not applied), never with credentials.
    RemoteUrl { remote: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitInfo {
    pub sha: String,
    pub subject: String,
    pub author: String,
    pub time: Timestamp,
    /// The hash as git abbreviates it in this repository (`%h`: its length grows with the
    /// repository and follows `core.abbrev`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abbrev: Option<String>,
    /// The full message, for a single commit's `Show`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusEntry {
    pub path: RelPath,
    /// The two porcelain status letters (`" M"`, `"??"`, `"UU"`).
    pub code: String,
}

/// The answer to one [`GitRead`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GitAnswer {
    /// `Show` with a path: a missing file at that revision is `found: false`. Also `Show` without
    /// a path of a revision that names no commit.
    Blob {
        found: bool,
        #[serde(with = "crate::blob::hexbytes")]
        bytes: Vec<u8>,
        truncated: bool,
    },
    /// `Show` without a path.
    Commit {
        commit: CommitInfo,
        stat: String,
        patch: String,
        truncated: bool,
    },
    Log {
        commits: Vec<CommitInfo>,
        truncated: bool,
    },
    Diff {
        stat: String,
        patch: String,
        truncated: bool,
    },
    IsAncestor {
        yes: bool,
    },
    Status {
        entries: Vec<StatusEntry>,
    },
    Fingerprint {
        tree: String,
        /// The item's own change in that tree: where its branch left the default branch (the
        /// nearest of the remote's and the local one) and the patch id of the diff from there
        /// (`-` for none). The same change rebased keeps its patch id. `None`: no such base.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        own: Option<OwnChange>,
    },
    TrackedIgnored {
        ignored: bool,
        tracked: bool,
    },
    ChangedPaths {
        paths: Vec<String>,
        truncated: bool,
    },
    /// `commit: None`: the change is not on `onto`.
    Landed {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        commit: Option<String>,
    },
    /// `(name, commit)` pairs, in the remote's order.
    RemoteTags {
        tags: Vec<(String, String)>,
    },
    /// `None`: no such remote.
    RemoteUrl {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
    },
}

/// See [`GitAnswer::Fingerprint`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnChange {
    pub base: String,
    pub patch: String,
}

/// A read with its answer and the commit it was answered at: the resolved `rev` of a `Show`, the
/// resolved `to` of a range, HEAD for the working-tree reads. A later decision checks it again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitReadFact {
    pub read: GitRead,
    pub observed: String,
    pub answer: GitAnswer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RangeFacts {
    pub from: String,
    pub to: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge_base: Option<String>,
    /// Commits of `to` that `from` lacks (`from..to`).
    pub ahead: u32,
    /// Commits of `from` that `to` lacks (`to..from`).
    pub behind: u32,
    /// Files both sides touched (for the merge lane's overlap check).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overlap: Vec<RelPath>,
    /// Would merging conflict, and where.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conflicts: Vec<RelPath>,
}

/// What git says, with the revision it was observed at (an apply checks it again).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitFacts {
    /// Changes whenever HEAD or any ref (branches, remote-tracking refs, tags) moves.
    pub revision: Revision,
    pub head: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    pub dirty: bool,
    #[serde(default)]
    pub refs: Vec<(String, Option<String>)>,
    #[serde(default)]
    pub ranges: Vec<RangeFacts>,
    /// The answers to [`GitQuery::reads`], in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reads: Vec<GitReadFact>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GitAction {
    /// Rebases the branch checked out in the workspace onto `onto`. On a conflict the Node aborts
    /// the rebase (the worktree as before) and the operation fails with `conflict`, its detail the
    /// conflicting paths one per line; uncommitted changes to tracked files fail it with `busy`.
    Rebase { onto: String },
    /// Merges `commit` into HEAD. On a conflict the Node aborts the merge when
    /// `back_out_on_conflict` (the worktree as before, the answer a `conflict`), or leaves it in
    /// progress for the session to resolve.
    Merge {
        commit: String,
        /// The merge commit's message; `None` takes git's default.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
        #[serde(default = "yes")]
        back_out_on_conflict: bool,
    },
    /// Pushes the local `branch` (which must be at `expected_local`) to the same branch at `url`.
    /// `expected_remote` is the remote ref's sha the push may replace, whatever it is (a lease);
    /// `None` is a plain push, which the remote takes as a new branch or a fast-forward only. The
    /// push runs from a throwaway repository that borrows the workspace's objects, with no system
    /// or global git config: the workspace's own config (hooks, `insteadOf`, credential helpers)
    /// plays no part.
    Push {
        remote: String,
        branch: String,
        expected_remote: Option<String>,
        /// The canonical target (`https://github.com/owner/name.git`), pushed to directly: what
        /// the repository's own remote config says never redirects it.
        url: String,
        /// `owner/name` the workspace's `remote` must point at, or the push is refused.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        origin_must_be: Option<String>,
        expected_local: String,
        /// The credential, taken from [`crate::ports::HubSecrets`] for this one push and handed to
        /// git as an HTTP header ([`SecretDelivery::Header`]); never on argv, never on disk.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        auth: Option<SecretRef>,
    },
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitApply {
    pub workspace: WorkspaceRef,
    /// The revision of the [`GitFacts`] the decision was made on (refs, index, dirty state).
    pub expected: Revision,
    pub expected_head: String,
    pub action: GitAction,
}

/// A secret delivered for one operation over the non-persistent channel; never its bytes here. The
/// Node takes the bytes with [`crate::ports::HubSecrets::take`] under the operation's id, once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretRef {
    pub id: String,
    pub how: SecretDelivery,
}

/// How the process gets the secret. `File` writes a temporary file: a Node offers it only when it
/// says so in its capabilities (`secret-file`), and the Hub allows it only for Nodes it trusts with
/// that (the Hub's own machine, for now).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SecretDelivery {
    Env {
        name: String,
    },
    Stdin,
    File {
        env: String,
    },
    /// An HTTP header for git (`name: <the secret>`), passed through git's environment config
    /// like `Env`: only for [`GitAction::Push`].
    Header {
        name: String,
    },
}

/// The bytes of a secret on their way to the one operation that uses them. Never printed: `Debug`
/// shows its length only; wiped on drop.
#[derive(PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretBytes(#[serde(with = "crate::blob::hexbytes")] Vec<u8>);

impl SecretBytes {
    pub fn new(bytes: Vec<u8>) -> Self {
        SecretBytes(bytes)
    }
    pub fn expose(&self) -> &[u8] {
        &self.0
    }
}

/// The bytes are wiped when it goes.
impl Drop for SecretBytes {
    fn drop(&mut self) {
        zeroize::Zeroize::zeroize(&mut self.0);
    }
}

impl std::fmt::Debug for SecretBytes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SecretBytes(<{} bytes>)", self.0.len())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecSpec {
    pub workspace: WorkspaceRef,
    /// `sh -c` text, run in the workspace (or `cwd` inside it).
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<RelPath>,
    pub policy: ExecutionPolicy,
    /// `limits.deadline` is the deadline: past it the whole process group is killed.
    pub limits: Limits,
    /// Plain environment for the command (never a secret: those go in `secrets`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<(String, String)>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secrets: Vec<SecretRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecResult {
    pub exit_code: Option<i32>,
    /// Bounded and with secrets masked.
    pub stdout: String,
    pub stderr: String,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileInfo {
    pub path: RelPath,
    pub is_dir: bool,
    pub size: u64,
    /// Changes when the file does (mtime + size, or a hash).
    pub version: Revision,
    /// When it was last modified, as the Node's filesystem says (what a knowledge folder compares
    /// with size to tell a changed file). `None`: the Node does not say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mtime: Option<Timestamp>,
    /// Over the scan's `max_file_bytes`: listed, so it is not taken for gone, but not to be read.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub too_big: bool,
}

/// A file on a Node: inside a workspace, or under an allowed root read as is (a knowledge folder,
/// never made a workspace).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "in", rename_all = "snake_case")]
pub enum FileAddress {
    Workspace(PathRef),
    Root(RootPath),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRead {
    pub path: FileAddress,
    /// `None` reads whatever is there; otherwise a changed file answers `conflict` (read again).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected: Option<Revision>,
    pub offset: u64,
    pub max: u32,
}

/// A file produced by one accepted execution. Unlike a general [`FileRead`], this cannot name
/// an arbitrary root: the Node takes the directory from the execution's durable artifact grant.
/// The Hub authorizes the reader before calling this face. A digest is a scope binding, not a
/// bearer token. Initially only session turns with an explicit artifact view are supported.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactRead {
    pub execution: crate::common::ExecutionRef,
    pub view_digest: Digest,
    /// Plain relative names below the execution's workspace; no symlink is followed.
    pub location: RelPath,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected: Option<Revision>,
    pub offset: u64,
    /// Bounded by the Node and transport even when the caller asks for more.
    pub max: u32,
}

/// A directory tree to list for a knowledge source (or a project's description).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanSpec {
    pub root: RootPath,
    /// Files to list ([`crate::glob`]); empty is [`crate::glob::DEFAULT_INCLUDE`].
    #[serde(default)]
    pub include: Vec<String>,
    /// Files and directories to leave out ([`crate::glob::Rules::skips_dir`]).
    #[serde(default)]
    pub exclude: Vec<String>,
    #[serde(default)]
    pub limits: ScanLimits,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanLimits {
    /// Wanted files beyond this are left out, and the scan is incomplete.
    pub max_files: u32,
    /// Bigger files are listed with [`FileInfo::too_big`].
    pub max_file_bytes: u64,
}

impl Default for ScanLimits {
    /// The knowledge folder's limits.
    fn default() -> Self {
        ScanLimits {
            max_files: 20_000,
            max_file_bytes: 2 * 1024 * 1024,
        }
    }
}

/// Something a scan reports besides its entries. Two kinds, as the knowledge folder's walk has
/// always had them ([`ScanIssue::hides_files`]): a gap (an unreadable directory or entry, files past
/// `max_files`, a cut-short scan) may hide files, and makes the scan incomplete; a hold is one
/// wanted path the scan saw but did not list, with why, and leaves the rest of the scan whole.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ScanIssue {
    /// Gap: a directory that could not be listed, or an entry whose name or type could not be
    /// read (so it may be a directory). An entry without a readable name has no path.
    Unreadable {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<RelPath>,
    },
    /// Gap: `count` wanted files past `max_files`.
    OverLimit { count: u64 },
    /// Gap: cut short (a restart, a cancel, the deadline).
    Interrupted,
    /// Hold: a wanted path that is there but was not listed, and why. A dangling link is neither
    /// listed nor held: its file is gone.
    Held { path: RelPath, why: HeldWhy },
}

/// Why a scan held a path back, and what that means for the file's document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeldWhy {
    /// A link resolving outside the root: not followed, its document fails (as reading it would).
    Outside,
    /// A path the Node's rules refuse: its document fails.
    Denied,
    /// Its metadata, or its link, could not be read (a loop, a permission): its document stays.
    Unreadable,
}

impl ScanIssue {
    /// A gap: the scan may not show every file that is there.
    pub fn hides_files(&self) -> bool {
        !matches!(self, ScanIssue::Held { .. })
    }
}

/// One page of a scan, its paths relative to the scanned root ([`RootPath::root`], usable as a
/// [`FileAddress::Root`]). Only a scan that ends with `complete` may be used to decide a file is gone;
/// a cut-short one (offline, limits, an unreadable directory) keeps the old index. Pages before the
/// last one are never complete, and no page with a gap ([`ScanIssue::hides_files`]) is; held paths
/// leave a scan complete.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanPage {
    pub scan: OperationId,
    /// The version of this scan's manifest; every page of one scan has the same.
    pub manifest: Revision,
    pub entries: Vec<FileInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<Cursor>,
    pub complete: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub issues: Vec<ScanIssue>,
}

impl ScanPage {
    /// The rule above: `complete` only on the last page and only with no gap.
    pub fn is_consistent(&self) -> bool {
        !self.complete || (self.next.is_none() && !self.issues.iter().any(ScanIssue::hides_files))
    }
}

/// `owner/name` of a GitHub remote URL (https, `git@github.com:`, `ssh://`, `git://`), lowercased;
/// `None` for anything else.
pub fn github_repo(url: &str) -> Option<String> {
    let u = url.trim();
    let rest = u
        .strip_prefix("https://github.com/")
        .or_else(|| u.strip_prefix("http://github.com/"))
        .or_else(|| u.strip_prefix("git@github.com:"))
        .or_else(|| u.strip_prefix("ssh://git@github.com/"))
        .or_else(|| u.strip_prefix("git://github.com/"))
        .or_else(|| {
            // https://user@github.com/o/r
            u.strip_prefix("https://")
                .and_then(|r| r.split_once('@'))
                .and_then(|(_, r)| r.strip_prefix("github.com/"))
        })?;
    let rest = rest.trim_end_matches('/');
    let rest = rest.strip_suffix(".git").unwrap_or(rest);
    let (o, n) = rest.split_once('/')?;
    let ok = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
    };
    (ok(o) && ok(n) && !n.contains('/')).then(|| format!("{o}/{n}").to_lowercase())
}
