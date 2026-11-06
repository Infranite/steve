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

use crate::channel::{ActAsUser, ChannelAdapter, ChannelFeatures, ProfileAdapter};

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

/// Whether the remote gives each message an id of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageIds {
    /// Every message has a stable provider id (`InboundMessage.external_id`).
    Platform,
    /// An adapter without platform message ids: messages are told apart by their delivery id
    /// (`source_id`) and, failing that, by content and time.
    None,
}

/// How a connection of the adapter is made on the 接入 page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StartMethod {
    /// The provider's OAuth flow.
    #[serde(rename = "oauth")]
    OAuth,
    /// A device code shown to you.
    Device,
    /// The provider's credential form.
    Form,
    /// A page of the adapter's own, under the Hub's API (`path`), named `label`.
    Route { path: String, label: String },
}

/// The bounds of [`ChannelDecl::created_precision_ms`].
pub const PRECISION_MS: std::ops::RangeInclusive<i64> = 1..=60_000;

/// How a channel-side adapter behaves.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ChannelDecl {
    /// `connections.adapter`: every view in the [`ChannelSide`] answers to this id.
    pub adapter: String,
    /// The `systems.id` of the people its messages come from (`Sender.handle`, `MessageRef.sender`).
    /// Required.
    pub handle_system: String,
    /// Whose identity a connection is: a bot's, or yours. Its `channel_settings` get this mode
    /// before its [`ConnectionHook`] first runs. Default: [`Mode::Bot`].
    pub mode: Mode,
    /// What the channel can do. With a channel adapter, exactly its
    /// [`ChannelAdapter::features`]. Default: nothing.
    pub features: ChannelFeatures,
    /// The capability a connection needs turned on to receive (`msg.receive`); `None`: receiving
    /// is not a capability of its own. Default: `None`.
    pub receive_cap: Option<String>,
    /// How messages come in. Default: [`Receive::LongConnection`].
    pub receive: Receive,
    /// Whether messages carry provider ids. Default: [`MessageIds::Platform`].
    pub message_ids: MessageIds,
    /// How precise the remote's create times are, in ms ([`PRECISION_MS`]). Default: 1000.
    pub created_precision_ms: i64,
    /// How a connection is made, in the order offered. Default: none listed.
    pub start: Vec<StartMethod>,
}

impl ChannelDecl {
    pub fn new(adapter: impl Into<String>) -> Self {
        Self {
            adapter: adapter.into(),
            handle_system: String::new(),
            mode: Mode::Bot,
            features: ChannelFeatures::default(),
            receive_cap: None,
            receive: Receive::LongConnection,
            message_ids: MessageIds::Platform,
            created_precision_ms: 1000,
            start: Vec::new(),
        }
    }

    pub fn handle_system(mut self, s: impl Into<String>) -> Self {
        self.handle_system = s.into();
        self
    }

    pub fn mode(mut self, m: Mode) -> Self {
        self.mode = m;
        self
    }

    pub fn features(mut self, f: ChannelFeatures) -> Self {
        self.features = f;
        self
    }

    pub fn message_ids(mut self, ids: MessageIds) -> Self {
        self.message_ids = ids;
        self
    }

    /// 1..=60_000 ([`ChannelSide::check`] refuses anything else).
    pub fn created_precision_ms(mut self, ms: i64) -> Self {
        self.created_precision_ms = ms;
        self
    }

    /// One more way to connect.
    pub fn start(mut self, m: StartMethod) -> Self {
        self.start.push(m);
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
    /// - every view answers to `decl.adapter`; `decl.handle_system` is set;
    /// - with a channel adapter, `decl.features` are its [`ChannelAdapter::features`];
    /// - acting as you is for your own account: `act_as_user` ⇒ [`Mode::Own`], and [`Mode::Own`] ⇒
    ///   `act_as_user`, `features.as_user` and the provider offers `msg.send_as_user`;
    /// - on your own account, `features.recall` and `features.reactions` say what the
    ///   act-as-user view supports ([`ActAsUser::supports_recall`],
    ///   [`ActAsUser::supports_reactions`]);
    /// - [`MessageIds::None`] ⇒ no channel adapter;
    /// - `created_precision_ms` is within [`PRECISION_MS`];
    /// - `receive_cap` is one of the provider's capabilities;
    /// - the provider's identity is `self` exactly when the side is [`Mode::Own`].
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
        if self.decl.handle_system.is_empty() {
            return Err(format!("adapter {want} declares no handle system"));
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
        if let Some(a) = &self.adapter
            && a.features() != self.decl.features
        {
            return Err(format!(
                "adapter {want} declares other features than its channel adapter has"
            ));
        }
        let f = self.decl.features;
        if let (Mode::Own, Some(u)) = (self.decl.mode, &self.act_as_user) {
            if !f.as_user {
                return Err(format!(
                    "adapter {want} is declared Mode::Own without the as_user feature"
                ));
            }
            if f.recall != u.supports_recall() {
                return Err(format!(
                    "adapter {want} declares recall {} but its act-as-user view says {}",
                    f.recall,
                    u.supports_recall()
                ));
            }
            if f.reactions != u.supports_reactions() {
                return Err(format!(
                    "adapter {want} declares reactions {} but its act-as-user view says {}",
                    f.reactions,
                    u.supports_reactions()
                ));
            }
        }
        if self.decl.message_ids == MessageIds::None && self.adapter.is_some() {
            return Err(format!(
                "adapter {want} has no platform message ids but a channel adapter"
            ));
        }
        if !PRECISION_MS.contains(&self.decl.created_precision_ms) {
            return Err(format!(
                "adapter {want} declares a create-time precision of {} ms (1..=60000)",
                self.decl.created_precision_ms
            ));
        }
        if let Some(cap) = &self.decl.receive_cap
            && !self.provider.caps().contains(&cap.as_str())
        {
            return Err(format!(
                "receive capability {cap} is not one of adapter {want}'s"
            ));
        }
        let own_identity = self.provider.integration().identity == "self";
        if own_identity != (self.decl.mode == Mode::Own) {
            return Err(format!(
                "adapter {want}'s provider identity is {} but the side is declared {:?}",
                self.provider.integration().identity,
                self.decl.mode
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::feishu::{Domain, FeishuApp, FeishuEnv, FeishuMedia};

    fn app_decl() -> ChannelDecl {
        ChannelDecl::new("feishu_app")
            .handle_system("feishu")
            .features(FeishuApp::new().features())
    }

    fn own_features() -> ChannelFeatures {
        ChannelFeatures {
            as_user: true,
            recall: true,
            reactions: true,
            ..ChannelFeatures::default()
        }
    }

    fn own() -> ChannelDecl {
        ChannelDecl::new("feishu_user")
            .handle_system("feishu")
            .mode(Mode::Own)
            .features(own_features())
    }

    #[test]
    fn every_view_answers_to_the_declared_adapter() {
        let app = Arc::new(FeishuApp::new());
        let ok = ChannelSide::new(app_decl(), app.clone())
            .adapter(app.clone())
            .profiles(app.clone());
        assert_eq!(ok.check(), Ok(()));
        let user = Arc::new(FeishuEnv::from_env(|_| None).user_provider(Domain::Feishu));
        let wrong = ChannelSide::new(app_decl(), app.clone()).profiles(user);
        assert_eq!(
            wrong.check(),
            Err("profile adapter feishu_user declared for adapter feishu_app".into())
        );
        let unnamed = ChannelSide::new(ChannelDecl::new(""), app.clone());
        assert!(unnamed.check().is_err());
        assert_eq!(
            ChannelSide::new(ChannelDecl::new("feishu_app"), app).check(),
            Err("adapter feishu_app declares no handle system".into())
        );
    }

    /// Acts as you on `.0` and never sends; recalls and reacts as `.1` says.
    struct Silent(&'static str, bool, bool);

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

        fn supports_recall(&self) -> bool {
            self.1
        }

        fn supports_reactions(&self) -> bool {
            self.2
        }
    }

    #[test]
    fn acting_as_you_goes_with_your_own_account() {
        let user = Arc::new(FeishuEnv::from_env(|_| None).user_provider(Domain::Feishu));
        let side = ChannelSide::new(own(), user.clone())
            .act_as_user(user.clone())
            .profiles(user.clone())
            .media(Arc::new(FeishuMedia::new("feishu_user")));
        assert_eq!(side.check(), Ok(()));
        // your own account without acting as you, or acting as you on a bot side
        assert_eq!(
            ChannelSide::new(own(), user.clone()).check(),
            Err("adapter feishu_user is declared Mode::Own without an act-as-user view".into())
        );
        assert_eq!(
            ChannelSide::new(own().mode(Mode::Bot), user.clone())
                .act_as_user(user.clone())
                .check(),
            Err("adapter feishu_user acts as the user but is not declared Mode::Own".into())
        );
        // the views answer to the declared adapter
        let app = Arc::new(FeishuApp::new());
        assert_eq!(
            ChannelSide::new(app_decl(), app.clone())
                .media(Arc::new(FeishuMedia::new("feishu_user")))
                .check(),
            Err("media view feishu_user declared for adapter feishu_app".into())
        );
        // a provider that cannot send as you
        assert_eq!(
            ChannelSide::new(app_decl().mode(Mode::Own), app.clone())
                .act_as_user(Arc::new(Silent("feishu_app", true, true)))
                .check(),
            Err(
                "adapter feishu_app is declared Mode::Own but its provider has no msg.send_as_user capability"
                    .into()
            )
        );
        // the built-in bot passes as it is (its features say `as_user`, it is not Mode::Own)
        assert_eq!(
            ChannelSide::new(app_decl(), app.clone())
                .adapter(app.clone())
                .profiles(app.clone())
                .check(),
            Ok(())
        );
        // a bot side on a provider whose identity is you
        assert_eq!(
            ChannelSide::new(own().mode(Mode::Bot), user.clone()).check(),
            Err(
                "adapter feishu_user's provider identity is self but the side is declared Bot"
                    .into()
            )
        );
        // your own account on a bot's provider (it may send as you, the identity says bot)
        struct BotSendsAsYou(FeishuApp);
        #[async_trait::async_trait]
        impl ConnectionProvider for BotSendsAsYou {
            fn id(&self) -> &str {
                "feishu_app"
            }
            fn system(&self) -> &str {
                self.0.system()
            }
            fn name(&self) -> &str {
                self.0.name()
            }
            fn caps(&self) -> &[&'static str] {
                &["msg.send_as_user"]
            }
            fn auth(&self) -> steve_modules::accounts::connections::AuthKind {
                self.0.auth()
            }
            fn integration(&self) -> steve_modules::accounts::connections::Integration {
                self.0.integration()
            }
            async fn verify(
                &self,
                cx: &steve_modules::accounts::connections::ConnCx,
            ) -> Result<
                steve_modules::accounts::connections::AccountInfo,
                steve_modules::accounts::connections::ProviderError,
            > {
                self.0.verify(cx).await
            }
        }
        assert_eq!(
            ChannelSide::new(
                app_decl().mode(Mode::Own).features(own_features()),
                Arc::new(BotSendsAsYou(FeishuApp::new()))
            )
            .act_as_user(Arc::new(Silent("feishu_app", true, true)))
            .check(),
            Err(
                "adapter feishu_app's provider identity is bot but the side is declared Own".into()
            )
        );
    }

    #[test]
    fn the_declared_features_are_what_the_views_do() {
        let app = Arc::new(FeishuApp::new());
        // a channel adapter's features are the declared ones
        assert_eq!(
            ChannelSide::new(app_decl().features(ChannelFeatures::default()), app.clone())
                .adapter(app.clone())
                .check(),
            Err("adapter feishu_app declares other features than its channel adapter has".into())
        );
        let user = Arc::new(FeishuEnv::from_env(|_| None).user_provider(Domain::Feishu));
        let side = |d: ChannelDecl, u: Silent| {
            ChannelSide::new(d, user.clone())
                .act_as_user(Arc::new(u))
                .check()
        };
        assert_eq!(side(own(), Silent("feishu_user", true, true)), Ok(()));
        // your own account acts as you
        assert_eq!(
            side(
                own().features(ChannelFeatures {
                    as_user: false,
                    ..own_features()
                }),
                Silent("feishu_user", true, true)
            ),
            Err("adapter feishu_user is declared Mode::Own without the as_user feature".into())
        );
        // recall and reactions, both ways
        assert_eq!(
            side(own(), Silent("feishu_user", false, true)),
            Err(
                "adapter feishu_user declares recall true but its act-as-user view says false"
                    .into()
            )
        );
        assert_eq!(
            side(
                own().features(ChannelFeatures {
                    recall: false,
                    ..own_features()
                }),
                Silent("feishu_user", true, true)
            ),
            Err(
                "adapter feishu_user declares recall false but its act-as-user view says true"
                    .into()
            )
        );
        assert_eq!(
            side(own(), Silent("feishu_user", true, false)),
            Err(
                "adapter feishu_user declares reactions true but its act-as-user view says false"
                    .into()
            )
        );
        // the built-in FeishuUser recalls and reacts
        assert_eq!(
            ChannelSide::new(own(), user.clone())
                .act_as_user(user.clone())
                .check(),
            Ok(())
        );
        // without platform message ids there is no bot-style channel adapter
        assert_eq!(
            ChannelSide::new(app_decl().message_ids(MessageIds::None), app.clone())
                .adapter(app.clone())
                .check(),
            Err("adapter feishu_app has no platform message ids but a channel adapter".into())
        );
        assert_eq!(
            side(
                own().message_ids(MessageIds::None),
                Silent("feishu_user", true, true)
            ),
            Ok(())
        );
        // the precision of create times
        for ms in [0, 60_001] {
            assert_eq!(
                side(
                    own().created_precision_ms(ms),
                    Silent("feishu_user", true, true)
                ),
                Err(format!(
                    "adapter feishu_user declares a create-time precision of {ms} ms (1..=60000)"
                ))
            );
        }
        assert_eq!(
            side(
                own().created_precision_ms(60_000),
                Silent("feishu_user", true, true)
            ),
            Ok(())
        );
    }

    #[test]
    fn the_receive_capability_is_one_of_the_providers() {
        let user = Arc::new(FeishuEnv::from_env(|_| None).user_provider(Domain::Feishu));
        let polls = own().receive(Receive::Poll, Some("msg.receive"));
        assert_eq!(polls.receive, Receive::Poll);
        assert_eq!(
            ChannelSide::new(polls, user.clone())
                .act_as_user(user.clone())
                .check(),
            Ok(())
        );
        let wrong = own()
            .mode(Mode::Bot)
            .receive(Receive::Poll, Some("mail.read"));
        assert_eq!(
            ChannelSide::new(wrong, user).check(),
            Err("receive capability mail.read is not one of adapter feishu_user's".into())
        );
        // the defaults
        let d = ChannelDecl::new("x");
        assert_eq!(
            (d.mode, d.receive, d.receive_cap.clone()),
            (Mode::Bot, Receive::LongConnection, None)
        );
        assert_eq!(
            (
                d.handle_system.as_str(),
                d.features,
                d.message_ids,
                d.created_precision_ms,
                d.start.len()
            ),
            (
                "",
                ChannelFeatures::default(),
                MessageIds::Platform,
                1000,
                0
            )
        );
        let d = d.start(StartMethod::OAuth).start(StartMethod::Route {
            path: "/x/connect".into(),
            label: "连接".into(),
        });
        assert_eq!(
            serde_json::to_value(&d.start).unwrap(),
            serde_json::json!([{"kind": "oauth"}, {"kind": "route", "path": "/x/connect", "label": "连接"}])
        );
    }
}
