//! Corrections: move a message between inline research, item creation, and supplement.
//! Backed by durable `thread_dispatch_refs` (no in-memory `OnceCell`) and task generation fencing
//! so an in-flight worker can never commit a stale reply after an explicit reroute.

use super::offer::{Handled, ThreadItem, not_found};
use super::{Conv, lock, meta_of, route_step};
use crate::Core;
use crate::error::{CoreError, Result};
use crate::gateway::{self, Route, Said};
use crate::orchestrator::ItemAction;
use crate::repo::{FileRef, Item, Message};
use crate::util::{js_head, now};
use serde_json::{Value, json};
use steve_store::{NewRevisionTask, ReceiptDispatch, thread};

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
    /// `POST /api/thread/:id/reroute`. Preserves caller `by` and fences any in-flight task for `origin`.
    pub async fn reroute(
        &self,
        ws: &str,
        msg_id: &str,
        to: RerouteTo,
        by: Option<&str>,
    ) -> Result<Handled> {
        let m = self
            .store()
            .message(msg_id)?
            .filter(|m| m.workspace_id == ws)
            .ok_or_else(not_found)?;
        let (origin, reply) = if m.author == "me" {
            let reply = self
                .store()
                .store()
                .read(|r| thread::latest_receipt_by_origin(r, &m.id))?
                .and_then(|r| r.reply_message_id)
                .and_then(|r| self.store().message(&r).ok().flatten());
            (m.id.clone(), reply)
        } else {
            (self.origin_of(ws, &m)?, Some(m.clone()))
        };
        match to {
            RerouteTo::Inline => self.to_inline(ws, &origin, reply.as_ref(), by),
            RerouteTo::Supplement(item) => self.to_supplement(ws, &origin, &item),
            RerouteTo::Item => {
                self.fence_origin(ws, &origin, "reroute")?;
                match reply {
                    Some(r) if r.meta["offer"]["state"] == "open" => {
                        self.offer(ws, &r.id, true, None, by)
                    }
                    Some(r) if r.item_id.is_some() => {
                        let item = self
                            .store()
                            .item(r.item_id.as_deref().unwrap_or_default())?;
                        let dispatch_ref_id = self.existing_dispatch_id(
                            ws,
                            &origin,
                            item.as_ref().map(|i| i.id.as_str()),
                        )?;
                        Ok(Handled {
                            message_id: r.id.clone(),
                            item,
                            already: true,
                            dispatch_ref_id,
                        })
                    }
                    Some(r)
                        if r.meta.get("chat").is_some_and(gateway::truthy)
                            || r.meta.get("promoted").is_some() =>
                    {
                        let existing_before = r.meta.get("promoted").is_some()
                            || self
                                .store()
                                .store()
                                .read(|rd| {
                                    thread::dispatch_by_origin(rd, ws, &origin, "create_item")
                                })?
                                .is_some_and(|d| d.state == "applied");
                        let item = self.promote(ws, &r.id, by).await?;
                        let dispatch_ref_id =
                            self.existing_dispatch_id(ws, &origin, Some(&item.id))?;
                        let id = self
                            .conv_messages(ws, &r.space)?
                            .into_iter()
                            .rfind(|x| x.item_id.as_deref() == Some(item.id.as_str()))
                            .map(|x| x.id)
                            .unwrap_or_else(|| r.id.clone());
                        Ok(Handled {
                            message_id: id,
                            item: Some(item),
                            already: existing_before,
                            dispatch_ref_id,
                        })
                    }
                    _ => self.user_message_to_item(ws, &origin, by).await,
                }
            }
        }
    }

    /// “改成在这里回答”: drop an undelivered item and enqueue a durable inline research revision.
    fn to_inline(
        &self,
        ws: &str,
        origin: &str,
        reply: Option<&Message>,
        by: Option<&str>,
    ) -> Result<Handled> {
        let _one = lock(&self.0.thread.acting);
        let om = self.store().message(origin)?.ok_or_else(not_found)?;
        let item = match reply.and_then(|r| r.item_id.clone()) {
            Some(i) => self.store().item(&i)?,
            None => None,
        };
        let Some(reply) = reply else {
            return self.research_again(ws, &om, None, None, by);
        };
        let Some(item) = item else {
            if reply.meta.get("chat").is_some() || reply.meta.get("answer").is_some() {
                return Ok(Handled {
                    message_id: reply.id.clone(),
                    item: None,
                    already: true,
                    dispatch_ref_id: None,
                });
            }
            return self.research_again(ws, &om, Some(&reply.id), None, by);
        };
        if item.is_completed() {
            return Err(CoreError::Status(
                409,
                steve_i18n::t!("core.thread.accepted_no_chat"),
            ));
        }
        if item.is_closed() {
            return Ok(Handled {
                message_id: reply.id.clone(),
                item: Some(item),
                already: true,
                dispatch_ref_id: None,
            });
        }
        self.store().store().write(|w| {
            thread::update_receipt_override(
                w,
                origin,
                "to_inline",
                Some(&item.id),
                ReceiptDispatch::NoDispatch,
                now(),
            )
        })?;
        let delivered = self
            .store()
            .decisions_of_item(&item.id)?
            .into_iter()
            .rfind(|d| d.kind == "accept" && d.status == steve_types::DecisionStatus::Open);
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
                dispatch_ref_id: None,
            });
        }
        let dropped = self.item_action(&item.id, ItemAction::Drop, "me")?;
        self.store()
            .timeline_add(&item.id, "me", "me", "改在对话里回答", None)?;
        self.research_again(ws, &om, Some(&reply.id), item.project_id.clone(), by)
            .map(|h| Handled {
                item: Some(dropped),
                ..h
            })
    }

    /// Fence any active task for `om` and enqueue a durable inline research revision preserving `by`.
    fn research_again(
        &self,
        ws: &str,
        om: &Message,
        reply_to: Option<&str>,
        project: Option<String>,
        by: Option<&str>,
    ) -> Result<Handled> {
        self.fence_origin(ws, &om.id, "reroute")?;
        let route_json = self
            .store()
            .store()
            .read(|r| thread::receipt_by_origin(r, &om.id))?
            .map(|r| {
                json!({"kind": "research", "modelKind": r.model_kind, "override": "to_inline"})
                    .to_string()
            });
        let files = files_of(om, "files");
        let files_json = (!files.is_empty())
            .then(|| serde_json::to_string(&files).ok())
            .flatten();
        let conv = if om.space == super::MAIN {
            Conv::main()
        } else {
            Conv {
                space: om.space.clone(),
                research: true,
                items: false,
                owner_only: false,
                start: false,
                hands_on: false,
            }
        };
        let conv_json = serde_json::to_string(&conv)?;
        self.store().store().write(|w| {
            if reply_to.is_none() {
                thread::update_receipt_override(
                    w,
                    &om.id,
                    "to_inline",
                    None,
                    ReceiptDispatch::NoDispatch,
                    now(),
                )?;
            }
            thread::enqueue_revision(
                w,
                &NewRevisionTask {
                    ws,
                    space: &om.space,
                    origin_message_id: &om.id,
                    user_id: by,
                    project_id: project.as_deref(),
                    conv_json: Some(&conv_json),
                    to_item: None,
                    to_reply_to: None,
                    files_json: files_json.as_deref(),
                    mentions_json: None,
                    reply_to_message_id: reply_to,
                    forced_kind: Some("research"),
                    route_json: route_json.as_deref(),
                    offer_json: None,
                    prev_receipt_id: None,
                    read_task_intent_key: None,
                },
            )?;
            Ok::<_, steve_store::StoreError>(())
        })?;
        self.wake_space(ws, &om.space);
        Ok(Handled {
            message_id: reply_to.unwrap_or(&om.id).into(),
            item: None,
            already: false,
            dispatch_ref_id: None,
        })
    }

    fn to_supplement(&self, ws: &str, origin: &str, item: &str) -> Result<Handled> {
        let it = self
            .store()
            .item(item)?
            .filter(|i| i.workspace_id == ws)
            .ok_or_else(|| CoreError::Status(404, steve_i18n::t!("core.item.not_found")))?;
        let om = self.store().message(origin)?.ok_or_else(not_found)?;
        self.fence_origin(ws, origin, "reroute")?;
        let disp_id = self.forward(&it.id, &om.text, &files_of(&om, "files"), origin)?;
        self.store().store().write(|w| {
            thread::update_receipt_override(
                w,
                origin,
                "to_supplement",
                Some(&it.id),
                ReceiptDispatch::Dispatched(&disp_id),
                now(),
            )
        })?;
        let id = self.store().message_add_in(
            ws,
            &om.space,
            "steve",
            &format!("已作为补充送到 {}。", it.id),
            Some(&it.id),
            &json!({"origin": origin}),
        )?;
        self.store()
            .store()
            .write(|w| thread::set_dispatch_reply_by_id(w, &disp_id, &id))?;
        super::flowback::adopt_quiet(self.store(), &it.id, &id);
        Ok(Handled {
            message_id: id,
            item: Some(it),
            already: false,
            dispatch_ref_id: Some(disp_id),
        })
    }

    async fn user_message_to_item(
        &self,
        ws: &str,
        origin: &str,
        by: Option<&str>,
    ) -> Result<Handled> {
        let om = self.store().message(origin)?.ok_or_else(not_found)?;
        if let Some(existing) = self
            .store()
            .store()
            .read(|r| thread::dispatch_by_origin(r, ws, origin, "create_item"))?
            .filter(|d| d.state == "applied")
            && let Some(ref item_id) = existing.target_item_id
            && let Some(item) = self.store().item(item_id)?
        {
            return Ok(Handled {
                message_id: existing.reply_message_id.unwrap_or_else(|| origin.into()),
                item: Some(item),
                already: true,
                dispatch_ref_id: Some(existing.id),
            });
        }
        let history = self.history_before(ws, origin)?;
        let r = match self.route(ws, &om.text, &history, true).await {
            Ok(r) => Some(r),
            Err(CoreError::Route(e)) => {
                tracing::warn!(
                    ws,
                    origin,
                    "reroute to_item naming route failed; using message prefix: {e}"
                );
                None
            }
            Err(e) => return Err(e),
        };
        let title = r
            .as_ref()
            .map(|r| r.title.trim())
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| js_head(&om.text, 24).to_string());
        let goal = r
            .as_ref()
            .map(|r| r.goal.trim())
            .filter(|g| !g.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| om.text.clone());
        let project = r.as_ref().and_then(|r| r.project.clone());
        let tags = r.map(|r| r.tag_ids).unwrap_or_default();
        let _one = lock(&self.0.thread.acting);
        let (item, already, disp_id) = self.thread_item_once(ThreadItem {
            ws,
            origin,
            title,
            goal,
            bot: self.named_bot(ws, &om.text)?.map(|b| b.id),
            placement_reason: None,
            placement_why: None,
            project,
            tags,
            files: files_of(&om, "files"),
            from_question: super::decide::pure_question(&om.text),
            answer: None,
            start: true,
            by: by.map(String::from),
        })?;
        if already {
            return Ok(Handled {
                message_id: origin.into(),
                item: Some(item),
                already: true,
                dispatch_ref_id: Some(disp_id),
            });
        }
        self.store().store().write(|w| {
            thread::update_receipt_override(
                w,
                origin,
                "to_item",
                Some(&item.id),
                ReceiptDispatch::Dispatched(&disp_id),
                now(),
            )
        })?;
        let text = format!(
            "好，做成事项 {} 交给 {} 了。",
            item.id,
            self.bot_name(&item)
        );
        let id = self.announce(ws, &item, origin, &text, Default::default())?;
        Ok(Handled {
            message_id: id,
            item: Some(item),
            already: false,
            dispatch_ref_id: Some(disp_id),
        })
    }

    /// Turn a chat answer into an item. Durable `thread_dispatch_refs` guarantees at most one
    /// item per origin across concurrent calls, tabs, offer races, and restarts (no `OnceCell`).
    pub async fn promote(&self, ws: &str, msg_id: &str, by: Option<&str>) -> Result<Item> {
        let refuse = || crate::error::msg(steve_i18n::t!("core.thread.not_promotable"));
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
        let origin = self.origin_of(ws, &m)?;
        if let Some(existing) = self
            .store()
            .store()
            .read(|r| thread::dispatch_by_origin(r, ws, &origin, "create_item"))?
            .filter(|d| d.state == "applied")
            && let Some(ref item_id) = existing.target_item_id
            && let Some(item) = self.store().item(item_id)?
        {
            return Ok(item);
        }
        if m.meta["offer"]["state"] == "open" {
            return self
                .offer(ws, msg_id, true, None, by)?
                .item
                .ok_or_else(refuse);
        }
        if m.meta.get("chat").is_none_or(|q| !gateway::truthy(q)) {
            return Err(refuse());
        }
        let q = gateway::jstr(&m.meta["chat"]);
        let from_question = super::decide::pure_question(&q);
        // Claim the durable origin-level dispatch slot BEFORE invoking the router so concurrent
        // promotions wait for the single in-flight promotion without duplicating model calls.
        let claim = self.store().store().write(|w| {
            thread::claim_dispatch(w, ws, &origin, "create_item", from_question, now())
        })?;
        let disp = match claim {
            steve_store::DispatchClaim::Claimed(d) => d,
            steve_store::DispatchClaim::Existing(mut existing) => {
                for _ in 0..1200 {
                    if existing.state != "pending" {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                    if let Some(next) = self
                        .store()
                        .store()
                        .read(|r| thread::dispatch_by_origin(r, ws, &origin, "create_item"))?
                    {
                        existing = next;
                    } else {
                        break;
                    }
                }
                if existing.state == "applied"
                    && let Some(ref item_id) = existing.target_item_id
                    && let Some(item) = self.store().item(item_id)?
                {
                    return Ok(item);
                }
                return Err(refuse());
            }
        };

        let all: Vec<Said> = match self.conv_messages(ws, &m.space) {
            Ok(msgs) => msgs
                .into_iter()
                .filter(|x| x.created <= m.created && x.id != m.id)
                .map(|x| Said {
                    author: x.author,
                    text: x.text,
                })
                .collect(),
            Err(e) => {
                let _ = self
                    .store()
                    .store()
                    .write(|w| thread::settle_dispatch(w, &disp.id, "rejected", None, None, now()));
                return Err(e);
            }
        };
        let mut history: Vec<Said> = all[all.len().saturating_sub(4)..].to_vec();
        history.push(Said {
            author: "steve".into(),
            text: m.text.clone(),
        });
        let t0 = now();
        let r = match self.route(ws, &q, &history, true).await {
            Ok(r) => Some(r),
            Err(CoreError::Route(e)) => {
                tracing::warn!(
                    ws,
                    msg = msg_id,
                    "promote naming route failed; using message prefix: {e}"
                );
                None
            }
            Err(e) => {
                let _ = self
                    .store()
                    .store()
                    .write(|w| thread::settle_dispatch(w, &disp.id, "rejected", None, None, now()));
                return Err(e);
            }
        };
        let t1 = now();
        let title = r
            .as_ref()
            .map(|r| r.title.trim())
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| js_head(&q, 24).to_string());
        let goal = r
            .as_ref()
            .map(|r| r.goal.trim())
            .filter(|g| !g.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| q.clone());
        let project = r.as_ref().and_then(|r| r.project.clone());
        let tag_ids = r.as_ref().map(|r| r.tag_ids.clone()).unwrap_or_default();
        let _one = lock(&self.0.thread.acting);
        let opening = match self.opening_for(ws, &origin, &goal, Some(&m)) {
            Ok(o) => Some(o),
            Err(e) => {
                let _ = self
                    .store()
                    .store()
                    .write(|w| thread::settle_dispatch(w, &disp.id, "rejected", None, None, now()));
                return Err(e);
            }
        };
        let bot = match self.named_bot(ws, &q) {
            Ok(b) => b.map(|b| b.id),
            Err(e) => {
                let _ = self
                    .store()
                    .store()
                    .write(|w| thread::settle_dispatch(w, &disp.id, "rejected", None, None, now()));
                return Err(e);
            }
        };
        let item = match self.tx().create_chat_item(crate::CreateItem {
            ws: ws.into(),
            goal,
            title: title.clone(),
            bot,
            project: Some(project.clone()),
            source: Some(("对话".into(), "chat".into())),
            tag_ids: tag_ids.clone(),
            opening,
            files: files_of(&m, "chatFiles"),
            propose: false,
            requester: by.map(String::from),
            ..Default::default()
        }) {
            Ok(item) => {
                self.store().store().write(|w| {
                    thread::settle_dispatch(w, &disp.id, "applied", Some(&item.id), None, now())
                })?;
                item
            }
            Err(e) => {
                let _ = self
                    .store()
                    .store()
                    .write(|w| thread::settle_dispatch(w, &disp.id, "rejected", None, None, now()));
                return Err(e);
            }
        };
        let mut meta = meta_of(&m);
        meta.remove("chat");
        meta.insert("promoted".into(), json!(item.id));
        if let Some(offer) = meta.get_mut("offer").and_then(|v| v.as_object_mut())
            && offer.get("state").and_then(Value::as_str) == Some("open")
        {
            offer.insert("state".into(), json!("taken"));
            offer.insert("item".into(), json!(item.id));
        }
        self.store()
            .message_patch_meta(&m.id, &Value::Object(meta))?;
        self.store().store().write(|w| {
            thread::update_receipt_override(
                w,
                &origin,
                "to_item",
                Some(&item.id),
                ReceiptDispatch::Dispatched(&disp.id),
                now(),
            )
        })?;
        let fallback_route;
        let route_for_step = match r.as_ref() {
            Some(r) => r,
            None => {
                fallback_route =
                    Route::deterministic("item", &q, None, "路由模型不可用，按目标开头起名");
                &fallback_route
            }
        };
        let mut route = route_step(route_for_step, None, t1 - t0);
        route["kind"] = json!("item");
        route["newTags"] = json!([]);
        let steps = json!({"route": route, "tools": [{"name": "create_item", "args": {"title": title, "project": project, "tags": tag_ids}, "result": {"item": item.id, "bot": item.bot_id, "project": item.project_id}, "status": "ok", "ms": now() - t1}], "ms": now() - t0});
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
