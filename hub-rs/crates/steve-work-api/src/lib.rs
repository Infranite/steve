//! Work's published facts (REVIEW-SUMMARY: a provider-owned small API). Only the topic names, their
//! payload versions and the payloads: steve-core builds and emits them in the transaction of the change
//! (`steve_store::outbox::emit`), consumers (steve-flow) read them. Nothing here touches a database.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// An item was finished (`status = done`), emitted in the transaction that finished it.
pub const ITEM_DONE: &str = "work.item.done";
/// Payload version of [`ITEM_DONE`].
pub const ITEM_DONE_V: i64 = 1;

/// [`ITEM_DONE`]: the item as it was when it was finished (consumers never re-read it to describe the
/// fact; it may have changed since).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ItemDone {
    pub item_id: String,
    pub title: String,
    pub status: String,
    pub bot_id: Option<String>,
    pub project_id: Option<String>,
    /// `items.source`: `["邮件", "mail"]`.
    pub source: Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn item_done_round_trips() {
        let d = ItemDone {
            item_id: "ST-1".into(),
            title: "写周报".into(),
            status: "done".into(),
            bot_id: Some("steve".into()),
            project_id: None,
            source: serde_json::json!(["对话", "chat"]),
        };
        let v = serde_json::to_value(&d).unwrap();
        assert_eq!(v["item_id"], "ST-1");
        assert_eq!(serde_json::from_value::<ItemDone>(v).unwrap(), d);
    }
}
