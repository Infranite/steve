//! Suggestion cards: when the router is unsure whether a message is work, Steve asks
//! with a card instead of quietly making an item.

use super::offer::{Handled, ThreadItem, not_found};
use super::{Conv, decide::Decision, lock, meta_of};
use crate::Core;
use crate::error::{CoreError, Result};
use crate::gateway::Route;
use crate::repo::{FileRef, Item};
use crate::util::now;
use serde_json::{Value, json};
use steve_store::{NewRevisionTask, ReceiptDispatch, thread};

/// After this long an unanswered card is `expired`.
const EXPIRES: i64 = 24 * 3600 * 1000;

fn closed_card(name: &str, work: Value) -> (String, Value) {
    let q = format!("{name} 已经结束了。这条请求要单独建一个事项，还是在这里讨论？");
    (
        q.clone(),
        json!({
            "question": q,
            "choices": [work, {"id": "here", "label": "在这里讨论"}],
            "state": "open",
        }),
    )
}

pub(crate) fn card(r: &Route, d: &Decision, item: Option<&Item>) -> (String, Value) {
    if let Some(id) = &d.for_item {
        let name = item
            .map(|i| format!("{id}「{}」", i.title))
            .unwrap_or_else(|| id.clone());
        let why = r
            .why
            .trim()
            .trim_end_matches(['。', '.', '；', ';', '，', ',']);
        let because = if why.is_empty() {
            format!("这条和 {name} 是同一件事的补充")
        } else {
            why.to_string()
        };
        let q = format!(
            "{because}，可以和它一起实现和验收。建议追加到 {name}，你想追加进去，还是单独建一个事项？"
        );
        let title = Some(r.title.trim()).filter(|t| !t.is_empty());
        let goal = Some(r.goal.trim()).filter(|g| !g.is_empty());
        let project = r
            .project
            .clone()
            .or_else(|| item.and_then(|i| i.project_id.clone()));
        if item.is_some_and(|i| i.is_closed()) {
            return closed_card(
                &name,
                json!({"id": "new", "label": "单独建项", "title": title, "goal": goal,
                "project": project, "tags": r.tag_ids}),
            );
        }
        return (
            q.clone(),
            json!({
                "question": q,
                "choices": [
                    {"id": "item", "label": format!("追加到 {id}"), "item": id, "recommended": true},
                    {"id": "new", "label": "单独建项", "title": title, "goal": goal,
                     "project": project, "tags": r.tag_ids},
                ],
                "state": "open",
            }),
        );
    }
    let q = if r.reply.trim().is_empty() || r.fallback {
        "这件事要我先在这里出个方案，还是直接建成事项交给 Bot 去做？".to_string()
    } else {
        r.reply.trim().to_string()
    };
    (
        q.clone(),
        json!({
            "question": q,
            "choices": [
                {"id": "here", "label": "在这里出方案"},
                {"id": "item", "label": format!("建成事项：{}", r.title), "title": r.title, "goal": r.goal,
                 "project": r.project, "tags": r.tag_ids},
            ],
            "state": "open",
        }),
    )
}

struct Suggestion<'a> {
    ws: &'a str,
    msg_id: &'a str,
    m: &'a crate::repo::Message,
    om: &'a crate::repo::Message,
    origin: String,
    files: Vec<FileRef>,
    meta: serde_json::Map<String, Value>,
}

impl Core {
    /// Cards nobody answered for 24 hours turn `expired`.
    pub(crate) fn expire_suggestions(&self, ws: &str, space: &str) -> Result<()> {
        for m in self.conv_messages(ws, space)? {
            if m.meta["suggest"]["state"] == "open" && now() - m.created >= EXPIRES {
                let mut meta = meta_of(&m);
                meta["suggest"]["state"] = json!("expired");
                self.store()
                    .message_patch_meta(&m.id, &Value::Object(meta))?;
                if let Some(o) = m.meta.get("origin").and_then(Value::as_str) {
                    self.store().store().write(|w| {
                        thread::update_receipt_outcome(
                            w,
                            o,
                            "suggest_expired",
                            None,
                            ReceiptDispatch::NoDispatch,
                        )
                    })?;
                }
            }
        }
        Ok(())
    }

    /// The user picked on a suggestion card (`POST /api/thread/:id/suggest`).
    /// Preserves caller `by` identity for inline research and item creation alike.
    pub fn suggest(
        &self,
        ws: &str,
        msg_id: &str,
        choice: &str,
        by: Option<&str>,
    ) -> Result<Handled> {
        if !matches!(choice, "here" | "item" | "new") {
            return Err(CoreError::Status(
                400,
                steve_i18n::t!("core.error.invalid", fields = "choice"),
            ));
        }
        let _one = lock(&self.0.thread.acting);
        let m = self
            .store()
            .message(msg_id)?
            .filter(|m| m.workspace_id == ws)
            .ok_or_else(not_found)?;
        let Some(card) = m.meta.get("suggest").filter(|s| s.is_object()).cloned() else {
            return Err(CoreError::Status(
                400,
                steve_i18n::t!("core.message.not_suggestion"),
            ));
        };
        if !matches!(card["state"].as_str(), Some("open" | "expired")) {
            let origin = self.origin_of(ws, &m)?;
            let item = card["item"]
                .as_str()
                .and_then(|i| self.store().item(i).ok().flatten());
            let dispatch_ref_id =
                self.existing_dispatch_id(ws, &origin, item.as_ref().map(|i| i.id.as_str()))?;
            return Ok(Handled {
                message_id: card["answer"].as_str().unwrap_or(msg_id).into(),
                item,
                already: true,
                dispatch_ref_id,
            });
        }
        let origin = self.origin_of(ws, &m)?;
        let om = self.store().message(&origin)?.ok_or_else(not_found)?;
        let files: Vec<FileRef> = om
            .meta
            .get("files")
            .cloned()
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default();
        let pick = card["choices"]
            .as_array()
            .and_then(|cs| cs.iter().find(|c| c["id"] == choice))
            .cloned()
            .ok_or_else(|| CoreError::Status(400, steve_i18n::t!("core.message.no_choice")))?;
        let mut meta = meta_of(&m);
        meta["suggest"]["state"] = json!("chosen");
        meta["suggest"]["chosen"] = json!(choice);
        let suggestion = Suggestion {
            ws,
            msg_id,
            m: &m,
            om: &om,
            origin,
            files,
            meta,
        };
        if choice == "here" {
            return self.answer_suggestion_here(suggestion, by);
        }
        if let Some(target) = pick["item"]
            .as_str()
            .filter(|_| pick.get("title").is_none())
        {
            return self.append_suggestion_to_item(suggestion, target, &card);
        }
        self.create_suggested_item(suggestion, &pick, by)
    }

    /// Queue a durable research revision for the original question, preserving caller `by`.
    fn answer_suggestion_here(
        &self,
        suggestion: Suggestion<'_>,
        by: Option<&str>,
    ) -> Result<Handled> {
        let Suggestion {
            ws,
            msg_id,
            m,
            origin,
            files,
            meta,
            ..
        } = suggestion;
        let project = m.meta["steps"]["route"]["project"]
            .as_str()
            .map(String::from);
        let route_json = m.meta["steps"]
            .get("route")
            .and_then(|v| serde_json::to_string(v).ok());
        let files_json = (!files.is_empty())
            .then(|| serde_json::to_string(&files).ok())
            .flatten();
        let conv = if m.space == super::MAIN {
            Conv::main()
        } else {
            Conv {
                space: m.space.clone(),
                research: true,
                items: false,
                owner_only: false,
                start: false,
                hands_on: false,
            }
        };
        let conv_json = serde_json::to_string(&conv)?;
        self.store().store().write(|w| {
            thread::enqueue_revision(
                w,
                &NewRevisionTask {
                    ws,
                    space: &m.space,
                    origin_message_id: &origin,
                    user_id: by,
                    project_id: project.as_deref(),
                    conv_json: Some(&conv_json),
                    to_item: None,
                    to_reply_to: None,
                    files_json: files_json.as_deref(),
                    mentions_json: None,
                    reply_to_message_id: Some(msg_id),
                    forced_kind: Some("research"),
                    route_json: route_json.as_deref(),
                    offer_json: None,
                    prev_receipt_id: None,
                    read_task_intent_key: None,
                },
            )?;
            thread::update_receipt_outcome(
                w,
                &origin,
                "suggest_here",
                None,
                ReceiptDispatch::NoDispatch,
            )?;
            Ok::<_, steve_store::StoreError>(())
        })?;
        self.store()
            .message_patch_meta(msg_id, &Value::Object(meta))?;
        self.wake_space(ws, &m.space);
        Ok(Handled {
            message_id: msg_id.into(),
            item: None,
            already: false,
            dispatch_ref_id: None,
        })
    }

    fn append_suggestion_to_item(
        &self,
        suggestion: Suggestion<'_>,
        target: &str,
        card: &Value,
    ) -> Result<Handled> {
        let Suggestion {
            ws,
            msg_id,
            m,
            om,
            origin,
            files,
            mut meta,
        } = suggestion;
        let Some(disp_id) = self.forward_open(target, &om.text, &files, &origin)? else {
            let work = card["choices"]
                .as_array()
                .and_then(|cs| cs.iter().find(|c| c["id"] == "new"))
                .cloned()
                .unwrap_or_else(|| json!({"id": "new", "label": "单独建项", "goal": om.text}));
            let (q, next) = closed_card(target, work);
            meta["suggest"] = next;
            self.store()
                .store()
                .messages()
                .patch_text_meta(msg_id, &q, &Value::Object(meta))?;
            self.store().store().write(|w| {
                thread::update_receipt_outcome(
                    w,
                    &origin,
                    "suggest_closed",
                    None,
                    ReceiptDispatch::NoDispatch,
                )
            })?;
            return Ok(Handled {
                message_id: msg_id.into(),
                item: None,
                already: false,
                dispatch_ref_id: None,
            });
        };
        meta["suggest"]["item"] = json!(target);
        self.store()
            .message_patch_meta(msg_id, &Value::Object(meta))?;
        self.store().store().write(|w| {
            thread::update_receipt_outcome(
                w,
                &origin,
                "suggest_item",
                Some(target),
                ReceiptDispatch::Dispatched(&disp_id),
            )
        })?;
        let id = self.store().message_add_in(
            ws,
            &m.space,
            "steve",
            &format!("已作为补充送到 {target}。"),
            Some(target),
            &json!({"origin": origin, "reply_to": msg_id}),
        )?;
        self.store()
            .store()
            .write(|w| thread::set_dispatch_reply_by_id(w, &disp_id, &id))?;
        super::flowback::adopt_quiet(self.store(), target, &id);
        Ok(Handled {
            message_id: id,
            item: self.store().item(target)?,
            already: false,
            dispatch_ref_id: Some(disp_id),
        })
    }

    fn create_suggested_item(
        &self,
        suggestion: Suggestion<'_>,
        pick: &Value,
        by: Option<&str>,
    ) -> Result<Handled> {
        let Suggestion {
            ws,
            msg_id,
            m,
            om,
            origin,
            files,
            mut meta,
        } = suggestion;
        let s = |k: &str| pick[k].as_str().map(String::from);
        let (item, already, disp_id) = self.thread_item_once(ThreadItem {
            ws,
            origin: &origin,
            title: s("title").unwrap_or_else(|| crate::util::js_head(&om.text, 24).into()),
            goal: s("goal").unwrap_or_else(|| om.text.clone()),
            bot: self.named_bot(ws, &om.text)?.map(|b| b.id),
            project: s("project"),
            tags: pick["tags"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|t| t.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
            files,
            from_question: super::decide::pure_question(&om.text),
            by: by.map(String::from),
            answer: Some(m),
            start: true,
        })?;
        meta["suggest"]["item"] = json!(item.id);
        self.store()
            .message_patch_meta(msg_id, &Value::Object(meta))?;
        if already {
            return Ok(Handled {
                message_id: msg_id.into(),
                item: Some(item),
                already: true,
                dispatch_ref_id: Some(disp_id),
            });
        }
        self.store().store().write(|w| {
            thread::update_receipt_outcome(
                w,
                &origin,
                "suggest_item",
                Some(&item.id),
                ReceiptDispatch::Dispatched(&disp_id),
            )
        })?;
        let text = format!("好，建了 {}，交给 {} 了。", item.id, self.bot_name(&item));
        let id = self.announce(
            ws,
            &item,
            &origin,
            &text,
            serde_json::Map::from_iter([("reply_to".to_string(), json!(msg_id))]),
        )?;
        Ok(Handled {
            message_id: id,
            item: Some(item),
            already: false,
            dispatch_ref_id: Some(disp_id),
        })
    }
}
