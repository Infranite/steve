//! SSE scoping: module events carry `ws` and reach only subscribers of that workspace; the old
//! event types keep going to every subscriber, unchanged.

use steve_store::ChangeEvent;

/// Should the `/api/stream` subscriber of workspace `subscriber_ws` get `e`?
pub fn delivers(e: &ChangeEvent, subscriber_ws: &str) -> bool {
    match e {
        ChangeEvent::SteveThinking { ws, .. } => ws
            .as_deref()
            .is_some_and(|w| !w.is_empty() && w == subscriber_ws),
        _ => e.ws().is_none_or(|w| !w.is_empty() && w == subscriber_ws),
    }
}

/// Only legacy invalidation notices and the global release hold may omit a workspace. New extension
/// events are private by default; adding one never requires updating a scoping whitelist.
pub fn requires_workspace(ty: &str) -> bool {
    !matches!(
        ty,
        "item"
            | "decision"
            | "session"
            | "message"
            | "bot"
            | "project"
            | "tag"
            | "workspace"
            | "settings"
            | "device"
            | "release.hold"
    )
}

/// Explicit workspace metadata is always authoritative, including extension events.
/// Known private event families fail closed if their workspace is absent or malformed.
pub fn delivers_json(e: &serde_json::Value, subscriber_ws: &str) -> bool {
    match e.get("ws") {
        Some(serde_json::Value::String(w)) => !w.is_empty() && w == subscriber_ws,
        Some(_) => false,
        None => !requires_workspace(e.get("type").and_then(|t| t.as_str()).unwrap_or("")),
    }
}

/// Every event type the module specs add.
pub const MODULE_TYPES: [&str; 17] = [
    "party",
    "mm",
    "changeset",
    "account",
    "connection",
    "vault",
    "signal",
    "flow",
    "run",
    "space",
    "chan_message",
    "persona",
    "digest",
    "channel",
    // GitHub (G2) and mail (E1) notices, published as plain JSON by the Hub
    "gh",
    "mail",
    // knowledge (`{type:"kb", id, ws}`, published as plain JSON by the Hub)
    "kb",
];

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn module_events_stay_in_their_workspace() {
        let p = ChangeEvent::Party {
            id: "ct_1".into(),
            ws: "a".into(),
        };
        assert!(delivers(&p, "a"));
        assert!(!delivers(&p, "b"));
        let thinking = ChangeEvent::SteveThinking {
            on: true,
            ws: Some("a".into()),
            phase: None,
        };
        assert!(delivers(&thinking, "a"));
        assert!(!delivers(&thinking, "b"));
        let unscoped = ChangeEvent::SteveThinking {
            on: true,
            ws: None,
            phase: None,
        };
        assert!(!delivers(&unscoped, "b"));
        let old = ChangeEvent::Item { id: "ST-1".into() };
        assert!(delivers(&old, "b"));
        let j = serde_json::to_value(&p).unwrap();
        assert!(delivers_json(&j, "a") && !delivers_json(&j, "b"));
        assert!(!delivers_json(
            &json!({"type": "steve.thinking", "on": true, "ws": "a"}),
            "b"
        ));
        assert!(delivers_json(&json!({"type": "item", "id": "x"}), "b"));
        assert!(!delivers_json(&json!({"type": "plugin.future"}), "b"));
        assert!(!delivers_json(&json!({"type": "mail", "ws": null}), "b"));
        assert!(delivers_json(
            &json!({"type": "plugin.future", "ws": "a"}),
            "a"
        ));
        assert!(!delivers_json(
            &json!({"type": "plugin.future", "ws": "a"}),
            "b"
        ));
        for t in MODULE_TYPES {
            assert!(
                !delivers_json(&json!({"type": t, "id": "x", "ws": "a"}), "b"),
                "{t}"
            );
        }
    }
}
