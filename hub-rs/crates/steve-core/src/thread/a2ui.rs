//! A2UI actions (ST-46): a button in an interface Steve drew in a reply (a ```a2ui block) sends
//! an A2UI v0.9.1 `action`. It is said in the main conversation as the user's message — the standard action JSON the
//! model reads, `meta.a2ui_action` for the page — and kept on the reply (`meta.a2ui["block:surface"]`, with the
//! surface's data then), so a surface is submitted once: a second click, another tab or a reload gets the record back.

use super::{SayTo, lock, meta_of, offer::not_found};
use crate::Core;
use crate::error::{CoreError, Result};
use crate::util::now;
use serde_json::{Map, Value, json};

/// The data snapshot kept on the reply is dropped above this (the action itself still goes).
const DATA_MAX: usize = 16 * 1024;
const CONTEXT_MAX: usize = 4 * 1024;

/// One press of an event button, as the page sends it.
#[derive(Debug, Clone)]
pub struct A2uiAction {
    /// the block's content key (the page hashes the block's text)
    pub block: String,
    pub surface: String,
    pub component: String,
    pub name: String,
    /// the button's text, for the line the page shows
    pub label: String,
    pub context: Value,
    /// the surface's data model when it was pressed (restores the form after a reload)
    pub data: Value,
    pub timestamp: String,
}

/// What became of it: `already` when the surface had been submitted (`action` is that record).
#[derive(Debug, Clone, serde::Serialize)]
pub struct A2uiActed {
    pub already: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    pub action: Value,
}

impl Core {
    /// `POST /api/thread/:id/a2ui`: the reply must be Steve's, in this workspace's main conversation, and carry
    /// the interface the action names.
    pub fn a2ui_action(
        &self,
        ws: &str,
        user: Option<&str>,
        msg_id: &str,
        a: A2uiAction,
    ) -> Result<A2uiActed> {
        let bad =
            |f: &str| CoreError::Status(400, steve_i18n::t!("core.error.invalid", fields = f));
        let ok_id = |s: &str, max: usize| !s.is_empty() && s.chars().count() <= max;
        if !ok_id(&a.block, 16) || !a.block.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err(bad("block"));
        }
        if !ok_id(&a.surface, 120) {
            return Err(bad("surfaceId"));
        }
        if !ok_id(&a.component, 120) {
            return Err(bad("sourceComponentId"));
        }
        if !ok_id(&a.name, 64) {
            return Err(bad("name"));
        }
        if !a.context.is_object() || a.context.to_string().len() > CONTEXT_MAX {
            return Err(bad("context"));
        }
        let _one = lock(&self.0.thread.acting);
        let m = self
            .store()
            .message(msg_id)?
            .filter(|m| m.workspace_id == ws)
            .ok_or_else(not_found)?;
        if m.author != "steve" || m.space != super::MAIN {
            return Err(CoreError::Status(
                400,
                steve_i18n::t!("core.a2ui.main_only"),
            ));
        }
        let surface_json = serde_json::to_string(&a.surface).unwrap_or_default();
        if !has_block(&m.text) || !m.text.contains(&surface_json) {
            return Err(CoreError::Status(
                400,
                steve_i18n::t!("core.a2ui.no_surface"),
            ));
        }
        let key = format!("{}:{}", a.block, a.surface);
        if let Some(r) = m.meta.get("a2ui").and_then(|x| x.get(&key)) {
            return Ok(A2uiActed {
                already: true,
                message_id: r["message"].as_str().map(String::from),
                action: r.clone(),
            });
        }
        let label: String = a.label.trim().chars().take(80).collect();
        let action = json!({
            "version": "v0.9.1",
            "action": {
                "name": a.name, "surfaceId": a.surface, "sourceComponentId": a.component,
                "timestamp": a.timestamp, "context": a.context,
            },
        });
        let shown = if label.is_empty() { &a.name } else { &label };
        let text = format!("[A2UI action] 提交「{shown}」\n{action}");
        let mut record = json!({
            "name": a.name, "label": label, "sourceComponentId": a.component, "context": a.context, "at": now(),
        });
        if a.data.to_string().len() <= DATA_MAX {
            record["data"] = a.data;
        }
        let mut said = record.clone();
        said["ref"] = json!(msg_id);
        said.as_object_mut().map(|o| o.remove("data"));
        let mut extra = Map::new();
        extra.insert("a2ui_action".into(), said);
        let mid = self.say_with(ws, user, &text, &[], &[], SayTo::default(), extra)?;
        record["message"] = json!(mid);
        let mut meta = meta_of(&m);
        let slot = meta.entry("a2ui").or_insert_with(|| json!({}));
        if !slot.is_object() {
            *slot = json!({});
        }
        slot[&key] = record.clone();
        self.store()
            .message_patch_meta(msg_id, &Value::Object(meta))?;
        Ok(A2uiActed {
            already: false,
            message_id: Some(mid),
            action: record,
        })
    }
}

/// Whether `text` has a ```a2ui (or ~~~a2ui) fence.
pub(crate) fn has_block(text: &str) -> bool {
    text.lines().any(|l| {
        let t = l.trim_start();
        (t.starts_with("```") || t.starts_with("~~~"))
            && t.trim_start_matches(['`', '~']).trim().starts_with("a2ui")
    })
}

#[cfg(test)]
mod tests {
    use super::has_block;

    #[test]
    fn finds_the_fence() {
        assert!(has_block("看图：\n```a2ui\n{}\n```"));
        assert!(has_block("  ~~~ a2ui\n{}\n~~~"));
        assert!(!has_block("```json\n{\"a2ui\":1}\n```"));
        assert!(!has_block("a2ui 是一种协议"));
    }
}
