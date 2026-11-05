//! Suggestion cards: when the router is unsure whether a message is work, Steve asks
//! with a card — “在这里出方案 / 建成事项：<标题>” — instead of quietly making an item. Only in the
//! conversation (not in the inbox), never taken by YOLO, greyed out after 24 hours but still clickable.

use super::offer::{Handled, ThreadItem, not_found};
use super::research::{Inline, InlineJob};
use super::{decide::Decision, lock, meta_of};
use crate::Core;
use crate::error::{CoreError, Result};
use crate::gateway::Route;
use crate::repo::{FileRef, Item};
use crate::util::now;
use serde_json::{Value, json};

/// After this long an unanswered card is `expired`.
const EXPIRES: i64 = 24 * 3600 * 1000;

/// The card's text and `meta.suggest` for a route the guards turned into a suggestion. About an existing
/// item (`item`, the one the model named): why the message belongs there, the recommendation, and the
/// decision that is actually open — append it to that item, or make an item of its own (decided by the
/// user on 2026-11-05, ST-44; no longer “这是给 ST-n 的吗？ 送到 ST-n / 在这里回答”).
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
        /* an item of its own goes where the item it was close to went, unless the router said otherwise */
        let title = Some(r.title.trim()).filter(|t| !t.is_empty());
        let goal = Some(r.goal.trim()).filter(|g| !g.is_empty());
        let project = r
            .project
            .clone()
            .or_else(|| item.and_then(|i| i.project_id.clone()));
        let bot = Some(r.bot.clone())
            .filter(|b| b != "steve" || r.bot_from_project)
            .or_else(|| {
                item.filter(|i| i.project_id == project)
                    .and_then(|i| i.bot_id.clone())
            });
        return (
            q.clone(),
            json!({
                "question": q,
                "choices": [
                    {"id": "item", "label": format!("追加到 {id}"), "item": id, "recommended": true},
                    {"id": "new", "label": "单独建项", "title": title, "goal": goal,
                     "project": project, "bot": bot, "tags": r.tag_ids},
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
                 "project": r.project, "bot": r.bot, "tags": r.tag_ids},
            ],
            "state": "open",
        }),
    )
}

impl Core {
    /// Cards nobody answered for 24 hours turn `expired` (`route_log.outcome = suggest_expired`).
    pub(crate) fn expire_suggestions(&self, ws: &str, space: &str) -> Result<()> {
        let th = self.store().store().thread();
        for m in self.conv_messages(ws, space)? {
            if m.meta["suggest"]["state"] == "open" && now() - m.created >= EXPIRES {
                let mut meta = meta_of(&m);
                meta["suggest"]["state"] = json!("expired");
                self.store()
                    .message_patch_meta(&m.id, &Value::Object(meta))?;
                if let Some(o) = m.meta.get("origin").and_then(Value::as_str) {
                    th.route_outcome(o, "suggest_expired", None)?;
                }
            }
        }
        Ok(())
    }

    /// The user picked on a suggestion card (`POST /api/thread/:id/suggest`): `here` answers the original
    /// message in place (the answer continues this card), `item` makes the item, or appends the message to
    /// the item a card about an existing item recommends; `new` (that card's other choice) makes an item
    /// of its own.
    pub fn suggest(&self, ws: &str, msg_id: &str, choice: &str) -> Result<Handled> {
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
            let item = card["item"]
                .as_str()
                .and_then(|i| self.store().item(i).ok().flatten());
            return Ok(Handled {
                message_id: card["answer"].as_str().unwrap_or(msg_id).into(),
                item,
                already: true,
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
        let th = self.store().store().thread();
        let pick = card["choices"]
            .as_array()
            .and_then(|cs| cs.iter().find(|c| c["id"] == choice))
            .cloned()
            .ok_or_else(|| CoreError::Status(400, steve_i18n::t!("core.message.no_choice")))?;
        let mut meta = meta_of(&m);
        meta["suggest"]["state"] = json!("chosen");
        meta["suggest"]["chosen"] = json!(choice);
        if choice == "here" {
            let job = InlineJob {
                kind: Inline::Research,
                origin: origin.clone(),
                question: om.text.clone(),
                files,
                earlier: vec![],
                user: None,
                project: m.meta["steps"]["route"]["project"]
                    .as_str()
                    .map(String::from),
                route: m.meta["steps"].get("route").cloned(),
                offer: None,
                reply_to: Some(msg_id.into()),
                run: None,
                stop: super::Stop::default(),
                t0: now(),
                resume: None,
                space: m.space.clone(),
            };
            if !self.enqueue_inline(ws, job)? {
                return Err(CoreError::Status(
                    409,
                    steve_i18n::t!("core.thread.queue_full"),
                ));
            }
            self.store()
                .message_patch_meta(msg_id, &Value::Object(meta))?;
            th.route_outcome(&origin, "suggest_here", None)?;
            return Ok(Handled {
                message_id: msg_id.into(),
                item: None,
                already: false,
            });
        }
        /* a card asking whether the message was for an existing item */
        if let Some(target) = pick["item"]
            .as_str()
            .filter(|_| pick.get("title").is_none())
        {
            self.forward(target, &om.text, &files, &origin)?;
            meta["suggest"]["item"] = json!(target);
            self.store()
                .message_patch_meta(msg_id, &Value::Object(meta))?;
            th.route_outcome(&origin, "suggest_item", Some(target))?;
            let id = self.store().message_add_in(
                ws,
                &m.space,
                "steve",
                &format!("已作为补充送到 {target}。"),
                Some(target),
                &json!({"origin": origin, "reply_to": msg_id}),
            )?;
            super::flowback::adopt_quiet(self.store(), target, &id);
            return Ok(Handled {
                message_id: id,
                item: self.store().item(target)?,
                already: false,
            });
        }
        let s = |k: &str| pick[k].as_str().map(String::from);
        let item = self.thread_item(ThreadItem {
            ws,
            origin: &origin,
            title: s("title").unwrap_or_else(|| crate::util::js_head(&om.text, 24).into()),
            goal: s("goal").unwrap_or_else(|| om.text.clone()),
            bot: s("bot"),
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
            answer: Some(&m),
            start: true, // your action in the web
        })?;
        meta["suggest"]["item"] = json!(item.id);
        self.store()
            .message_patch_meta(msg_id, &Value::Object(meta))?;
        th.route_outcome(&origin, "suggest_item", Some(&item.id))?;
        let text = format!("好，建了 {}，交给{}了。", item.id, self.bot_name(&item));
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
        })
    }
}
