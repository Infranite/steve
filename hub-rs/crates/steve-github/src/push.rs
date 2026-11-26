//! Pushing an item's branch via the Node that holds its workspace (`GitNodes`).
//!
//! The Hub never invokes `git` directly or reads project directories on disk: every git read
//! (`ahead`, `head_of`, `origins`) and branch push runs on the target Node over `steve-node-api`
//! with one-shot credentials from `HubSecrets`.

use std::path::Path;

/// Why a push did not happen. Chinese, never containing the token.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PushError {
    #[error("分支名不对：{0}")]
    BadBranch(String),
    #[error("工作目录的 origin 不是 {expected}")]
    WrongRemote { expected: String },
    #[error("分支 {0} 不存在")]
    NoBranch(String),
    /// The branch is not at the commit it must be at.
    #[error("分支 {branch} 在批准后有了新的提交（现在是 {now}），需要重新确认")]
    Moved { branch: String, now: String },
    #[error("git 失败：{0}")]
    Git(String),
    /// Refused by the test guard (`STEVE_FORBID_REAL_HOSTS=1`), nothing pushed.
    #[error("{0}")]
    HostForbidden(String),
    /// The workspace's Node could not be asked (offline, or no workspace there): nothing pushed.
    #[error("{0}")]
    Node(String),
    /// No Node is attached or holds this item's workspace (Hub never falls back to local `git`).
    #[error("没有可用的设备节点执行 Git 操作：{0}")]
    NoNodeAvailable(String),
    /// A push through a Node whose end is not known (a restart, the deadline): it may have landed,
    /// and is never pushed again; what the remote branch is says.
    #[error("推送的结果未知，不会再推送一次")]
    Lost,
    /// A push through a Node that has not ended yet (still on its way, or still running there):
    /// asked again later, never compared with GitHub while it may still land.
    #[error("推送还没有结束，稍后再看")]
    Running,
}

/// A push through a Node ([`GitNodes::push`]).
pub struct NodePush<'a> {
    pub item: &'a str,
    /// What the push is for (the decision): asked again, never pushed twice.
    pub key: &'a str,
    pub branch: &'a str,
    /// `owner/name` of the linked repo.
    pub full_name: &'a str,
    /// `<git_base>/<owner>/<name>.git`.
    pub url: &'a str,
    /// Installation token or PAT.
    pub token: &'a str,
    /// The commit the branch must be at.
    pub expect: &'a str,
}

/// An item's (or a project's) git on the Node that holds it (the Hub's own machine included).
/// `None` means no Node holds that item/project here (callers return [`PushError::NoNodeAvailable`],
/// never falling back to Hub-local `git`).
#[async_trait::async_trait]
pub trait GitNodes: Send + Sync {
    /// Whether the item's `branch` has commits `base` lacks (`origin/<base>` when the workspace
    /// has it, else `<base>`).
    async fn ahead(&self, item: &str, base: &str, branch: &str) -> Option<Result<bool, PushError>>;
    /// The commit the item's `branch` is at.
    async fn head_of(&self, item: &str, branch: &str) -> Option<Result<String, PushError>>;
    /// Pushes the item's branch; the pushed sha.
    async fn push(&self, p: &NodePush<'_>) -> Option<Result<String, PushError>>;
    /// For each of `projects`, in order: `owner/name` of the project directory's `origin`, when it
    /// is a GitHub repository (`None` on any problem).
    async fn origins(&self, projects: &[String]) -> Option<Vec<Option<String>>>;
}

/// Branch names Steve pushes: `steve/st-12` and the like. No `..`, no leading `-`, no spaces or
/// refspec characters.
pub fn branch_ok(b: &str) -> bool {
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

/// The test guard on the push target: a local path (a test's bare remotes) is fine, a URL must be
/// on loopback when `STEVE_FORBID_REAL_HOSTS=1`.
pub(crate) fn check_git_base(base: &str) -> Result<(), PushError> {
    if Path::new(base).is_absolute() {
        return Ok(());
    }
    steve_modules::net::check_url(base).map_err(|e| PushError::HostForbidden(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branch_names() {
        for b in ["steve/st-12", "main", "fix_1.2"] {
            assert!(branch_ok(b), "{b}");
        }
        for b in [
            "", "-x", "a..b", "a b", "a:b", "+main", "a/", "/a", "x.lock", "a~1",
        ] {
            assert!(!branch_ok(b), "{b}");
        }
    }

    #[test]
    fn real_git_base_refused_under_the_flag() {
        if !steve_modules::net::forbidding_child(
            "push::tests::real_git_base_refused_under_the_flag",
        ) {
            return;
        }
        assert_eq!(
            check_git_base("https://github.com"),
            Err(PushError::HostForbidden(
                "测试环境禁止连接外部主机：github.com".into()
            ))
        );
        assert_eq!(check_git_base("/tmp/steve-fake-remotes"), Ok(()));
        assert_eq!(check_git_base("http://127.0.0.1:9/git"), Ok(()));
    }
}
