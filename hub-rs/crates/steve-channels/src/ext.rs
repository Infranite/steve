//! What a channel-side adapter hands the Hub's extension entry (`steve_hub::extension`): one
//! [`ChannelSide`] per `connections.adapter`, holding the views of the adapter the Hub registers
//! with `Connections`, [`crate::ChannelsBuilder`] and the [`crate::Outbox`].
//!
//! Part of the public extension API. The declaration grows field by field; build it with
//! [`ChannelDecl::new`] so new fields keep their defaults.

use std::sync::Arc;

use steve_modules::accounts::connections::ConnectionProvider;

use crate::adapters::feishu::SendAsUser;
use crate::channel::{ChannelAdapter, ProfileAdapter};

/// How a channel-side adapter behaves. For now only which `connections.adapter` it serves.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ChannelDecl {
    /// `connections.adapter`: every view in the [`ChannelSide`] answers to this id.
    pub adapter: String,
}

impl ChannelDecl {
    pub fn new(adapter: impl Into<String>) -> Self {
        Self {
            adapter: adapter.into(),
        }
    }
}

/// One channel-side adapter. `provider` is required (how a connection is made); the other views
/// are optional and usually the same object as `provider`.
#[derive(Clone)]
pub struct ChannelSide {
    pub decl: ChannelDecl,
    /// Connecting, refreshing, verifying, health checks (registered with `Connections`).
    pub provider: Arc<dyn ConnectionProvider>,
    /// Bot-style: long connection or webhook, sending as the connection's own identity.
    pub adapter: Option<Arc<dyn ChannelAdapter>>,
    /// Sending, replying and reacting as you (the outbox's `Mode::Own`).
    pub as_user: Option<Arc<dyn SendAsUser>>,
    /// Looking people up on the platform.
    pub profiles: Option<Arc<dyn ProfileAdapter>>,
}

impl ChannelSide {
    pub fn new(decl: ChannelDecl, provider: Arc<dyn ConnectionProvider>) -> Self {
        Self {
            decl,
            provider,
            adapter: None,
            as_user: None,
            profiles: None,
        }
    }

    pub fn adapter(mut self, a: Arc<dyn ChannelAdapter>) -> Self {
        self.adapter = Some(a);
        self
    }

    pub fn as_user(mut self, a: Arc<dyn SendAsUser>) -> Self {
        self.as_user = Some(a);
        self
    }

    pub fn profiles(mut self, p: Arc<dyn ProfileAdapter>) -> Self {
        self.profiles = Some(p);
        self
    }

    /// Every view answers to `decl.adapter`; the first one that does not, otherwise.
    pub fn check(&self) -> Result<(), String> {
        let want = self.decl.adapter.as_str();
        if want.is_empty() {
            return Err("a channel side without an adapter id".into());
        }
        let views = [
            ("provider", Some(self.provider.id())),
            ("channel adapter", self.adapter.as_ref().map(|a| a.id())),
            (
                "profile adapter",
                self.profiles.as_ref().map(|p| p.adapter_id()),
            ),
        ];
        for (what, id) in views {
            if let Some(id) = id
                && id != want
            {
                return Err(format!("{what} {id} declared for adapter {want}"));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::feishu::{Domain, FeishuApp, FeishuEnv};

    #[test]
    fn every_view_answers_to_the_declared_adapter() {
        let app = Arc::new(FeishuApp::new());
        let ok = ChannelSide::new(ChannelDecl::new("feishu_app"), app.clone())
            .adapter(app.clone())
            .profiles(app.clone());
        assert_eq!(ok.check(), Ok(()));
        let user = Arc::new(FeishuEnv::from_env(|_| None).user_provider(Domain::Feishu));
        let wrong = ChannelSide::new(ChannelDecl::new("feishu_app"), app.clone()).profiles(user);
        assert_eq!(
            wrong.check(),
            Err("profile adapter feishu_user declared for adapter feishu_app".into())
        );
        let unnamed = ChannelSide::new(ChannelDecl::new(""), app);
        assert!(unnamed.check().is_err());
    }
}
