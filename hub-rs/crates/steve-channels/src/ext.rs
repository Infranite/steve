//! What a channel-side adapter hands the Hub's extension entry (`steve_hub::extension`): one
//! [`ChannelSide`] per `connections.adapter`, holding the views of the adapter the Hub registers
//! with `Connections`, [`crate::ChannelsBuilder`] and the [`crate::Outbox`], and what runs for each
//! of its connections ([`ConnectionHook`]).
//!
//! Part of the public extension API. The declaration grows field by field; build it with
//! [`ChannelDecl::new`] so new fields keep their defaults.

use std::sync::Arc;

use serde::Serialize;
use steve_modules::accounts::connections::ConnectionProvider;

use crate::channel::{ActAsUser, ChannelAdapter, ProfileAdapter};

pub use crate::channel::{ActTarget, MessageRef, SendKey, Sent};
pub use crate::lifecycle::{ConnRun, ConnSink, ConnState, ConnectionHook};
pub use crate::media::{MediaError, MediaFetch};
pub use crate::queries::{ConnQueries, Queries};
pub use crate::spaces::Mode;
pub use steve_modules::accounts::connections::{PAIR_KEY, Paired, Pairing};
pub use tokio_util::sync::CancellationToken;

/// How the messages of a connection come in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Receive {
    /// A connection the adapter keeps open (a websocket).
    LongConnection,
    /// The provider calls the Hub.
    Webhook,
    /// The adapter asks the provider every so often.
    Poll,
    /// A stream the adapter reads.
    Stream,
}

/// How a channel-side adapter behaves.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ChannelDecl {
    /// `connections.adapter`: every view in the [`ChannelSide`] answers to this id.
    pub adapter: String,
    /// Whose identity a connection is: a bot's, or yours. Its `channel_settings` get this mode
    /// before its [`ConnectionHook`] first runs. Default: [`Mode::Bot`].
    pub mode: Mode,
    /// The capability a connection needs turned on to receive (`msg.receive`); `None`: receiving
    /// is not a capability of its own. Default: `None`.
    pub receive_cap: Option<String>,
    /// How messages come in. Default: [`Receive::LongConnection`].
    pub receive: Receive,
}

impl ChannelDecl {
    pub fn new(adapter: impl Into<String>) -> Self {
        Self {
            adapter: adapter.into(),
            mode: Mode::Bot,
            receive_cap: None,
            receive: Receive::LongConnection,
        }
    }

    pub fn mode(mut self, m: Mode) -> Self {
        self.mode = m;
        self
    }

    /// How messages come in, and the capability that turns receiving on (one of the provider's).
    pub fn receive(mut self, how: Receive, cap: Option<&str>) -> Self {
        self.receive = how;
        self.receive_cap = cap.map(str::to_string);
        self
    }
}

/// One channel-side adapter. `provider` is required (how a connection is made); the other views
/// are optional and usually the same object as `provider`.
#[derive(Clone)]
#[non_exhaustive]
pub struct ChannelSide {
    pub decl: ChannelDecl,
    /// Connecting, refreshing, verifying, health checks (registered with `Connections`).
    pub provider: Arc<dyn ConnectionProvider>,
    /// Bot-style: long connection or webhook, sending as the connection's own identity.
    pub adapter: Option<Arc<dyn ChannelAdapter>>,
    /// Sending, replying, reacting and recalling as you (the outbox's `Mode::Own`).
    pub act_as_user: Option<Arc<dyn ActAsUser>>,
    /// Looking people up on the platform.
    pub profiles: Option<Arc<dyn ProfileAdapter>>,
    /// Fetching the images and files of messages its connections received.
    pub media: Option<Arc<dyn MediaFetch>>,
}

impl ChannelSide {
    pub fn new(decl: ChannelDecl, provider: Arc<dyn ConnectionProvider>) -> Self {
        Self {
            decl,
            provider,
            adapter: None,
            act_as_user: None,
            profiles: None,
            media: None,
        }
    }

    pub fn adapter(mut self, a: Arc<dyn ChannelAdapter>) -> Self {
        self.adapter = Some(a);
        self
    }

    pub fn act_as_user(mut self, a: Arc<dyn ActAsUser>) -> Self {
        self.act_as_user = Some(a);
        self
    }

    pub fn profiles(mut self, p: Arc<dyn ProfileAdapter>) -> Self {
        self.profiles = Some(p);
        self
    }

    pub fn media(mut self, m: Arc<dyn MediaFetch>) -> Self {
        self.media = Some(m);
        self
    }

    /// The declaration holds, or the first rule it breaks:
    /// - every view answers to `decl.adapter`;
    /// - acting as you is for your own account: `act_as_user` ⇒ [`Mode::Own`], and [`Mode::Own`] ⇒
    ///   `act_as_user` and the provider offers `msg.send_as_user`;
    /// - `receive_cap` is one of the provider's capabilities.
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
            (
                "act-as-user view",
                self.act_as_user.as_ref().map(|a| a.adapter_id()),
            ),
            ("media view", self.media.as_ref().map(|m| m.adapter_id())),
        ];
        for (what, id) in views {
            if let Some(id) = id
                && id != want
            {
                return Err(format!("{what} {id} declared for adapter {want}"));
            }
        }
        match (self.decl.mode, self.act_as_user.is_some()) {
            (Mode::Bot, true) => {
                return Err(format!(
                    "adapter {want} acts as the user but is not declared Mode::Own"
                ));
            }
            (Mode::Own, false) => {
                return Err(format!(
                    "adapter {want} is declared Mode::Own without an act-as-user view"
                ));
            }
            _ => {}
        }
        if self.decl.mode == Mode::Own
            && !self
                .provider
                .caps()
                .contains(&crate::spaces::SEND_AS_USER_CAP)
        {
            return Err(format!(
                "adapter {want} is declared Mode::Own but its provider has no {} capability",
                crate::spaces::SEND_AS_USER_CAP
            ));
        }
        if let Some(cap) = &self.decl.receive_cap
            && !self.provider.caps().contains(&cap.as_str())
        {
            return Err(format!(
                "receive capability {cap} is not one of adapter {want}'s"
            ));
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
        let unnamed = ChannelSide::new(ChannelDecl::new(""), app.clone());
        assert!(unnamed.check().is_err());
    }

    /// Acts as you on `.0` and never sends.
    struct Silent(&'static str);

    #[async_trait::async_trait]
    impl ActAsUser for Silent {
        fn adapter_id(&self) -> &str {
            self.0
        }

        async fn send(
            &self,
            _: &steve_modules::accounts::connections::ConnCx,
            _: &ActTarget,
            _: &crate::channel::OutContent,
            _: &SendKey,
        ) -> Result<Sent, steve_flow::AdapterError> {
            Err(steve_modules::accounts::connections::ProviderError::Unsupported.into())
        }
    }

    #[test]
    fn acting_as_you_goes_with_your_own_account() {
        let user = Arc::new(FeishuEnv::from_env(|_| None).user_provider(Domain::Feishu));
        let own = || ChannelDecl::new("feishu_user").mode(Mode::Own);
        let side = ChannelSide::new(own(), user.clone())
            .act_as_user(user.clone())
            .profiles(user.clone())
            .media(Arc::new(crate::adapters::feishu::FeishuMedia::new(
                "feishu_user",
            )));
        assert_eq!(side.check(), Ok(()));
        // your own account without acting as you, or acting as you on a bot side
        assert_eq!(
            ChannelSide::new(own(), user.clone()).check(),
            Err("adapter feishu_user is declared Mode::Own without an act-as-user view".into())
        );
        assert_eq!(
            ChannelSide::new(ChannelDecl::new("feishu_user"), user.clone())
                .act_as_user(user.clone())
                .check(),
            Err("adapter feishu_user acts as the user but is not declared Mode::Own".into())
        );
        // the views answer to the declared adapter
        let app = Arc::new(FeishuApp::new());
        assert_eq!(
            ChannelSide::new(ChannelDecl::new("feishu_app"), app.clone())
                .media(Arc::new(crate::adapters::feishu::FeishuMedia::new(
                    "feishu_user"
                )))
                .check(),
            Err("media view feishu_user declared for adapter feishu_app".into())
        );
        // a provider that cannot send as you
        assert_eq!(
            ChannelSide::new(ChannelDecl::new("feishu_app").mode(Mode::Own), app.clone())
                .act_as_user(Arc::new(Silent("feishu_app")))
                .check(),
            Err(
                "adapter feishu_app is declared Mode::Own but its provider has no msg.send_as_user capability"
                    .into()
            )
        );
        // the built-in bot passes as it is (its features say `as_user`, it is not Mode::Own)
        assert_eq!(
            ChannelSide::new(ChannelDecl::new("feishu_app"), app.clone())
                .adapter(app.clone())
                .profiles(app)
                .check(),
            Ok(())
        );
    }

    #[test]
    fn the_receive_capability_is_one_of_the_providers() {
        let user = Arc::new(FeishuEnv::from_env(|_| None).user_provider(Domain::Feishu));
        let polls = ChannelDecl::new("feishu_user")
            .mode(Mode::Own)
            .receive(Receive::Poll, Some("msg.receive"));
        assert_eq!(polls.receive, Receive::Poll);
        assert_eq!(
            ChannelSide::new(polls, user.clone())
                .act_as_user(user.clone())
                .check(),
            Ok(())
        );
        let wrong = ChannelDecl::new("feishu_user").receive(Receive::Poll, Some("mail.read"));
        assert_eq!(
            ChannelSide::new(wrong, user).check(),
            Err("receive capability mail.read is not one of adapter feishu_user's".into())
        );
        // the defaults
        let d = ChannelDecl::new("x");
        assert_eq!(
            (d.mode, d.receive, d.receive_cap),
            (Mode::Bot, Receive::LongConnection, None)
        );
    }
}
