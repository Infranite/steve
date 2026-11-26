//! Links that open a chat, or one of its messages, in its platform's own client:
//! `open_url` and `open_provider` on spaces, conversations, chat messages, and message decisions.
//!
//! Each channel platform registers its [`PlatformLinks`] implementation via [`register`] (or
//! through [`crate::ext::ChannelSide::links`]). The Hub asks the connections that see a space,
//! trying native adapters first (they know the tenant's brand), then the space's voice, the
//! connection it was first seen through, and every other identity that sees it.
//!
//! A message gets its platform's link to that one message when it carries a provider id and the
//! platform has such a link ([`PlatformLinks::message`]); otherwise its chat's link. Messages
//! stored without a provider id (`fp:…`, [`crate::fallback`]) always link to the chat.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, LazyLock, RwLock};

use serde::Serialize;
use serde_json::Value;
use steve_store::base::Viewer;
use steve_store::{Read, Store, channel_message_repo, channel_spaces, connections};

use crate::ChannelResult;
use crate::spaces::SpaceRow;

/// `connections.config` key a channel service's platform is stored under (ext_ws protocol §16).
pub const PLATFORM_KEY: &str = "platform";

/// A messaging platform's links into its own client.
pub trait PlatformLinks: Send + Sync {
    /// The platform: a provider id (e.g. `feishu`, `slack`), as a channel service names it.
    fn id(&self) -> &'static str;
    /// Whether `adapter` (`connections.adapter`) is one of the platform's own adapters.
    fn native(&self, adapter: &str) -> bool;
    /// The link that opens chat `chat_id` (the space's external id) in the client, for a connection
    /// with this `config`; `None` when the id is not one the client opens.
    fn chat(&self, config: &Value, chat_id: &str) -> Option<String>;
    /// The link to message `message_id` (a provider id) in chat `chat_id`; `None` when the platform
    /// has no link to one message (the chat's link is used instead).
    fn message(&self, _config: &Value, _chat_id: &str, _message_id: &str) -> Option<String> {
        None
    }
}

/// Resolved client deep-link and the provider id that owns it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OpenLink {
    pub url: String,
    pub provider: String,
}

static PLATFORMS: LazyLock<RwLock<BTreeMap<String, Arc<dyn PlatformLinks>>>> =
    LazyLock::new(|| RwLock::new(BTreeMap::new()));

/// Register a platform's client link builder (idempotent by [`PlatformLinks::id`]).
pub fn register(links: Arc<dyn PlatformLinks>) {
    PLATFORMS
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .insert(links.id().to_string(), links);
}

/// The platform of a connection: the one whose adapter it is, else the one its service named.
pub fn platform_of(adapter: &str, config: &Value) -> Option<Arc<dyn PlatformLinks>> {
    let platforms = PLATFORMS.read().unwrap_or_else(|e| e.into_inner());
    platforms
        .values()
        .find(|p| p.native(adapter))
        .or_else(|| {
            let named = config.get(PLATFORM_KEY).and_then(Value::as_str)?;
            platforms.get(named)
        })
        .cloned()
}

/// How a space's links are made: its platform and the connection config that decides them.
pub struct Route {
    pub links: Arc<dyn PlatformLinks>,
    pub config: Value,
    pub chat_id: String,
}

impl Route {
    pub fn chat(&self) -> Option<String> {
        self.links.chat(&self.config, &self.chat_id)
    }

    pub fn chat_link(&self) -> Option<OpenLink> {
        self.chat().map(|url| OpenLink {
            url,
            provider: self.links.id().to_string(),
        })
    }

    /// The link to a message with provider id `ext` (`None` or a Hub-made id: the chat's link).
    pub fn message(&self, ext: Option<&str>) -> Option<String> {
        crate::fallback::platform(ext.map(str::to_string))
            .and_then(|id| self.links.message(&self.config, &self.chat_id, &id))
            .or_else(|| self.chat())
    }

    pub fn message_link(&self, ext: Option<&str>) -> Option<OpenLink> {
        self.message(ext).map(|url| OpenLink {
            url,
            provider: self.links.id().to_string(),
        })
    }
}

/// The route of a space, when one of its connections can open it.
fn route(r: &mut impl Read, s: &SpaceRow) -> ChannelResult<Option<Route>> {
    if s.external_id.trim().is_empty() {
        return Ok(None);
    }
    let mut ids = channel_spaces::active_connections(r, &s.id)?;
    ids.extend(
        [&s.connection_id, &s.via]
            .into_iter()
            .filter(|id| !id.is_empty())
            .cloned(),
    );
    let found = connections::adapter_configs(r, &s.workspace_id, &ids)?
        .into_iter()
        .filter_map(|row| {
            let links = platform_of(&row.adapter, &row.config)?;
            let rank = (
                !links.native(&row.adapter),
                row.id != s.connection_id,
                row.id != s.via,
            );
            Some((rank, links, row.config))
        })
        .min_by_key(|(rank, ..)| *rank);
    Ok(found.map(|(_, links, config)| Route {
        links,
        config,
        chat_id: s.external_id.clone(),
    }))
}

/// The link + provider that opens space `s` in its platform's client.
pub fn space_link(r: &mut impl Read, s: &SpaceRow) -> ChannelResult<Option<OpenLink>> {
    Ok(route(r, s)?.and_then(|r| r.chat_link()))
}

/// The link that opens space `s` in its platform's client.
pub fn space_url(r: &mut impl Read, s: &SpaceRow) -> ChannelResult<Option<String>> {
    Ok(space_link(r, s)?.map(|l| l.url))
}

/// The link + provider for a channel message when supported, or for its chat.
pub fn message_link(
    r: &mut impl Read,
    s: &SpaceRow,
    external: Option<&str>,
) -> ChannelResult<Option<OpenLink>> {
    Ok(route(r, s)?.and_then(|r| r.message_link(external)))
}

/// The link to a channel message when supported, or to its chat.
pub fn message_url(
    r: &mut impl Read,
    s: &SpaceRow,
    external: Option<&str>,
) -> ChannelResult<Option<String>> {
    Ok(message_link(r, s, external)?.map(|l| l.url))
}

/// Links + providers for the subjects of channel-message decisions visible to this viewer.
pub fn message_links(
    store: &Store,
    viewer: &Viewer,
    ids: &[String],
) -> ChannelResult<HashMap<String, OpenLink>> {
    store.read(|r| {
        let mut routes: HashMap<String, Option<Route>> = HashMap::new();
        let mut out = HashMap::new();
        for message in channel_message_repo::link_targets(r, &viewer.ws, ids)? {
            if !routes.contains_key(&message.space) {
                let space = steve_store::channel_spaces::rows::get_in_workspace(
                    r,
                    &viewer.ws,
                    &message.space,
                )?;
                let route = match space {
                    Some(space) => route(r, &space)?,
                    None => None,
                };
                routes.insert(message.space.clone(), route);
            }
            if let Some(link) = routes[&message.space]
                .as_ref()
                .and_then(|r| r.message_link(message.external.as_deref()))
            {
                out.insert(message.id, link);
            }
        }
        Ok(out)
    })
}

/// Links for the subjects of channel-message decisions visible to this viewer.
pub fn message_urls(
    store: &Store,
    viewer: &Viewer,
    ids: &[String],
) -> ChannelResult<HashMap<String, String>> {
    Ok(message_links(store, viewer, ids)?
        .into_iter()
        .map(|(k, v)| (k, v.url))
        .collect())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// A platform with links to single messages, to see that a message gets its own.
    struct Threads;
    impl PlatformLinks for Threads {
        fn id(&self) -> &'static str {
            "threads"
        }
        fn native(&self, _: &str) -> bool {
            false
        }
        fn chat(&self, _: &Value, chat: &str) -> Option<String> {
            Some(format!("threads://chat/{chat}"))
        }
        fn message(&self, _: &Value, chat: &str, msg: &str) -> Option<String> {
            Some(format!("threads://chat/{chat}/{msg}"))
        }
    }

    #[test]
    fn a_connection_reaches_its_adapters_platform_or_the_one_its_service_names() {
        crate::adapters::feishu::register_platform();
        let id = |a: &str, c: Value| platform_of(a, &c).map(|p| p.id());
        assert_eq!(id("feishu_app", json!({})), Some("feishu"));
        assert_eq!(id("feishu_user", Value::Null), Some("feishu"));
        assert_eq!(id("ext_ws", json!({"platform": "feishu"})), Some("feishu"));
        assert_eq!(
            id("ext_ws.bot", json!({"platform": "feishu"})),
            Some("feishu")
        );
        assert_eq!(id("ext_ws", json!({})), None);
        assert_eq!(id("ext_ws", json!({"platform": "slack"})), None);
        assert_eq!(id("ext_ws", json!({"platform": 3})), None);
        assert_eq!(id("mail", json!({})), None);
    }

    #[test]
    fn a_message_links_to_itself_where_it_can_else_to_its_chat() {
        let feishu = Route {
            links: Arc::new(crate::adapters::feishu::links::FeishuLinks),
            config: json!({"domain": "lark"}),
            chat_id: "oc_1".into(),
        };
        let chat = "https://applink.larksuite.com/client/chat/open?openChatId=oc_1";
        assert_eq!(feishu.message(Some("om_1")).as_deref(), Some(chat));
        assert_eq!(feishu.message(None).as_deref(), Some(chat));
        assert_eq!(
            feishu.message_link(Some("om_1")),
            Some(OpenLink {
                url: chat.into(),
                provider: "feishu".into(),
            })
        );
        let threads = Route {
            links: Arc::new(Threads),
            config: Value::Null,
            chat_id: "c".into(),
        };
        assert_eq!(
            threads.message(Some("m")).as_deref(),
            Some("threads://chat/c/m")
        );
        // a Hub-made id never leaves the Hub
        assert_eq!(
            threads.message(Some("fp:v1:x:1:0")).as_deref(),
            Some("threads://chat/c")
        );
        assert_eq!(
            threads.message(Some("")).as_deref(),
            Some("threads://chat/c")
        );
    }
}
