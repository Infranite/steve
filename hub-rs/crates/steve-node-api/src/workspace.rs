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
    /// A worktree of `repository` on `branch` (created from `base` when the branch is new).
    Worktree {
        repository: RootPath,
        branch: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Prepare {
    /// The Hub's name for it (`ST-12`), stable across retries: preparing twice gives the same workspace.
    pub key: String,
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RangeFacts {
    pub from: String,
    pub to: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge_base: Option<String>,
    pub ahead: u32,
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
    pub revision: Revision,
    pub head: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    pub dirty: bool,
    #[serde(default)]
    pub refs: Vec<(String, Option<String>)>,
    #[serde(default)]
    pub ranges: Vec<RangeFacts>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GitAction {
    Rebase {
        onto: String,
    },
    Merge {
        commit: String,
    },
    /// `expected_remote` is the remote ref's sha the push may replace (`None`: it must not exist).
    Push {
        remote: String,
        branch: String,
        expected_remote: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitApply {
    pub workspace: WorkspaceRef,
    pub expected_head: String,
    pub action: GitAction,
    /// A credential the Hub hands over for this one action (a push token), by reference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret: Option<SecretRef>,
}

/// A secret delivered for one operation over the non-persistent channel; never its bytes here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretRef {
    pub id: String,
    /// `env:NAME`, `stdin`, or `file` (the last one only where the Node can keep it off disk).
    pub how: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecSpec {
    pub workspace: WorkspaceRef,
    /// `sh -c` text, run in the workspace (or `cwd` inside it).
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<RelPath>,
    pub policy: ExecutionPolicy,
    pub limits: Limits,
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRead {
    pub path: PathRef,
    /// `None` reads whatever is there; otherwise a changed file answers `conflict`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected: Option<Revision>,
    pub offset: u64,
    pub max: u32,
}

/// A directory tree to list for a knowledge source (or a project's description).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanSpec {
    pub root: RootPath,
    #[serde(default)]
    pub include: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
}

/// One page of a scan. Only a scan that ends with `complete` may be used to decide a file is gone; a
/// cut-short one (offline, limits, a refused path) keeps the old index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanPage {
    pub scan: OperationId,
    pub entries: Vec<FileInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<Cursor>,
    pub complete: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub issues: Vec<String>,
}
