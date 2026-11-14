//! The extensions of the `steve-hub` binary: Feishu as the bot and as you, an external channel
//! service, IMAP mailboxes, GitHub. They register the same objects the Hub created inline before
//! the extension entry.

use std::sync::{Arc, Mutex};

use steve_channels::adapters::ext_ws::{self, ExtWs, ExtWsHook};
use steve_channels::adapters::feishu::{self, Domain, FeishuApp, FeishuEnv, FeishuUser};
use steve_channels::ext::ChannelSide;
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
            ChannelSide::new(feishu::app_decl(&app), app.clone())
                .adapter(app.clone())
                .profiles(app.clone())
                .media(Arc::new(feishu::FeishuMedia::new(app.id()))),
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

/// `feishu.self`: you on Feishu (F6), one provider for connections, the device flow, acting as
/// you (C6: sending, reactions, recalling what you sent), looking people up as you (first, before
/// the bot), the media of your messages, and one self poller per connection with `msg.receive`.
#[derive(Default)]
pub(crate) struct FeishuSelf {
    /// The provider its last declaration registered, for its routes and its pollers.
    user: Mutex<Option<Arc<FeishuUser>>>,
}

impl Extension for FeishuSelf {
    fn id(&self) -> &str {
        "feishu.self"
    }

    fn declare(&self, d: &mut Declare<'_>) -> Result<(), ExtensionError> {
        let user = Arc::new(FeishuEnv::from_env(|k| (d.vars)(k)).user_provider(Domain::Feishu));
        d.channel(
            ChannelSide::new(feishu::user_decl(), user.clone())
                .act_as_user(user.clone())
                .profiles(user.clone())
                .media(Arc::new(feishu::FeishuMedia::new(user.id())))
                .history(user.clone()),
        )?;
        // the one its start goes with (an assembly wired again declares again)
        *self.user.lock().unwrap_or_else(|e| e.into_inner()) = Some(user);
        Ok(())
    }

    fn start(&self, s: &Started) -> Result<Mounted, ExtensionError> {
        let user = self
            .user
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .ok_or_else(|| ExtensionError::new("started without its declared provider"))?;
        let api_routes = feishu::device_routes(feishu::DeviceFlows::new(
            s.connections.clone(),
            user.clone(),
        ))
        .merge(feishu::user_routes(s.connections.clone()));
        Ok(Mounted::default().routes(api_routes).lifecycle(
            feishu::auth::USER_ADAPTER,
            Arc::new(feishu::SelfPollHook::new(s.channels.clone(), user)),
        ))
    }
}

/// External channel services the Hub dials over WebSocket: self (`ext_ws`, ActAsUser) and an
/// explicitly selected bot (`ext_ws.bot`, ChannelAdapter). Each has its own feed/session registry;
/// both expose media, explicit history, connector tools and people. Nothing runs until connected.
#[derive(Default)]
pub(crate) struct ExternalChannel {
    /// The views its last declaration registered, shared with its feeds.
    ext: Mutex<Option<(Arc<ExtWs>, Arc<ExtWs>)>>,
}

impl Extension for ExternalChannel {
    fn id(&self) -> &str {
        "ext_ws"
    }

    fn declare(&self, d: &mut Declare<'_>) -> Result<(), ExtensionError> {
        d.system(ext_ws::SYSTEM, ext_ws::SYSTEM_LABEL)?;
        let ext = Arc::new(ExtWs::new());
        d.channel(
            ChannelSide::new(ext_ws::decl(), ext.clone())
                .act_as_user(ext.clone())
                .media(ext.clone())
                .connector(ext.clone())
                .history(ext.clone())
                .profiles(ext.clone()),
        )?;
        let bot = Arc::new(ExtWs::bot());
        d.channel(
            ChannelSide::new(ext_ws::bot_decl(), bot.clone())
                .adapter(bot.clone())
                .media(bot.clone())
                .connector(bot.clone())
                .history(bot.clone())
                .profiles(bot.clone()),
        )?;
        *self.ext.lock().unwrap_or_else(|e| e.into_inner()) = Some((ext, bot));
        Ok(())
    }

    fn start(&self, s: &Started) -> Result<Mounted, ExtensionError> {
        let (ext, bot) = self
            .ext
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .ok_or_else(|| ExtensionError::new("started without its declared provider"))?;
        Ok(Mounted::default()
            .lifecycle(
                ext_ws::ADAPTER,
                Arc::new(ExtWsHook::new(s.channels.clone(), ext)),
            )
            .lifecycle(
                ext_ws::BOT_ADAPTER,
                Arc::new(ExtWsHook::new(s.channels.clone(), bot)),
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
