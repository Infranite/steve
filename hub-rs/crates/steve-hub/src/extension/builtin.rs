//! The extensions of the `steve-hub` binary: Feishu as the bot and as you, IMAP mailboxes,
//! GitHub. They register the same objects the Hub created inline before the extension entry.

use std::sync::Arc;

use steve_channels::adapters::feishu::{self, Domain, FeishuApp, FeishuEnv};
use steve_channels::ext::{ChannelDecl, ChannelSide, Mode, Receive};
use steve_modules::accounts::connections::ConnectionProvider;

use super::{Declare, Extension, ExtensionError, Job, Mounted, Started};

/// `feishu.bot`: the bot's app (long connection, sending as the bot, its address book), the app
/// registrations started from Steve, and one long connection per bot connection.
pub(crate) struct FeishuBot;

impl Extension for FeishuBot {
    fn id(&self) -> &str {
        "feishu.bot"
    }

    fn declare(&self, d: &mut Declare<'_>) -> Result<(), ExtensionError> {
        let app = Arc::new(FeishuApp::new());
        d.channel(
            ChannelSide::new(
                ChannelDecl::new(app.id())
                    .mode(Mode::Bot)
                    .receive(Receive::LongConnection, None),
                app.clone(),
            )
            .adapter(app.clone())
            .profiles(app),
        )
    }

    fn start(&self, s: &Started) -> Result<Mounted, ExtensionError> {
        // the bot's app created from Steve (PersonalAgent registration): the real hosts, or the
        // fake a test home points STEVE_FEISHU_API_BASE at
        let bases = match FeishuEnv::from_env(|k| (s.vars)(k)).api_base {
            Some(b) => feishu::app_registration::Bases::fake(&b),
            None => feishu::app_registration::Bases::real(),
        };
        let registrations =
            feishu::app_registration::Registrations::new(s.connections.clone(), bases);
        let api_routes = feishu::app_registration::routes(registrations.clone());
        // app registrations a stop interrupted: polled until their own deadline, checks resumed
        let resume: Job = Box::pin(async move {
            match registrations.resume() {
                Ok(n) if n > 0 => tracing::info!("feishu app registrations resumed: {n}"),
                Ok(_) => {}
                Err(e) => tracing::error!("feishu app registrations not resumed: {e}"),
            }
        });
        // none on a fresh STEVE_HOME; started and stopped as connections change
        Ok(Mounted::default().routes(api_routes).job(resume).lifecycle(
            feishu::auth::APP_ADAPTER,
            Arc::new(feishu::BotHook::new(s.channels.clone())),
        ))
    }
}

/// `feishu.self`: you on Feishu (F6), one provider for connections, the device flow, sending as
/// you (C6), looking people up as you (first, before the bot), and one self poller per connection
/// with `msg.receive`.
pub(crate) struct FeishuSelf;

impl Extension for FeishuSelf {
    fn id(&self) -> &str {
        "feishu.self"
    }

    fn declare(&self, d: &mut Declare<'_>) -> Result<(), ExtensionError> {
        let user = Arc::new(
            FeishuEnv::from_env(|k| (d.vars)(k))
                .user_provider(Domain::Feishu)
                .with_party_hooks(d.party_hooks.clone()),
        );
        d.channel(
            ChannelSide::new(
                ChannelDecl::new(user.id())
                    .mode(Mode::Own)
                    .receive(Receive::Poll, Some("msg.receive")),
                user.clone(),
            )
            .as_user(user.clone())
            .profiles(user.clone()),
        )?;
        d.feishu_user = Some(user);
        Ok(())
    }

    fn start(&self, s: &Started) -> Result<Mounted, ExtensionError> {
        let user = s
            .feishu_user
            .clone()
            .ok_or_else(|| ExtensionError::new("started without its declared provider"))?;
        let api_routes = feishu::device_routes(feishu::DeviceFlows::new(
            s.connections.clone(),
            user.clone(),
        ))
        .merge(feishu::user_routes(
            s.connections.clone(),
            user.claims().clone(),
        ));
        Ok(Mounted::default().routes(api_routes).lifecycle(
            feishu::auth::USER_ADAPTER,
            Arc::new(feishu::SelfPollHook::new(s.channels.clone(), user)),
        ))
    }
}

/// `mail.imap`: IMAP + SMTP mailboxes (E1).
pub(crate) struct MailImap;

impl Extension for MailImap {
    fn id(&self) -> &str {
        "mail.imap"
    }

    fn declare(&self, d: &mut Declare<'_>) -> Result<(), ExtensionError> {
        let cfg = steve_mail::MailConfig::from_env(|k| (d.vars)(k))
            .map_err(|e| ExtensionError::new(format!("mail config: {e}")))?;
        d.mail(Arc::new(steve_mail::ImapAdapter::new(cfg)))
    }
}

/// `github`: the GitHub App and token adapters (G1), each an event source for flows too.
pub(crate) struct GitHub;

impl Extension for GitHub {
    fn id(&self) -> &str {
        "github"
    }

    fn declare(&self, d: &mut Declare<'_>) -> Result<(), ExtensionError> {
        // the same env as the GitHub module's (`github_pat` honours STEVE_GITHUB_API_BASE)
        let env = steve_github::GitHubEnv::from_env(|k| (d.vars)(k));
        for p in steve_github::providers_for(&env) {
            d.provider(p.clone())?;
            d.event_source(Arc::new(crate::wire::signals::GithubSource(p)))?;
        }
        Ok(())
    }
}
