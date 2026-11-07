//! Offers and the items the conversation makes: an answer may end with “要我改吗？
//! → 做成事项：<标题>”; taking it (button, or “好，改吧”) makes the item with the conversation as context.
//! Not a decision: YOLO never takes one.

use super::{lock, meta_of};
use crate::Core;
use crate::error::{CoreError, Result};
use crate::gateway::Offer;
use crate::orchestrator::CreateItem;
use crate::repo::{BotExt, FileRef, Item, Message};
use crate::util::{js_head, js_len, now};
use chrono::{Local, TimeZone};
use regex::Regex;
use serde::Serialize;
use serde_json::{Value, json};
use std::sync::LazyLock;

/// The result of a click on an offer, a suggestion card or a correction.
#[derive(Debug, Clone, Serialize)]
pub struct Handled {
    /// Steve's message that says what happened (or the message continued)
    pub message_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item: Option<Item>,
    /// it was handled before: nothing changed now (the API answers 409 with this result)
    #[serde(skip)]
    pub already: bool,
}

/// A short reply that confirms Steve's last proposal.
static CONFIRM: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(好|好的|好啊|行|可以|嗯|对|改吧|做吧|就这么办|按建议|按你说的|那就改|那就做|做成事项|改一下吧|好，?(改|做)吧)[。！!，,～~ ]*$")
        .expect("valid regex")
});

/// Whether `text` only confirms a proposal (“好/改吧/做吧/按建议/可以”).
pub(crate) fn confirms(text: &str) -> bool {
    CONFIRM.is_match(text.trim())
}

pub(crate) fn not_found() -> CoreError {
    CoreError::Status(404, steve_i18n::t!("core.message.not_found"))
}

/// What a new item made from the conversation needs.
pub(crate) struct ThreadItem<'a> {
    pub ws: &'a str,
    /// the user message it starts from
    pub origin: &'a str,
    pub title: String,
    pub goal: String,
    pub bot: Option<String>,
    pub project: Option<String>,
    pub tags: Vec<String>,
    pub files: Vec<FileRef>,
    pub from_question: bool,
    /// the answer it grows out of: the conversation up to it goes into the opening
    pub answer: Option<&'a Message>,
    /// the owner asked for it: work starts. Anyone else: a proposal the owner says yes to first, from a
    /// source of its own (`chat_proposal`) so that nothing (YOLO included) takes it for the owner's
    pub start: bool,
    /// The member who said it (`None`: not known), whose access the item's runs use.
    pub by: Option<String>,
}

fn hm(t: i64) -> String {
    Local
        .timestamp_millis_opt(t)
        .single()
        .map(|d| d.format("%H:%M").to_string())
        .unwrap_or_default()
}

impl Core {
    /// `meta.offer` of an answer: the work, where and by whom it would be done, `open`.
    pub(crate) fn offer_meta(&self, ws: &str, o: &Offer, project: Option<&str>) -> Result<Value> {
        let bot = match project {
            Some(p) => self
                .store()
                .projects(ws)?
                .into_iter()
                .find(|x| x.id == p)
                .and_then(|x| x.default_bot),
            None => None,
        };
        Ok(json!({
            "title": o.title, "goal": o.goal, "project": project,
            "bot": bot.unwrap_or_else(|| "steve".into()), "state": "open",
        }))
    }

    /// A new offer folds the older open ones (they stay clickable).
    pub(crate) fn fold_offers(&self, ws: &str, newest: &str) -> Result<()> {
        for m in self.conv_messages(ws, &self.conv_of(newest))? {
            if m.id != newest
                && m.meta["offer"]["state"] == "open"
                && m.meta["offer"].get("folded").is_none()
            {
                let mut meta = meta_of(&m);
                if let Some(o) = meta.get_mut("offer") {
                    o["folded"] = json!(true);
                }
                self.store()
                    .message_patch_meta(&m.id, &Value::Object(meta))?;
            }
        }
        Ok(())
    }

    /// The opening of an item made from the conversation: the related part of the conversation,
    /// oldest first — the run of messages before the origin (gaps ≤30 min, ≤8 messages, ≤6000 chars)
    /// and everything up to the answer it grows out of, with the files that answer read.
    pub(crate) fn opening_for(
        &self,
        ws: &str,
        origin: &str,
        goal: &str,
        answer: Option<&Message>,
    ) -> Result<String> {
        /* what was said up to the origin (or the answer), however much was said since */
        let all = match self.store().message(origin)? {
            Some(o) => {
                let upto = answer.map_or(o.created, |a| a.created.max(o.created));
                self.store()
                    .store()
                    .messages()
                    .list_until(ws, &o.space, upto, 200)?
            }
            None => vec![],
        };
        let Some(at) = all.iter().position(|m| m.id == origin) else {
            return Ok(format!("这件事来自你和用户的主线对话。\n目标：{goal}"));
        };
        /* a chat's conversation: say where it came from */
        let place = all[at]
            .meta
            .get("chan")
            .filter(|c| c.is_object())
            .map(|c| {
                format!(
                    "这件事来自{}「{}」里 {} 的消息（渠道对话，进展会回到那里）。",
                    c["provider"].as_str().unwrap_or("渠道"),
                    c["space_name"].as_str().unwrap_or(""),
                    c["sender"].as_str().unwrap_or("某人"),
                )
            })
            .unwrap_or_else(|| "这件事来自你和用户的主线对话。".to_string());
        let end = answer
            .and_then(|a| all.iter().position(|m| m.id == a.id))
            .unwrap_or(at)
            .max(at);
        let mut start = at;
        while start > 0
            && at - start < 7
            && all[start].created - all[start - 1].created <= 30 * 60 * 1000
        {
            start -= 1;
        }
        let line = |m: &Message| {
            let who = if m.author == "me" { "用户" } else { "Steve" };
            let mut s = format!("  {who} {}：{}", hm(m.created), js_head(&m.text, 4000));
            let read: Vec<String> = m.meta["steps"]["tools"]
                .as_array()
                .map(|ts| {
                    ts.iter()
                        .filter(|t| t["name"] == "Read")
                        .filter_map(|t| t["args"]["brief"].as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            if !read.is_empty() {
                s.push_str(&format!("\n    读过的文件：{}", read.join("、")));
            }
            s
        };
        let mut lines: Vec<String> = all[start..=end]
            .iter()
            .filter(|m| {
                m.id == origin || answer.is_some_and(|a| a.id == m.id) || m.item_id.is_none()
            })
            .map(line)
            .collect();
        /* over budget: the oldest go first, the origin and the answer stay */
        while lines.len() > 2 && lines.iter().map(|l| js_len(l)).sum::<usize>() > 6000 {
            lines.remove(0);
        }
        Ok(format!(
            "{place}\n目标：{goal}\n对话里相关的部分（原文，旧→新）：\n{}\n用户后续在对话里说的话会转给你；你的交付摘要会贴回对话给用户看：第一段用三句话以内写结论，能单独读懂。",
            lines.join("\n")
        ))
    }

    /// What an item made from the conversation starts with, rebuilt from where it came from: the
    /// conversation up to its origin message (and the answer it was taken from), and the origin's files. A proposal starts with it once
    /// you said yes; `None` for an item that didn't come from a conversation.
    pub(crate) fn thread_start(&self, item: &Item) -> Result<Option<(String, Vec<FileRef>)>> {
        let Some(ti) = self.store().store().thread().item(&item.id)? else {
            return Ok(None);
        };
        let goal = self.store().goal(&item.id).unwrap_or_default();
        // an offer taken from an answer grew out of that answer (marked `promoted` with the item)
        let answer = self
            .store()
            .store()
            .messages()
            .promoted_to(&item.workspace_id, &item.id)?;
        let opening =
            self.opening_for(&item.workspace_id, &ti.origin_msg, &goal, answer.as_ref())?;
        let refs = |m: &Message, key: &str| -> Vec<FileRef> {
            m.meta
                .get(key)
                .cloned()
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or_default()
        };
        // the origin's own files, or (an offer) the files the answer read
        let mut files = match self.store().message(&ti.origin_msg)? {
            Some(m) => refs(&m, "files"),
            None => vec![],
        };
        if let Some(a) = &answer {
            for f in refs(a, "chatFiles") {
                if !files.iter().any(|x| x.id == f.id) {
                    files.push(f);
                }
            }
        }
        Ok(Some((opening, files)))
    }

    /// Make an item from the conversation and remember where it came from (`thread_items`).
    pub(crate) fn thread_item(&self, n: ThreadItem<'_>) -> Result<Item> {
        let opening = Some(self.opening_for(n.ws, n.origin, &n.goal, n.answer)?);
        let item = self.create_item(CreateItem {
            ws: n.ws.into(),
            goal: n.goal,
            title: n.title,
            bot: n.bot,
            project: Some(n.project),
            source: Some(if n.start {
                ("对话".into(), "chat".into())
            } else {
                ("对话里别人的提议".into(), "chat_proposal".into())
            }),
            tag_ids: n.tags,
            opening,
            files: n.files,
            propose: !n.start,
            requester: n.by,
            ..Default::default()
        })?;
        self.store()
            .store()
            .thread()
            .item_add(&item.id, n.ws, n.origin, None, n.from_question)?;
        Ok(item)
    }

    /// Steve's message announcing an item made from the conversation; tied to the item's origin.
    pub(crate) fn announce(
        &self,
        ws: &str,
        item: &Item,
        origin: &str,
        text: &str,
        mut meta: serde_json::Map<String, Value>,
    ) -> Result<String> {
        meta.insert("origin".into(), json!(origin));
        let space = self.conv_of(origin);
        let id = self.store().message_add_in(
            ws,
            &space,
            "steve",
            text,
            Some(&item.id),
            &Value::Object(meta),
        )?;
        let th = self.store().store().thread();
        th.item_reply(&item.id, &id)?;
        super::flowback::adopt_quiet(self.store(), &item.id, &id);
        self.said(ws, &space, &id);
        if th.route_of(origin)?.is_some_and(|r| r.reply_id.is_none()) {
            th.route_reply(origin, &id, Some(&item.id))?;
        }
        Ok(id)
    }

    /// “Steve 开发 Bot” / “Steve” for an item's bot.
    pub(crate) fn bot_name(&self, item: &Item) -> String {
        item.bot_id
            .as_deref()
            .and_then(|b| self.store().bot(b).ok().flatten())
            .map(|b| b.display())
            .unwrap_or_else(|| "它".into())
    }

    /// The question an answer replied to: its `meta.origin`, else the user message right before it.
    pub(crate) fn origin_of(&self, ws: &str, m: &Message) -> Result<String> {
        if let Some(o) = m.meta.get("origin").and_then(Value::as_str) {
            return Ok(o.into());
        }
        Ok(self
            .conv_messages(ws, &m.space)?
            .into_iter()
            .rfind(|x| x.author == "me" && x.created <= m.created)
            .map(|x| x.id)
            .unwrap_or_else(|| m.id.clone()))
    }

    /// Take (`take: true`) or dismiss an offer on Steve's answer `msg_id` (`POST /api/thread/:id/offer`).
    /// Taken by you (the web's button): an item that starts.
    /// `by`: the member who took it, whose access the item runs with.
    pub fn offer(&self, ws: &str, msg_id: &str, take: bool, by: Option<&str>) -> Result<Handled> {
        self.offer_as(ws, msg_id, take, true, by)
    }

    /// `start`: the one taking the offer may start work (see [`ThreadItem::start`]).
    pub(crate) fn offer_as(
        &self,
        ws: &str,
        msg_id: &str,
        take: bool,
        start: bool,
        by: Option<&str>,
    ) -> Result<Handled> {
        let _one = lock(&self.0.thread.acting);
        let m = self
            .store()
            .message(msg_id)?
            .filter(|m| m.workspace_id == ws)
            .ok_or_else(not_found)?;
        let Some(o) = m.meta.get("offer").filter(|o| o.is_object()).cloned() else {
            return Err(CoreError::Status(
                400,
                steve_i18n::t!("core.message.no_offer"),
            ));
        };
        if o["state"] != "open" {
            let item = o["item"]
                .as_str()
                .and_then(|i| self.store().item(i).ok().flatten());
            return Ok(Handled {
                message_id: msg_id.into(),
                item,
                already: true,
            });
        }
        let origin = self.origin_of(ws, &m)?;
        let th = self.store().store().thread();
        let mut meta = meta_of(&m);
        if !take {
            meta["offer"]["state"] = json!("dismissed");
            self.store()
                .message_patch_meta(msg_id, &Value::Object(meta))?;
            th.route_outcome(&origin, "offer_dismissed", None)?;
            return Ok(Handled {
                message_id: msg_id.into(),
                item: None,
                already: false,
            });
        }
        let from_question = th
            .route_of(&origin)?
            .is_some_and(|r| r.signals.iter().any(|s| s == "question"))
            || super::decide::pure_question(
                &self
                    .store()
                    .message(&origin)?
                    .map(|x| x.text)
                    .unwrap_or_default(),
            );
        let item = self.thread_item(ThreadItem {
            ws,
            origin: &origin,
            title: crate::gateway::jstr(&o["title"]),
            goal: crate::gateway::jstr(&o["goal"]),
            bot: o["bot"].as_str().map(String::from),
            project: o["project"].as_str().map(String::from),
            tags: vec![],
            files: m
                .meta
                .get("chatFiles")
                .cloned()
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or_default(),
            from_question,
            start,
            answer: Some(&m),
            by: by.map(String::from),
        })?;
        meta["offer"]["state"] = json!("taken");
        meta["offer"]["item"] = json!(item.id);
        meta.remove("chat");
        meta.insert("promoted".into(), json!(item.id));
        self.store()
            .message_patch_meta(msg_id, &Value::Object(meta))?;
        th.route_outcome(&origin, "offer_taken", Some(&item.id))?;
        let text = format!(
            "好，做成事项 {} 交给{}了，对话内容作为上下文带入。",
            item.id,
            self.bot_name(&item)
        );
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

    /// The latest Steve message before `before` carrying an open offer, if it is the last Steve message.
    pub(crate) fn open_offer_before(&self, ws: &str, before: &str) -> Result<Option<Message>> {
        let all = self.conv_messages(ws, &self.conv_of(before))?;
        let at = all.iter().position(|m| m.id == before).unwrap_or(all.len());
        Ok(all[..at]
            .iter()
            .rev()
            .find(|m| m.author == "steve" && m.item_id.is_none())
            .filter(|m| m.meta["offer"]["state"] == "open" && now() - m.created < 24 * 3600 * 1000)
            .cloned())
    }
}
