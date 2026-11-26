//! Feishu / Lark platform syntax hooks ([`PlatformSyntax`]) and idempotent registration for
//! [`crate::open_link`] and [`crate::syntax`].

use std::sync::{Arc, Once};

use serde_json::Value;

use super::auth::{APP_ADAPTER, USER_ADAPTER};
use super::directory::SYSTEM;
use super::{emoji, inbound, links, send};
use crate::channel::SpaceKind;
use crate::syntax::{PlatformSyntax, RawTenantEvidence};

/// Feishu / Lark syntax and threading rules plugged into the channel core.
pub struct FeishuSyntax;

impl PlatformSyntax for FeishuSyntax {
    fn id(&self) -> &'static str {
        SYSTEM
    }

    fn is_wordless_token(&self, token: &str) -> bool {
        if inbound::WORDLESS_TYPES
            .iter()
            .any(|w| inbound::placeholder(w) == token)
        {
            return true;
        }
        token
            .strip_prefix('[')
            .and_then(|t| t.strip_suffix(']'))
            .is_some_and(emoji::is_emoji)
    }

    fn reparse_raw(&self, raw: &Value) -> Option<(String, Vec<Value>)> {
        inbound::reparse(raw)
    }

    fn raw_tenant_evidence(&self, payload: &Value) -> RawTenantEvidence {
        let raw = &payload["raw"];
        let sender = raw
            .get("event")
            .and_then(|e| e.get("sender"))
            .or_else(|| raw.get("sender"));
        let sender_is_app = sender
            .and_then(|s| s.get("sender_type"))
            .and_then(Value::as_str)
            == Some("app");
        let sender_tenant = sender
            .and_then(|s| s.get("tenant_key"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from);
        let mentions = raw
            .get("event")
            .and_then(|e| e.get("message"))
            .and_then(|m| m.get("mentions"))
            .or_else(|| raw.get("mentions"))
            .and_then(Value::as_array);
        let mut mentioned = Vec::new();
        if let Some(items) = mentions {
            for item in items {
                let handle = item
                    .get("id")
                    .and_then(|id| {
                        id.get("open_id")
                            .and_then(Value::as_str)
                            .or_else(|| id.as_str())
                    })
                    .map(str::trim)
                    .filter(|s| !s.is_empty());
                let tenant = item
                    .get("tenant_key")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty());
                if let (Some(h), Some(t)) = (handle, tenant) {
                    mentioned.push((h.to_string(), t.to_string()));
                }
            }
        }
        RawTenantEvidence {
            sender_tenant,
            sender_is_app,
            mentioned,
        }
    }

    fn reply_in_thread(
        &self,
        adapter: &str,
        kind: SpaceKind,
        thread_key: Option<&str>,
    ) -> Option<bool> {
        if adapter == APP_ADAPTER
            || adapter == USER_ADAPTER
            || thread_key.is_some_and(|t| t.starts_with("omt_"))
        {
            Some(send::reply_in_thread(kind, thread_key))
        } else {
            None
        }
    }

    fn mention_handle_prefix(&self, adapter: &str) -> Option<&'static str> {
        (adapter == APP_ADAPTER || adapter == USER_ADAPTER).then_some("ou_")
    }

    fn is_connection_scoped_handle(&self, handle: &str) -> bool {
        handle.starts_with("ou_")
    }
}

static REGISTERED: Once = Once::new();

/// Register Feishu's open-in-app links and platform syntax hooks once per process.
pub fn register_platform() {
    REGISTERED.call_once(|| {
        crate::open_link::register(Arc::new(links::FeishuLinks));
        crate::syntax::register(Arc::new(FeishuSyntax));
    });
}
