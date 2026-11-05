//! Corrections: the user moves a message to the other side — an item back into the
//! conversation (“改成在这里回答”), an answer into an item (“做成事项”, the old `promote`), or a message to
//! an existing item. The count of corrections is the router's error rate (`route_log.override`).

use super::offer::{Handled, ThreadItem, not_found};
use super::research::{Inline, InlineJob, QUEUE_FULL};
use super::{Promotion, lock, meta_of, route_step};
use crate::Core;
use crate::error::{CoreError, Result};
use crate::gateway::{self, Said};
use crate::orchestrator::ItemAction;
use crate::repo::{FileRef, Item, Message};
use crate::util::now;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::OnceCell;

/// Where a correction sends a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RerouteTo {
    /// answer it here (an item made from it is dropped)
    Inline,
    /// make it an item
    Item,
    /// send it to this item
    Supplement(String),
}

impl RerouteTo {
    /// `inline` | `item` | `item:ST-n` (the API's `to`).
    pub fn parse(s: &str) -> Option<RerouteTo> {
        match s {
            "inline" => Some(RerouteTo::Inline),
            "item" => Some(RerouteTo::Item),
            _ => s
                .strip_prefix("item:")
                .filter(|id| !id.is_empty())
                .map(|id| RerouteTo::Supplement(id.to_string())),
        }
    }
}

fn files_of(m: &Message, key: &str) -> Vec<FileRef> {
    m.meta
        .get(key)
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default()
}

impl Core {
    /// `POST /api/thread/:id/reroute`. `msg_id` is Steve's reply or the user's own message.
    pub async fn reroute(&self, ws: &str, msg_id: &str, to: RerouteTo) -> Result<Handled> {
        let m = self
            .store()
            .message(msg_id)?
            .filter(|m| m.workspace_id == ws)
            .ok_or_else(not_found)?;
        /* the user's own message: act on Steve's reply to it */
        let (origin, reply) = if m.author == "me" {
            let reply = self
                .store()
                .store()
                .thread()
                .route_of(&m.id)?
                .and_then(|r| r.reply_id)
                .and_then(|r| self.store().message(&r).ok().flatten());
            (m.id.clone(), reply)
        } else {
            (self.origin_of(ws, &m)?, Some(m.clone()))
        };
        match to {
            RerouteTo::Inline => self.to_inline(ws, &origin, reply.as_ref()),
            RerouteTo::Supplement(item) => self.to_supplement(ws, &origin, &item),
            RerouteTo::Item => match reply {
                Some(r) if r.meta["offer"]["state"] == "open" => self.offer(ws, &r.id, true),
                Some(r) if r.meta.get("suggest").is_some_and(Value::is_object) => {
                    self.suggest(ws, &r.id, "item")
                }
                Some(r) if r.item_id.is_some() => Ok(Handled {
                    message_id: r.id.clone(),
                    item: self
                        .store()
                        .item(r.item_id.as_deref().unwrap_or_default())?,
                    already: true,
                }),
                Some(r)
                    if r.meta.get("chat").is_some_and(gateway::truthy)
                        || r.meta.get("promoted").is_some() =>
                {
                    let before = r.meta.get("promoted").is_some();
                    let item = self.promote(ws, &r.id).await?;
                    let id = self
                        .conv_messages(ws, &r.space)?
                        .into_iter()
                        .rfind(|x| x.item_id.as_deref() == Some(item.id.as_str()))
                        .map(|x| x.id)
                        .unwrap_or_else(|| r.id.clone());
                    Ok(Handled {
                        message_id: id,
                        item: Some(item),
                        already: before,
                    })
                }
                _ => self.user_message_to_item(ws, &origin).await,
            },
        }
    }

    /// “改成在这里回答”: an item made from this message is dropped (if not delivered yet) and the
    /// message is researched here, the answer continuing Steve's reply.
    fn to_inline(&self, ws: &str, origin: &str, reply: Option<&Message>) -> Result<Handled> {
        let _one = lock(&self.0.thread.acting);
        let om = self.store().message(origin)?.ok_or_else(not_found)?;
        let th = self.store().store().thread();
        let item = match reply.and_then(|r| r.item_id.clone()) {
            Some(i) => self.store().item(&i)?,
            None => None,
        };
        let Some(reply) = reply else {
            return self.research_again(ws, &om, None, None);
        };
        let Some(item) = item else {
            if reply.meta.get("chat").is_some() || reply.meta.get("answer").is_some() {
                return Ok(Handled {
                    message_id: reply.id.clone(),
                    item: None,
                    already: true,
                });
            }
            return self.research_again(ws, &om, Some(&reply.id), None);
        };
        if item.status == "done" {
            return Err(CoreError::Status(
                409,
                "这件事项已经验收，不能改回对话".into(),
            ));
        }
        if item.status == "drop" {
            return Ok(Handled {
                message_id: reply.id.clone(),
                item: Some(item),
                already: true,
            });
        }
        th.route_override(origin, "to_inline", Some(&item.id))?;
        /* delivered: keep the item, put its delivery summary into the conversation */
        let delivered = self
            .store()
            .decisions_of_item(&item.id)?
            .into_iter()
            .rfind(|d| d.kind == "accept" && d.status == "open");
        if let Some(d) = delivered {
            let id = self.store().message_add_in(
                ws,
                &reply.space,
                "steve",
                if d.draft.trim().is_empty() {
                    "（没有交付摘要）"
                } else {
                    &d.draft
                },
                Some(&item.id),
                &json!({"origin": origin, "reply_to": reply.id, "update": {"kind": "deliver", "item": item.id}}),
            )?;
            return Ok(Handled {
                message_id: id,
                item: Some(item),
                already: false,
            });
        }
        let dropped = self.item_action(&item.id, ItemAction::Drop, "me")?;
        self.store()
            .timeline_add(&item.id, "me", "me", "改在对话里回答", None)?;
        self.research_again(ws, &om, Some(&reply.id), item.project_id.clone())
            .map(|h| Handled {
                item: Some(dropped),
                ..h
            })
    }

    /// Research the user message `om` here; the answer continues `reply_to` when given.
    fn research_again(
        &self,
        ws: &str,
        om: &Message,
        reply_to: Option<&str>,
        project: Option<String>,
    ) -> Result<Handled> {
        let th = self.store().store().thread();
        if reply_to.is_none() {
            th.route_override(&om.id, "to_inline", None)?;
        }
        let route = th.route_of(&om.id)?.map(
            |r| json!({"kind": "research", "modelKind": r.model_kind, "override": "to_inline"}),
        );
        let job = InlineJob {
            kind: Inline::Research,
            origin: om.id.clone(),
            question: om.text.clone(),
            files: files_of(om, "files"),
            earlier: vec![],
            user: None,
            project,
            route,
            offer: None,
            reply_to: reply_to.map(String::from),
            run: None,
            stop: super::Stop::default(),
            t0: now(),
            resume: None,
            space: om.space.clone(),
        };
        if !self.enqueue_inline(ws, job)? {
            /* the item is already back in the conversation; the answer waits for the user to ask again */
            let id = self.post(
                ws,
                &om.space,
                QUEUE_FULL,
                None,
                Some(&json!({"origin": om.id, "reply_to": reply_to})),
            )?;
            return Ok(Handled {
                message_id: id,
                item: None,
                already: false,
            });
        }
        Ok(Handled {
            message_id: reply_to.unwrap_or(&om.id).into(),
            item: None,
            already: false,
        })
    }

    fn to_supplement(&self, ws: &str, origin: &str, item: &str) -> Result<Handled> {
        let it = self
            .store()
            .item(item)?
            .filter(|i| i.workspace_id == ws)
            .ok_or_else(|| CoreError::Status(404, "没有这个事项".into()))?;
        let om = self.store().message(origin)?.ok_or_else(not_found)?;
        self.forward(&it.id, &om.text, &files_of(&om, "files"), origin)?;
        self.store()
            .store()
            .thread()
            .route_override(origin, "to_supplement", Some(&it.id))?;
        let id = self.store().message_add_in(
            ws,
            &om.space,
            "steve",
            &format!("已作为补充送到 {}。", it.id),
            Some(&it.id),
            &json!({"origin": origin}),
        )?;
        super::flowback::adopt_quiet(self.store(), &it.id, &id);
        Ok(Handled {
            message_id: id,
            item: Some(it),
            already: false,
        })
    }

    /// A user message nothing became an item: route it as “做成事项” and make the item.
    async fn user_message_to_item(&self, ws: &str, origin: &str) -> Result<Handled> {
        let om = self.store().message(origin)?.ok_or_else(not_found)?;
        let history = self.history_before(ws, origin)?;
        let r = self.route(ws, &om.text, &history, true).await?;
        let _one = lock(&self.0.thread.acting);
        let th = self.store().store().thread();
        if let Some(i) = th.route_of(origin)?.and_then(|r| r.item_id)
            && let Some(item) = self.store().item(&i)?
        {
            return Ok(Handled {
                message_id: origin.into(),
                item: Some(item),
                already: true,
            });
        }
        let item = self.thread_item(ThreadItem {
            ws,
            origin,
            title: r.title.clone(),
            goal: r.goal.clone(),
            bot: Some(r.bot.clone()),
            project: r.project.clone(),
            tags: r.tag_ids.clone(),
            files: files_of(&om, "files"),
            from_question: super::decide::pure_question(&om.text),
            answer: None,
            start: true, // your action in the web
        })?;
        th.route_override(origin, "to_item", Some(&item.id))?;
        let text = format!("好，做成事项 {} 交给{}了。", item.id, self.bot_name(&item));
        let id = self.announce(ws, &item, origin, &text, Default::default())?;
        Ok(Handled {
            message_id: id,
            item: Some(item),
            already: false,
        })
    }

    /// Turn a chat answer into an item, with the conversation as context. A message becomes at most one item:
    /// clicking again (or from another tab, or while routing) returns the same one. Counts as a correction
    /// (`to_item`) unless the answer offered that work.
    pub async fn promote(&self, ws: &str, msg_id: &str) -> Result<Item> {
        let refuse = || crate::error::msg("这条消息不能转成事项");
        let m = self
            .store()
            .message(msg_id)?
            .filter(|m| m.workspace_id == ws)
            .ok_or_else(refuse)?;
        if let Some(done) = m.meta.get("promoted").and_then(Value::as_str)
            && let Some(i) = self.store().item(done)?
        {
            return Ok(i);
        }
        if m.meta["offer"]["state"] == "open" {
            return self.offer(ws, msg_id, true)?.item.ok_or_else(refuse);
        }
        let cell = {
            let mut p = lock(&self.0.thread.promoting);
            match p.get(msg_id) {
                Some(c) => c.clone(),
                None => {
                    if m.meta.get("chat").is_none_or(|q| !gateway::truthy(q)) {
                        return Err(refuse());
                    }
                    let c: Promotion = Arc::new(OnceCell::new());
                    p.insert(msg_id.into(), c.clone());
                    c
                }
            }
        };
        let r = cell
            .get_or_init(|| async { self.promote_now(ws, &m).await.map_err(|e| e.to_string()) })
            .await
            .clone();
        lock(&self.0.thread.promoting).remove(msg_id);
        r.map_err(crate::CoreError::Msg)
    }

    async fn promote_now(&self, ws: &str, m: &Message) -> Result<Item> {
        let meta = if m.meta.is_object() {
            m.meta.clone()
        } else {
            json!({})
        };
        let q = gateway::jstr(&meta["chat"]);
        /* the conversation up to Steve's answer decides goal and title, not the bare question */
        let all: Vec<Said> = self
            .conv_messages(ws, &m.space)?
            .into_iter()
            .filter(|x| x.created <= m.created && x.id != m.id)
            .map(|x| Said {
                author: x.author,
                text: x.text,
            })
            .collect();
        let mut history: Vec<Said> = all[all.len().saturating_sub(4)..].to_vec();
        history.push(Said {
            author: "steve".into(),
            text: m.text.clone(),
        });
        let t0 = now();
        let r = self.route(ws, &q, &history, true).await?;
        let t1 = now();
        let goal = if r.goal.trim().is_empty() {
            q.clone()
        } else {
            r.goal.trim().to_string()
        };
        let origin = self.origin_of(ws, m)?;
        let item = self.thread_item(ThreadItem {
            ws,
            origin: &origin,
            title: r.title.clone(),
            goal,
            bot: Some(r.bot.clone()),
            project: r.project.clone(),
            tags: r.tag_ids.clone(),
            files: files_of(m, "chatFiles"),
            from_question: super::decide::pure_question(&q),
            answer: Some(m),
            start: true, // your action in the web
        })?;
        let mut meta = meta_of(m);
        meta.remove("chat");
        meta.insert("promoted".into(), json!(item.id));
        self.store()
            .message_patch_meta(&m.id, &Value::Object(meta))?;
        self.store()
            .store()
            .thread()
            .route_override(&origin, "to_item", Some(&item.id))?;
        let mut route = route_step(&r, None, t1 - t0);
        route["kind"] = json!("item");
        route["newTags"] = json!([]);
        let steps = json!({"route": route, "tools": [{"name": "create_item", "args": {"title": r.title, "bot": r.bot, "project": r.project, "tags": r.tag_ids}, "result": {"item": item.id, "bot": item.bot_id, "project": item.project_id}, "status": "ok", "ms": now() - t1}], "ms": now() - t0});
        self.announce(
            ws,
            &item,
            &origin,
            "好，转成事项交给它了，对话内容作为上下文带入。",
            serde_json::Map::from_iter([
                ("steps".to_string(), steps),
                ("reply_to".to_string(), json!(m.id)),
            ]),
        )?;
        Ok(item)
    }
}
