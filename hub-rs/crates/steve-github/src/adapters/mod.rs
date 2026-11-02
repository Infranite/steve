//! The two ways into GitHub (github.md §1), as connection adapters of the A2 framework
//! (`SystemAdapter` = [`steve_modules::accounts::connections::ConnectionProvider`]).

pub mod github_app;
pub mod github_pat;

pub use github_app::GitHubApp;
pub use github_pat::GitHubPat;

/// Capabilities of both (§1.1 step 3).
pub const CAPS: [&str; 3] = ["repo.read", "repo.write", "events"];

/// Both GitHub connections on the 接入 page: provider `github`, one area, 代码仓库.
pub fn integration(identity: &str) -> steve_modules::accounts::connections::Integration {
    use steve_modules::accounts::connections::{CapSpec, Integration};
    Integration {
        provider: "github".into(),
        provider_name: "GitHub".into(),
        identity: identity.into(),
        capabilities: vec![
            CapSpec::supported("repo", &["repo.read", "repo.write"])
                .with_note("关联仓库、开 PR、接收检查和评论事件"),
        ],
    }
}
