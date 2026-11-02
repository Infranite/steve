//! What a directory (an office IM's address book) hands the sync: one full snapshot of the departments
//! and people the connection can see (contacts.md §7). Feishu's implementation lives with the channels
//! module (`feishu::directory`, slice C7/F5); [`FakeDirectory`] is the test double.

use std::sync::Mutex;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// A department, by the directory's own id.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Department {
    /// Stable id in the directory (Feishu `open_department_id`).
    pub id: String,
    pub name: String,
    /// The parent's id; `None` (or an id not in the snapshot, like Feishu's root `"0"`) = top level.
    pub parent_id: Option<String>,
    /// The leader's [`DirUser::id`].
    pub leader_id: Option<String>,
}

/// A person in the directory.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DirUser {
    /// Primary handle in [`DirectoryAdapter::system`] (Feishu `union_id`, the same in every app).
    pub id: String,
    /// Handles valid only under this connection (Feishu `open_id`), recorded with the connection id.
    pub extra_ids: Vec<String>,
    pub name: String,
    /// Work and personal email addresses; they attach to the contact as mail accounts.
    pub emails: Vec<String>,
    pub title: Option<String>,
    /// The manager's [`DirUser::id`] (`reports_to`).
    pub leader_id: Option<String>,
    pub department_ids: Vec<String>,
    /// Left the company (Feishu `status.is_resigned`): removed from teams, the contact stays.
    pub departed: bool,
}

/// One full read of the directory. Anything missing from it is gone from the source.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Snapshot {
    pub departments: Vec<Department>,
    pub users: Vec<DirUser>,
}

/// Why a snapshot could not be read. The sync job fails and changes nothing.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DirectoryError {
    /// Credentials rejected or the directory scope is not granted.
    #[error("通讯录没有授权：{0}")]
    Auth(String),
    /// Network, rate limit, server errors.
    #[error("读取通讯录失败：{0}")]
    Unavailable(String),
}

/// How much of the organization the connection may see (the app's contact visibility range), as the
/// last [`DirectoryAdapter::snapshot`] found it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DirScope {
    /// The whole organization is visible.
    pub whole: bool,
    /// Departments in the range (not counting their descendants).
    pub departments: usize,
    /// People in the range by themselves (or through user groups).
    pub users: usize,
}

/// A directory behind one connection (capability `directory.read`).
#[async_trait]
pub trait DirectoryAdapter: Send + Sync {
    /// The accounts system of [`DirUser::id`] (`feishu`).
    fn system(&self) -> &str;

    /// Reads every visible department and person. Read-only: the sync never writes back (§8).
    async fn snapshot(&self) -> Result<Snapshot, DirectoryError>;

    /// The visibility range the last snapshot read, when the directory tells.
    fn scope(&self) -> Option<DirScope> {
        None
    }

    /// Provider permissions the last snapshot found missing: `(scope, what it is for)`.
    fn missing_scopes(&self) -> Vec<(String, String)> {
        Vec::new()
    }
}

/// A directory whose snapshot the test sets. Counts reads.
pub struct FakeDirectory {
    system: String,
    next: Mutex<Result<Snapshot, DirectoryError>>,
    reads: Mutex<usize>,
}

impl FakeDirectory {
    /// A fake of `system` (an accounts system id such as `feishu`) answering `snapshot`.
    pub fn new(system: &str, snapshot: Snapshot) -> Self {
        Self {
            system: system.into(),
            next: Mutex::new(Ok(snapshot)),
            reads: Mutex::new(0),
        }
    }

    /// The next reads answer this snapshot.
    pub fn set(&self, snapshot: Snapshot) {
        *lock(&self.next) = Ok(snapshot);
    }

    /// Changes the current snapshot in place.
    pub fn update(&self, f: impl FnOnce(&mut Snapshot)) {
        let mut g = lock(&self.next);
        if let Ok(s) = g.as_mut() {
            f(s);
        }
    }

    /// The next reads fail.
    pub fn fail(&self, e: DirectoryError) {
        *lock(&self.next) = Err(e);
    }

    pub fn reads(&self) -> usize {
        *lock(&self.reads)
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[async_trait]
impl DirectoryAdapter for FakeDirectory {
    fn system(&self) -> &str {
        &self.system
    }

    async fn snapshot(&self) -> Result<Snapshot, DirectoryError> {
        *lock(&self.reads) += 1;
        lock(&self.next).clone()
    }
}
