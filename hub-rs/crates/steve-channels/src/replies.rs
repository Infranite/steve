//! [`Replies`]: C4's [`Responder`]. `reply` / `draft` generate the
//! text ([`crate::compose`]); `create_item` creates the item through steve-core (the flow module's
//! `Core` port), links it to the message and its sender, and answers “收到，已记为 <编号>。”.
//! Everything said goes to the [`Outbound`] (C6's [`crate::outbound::Outbox`]; [`DraftOnly`] by
//! default), so this module never sends by itself; it says what would let a message out
//! ([`Auth`]) and the outbound checks it.
//!
//! Trigger words: your own message starting with one of your words, where they work →
//! the rest is the instruction; the reply is generated with the space's last 20 messages and goes
//! out as you (`on_behalf=proxy`, `[AI]`-marked when the settings say), unless the model says it
//! needs approval (an outward promise, money, deleting) — then it is a draft. It is still the
//! composer's reply: nothing else is said as you.
//!
//! On your own account only the composer's reply speaks as you ([`crate::deliver::not_as_you`]):
//! the item and handoff receipts are not said there.

use std::sync::Arc;

use async_trait::async_trait;
use rusqlite::params;
use steve_flow::ports::ItemRequest;
use steve_store::base::ObjRef;
use steve_store::base::links::{self, NewLink};

use crate::compose::{self, ComposeOpts, Composer};
use crate::conversations::sender_is_owner;
use crate::deliver::{Auth, Delivered, DraftOnly, OutRequest, OutTarget, Outbound, Purpose};
use crate::respond::{self, Acted, Plan, Responder};
use crate::rules::{Act, ItemMode};
use crate::spaces::{self, Mode};
use crate::{ChannelResult, Channels, bindings, messages, personas, write_blocks};

/// Context lines put into an item's goal.
pub const ITEM_CONTEXT: usize = 10;
/// An item's title, in characters.
const TITLE_MAX: usize = 80;
/// `links.source` of the links C4 writes.
pub const LINK_SOURCE: &str = "chan";

/// The goal of an item made from a group message: the message, then the conversation before it.
/// What somebody other than the owner wrote goes between the untrusted-data markers (the agent reads
/// it as data, not as the user's instruction): the message itself unless the owner sent it, and the
/// conversation always.
pub fn item_goal(text: &str, context: &[(String, String)], from_owner: bool) -> String {
    use steve_modules::outbound::quote_untrusted;
    let mut talk = String::new();
    for (who, t) in context {
        talk.push_str(&format!(
            "\n- {}：{}",
            if who.is_empty() { "某人" } else { who },
            t
        ));
    }
    let text = text.trim();
    match (from_owner, talk.is_empty()) {
        (true, true) => text.to_string(),
        (true, false) => format!(
            "{text}\n\n最近的对话：\n{}",
            quote_untrusted(talk.trim_start())
        ),
        (false, true) => quote_untrusted(text),
        (false, false) => quote_untrusted(&format!("{text}\n\n最近的对话：{talk}")),
    }
}

/// “收到，已记为 <编号>。”
pub fn item_ack(item_id: &str) -> String {
    ITEM_ACK.replacen("{}", item_id, 1)
}

/// [`item_ack`], `{}` for the item.
pub const ITEM_ACK: &str = "收到，已记为 {}。";
/// [`propose_ack`], `{}` for your name.
pub const PROPOSE_ACK: &str = "收到，我整理成了一条需求，已经提给 {}。";

/// “收到，我整理成了一条需求，已经提给 <主人称呼>。”
pub fn propose_ack(owner: &str) -> String {
    PROPOSE_ACK.replacen("{}", owner, 1)
}

/// C4's responder. Cheap to clone; share one between the builder and the flow actions.
#[derive(Clone)]
pub struct Replies {
    composer: Composer,
    outbound: Arc<dyn Outbound>,
}

impl Default for Replies {
    fn default() -> Self {
        Self::new()
    }
}

impl Replies {
    /// With [`DraftOnly`] as the outbound and the smart tier as the model.
    pub fn new() -> Self {
        Self {
            composer: Composer::new(),
            outbound: Arc::new(DraftOnly),
        }
    }

    /// C6's outbound.
    pub fn outbound(mut self, o: Arc<dyn Outbound>) -> Self {
        self.outbound = o;
        self
    }

    /// The generator (e.g. with a [`compose::PinnedModel`]).
    pub fn composer(mut self, c: Composer) -> Self {
        self.composer = c;
        self
    }

    pub fn composer_ref(&self) -> &Composer {
        &self.composer
    }

    pub fn outbound_ref(&self) -> &dyn Outbound {
        self.outbound.as_ref()
    }

    async fn reply(&self, ch: &Channels, plan: &Plan) -> ChannelResult<Option<Acted>> {
        // writes are refused there for now: no reply is written that could not go out
        let now = ch.now();
        if !ch.store().reader(|c| {
            write_blocks::explicit_request(
                c,
                &plan.ws,
                &plan.connection_id,
                &plan.space_id,
                &plan.message_id,
            )
        })? && let Some(why) = ch
            .store()
            .reader(|c| write_blocks::active(c, &plan.connection_id, &plan.space_id, now))?
        {
            return Ok(Some(Acted {
                label: format!("没有起草回复：{why}"),
                decision_id: None,
                out_message_id: None,
                item_id: None,
                composer_skip: None,
            }));
        }
        // on your own account one sender's run of messages is one turn: answered together. The
        // earlier ones are marked as taken by it now, so whichever composer finishes first, no reply
        // of theirs opens, is approved by YOLO or is sent (crate::presence::take_turn)
        let turn = if plan.mode == Mode::Own && plan.trigger.is_none() {
            ch.store()
                .transaction(|tx| crate::presence::take_turn(tx, &plan.ws, &plan.message_id))?
        } else {
            Vec::new()
        };
        let mut whole = plan.clone();
        if turn.len() > 1 {
            whole.text = turn
                .iter()
                .map(|(_, t)| t.as_str())
                .collect::<Vec<_>>()
                .join("\n");
        }
        let plan = &whole;
        let r = match self
            .composer
            .generate(ch, plan, &ComposeOpts::default())
            .await
        {
            Ok(r) => r,
            Err(e) => {
                // C3's fallback opens an empty draft: the message still reaches you, and the earlier
                // messages of the turn are their own again
                tracing::warn!(space = plan.space_id, "channel reply not generated: {e}");
                if turn.len() > 1 {
                    ch.store()
                        .transaction(|tx| crate::presence::release_turn(tx, &plan.message_id))?;
                }
                return Ok(None);
            }
        };
        let earlier: Vec<String> = turn
            .iter()
            .map(|(id, _)| id.clone())
            .filter(|id| *id != plan.message_id)
            .collect();
        if !earlier.is_empty() {
            ch.store().transaction(|tx| {
                crate::presence::supersede(
                    tx,
                    &plan.ws,
                    &plan.space_id,
                    crate::presence::Answering::Messages(&earlier),
                    crate::presence::BY_LATER,
                    &plan.message_id,
                )
            })?;
        }
        // drafting took a while: you may have answered meanwhile, or the sender went on and a later
        // turn took this one (crate::presence, gate 2)
        if let Some(e) = ch.owner_stop(plan)? {
            tracing::info!(
                space = plan.space_id,
                reason = e.reason(),
                "draft dropped: not said in your name"
            );
            return Ok(Some(Acted {
                label: e.label().into(),
                composer_skip: Some(format!("{}（{}）", e.label(), e.reason())),
                ..Acted::default()
            }));
        }
        // a contact you starred: whatever the composer made of it, it reaches you as a card, its
        // reply (if any) the card's candidate draft (crate::decide)
        let starred = plan.mode == Mode::Own
            && ch
                .store()
                .reader(|c| crate::decide::starred(c, &plan.ws, plan.sender_party_id.as_deref()))?;
        if starred {
            let mut g = r.generation.clone();
            if g.outcome == crate::deliver::Outcome::Reply && g.draft.trim().is_empty() {
                g.draft = r.text.clone();
            }
            if g.outcome != crate::deliver::Outcome::AskOwner {
                g.reason = crate::decide::Reason::Starred.code().into();
            }
            let d = ch.store().transaction(|tx| {
                crate::decide::open_card(
                    tx,
                    plan,
                    &crate::decide::Card::of(&g, "", Mode::Own, true),
                )
            })?;
            return Ok(Some(Acted {
                label: STARRED_ASKED.into(),
                decision_id: Some(d),
                ..Acted::default()
            }));
        }
        if let Some(a) = not_a_reply(ch, plan, &r)? {
            return Ok(Some(a));
        }
        // needs_approval already carries needs_commitment (decide::settle)
        let approve =
            plan.act == Act::Draft || plan.mode == Mode::Own || r.generation.needs_approval;
        let auth = if plan.act == Act::Reply && plan.mode == Mode::Bot {
            Auth::Rule {
                source: plan.message_id.clone(),
                act: Act::Reply,
            }
        } else {
            Auth::None
        };
        let d = self
            .outbound
            .deliver(
                ch,
                &OutRequest {
                    target: OutTarget::reply(plan),
                    text: r.text,
                    purpose: Purpose::Reply,
                    approve,
                    generation: Some(r.generation),
                    auth,
                },
            )
            .await?;
        Ok(Some(acted(&d, None, None)))
    }

    async fn create_item(
        &self,
        ch: &Channels,
        plan: &Plan,
        mode: ItemMode,
    ) -> ChannelResult<Option<Acted>> {
        let Some(flow) = ch.flow() else {
            return Ok(None);
        };
        let ws = plan.ws.clone();
        let (req, owner) = ch.store().reader(|c| -> ChannelResult<_> {
            let space = spaces::get(c, &plan.space_id)?
                .ok_or_else(|| steve_modules::ApiError::not_found("空间"))?;
            let set = spaces::settings(c, &space.id)?;
            let d = &set.item_defaults;
            let persona_bot = match plan.mode {
                Mode::Bot => bindings::effective(c, &space)?.map(|e| e.persona.bot_id),
                Mode::Own => None,
            };
            let bot = compose::pick_bot(c, &ws, &[d["bot_id"].as_str(), persona_bot.as_deref()])?;
            let context = messages::context_before(c, &space.id, &plan.message_id, ITEM_CONTEXT)?;
            let from_owner = sender_is_owner(c, &ws, plan.sender_party_id.as_deref())?;
            let goal = item_goal(&plan.text, &context, from_owner);
            let title = {
                let flat = plan.text.split_whitespace().collect::<Vec<_>>().join(" ");
                if flat.is_empty() {
                    format!("来自「{}」的消息", plan.space_name)
                } else {
                    flat.chars().take(TITLE_MAX).collect()
                }
            };
            let req = bot.map(|bot| ItemRequest {
                requester: None,
                ws: ws.clone(),
                title,
                goal,
                bot,
                project: d["project_id"].as_str().map(String::from),
                tag_ids: compose::str_list(&d["tags"]),
                source: (respond::space_label(&space), LINK_SOURCE.into()),
                propose: mode == ItemMode::Propose,
                accept_auto: false,
                run_id: String::new(),
            });
            Ok((req, personas::owner_name_for_space(c, &ws, &space.id)?))
        })?;
        let Some(req) = req else {
            tracing::warn!(ws, "channel item: the workspace has no Bot");
            return Ok(None);
        };
        let item = match flow.core.create_item(req).await {
            Ok(id) => id,
            Err(e) => {
                tracing::warn!(space = plan.space_id, "channel item not created: {e}");
                return Ok(None);
            }
        };
        ch.store().transaction(|tx| -> ChannelResult<()> {
            let it = || ObjRef::new("item", &item);
            links::add(
                tx,
                &ws,
                &NewLink::new(
                    it(),
                    "from",
                    ObjRef::new("chan_message", &plan.message_id),
                    LINK_SOURCE,
                ),
            )?;
            if let Some(p) = &plan.sender_party_id {
                links::add(
                    tx,
                    &ws,
                    &NewLink::new(it(), "raised_by", ObjRef::new("party", p), LINK_SOURCE),
                )?;
            }
            tx.execute(
                "UPDATE chan_messages SET work_item_id = ? WHERE id = ?",
                params![item, plan.message_id],
            )?;
            messages::event(tx, &ws, &plan.space_id, &plan.message_id);
            Ok(())
        })?;
        let text = match mode {
            ItemMode::Start => item_ack(&item),
            ItemMode::Propose => propose_ack(&owner),
        };
        let d = self
            .outbound
            .deliver(
                ch,
                &OutRequest {
                    target: OutTarget::reply(plan),
                    text,
                    purpose: Purpose::Item,
                    approve: plan.mode == Mode::Own,
                    generation: None,
                    auth: Auth::Rule {
                        source: plan.message_id.clone(),
                        act: plan.act,
                    },
                },
            )
            .await;
        let d = match d {
            Ok(d) => d,
            Err(e) => {
                // the item exists either way
                tracing::warn!(
                    space = plan.space_id,
                    "item acknowledgement not delivered: {e}"
                );
                Delivered::Skipped
            }
        };
        let label = match mode {
            ItemMode::Start => format!("建了事项 {item}"),
            ItemMode::Propose => format!("提了方案 {item}"),
        };
        Ok(Some(acted(&d, Some(item), Some(label))))
    }
}

impl Replies {
    /// Your trigger words: only on your own account (C3 hands them on only for your own
    /// messages there).
    async fn trigger(
        &self,
        ch: &Channels,
        plan: &Plan,
        instruction: &str,
    ) -> ChannelResult<Option<Acted>> {
        if plan.mode != Mode::Own {
            // your `/bot` where a bot speaks: the bot answers it, as itself
            return self.command_as_bot(ch, plan, instruction).await;
        }
        let opts = ComposeOpts {
            persona_id: None,
            instruction: Some(instruction.to_string()).filter(|i| !i.trim().is_empty()),
            history: None,
            revise: None,
        };
        let r = match self.composer.generate(ch, plan, &opts).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(space = plan.space_id, "trigger reply not generated: {e}");
                return Ok(None);
            }
        };
        if let Some(a) = not_a_reply(ch, plan, &r)? {
            return Ok(Some(a));
        }
        // where its settings say: the message's thread, a quoting reply, or a plain message
        let target = crate::command_skill::target(ch, plan)?;
        let d = self
            .outbound
            .deliver(
                ch,
                &OutRequest {
                    target,
                    // your trigger words: out as their settings say, unless the model asks you first
                    approve: r.generation.needs_approval,
                    text: r.text,
                    purpose: Purpose::Trigger,
                    generation: Some(r.generation),
                    auth: Auth::Trigger {
                        source: plan.message_id.clone(),
                    },
                },
            )
            .await?;
        let label = match &d {
            Delivered::Sent { .. } => Some("触发语：已代你发出".to_string()),
            Delivered::Drafted { .. } => Some("触发语：起草，等你批准".to_string()),
            _ => None,
        };
        Ok(Some(acted(&d, None, label)))
    }
}

impl Replies {
    async fn command_as_bot(
        &self,
        ch: &Channels,
        plan: &Plan,
        instruction: &str,
    ) -> ChannelResult<Option<Acted>> {
        let opts = ComposeOpts {
            persona_id: None,
            instruction: Some(instruction.to_string()).filter(|i| !i.trim().is_empty()),
            history: None,
            revise: None,
        };
        let r = match self.composer.generate(ch, plan, &opts).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(space = plan.space_id, "command reply not generated: {e}");
                return Ok(None);
            }
        };
        if let Some(a) = not_a_reply(ch, plan, &r)? {
            return Ok(Some(a));
        }
        let d = self
            .outbound
            .deliver(
                ch,
                &OutRequest {
                    target: OutTarget::reply(plan),
                    approve: r.generation.needs_approval,
                    text: r.text,
                    purpose: Purpose::Reply,
                    generation: Some(r.generation),
                    auth: Auth::Rule {
                        source: plan.message_id.clone(),
                        act: Act::Reply,
                    },
                },
            )
            .await?;
        Ok(Some(acted(&d, None, None)))
    }
}

/// A generation that isn't a reply (compose `Outcome`): nothing goes out. A question to you opens
/// the hand-off in your inbox with it; a skip only records. `None` for a reply.
fn not_a_reply(
    ch: &Channels,
    plan: &Plan,
    r: &crate::compose::Reply,
) -> ChannelResult<Option<Acted>> {
    use crate::deliver::Outcome;
    match r.generation.outcome {
        Outcome::Reply => Ok(None),
        Outcome::AskOwner | Outcome::NeedContext => {
            let d = ch.store().transaction(|tx| {
                crate::decide::open_card(
                    tx,
                    plan,
                    &crate::decide::Card::of(&r.generation, &r.text, plan.mode, false),
                )
            })?;
            Ok(Some(Acted {
                label: ASKED.into(),
                decision_id: Some(d),
                ..Acted::default()
            }))
        }
        Outcome::Skip => Ok(Some(Acted {
            label: format!("{}{}", crate::conversations::NO_REPLY, r.text.trim()),
            composer_skip: Some(skip_reason(&r.text, &r.generation.why)),
            ..Acted::default()
        })),
    }
}

/// What a message the composer turned into a question to you says on its event.
pub const ASKED: &str = "先问你，没有起草";
/// … a starred contact's message, handed to you.
pub const STARRED_ASKED: &str = "星标联系人的消息，交给你";

/// The composer's reason for a skip, for the message's trail: its `why`, else its text.
pub(crate) fn skip_reason(text: &str, why: &str) -> String {
    if why.trim().is_empty() {
        text.trim().to_string()
    } else {
        why.trim().to_string()
    }
}

fn acted(d: &Delivered, item: Option<String>, label: Option<String>) -> Acted {
    Acted {
        label: label.unwrap_or_else(|| match d {
            Delivered::Drafted { .. } => "等你确认回复".into(),
            Delivered::Sent { .. } => "已回复".into(),
            Delivered::Failed { .. } => "回复没有发出去".into(),
            Delivered::Blocked { .. } => "回复没有发出去：这里现在不允许发消息".into(),
            Delivered::Queued { .. } => "回复正在发出".into(),
            Delivered::Skipped => String::new(),
        }),
        decision_id: d.decision_id().map(String::from),
        out_message_id: d.message_id().map(String::from),
        item_id: item,
        composer_skip: None,
    }
}

#[async_trait]
impl Responder for Replies {
    async fn act(&self, ch: &Channels, plan: &Plan) -> ChannelResult<Option<Acted>> {
        if let Some(instruction) = &plan.trigger {
            return self.trigger(ch, plan, instruction).await;
        }
        match plan.act {
            Act::Reply | Act::Draft => self.reply(ch, plan).await,
            Act::CreateItem(m) => self.create_item(ch, plan, m).await,
            Act::Record | Act::Handoff => Ok(None),
        }
    }

    async fn acknowledge(&self, ch: &Channels, plan: &Plan, text: &str) -> ChannelResult<bool> {
        let d = self
            .outbound
            .deliver(
                ch,
                &OutRequest {
                    target: OutTarget::reply(plan),
                    text: text.into(),
                    purpose: Purpose::HandoffAck,
                    approve: plan.mode == Mode::Own,
                    generation: None,
                    auth: Auth::Rule {
                        source: plan.message_id.clone(),
                        act: Act::Handoff,
                    },
                },
            )
            .await?;
        Ok(matches!(d, Delivered::Sent { .. }))
    }

    fn composer(&self) -> Option<&Composer> {
        Some(&self.composer)
    }

    fn outbound(&self) -> Option<&dyn Outbound> {
        Some(self.outbound.as_ref())
    }
}
