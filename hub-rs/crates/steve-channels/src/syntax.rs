//! Channel-neutral registry for platform-specific message syntax, threading hints, mention
//! handle families, raw payload reparsing, and raw tenant evidence extraction.
//!
//! Each channel adapter registers its own [`PlatformSyntax`] when declared or initialized so the
//! generic channel core never imports or branches on a specific messaging platform.

use std::collections::BTreeMap;
use std::sync::{Arc, LazyLock, RwLock};

use serde_json::Value;

use crate::channel::SpaceKind;

/// Organization/tenant hints extracted by a platform adapter from a stored raw event payload.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RawTenantEvidence {
    pub sender_tenant: Option<String>,
    pub sender_is_app: bool,
    /// `(raw_handle, tenant_key)` pairs from mentions inside the raw payload.
    pub mentioned: Vec<(String, String)>,
}

/// Platform-specific syntax and wire conventions plugged in by channel adapters.
pub trait PlatformSyntax: Send + Sync {
    /// Stable platform identifier (e.g. `"feishu"`).
    fn id(&self) -> &'static str;

    /// Whether a bracketed token (such as a platform emoji name or wordless message placeholder)
    /// carries no textual content to answer.
    fn is_wordless_token(&self, _token: &str) -> bool {
        false
    }

    /// Re-extract readable body text and attachments from a raw provider payload when the
    /// external bridge could not render it.
    fn reparse_raw(&self, _raw: &Value) -> Option<(String, Vec<Value>)> {
        None
    }

    /// Extract sender/mentioned tenant evidence from a stored `ext_events.payload`.
    fn raw_tenant_evidence(&self, _payload: &Value) -> RawTenantEvidence {
        RawTenantEvidence::default()
    }

    /// Whether a reply to a message in `kind` with `thread_key` should go into the thread on
    /// `adapter`. Return `None` when this platform does not handle `adapter` (or use default rules).
    fn reply_in_thread(
        &self,
        _adapter: &str,
        _kind: SpaceKind,
        _thread_key: Option<&str>,
    ) -> Option<bool> {
        None
    }

    /// Prefix of the per-connection handle family used to `@`-mention users on `adapter`, if the
    /// platform uses multiple handle families per account.
    fn mention_handle_prefix(&self, _adapter: &str) -> Option<&'static str> {
        None
    }

    /// Whether `handle` is scoped to a single connection/installation rather than valid globally.
    fn is_connection_scoped_handle(&self, _handle: &str) -> bool {
        false
    }
}

static REGISTRY: LazyLock<RwLock<BTreeMap<String, Arc<dyn PlatformSyntax>>>> =
    LazyLock::new(|| RwLock::new(BTreeMap::new()));

/// Register a platform's syntax hooks (idempotent by [`PlatformSyntax::id`]).
pub fn register(syntax: Arc<dyn PlatformSyntax>) {
    REGISTRY
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .insert(syntax.id().to_string(), syntax);
}

fn all() -> Vec<Arc<dyn PlatformSyntax>> {
    REGISTRY
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .values()
        .cloned()
        .collect()
}

pub fn is_wordless_token(token: &str) -> bool {
    all().iter().any(|s| s.is_wordless_token(token))
}

pub fn reparse_raw(raw: &Value) -> Option<(String, Vec<Value>)> {
    all().iter().find_map(|s| s.reparse_raw(raw))
}

pub fn raw_tenant_evidence(payload: &Value) -> RawTenantEvidence {
    for s in all() {
        let ev = s.raw_tenant_evidence(payload);
        if ev.sender_tenant.is_some() || ev.sender_is_app || !ev.mentioned.is_empty() {
            return ev;
        }
    }
    RawTenantEvidence::default()
}

/// Whether a reply in `kind` with `thread_key` goes in-thread for `adapter`.
pub fn reply_in_thread(adapter: &str, kind: SpaceKind, thread_key: Option<&str>) -> bool {
    for s in all() {
        if let Some(in_thread) = s.reply_in_thread(adapter, kind, thread_key) {
            return in_thread;
        }
    }
    kind == SpaceKind::Channel
}

/// Optional handle prefix an adapter requires when looking up a mention handle.
pub fn mention_handle_prefix(adapter: &str) -> Option<&'static str> {
    all().iter().find_map(|s| s.mention_handle_prefix(adapter))
}

/// Whether `handle` is scoped to one connection/installation according to any registered platform.
pub fn is_connection_scoped_handle(handle: &str) -> bool {
    all().iter().any(|s| s.is_connection_scoped_handle(handle))
}
