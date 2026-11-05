//! The two ways into GitHub, as connection adapters of the A2 framework
//! (`SystemAdapter` = [`steve_modules::accounts::connections::ConnectionProvider`]).

pub mod github_app;
pub mod github_pat;

pub use github_app::GitHubApp;
pub use github_pat::GitHubPat;

/// Capabilities of both.
pub const CAPS: [&str; 3] = ["repo.read", WRITE_CAP, "events"];
/// What a write to a repo needs (push, PR, merge, comment, rerun).
pub const WRITE_CAP: &str = "repo.write";

/// Both GitHub connections on the 接入 page: provider `github`, one area, 代码仓库.
pub fn integration(identity: &str) -> steve_modules::accounts::connections::Integration {
    use steve_modules::accounts::connections::{CapSpec, Integration};
    Integration {
        provider: "github".into(),
        provider_name: "GitHub".into(),
        identity: identity.into(),
        capabilities: vec![
            CapSpec::supported("repo", &["repo.read", WRITE_CAP])
                .with_note(&steve_i18n::t!("github.provider.note")),
        ],
    }
}
