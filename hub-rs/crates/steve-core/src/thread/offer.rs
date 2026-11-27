//! Offers and conversation-originated items backed by durable `thread_dispatch_refs`
//! (at most one `create_item` dispatch per origin message across races, retries, and restarts).

use super::{lock, meta_of};
use crate::Core;
use crate::error::{CoreError, Result};
use crate::gateway::Offer;
use crate::orchestrator::CreateItem;
use crate::repo::{BotExt, FileRef, Item, Message};
use crate::util::{js_head, js_len, now};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::LazyLock;
use steve_store::{DispatchClaim, ReceiptDispatch, thread};
use steve_types::{PlacementBlocked, PlacementBlocker, PlacementReason, PlacementRejection};

/// Typed lifecycle state stored in `messages.meta.offer.state`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum OfferState {
    Open,
    Taken,
    Dismissed,
}

/// Typed contract for `messages.meta.offer` persisted on inline Steve answers.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct OfferMeta {
    #[serde(default)]
    title: String,
    #[serde(default)]
    goal: String,
    #[serde(default)]
    project: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    bot: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    why: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reason: Option<PlacementReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    rejected: Option<PlacementRejection>,
    state: OfferState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    item: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    folded: Option<bool>,
}

fn record_dispatch_invalid_choice(source: &'static str, reason: PlacementRejection) {
    let reason_code = reason.as_str();
    metrics::counter!(
        "steve_dispatch_invalid_choice_total",
        "source" => source,
        "reason" => reason_code
    )
    .increment(1);
}

fn record_offer_bot_override(from: &'static str) {
    metrics::counter!("steve_offer_bot_override_total", "from" => from).increment(1);
}

fn record_offer_action(action: &'static str, bot_override: &'static str) {
    thread_local! {
        static ACTION_TOTAL: std::cell::RefCell<
            std::collections::HashMap<(&'static str, &'static str), metrics::Counter>,
        > = std::cell::RefCell::new(std::collections::HashMap::new());
    }
    ACTION_TOTAL.with(|cache| {
        cache
            .borrow_mut()
            .entry((action, bot_override))
            .or_insert_with(|| {
                metrics::counter!(
                    "steve_offer_action_total",
                    "action" => action,
                    "bot_override" => bot_override
                )
            })
            .increment(1);
    });
}

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
    /// Durable `thread_dispatch_refs.id` when this action created or appended to an item.
    #[serde(skip)]
    pub dispatch_ref_id: Option<String>,
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
    pub placement_reason: Option<PlacementReason>,
    pub placement_why: Option<String>,
    pub project: Option<String>,
    pub tags: Vec<String>,
    pub files: Vec<FileRef>,
    pub from_question: bool,
    /// the answer it grows out of: the conversation up to it goes into the opening
    pub answer: Option<&'a Message>,
    /// the owner asked for it: work starts. Anyone else: a proposal the owner says yes to first.
    pub start: bool,
    /// The member who said it (`None`: not known), whose access the item's runs use.
    pub by: Option<String>,
}

fn hm(t: i64, zone: steve_runtime::tz::Zone) -> String {
    zone.local(t)
        .map(|d| d.format("%H:%M").to_string())
        .unwrap_or_default()
}

impl Core {
    fn resolve_offer_tags(
        &self,
        ws: &str,
        tag_ids: &[String],
    ) -> Result<(Vec<String>, Vec<String>)> {
        let mut ids = Vec::new();
        let mut names = Vec::new();
        for id in tag_ids {
            let trimmed = id.trim();
            if trimmed.is_empty() || ids.iter().any(|seen| seen == trimmed) {
                continue;
            }
            if let Some(tag) = self.store().tag(trimmed)?.filter(|t| t.workspace_id == ws) {
                ids.push(tag.id);
                names.push(tag.name);
            }
        }
        Ok((ids, names))
    }

    fn validate_offer_bot(
        &self,
        ws: &str,
        bot_id: &str,
        project: Option<&str>,
        reason: PlacementReason,
        why: Option<String>,
    ) -> Result<steve_types::BotPlacement> {
        let db = self.store();
        let team = db
            .store()
            .read(|r| steve_store::bot_dispatch::selected(r, ws, None))?;
        let project_opt = Some(project.map(str::to_owned));
        let default_model = |rt_id: &str| self.models().agent_model(ws, rt_id);
        let (placement, _) = crate::placement::check_bot(
            db,
            self.rt(),
            &crate::placement::DispatchRequest {
                workspace: ws,
                bot: Some(bot_id),
                harness: None,
                model: None,
                effort: None,
                team: team.as_ref(),
                project: &project_opt,
                required_specialties: &[],
                node: None,
                required_nodes: None,
                allow_main: reason != PlacementReason::SteveSelected,
            },
            default_model,
            reason,
            why,
        )?;
        Ok(placement)
    }

    /// `meta.offer` of an answer: validates Steve's chosen Bot without silently swapping, or fills
    /// the sole eligible executor when Steve omitted `bot`.
    pub(crate) fn offer_meta(
        &self,
        ws: &str,
        o: &Offer,
        project: Option<&str>,
        tag_ids: &[String],
    ) -> Result<Value> {
        let (tags, _tag_names) = self.resolve_offer_tags(ws, tag_ids)?;
        let (bot, why, reason, rejected) = if let Some(chosen) = o.bot.as_deref() {
            match self.validate_offer_bot(
                ws,
                chosen,
                project,
                PlacementReason::SteveSelected,
                o.why.clone(),
            ) {
                Ok(placement) => (
                    Some(placement.explanation.bot.as_str().to_owned()),
                    placement.explanation.why,
                    Some(PlacementReason::SteveSelected),
                    None,
                ),
                Err(CoreError::Placement(blocked)) => {
                    record_dispatch_invalid_choice("offer", blocked.reason);
                    (
                        Some(chosen.to_owned()),
                        o.why.clone(),
                        None,
                        Some(blocked.reason),
                    )
                }
                Err(e) => return Err(e),
            }
        } else {
            let db = self.store();
            let team = db
                .store()
                .read(|r| steve_store::bot_dispatch::selected(r, ws, None))?;
            let project_opt = Some(project.map(str::to_owned));
            let default_model = |rt_id: &str| self.models().agent_model(ws, rt_id);
            let assessed = crate::placement::eligible(
                db,
                self.rt(),
                &crate::placement::DispatchRequest {
                    workspace: ws,
                    bot: None,
                    harness: None,
                    model: None,
                    effort: None,
                    team: team.as_ref(),
                    project: &project_opt,
                    required_specialties: &[],
                    node: None,
                    required_nodes: None,
                    allow_main: false,
                },
                default_model,
            )?;
            match assessed.candidates.as_slice() {
                [] => (
                    None,
                    None,
                    None,
                    Some(
                        assessed
                            .primary_rejection
                            .unwrap_or(PlacementRejection::NoCandidates),
                    ),
                ),
                [only] => (
                    Some(only.bot.id.clone()),
                    None,
                    Some(PlacementReason::OnlyEligible),
                    None,
                ),
                _ => (None, None, None, None),
            }
        };
        let offer = OfferMeta {
            title: o.title.clone(),
            goal: o.goal.clone(),
            project: project.map(str::to_owned),
            tags,
            bot,
            why,
            reason,
            rejected,
            state: OfferState::Open,
            item: None,
            folded: None,
        };
        Ok(serde_json::to_value(offer)?)
    }

    /// A new offer folds the older open ones (they stay clickable).
    pub(crate) fn fold_offers(&self, ws: &str, newest: &str) -> Result<()> {
        let space = self.conv_of(newest);
        if space.is_empty() {
            return Ok(());
        }
        for m in self.conv_messages(ws, &space)? {
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

    /// The opening of an item made from the conversation.
    pub(crate) fn opening_for(
        &self,
        ws: &str,
        origin: &str,
        goal: &str,
        answer: Option<&Message>,
    ) -> Result<String> {
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
        let zone = self
            .store()
            .store()
            .read(|r| steve_modules::zone::workspace_tz(r, ws))?;
        let zone = steve_runtime::tz::Zone::parse(zone.name()).expect("IANA zone");
        let line = |m: &Message| {
            let who = if m.author == "me" { "用户" } else { "Steve" };
            let mut s = format!(
                "  {who} {}：{}",
                hm(m.created, zone),
                js_head(&m.text, 4000)
            );
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
        while lines.len() > 2 && lines.iter().map(|l| js_len(l)).sum::<usize>() > 6000 {
            lines.remove(0);
        }
        Ok(format!(
            "{place}\n目标：{goal}\n对话里相关的部分（原文，旧→新）：\n{}\n用户后续在对话里说的话会转给你；你的交付摘要会贴回对话给用户看：第一段用三句话以内写结论，能单独读懂。",
            lines.join("\n")
        ))
    }

    /// Rebuilt opening and attachments for an item created from a conversation.
    pub(crate) fn thread_start(&self, item: &Item) -> Result<Option<(String, Vec<FileRef>)>> {
        let Some(ti) = self
            .store()
            .store()
            .read(|r| thread::dispatch_by_item(r, &item.id))?
        else {
            return Ok(None);
        };
        let goal = self.store().goal(&item.id).unwrap_or_default();
        let answer = self
            .store()
            .store()
            .messages()
            .promoted_to(&item.workspace_id, &item.id)?;
        let opening = self.opening_for(
            &item.workspace_id,
            &ti.origin_message_id,
            &goal,
            answer.as_ref(),
        )?;
        let refs = |m: &Message, key: &str| -> Vec<FileRef> {
            m.meta
                .get(key)
                .cloned()
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or_default()
        };
        let mut files = match self.store().message(&ti.origin_message_id)? {
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

    /// Claim the single origin-level `create_item` dispatch slot and create (or return existing)
    /// item. Returns `(item, already_existed)`.
    pub(crate) fn thread_item_once(&self, n: ThreadItem<'_>) -> Result<(Item, bool, String)> {
        let claim = self.store().store().write(|w| {
            thread::claim_dispatch(w, n.ws, n.origin, "create_item", n.from_question, now())
        })?;
        match claim {
            DispatchClaim::Existing(mut existing) => {
                // If another Core/worker just claimed the dispatch slot and is mid-commit,
                // wait briefly for it to settle so concurrent callers observe idempotency.
                for _ in 0..200 {
                    if existing.state != "pending" {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(5));
                    if let Some(next) = self
                        .store()
                        .store()
                        .read(|r| thread::dispatch_by_origin(r, n.ws, n.origin, "create_item"))?
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
                    return Ok((item, true, existing.id));
                }
                Err(CoreError::Status(
                    409,
                    steve_i18n::t!("core.thread.not_promotable"),
                ))
            }
            DispatchClaim::Claimed(disp) => {
                let opening = Some(self.opening_for(n.ws, n.origin, &n.goal, n.answer)?);
                match self.tx().create_chat_item(CreateItem {
                    ws: n.ws.into(),
                    goal: n.goal,
                    title: n.title,
                    bot: n.bot,
                    placement_reason: n.placement_reason,
                    placement_why: n.placement_why,
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
                }) {
                    Ok(item) => {
                        self.store().store().write(|w| {
                            thread::settle_dispatch(
                                w,
                                &disp.id,
                                "applied",
                                Some(&item.id),
                                None,
                                now(),
                            )
                        })?;
                        Ok((item, false, disp.id))
                    }
                    Err(e) => {
                        let _ = self.store().store().write(|w| {
                            thread::settle_dispatch(w, &disp.id, "rejected", None, None, now())
                        });
                        Err(e)
                    }
                }
            }
        }
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
        let space = if space.is_empty() {
            super::MAIN.to_string()
        } else {
            space
        };
        let blocked = self.unplaced_reply(item);
        let id = self.store().message_add_in(
            ws,
            &space,
            "steve",
            blocked.as_deref().unwrap_or(text),
            Some(&item.id),
            &Value::Object(meta),
        )?;
        self.store()
            .store()
            .write(|w| thread::set_dispatch_reply(w, &item.id, &id))?;
        super::flowback::adopt_quiet(self.store(), &item.id, &id);
        self.said(ws, &space, &id);
        Ok(id)
    }

    pub(crate) fn unplaced_reply(&self, item: &Item) -> Option<String> {
        if item.bot_id.is_some() {
            return None;
        }
        let dispatching = self
            .store()
            .store()
            .read(|r| steve_store::bot_dispatch::is_dispatching(r, &item.id))
            .unwrap_or(false);
        if dispatching {
            return Some(steve_i18n::t!(
                "core.thread.dispatching_execution_bot",
                id = item.id
            ));
        }
        Some(format!(
            "已做成事项 {}。{}",
            item.id,
            steve_i18n::t!("core.thread.choose_execution_bot")
        ))
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
    pub fn offer(
        &self,
        ws: &str,
        msg_id: &str,
        take: bool,
        bot: Option<&str>,
        by: Option<&str>,
    ) -> Result<Handled> {
        self.offer_as(ws, msg_id, take, true, bot, by)
    }

    pub(crate) fn offer_as(
        &self,
        ws: &str,
        msg_id: &str,
        take: bool,
        start: bool,
        bot: Option<&str>,
        by: Option<&str>,
    ) -> Result<Handled> {
        let _one = lock(&self.0.thread.acting);
        let m = self
            .store()
            .message(msg_id)?
            .filter(|m| m.workspace_id == ws)
            .ok_or_else(not_found)?;
        let Some(raw_offer) = m.meta.get("offer").filter(|o| o.is_object()).cloned() else {
            return Err(CoreError::Status(
                400,
                steve_i18n::t!("core.message.no_offer"),
            ));
        };
        let mut offer: OfferMeta = serde_json::from_value(raw_offer)
            .map_err(|_| CoreError::Status(400, steve_i18n::t!("core.message.no_offer")))?;
        let bot_override = bot.map(str::trim).filter(|s| !s.is_empty());
        let override_label = if take && bot_override.is_some() {
            "explicit"
        } else {
            "auto"
        };
        if offer.state != OfferState::Open {
            record_offer_action("already", override_label);
            let origin = self.origin_of(ws, &m)?;
            let item = offer
                .item
                .as_deref()
                .and_then(|i| self.store().item(i).ok().flatten());
            let dispatch_ref_id =
                self.existing_dispatch_id(ws, &origin, item.as_ref().map(|i| i.id.as_str()))?;
            return Ok(Handled {
                message_id: msg_id.into(),
                item,
                already: true,
                dispatch_ref_id,
            });
        }
        let origin = self.origin_of(ws, &m)?;
        let mut meta = meta_of(&m);
        if !take {
            offer.state = OfferState::Dismissed;
            meta.insert("offer".into(), serde_json::to_value(&offer)?);
            self.store()
                .message_patch_meta(msg_id, &Value::Object(meta))?;
            self.store().store().write(|w| {
                thread::update_receipt_outcome(
                    w,
                    &origin,
                    "offer_dismissed",
                    None,
                    ReceiptDispatch::NoDispatch,
                )
            })?;
            record_offer_action("dismiss", "auto");
            return Ok(Handled {
                message_id: msg_id.into(),
                item: None,
                already: false,
                dispatch_ref_id: None,
            });
        }
        let (tags, _tag_names) = self.resolve_offer_tags(ws, &offer.tags)?;
        let override_from = bot_override.map(|_| {
            if offer.rejected.is_some() {
                "invalid"
            } else if offer.bot.is_some() {
                "valid"
            } else {
                "none"
            }
        });
        let (chosen_bot, chosen_reason, chosen_why) = if let Some(explicit_bot) = bot_override {
            self.validate_offer_bot(
                ws,
                explicit_bot,
                offer.project.as_deref(),
                PlacementReason::Explicit,
                None,
            )?;
            (
                Some(explicit_bot.to_owned()),
                Some(PlacementReason::Explicit),
                None,
            )
        } else if let Some(rej) = offer.rejected
            && offer.bot.is_some()
        {
            // Never silently swap a rejected Steve choice: owner must pick another Bot.
            let rejected = offer
                .bot
                .as_ref()
                .map(|b| PlacementBlocker {
                    bot: Some(b.clone().into()),
                    reason: rej,
                })
                .into_iter()
                .collect();
            return Err(CoreError::Placement(PlacementBlocked {
                reason: rej,
                rejected,
            }));
        } else if let Some(selected_bot) = offer.bot.as_deref() {
            let reason = offer.reason.unwrap_or(PlacementReason::SteveSelected);
            self.validate_offer_bot(
                ws,
                selected_bot,
                offer.project.as_deref(),
                reason,
                offer.why.clone(),
            )?;
            (
                Some(selected_bot.to_owned()),
                Some(reason),
                offer.why.clone(),
            )
        } else {
            (None, None, None)
        };
        let from_question = self
            .store()
            .store()
            .read(|r| thread::receipt_by_origin(r, &origin))?
            .is_some_and(|r| r.route_signals.iter().any(|s| s == "question"))
            || super::decide::pure_question(
                &self
                    .store()
                    .message(&origin)?
                    .map(|x| x.text)
                    .unwrap_or_default(),
            );
        let (item, already, disp_id) = self.thread_item_once(ThreadItem {
            ws,
            origin: &origin,
            title: offer.title.clone(),
            goal: offer.goal.clone(),
            bot: chosen_bot,
            placement_reason: chosen_reason,
            placement_why: chosen_why,
            project: offer.project.clone(),
            tags: tags.clone(),
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
        let initial = self
            .store()
            .store()
            .read(|r| steve_store::bot_dispatch::initial(r, &item.id))?;
        offer.state = OfferState::Taken;
        offer.item = Some(item.id.clone());
        offer.tags = tags;
        offer.bot = item.bot_id.clone();
        offer.reason = initial.as_ref().map(|p| p.explanation.reason);
        offer.why = initial.as_ref().and_then(|p| p.explanation.why.clone());
        offer.rejected = None;
        meta.insert("offer".into(), serde_json::to_value(&offer)?);
        meta.remove("chat");
        meta.insert("promoted".into(), json!(item.id));
        self.store()
            .message_patch_meta(msg_id, &Value::Object(meta))?;
        if already {
            record_offer_action("already", override_label);
            return Ok(Handled {
                message_id: msg_id.into(),
                item: Some(item),
                already: true,
                dispatch_ref_id: Some(disp_id),
            });
        }
        record_offer_action("take", override_label);
        if let Some(from) = override_from {
            record_offer_bot_override(from);
        }
        self.store().store().write(|w| {
            thread::update_receipt_outcome(
                w,
                &origin,
                "offer_taken",
                Some(&item.id),
                ReceiptDispatch::Dispatched(&disp_id),
            )
        })?;
        let text = format!(
            "好，做成事项 {} 交给 {} 了，对话内容作为上下文带入。",
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
            dispatch_ref_id: Some(disp_id),
        })
    }

    /// The latest Steve message before `before` carrying an open offer, if it is the last Steve message.
    pub(crate) fn open_offer_before(&self, ws: &str, before: &str) -> Result<Option<Message>> {
        let space = self.conv_of(before);
        if space.is_empty() {
            return Ok(None);
        }
        let all = self.conv_messages(ws, &space)?;
        let at = all.iter().position(|m| m.id == before).unwrap_or(all.len());
        Ok(all[..at]
            .iter()
            .rev()
            .find(|m| m.author == "steve" && m.item_id.is_none())
            .filter(|m| m.meta["offer"]["state"] == "open" && now() - m.created < 24 * 3600 * 1000)
            .cloned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn runtime_dst_table() {
        for (name, text) in [
            ("America/New_York", "2026-03-08 02:15"),
            ("Europe/London", "2026-10-25 01:30"),
        ] {
            let z = steve_runtime::tz::Zone::parse(name).unwrap();
            let wall = chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M").unwrap();
            let ms = z.instant(wall).unwrap();
            assert_eq!(hm(ms, z), z.local(ms).unwrap().format("%H:%M").to_string());
        }
    }
}
