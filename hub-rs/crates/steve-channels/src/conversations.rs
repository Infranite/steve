//! IM conversations are Steve conversations (channels-personas.md §2.12, domain-model.md §1.6).
//!
//! Every space where Steve answers or drafts has exactly one conversation: the messages of
//! `messages.space = "chan:<space id>"` (the web's 对话 is `space = "steve"`). An inbound message
//! Steve responds to is posted there as the turn of whoever wrote it (author `me`, with
//! `meta.chan` naming the provider, space and sender) and routed by steve-core exactly like web
//! chat: an inline answer, read-only research, a suggestion, or an item when there is real work.
//! What Steve says there goes back out to the space through the channel's own path — the persona,
//! the disclosure check, `[AI]` and proxy marks, the reply target — by [`Channels::conv_said`].
//!
//! The channel-specific parts stay where they were:
//! - the inline answer is composed by the channel composer (persona, safety rules, knowledge by
//!   disclosure level: [`Channels::conv_answer`]), research gets the same frame
//!   ([`Channels::conv_research_context`]);
//! - in a space with outside members (or for your own account, which only drafts) research and items
//!   are off: Steve answers in place, and what would have been an item is offered to you in the
//!   conversation instead;
//! - configuration changes, `#ST-n`, “送到事项” and `/汇报` never come from a chat (steve-core treats
//!   a `chan:` conversation as a guest's).
//!
//! The web shows these conversations read-only (a note links to the space, where a message can be
//! sent by hand through the same outbound checks); the suggestion cards and offers in them are yours
//! to click.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use rusqlite::{OptionalExtension, params};
use serde_json::{Map, Value, json};
use steve_store::base::ObjRef;
use steve_store::base::links::{self, NewLink};

use crate::deliver::{Auth, Delivered, Generation, OutRequest, OutTarget, Outcome, Purpose};
use crate::flow_actions::plan_of;
use crate::reactions::Finish;
use crate::respond::Plan;
use crate::rules::Act;
use crate::spaces::{self, Mode};
use crate::{ChannelResult, Channels, bindings, messages, personas};

/// `messages.space` prefix of IM conversations.
pub const PREFIX: &str = "chan:";

/// The conversation of a space.
pub fn conv_space(space_id: &str) -> String {
    format!("{PREFIX}{space_id}")
}

/// The space of an IM conversation.
pub fn space_of_conv(conv: &str) -> Option<&str> {
    conv.strip_prefix(PREFIX).filter(|s| !s.is_empty())
}

/// The short note an IM space gets when Steve could not answer (the details are in the conversation).
pub fn failure_note(owner: &str) -> String {
    format!("抱歉，这次没能答上来，我请 {owner} 来看。")
}

/// Only the user whose view connection `conn` speaks with reads `space`: their private chat with Steve.
pub fn owner_only(
    c: &rusqlite::Connection,
    ws: &str,
    space: &spaces::SpaceRow,
    conn: &str,
) -> ChannelResult<bool> {
    if space.has_external {
        return Ok(false);
    }
    let (account, _) = crate::bindings::connection_account(c, ws, conn)?;
    let owner = crate::compose::owner_user(c, ws, conn)?;
    let a = crate::disclosure::space_audience(c, space, &account, &owner)?;
    Ok(!a.external && a.scopes.iter().any(|s| s == "private"))
}

/// One turn posted into a conversation and routed like web chat.
#[derive(Debug, Clone, PartialEq)]
pub struct Turn {
    /// `messages.space` (`chan:<space>`; `steve` for your web conversation).
    pub space: String,
    pub text: String,
    /// The message's `meta` (`chan`: where it came from).
    pub meta: Value,
    /// Read-only research may answer it.
    pub research: bool,
    /// It may become an item.
    pub items: bool,
    /// Only the workspace's owner reads the conversation (their private chat with Steve).
    pub owner_only: bool,
    /// The sender is the workspace's owner: what they ask for may start. Anyone else's request
    /// becomes a proposal the owner says yes to first.
    pub start: bool,
}

/// The sender (`party`) is the workspace's owner: a contact linked to the user who owns it. A sender
/// Steve can't place is not the owner.
pub fn sender_is_owner(
    c: &rusqlite::Connection,
    ws: &str,
    party: Option<&str>,
) -> ChannelResult<bool> {
    let Some(party) = party.filter(|p| !p.is_empty()) else {
        return Ok(false);
    };
    Ok(c.query_row(
        "SELECT EXISTS(SELECT 1 FROM parties p JOIN workspace_members wm
            ON wm.user_id = p.user_id AND wm.workspace_id = ?1 AND wm.role = 'owner'
          WHERE p.id = ?2 AND p.workspace_id = ?1)",
        rusqlite::params![ws, party],
        |r| r.get(0),
    )?)
}

/// Steve's conversation, as the channel module needs it (the hub implements it over steve-core).
pub trait Thread: Send + Sync {
    /// Stores the turn and queues it on the conversation's lane; the message id.
    fn say(&self, ws: &str, turn: Turn) -> Result<String, String>;
}

/// How long a generated reply waits for its delivery before it is forgotten.
const GENERATION_TTL: Duration = Duration::from_secs(30 * 60);
/// At most this many are kept; the oldest go first.
const GENERATIONS_MAX: usize = 500;

/// Generated replies waiting to be delivered, by the conversation message they answer.
#[derive(Default)]
pub struct Generations(Mutex<HashMap<String, (Instant, Generation)>>);

impl Generations {
    fn put(&self, origin: &str, g: Generation) {
        self.put_at(origin, g, Instant::now());
    }

    /// Forgets expired ones and then the oldest over the cap, never all of them at once: a
    /// missing generation lets its reply skip the skip and approval checks.
    fn put_at(&self, origin: &str, g: Generation, now: Instant) {
        let mut m = self.0.lock().unwrap_or_else(|e| e.into_inner());
        m.retain(|_, (t, _)| now.saturating_duration_since(*t) < GENERATION_TTL);
        while m.len() >= GENERATIONS_MAX {
            let Some(oldest) = m
                .iter()
                .min_by_key(|(_, (t, _))| *t)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            m.remove(&oldest);
        }
        m.insert(origin.into(), (now, g));
    }

    fn take(&self, origin: &str) -> Option<Generation> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(origin)
            .map(|(_, g)| g)
    }
}

/// `meta.chan` of a turn: what the conversation and its delivery need to know about the source.
fn chan_meta(ch: &Channels, plan: &Plan, command: bool) -> ChannelResult<Value> {
    ch.store().reader(|c| {
        let space = spaces::get(c, &plan.space_id)?
            .ok_or_else(|| steve_modules::ApiError::not_found("空间"))?;
        let provider = provider_name(c, &space.system_id);
        Ok(json!({
            "message": plan.message_id,
            "space": space.id,
            "connection": plan.connection_id,
            "mode": plan.mode.code(),
            "system": space.system_id,
            "provider": provider,
            "space_name": crate::respond::space_label(&space),
            "kind": space.kind,
            "sender": plan.sender_name,
            "party": plan.sender_party_id,
            "act": plan.act.code(),
            "command": command,
        }))
    })
}

impl Channels {
    /// Hands a `reply` / `draft` (or your `/bot` command, `command`) to the conversation of its
    /// space. `None` without a conversation port (the C4 responder then composes directly).
    pub(crate) async fn converse(
        &self,
        plan: &Plan,
        command: bool,
    ) -> ChannelResult<Option<String>> {
        let Some(thread) = self.thread().cloned() else {
            return Ok(None);
        };
        let has_external = self.store().reader(|c| -> ChannelResult<bool> {
            Ok(spaces::get(c, &plan.space_id)?.is_some_and(|s| s.has_external))
        })?;
        // drafts as you, and spaces with outside members, answer in place only
        let open = !has_external && (plan.mode == Mode::Bot || command);
        let owner_only = open
            && self.store().reader(|c| -> ChannelResult<bool> {
                match spaces::get(c, &plan.space_id)? {
                    Some(space) => owner_only(c, &plan.ws, &space, &plan.connection_id),
                    None => Ok(false),
                }
            })?;
        // work starts only when the owner sent it, a /bot command too (any workspace user may send one)
        let start = self
            .store()
            .reader(|c| sender_is_owner(c, &plan.ws, plan.sender_party_id.as_deref()))?;
        let mut meta = json!({ "chan": chan_meta(self, plan, command)? });
        meta["chan"]["owner_only"] = json!(owner_only);
        meta["chan"]["start"] = json!(start);
        let text = match (&plan.trigger, command) {
            (Some(i), true) if !i.trim().is_empty() => i.clone(),
            _ => plan.text.clone(),
        };
        let text = if text.trim().is_empty() {
            "[消息]".to_string()
        } else {
            text
        };
        let turn = Turn {
            space: conv_space(&plan.space_id),
            text,
            meta,
            research: open,
            items: open,
            owner_only,
            start,
        };
        let ws = plan.ws.clone();
        let id = tokio::task::spawn_blocking(move || thread.say(&ws, turn))
            .await
            .map_err(|e| steve_modules::ApiError::Internal(e.to_string()))?
            .map_err(steve_modules::ApiError::Internal)?;
        Ok(Some(id))
    }

    /// Your `/bot` command when no identity speaks in its space: into your web conversation.
    pub(crate) async fn converse_web(
        &self,
        plan: &Plan,
        instruction: &str,
    ) -> ChannelResult<Option<String>> {
        let Some(thread) = self.thread().cloned() else {
            return Ok(None);
        };
        // your web conversation acts with all your powers (settings, items, rerouting): only your own
        // command goes there; any other workspace user's is recorded where it was said
        if !self
            .store()
            .reader(|c| sender_is_owner(c, &plan.ws, plan.sender_party_id.as_deref()))?
        {
            return Err(steve_modules::ApiError::forbidden(
                "只有你本人的 /bot 指令会转到你的网页对话",
            ));
        }
        let mut meta = chan_meta(self, plan, true)?;
        meta["web"] = json!(true);
        let turn = Turn {
            space: "steve".into(),
            text: if instruction.trim().is_empty() {
                plan.text.clone()
            } else {
                instruction.to_string()
            },
            meta: json!({ "chan": meta }),
            research: true,
            items: true,
            owner_only: false,
            start: true, // only the owner's command gets here (above)
        };
        let ws = plan.ws.clone();
        let id = tokio::task::spawn_blocking(move || thread.say(&ws, turn))
            .await
            .map_err(|e| steve_modules::ApiError::Internal(e.to_string()))?
            .map_err(steve_modules::ApiError::Internal)?;
        Ok(Some(id))
    }

    /// The source of a conversation turn: the turn's `meta.chan` and the plan for its inbound message.
    fn turn_source(&self, ws: &str, origin: &str) -> ChannelResult<Option<(Value, Plan)>> {
        let Some(m) = self.store().messages().get(origin)? else {
            return Ok(None);
        };
        if m.workspace_id != ws {
            return Ok(None);
        }
        let chan = m.meta.get("chan").cloned().unwrap_or(Value::Null);
        let Some(cm) = chan["message"].as_str() else {
            return Ok(None);
        };
        let mut plan = self.store().reader(|c| plan_of(c, ws, cm))?;
        if chan["act"] == "draft" {
            plan.act = Act::Draft;
        }
        if chan["command"] == true {
            plan.trigger = Some(m.text.clone());
        }
        Ok(Some((chan, plan)))
    }

    /// The inline answer of a conversation turn (steve-core calls it instead of its own chat
    /// answer): the channel composer, so the persona, the safety rules and the knowledge by
    /// disclosure level hold. Your `/bot` command is the instruction.
    pub async fn conv_answer(&self, ws: &str, origin: &str) -> Result<String, String> {
        let (_, plan) = self
            .turn_source(ws, origin)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "这条消息不是渠道消息".to_string())?;
        let replies = self
            .responder()
            .composer()
            .ok_or_else(|| "没有回复生成器".to_string())?;
        let opts = crate::compose::ComposeOpts {
            persona_id: None,
            instruction: plan.trigger.clone().filter(|i| !i.trim().is_empty()),
            history: None,
        };
        let r = replies
            .generate(self, &plan, &opts)
            .await
            .map_err(|e| e.to_string())?;
        let text = match r.generation.outcome {
            Outcome::Reply => r.text.clone(),
            Outcome::AskOwner => r.text.clone(),
            Outcome::Skip => format!("不需要回复：{}", r.text.trim()),
            Outcome::NeedContext => r.text.clone(),
        };
        self.generations().put(origin, r.generation);
        Ok(text)
    }

    /// What research in a conversation is told about where its answer goes.
    pub fn conv_research_context(&self, ws: &str, conv: &str, origin: &str) -> String {
        let Some(space_id) = space_of_conv(conv) else {
            return String::new();
        };
        let turn = self.store().messages().get(origin).ok().flatten();
        let r = self.store().reader(|c| -> ChannelResult<String> {
            let Some(space) = spaces::get(c, space_id)? else {
                return Ok(String::new());
            };
            let where_ = if space.kind == "dm" { "私聊" } else { "群聊" };
            let who = match bindings::effective(c, &space)? {
                Some(e) => format!("以对外身份「{}」说话", e.persona.name),
                None => "以你本人的名义起草".to_string(),
            };
            // members are read again now; the turn says who asked
            let own = owner_only(c, ws, &space, &space.connection_id)?;
            let chan = turn.as_ref().map(|m| &m.meta["chan"]);
            let card = crate::conversation_card::build(
                c,
                ws,
                &space,
                chan.and_then(|m| m["party"].as_str()),
                chan.and_then(|m| m["sender"].as_str()).unwrap_or_default(),
                &who,
                own,
            )?
            .render();
            if own {
                return Ok(format!(
                    "{card}\n\n这次的问题来自你和 {} 的私聊（{}），只有你本人能看到，回答会发回那里。需要时直接查事项、会话、知识和代码，照实回答。",
                    personas::owner_name(c, ws)?,
                    crate::respond::space_label(&space),
                ));
            }
            let owner = personas::owner_name(c, ws)?;
            Ok(format!(
                "{card}\n\n这次的问题来自渠道「{}」（{where_}{}）。你{who}，回答会发回那里，发出前要过披露检查：\
                 只说结论，不提其他群、其他客户、内部事项编号、文件路径和代码细节；对方这次自己提到的事项编号或说法可以照着提、用来澄清，但它不代表能说这件事的内容，内容仍只按能说的部分说；拿不准能不能说的，就当不知道，或者说会请 {owner} 来看。查不到、查询失败或没有权限时，笼统地说明即可，不说是什么权限、哪里报错。",
                crate::respond::space_label(&space),
                if space.has_external { "，有外部成员" } else { "" },
            ))
        });
        r.unwrap_or_default()
    }

    /// Steve stored message `mid` in conversation `conv`: take it out to the space when it answers a
    /// turn that came from there. Runs in the background; never fails the caller.
    pub fn conv_said(&self, ws: &str, conv: &str, mid: &str) {
        if space_of_conv(conv).is_none() {
            return;
        }
        let (ch, ws, mid) = (self.clone(), ws.to_string(), mid.to_string());
        tokio::spawn(async move {
            if let Err(e) = ch.conv_deliver(&ws, &mid).await {
                tracing::warn!(message = mid, "conversation reply not delivered: {e}");
            }
        });
    }

    /// [`Channels::conv_said`], awaited (tests).
    pub async fn conv_deliver(&self, ws: &str, mid: &str) -> ChannelResult<Option<Delivered>> {
        let Some(m) = self.store().messages().get(mid)? else {
            return Ok(None);
        };
        if m.author != "steve" || m.meta.get("chan_out").is_some() {
            return Ok(None);
        }
        // flow-back cards, hidden notes: the conversation's own
        if m.meta.get("update").is_some()
            || m.meta.get("card_only").is_some_and(|v| v == true)
            || m.meta.get("hidden").is_some_and(|v| v == true)
        {
            return Ok(None);
        }
        let Some(origin) = m.meta.get("origin").and_then(Value::as_str) else {
            return Ok(None);
        };
        let Some((chan, plan)) = self.turn_source(ws, origin)? else {
            return Ok(None);
        };
        let command = chan["command"] == true;
        let generation = self.generations().take(origin);
        // only a reply is said in the space: a question to you goes to your inbox, a skip nowhere
        if let Some(g) = generation.as_ref().filter(|g| g.outcome != Outcome::Reply) {
            self.react_finish(&plan.message_id, Finish::Clear).await;
            if g.outcome == Outcome::AskOwner {
                let q = m.text.clone();
                self.store()
                    .transaction(|tx| crate::respond::open_handoff_asking(tx, &plan, Some(&q)))?;
            }
            return Ok(None);
        }
        if m.meta.get("stopped").is_some_and(|v| v == true) {
            self.react_finish(&plan.message_id, Finish::Clear).await;
            return Ok(None);
        }
        let Some(out) = self.responder().outbound() else {
            return Ok(None);
        };
        if m.meta.get("error").is_some_and(|v| v == true) {
            self.react_finish(&plan.message_id, Finish::Failed).await;
            if plan.mode == Mode::Bot {
                let owner = self
                    .store()
                    .reader(|c| personas::owner_name(c, ws))
                    .unwrap_or_default();
                let d = out
                    .deliver(
                        self,
                        &OutRequest {
                            target: OutTarget::reply(&plan),
                            text: failure_note(&owner),
                            purpose: Purpose::HandoffAck,
                            approve: false,
                            generation: None,
                            auth: Auth::Rule {
                                source: plan.message_id.clone(),
                                act: Act::Reply,
                            },
                        },
                    )
                    .await?;
                self.mark_out(&m, &d)?;
                return Ok(Some(d));
            }
            return Ok(None);
        }
        // an item made from the turn: linked to the message (back to the source when it ends)
        let (text, purpose) = match &m.item_id {
            Some(item) => {
                self.link_item(ws, &plan, item)?;
                (crate::replies::item_ack(item), Purpose::Item)
            }
            None => (said_text(&m.text, &m.meta), Purpose::Reply),
        };
        if text.trim().is_empty() {
            return Ok(None);
        }
        let as_you = plan.mode == Mode::Own;
        let purpose = if command && as_you && purpose == Purpose::Reply {
            Purpose::Trigger
        } else {
            purpose
        };
        let approve = plan.act == Act::Draft
            || (as_you && !command)
            || generation.as_ref().is_some_and(|g| g.needs_approval);
        let auth = if command && as_you {
            Auth::Trigger {
                source: plan.message_id.clone(),
            }
        } else if plan.mode == Mode::Bot {
            Auth::Rule {
                source: plan.message_id.clone(),
                act: Act::Reply,
            }
        } else {
            Auth::None
        };
        let mut target = OutTarget::reply(&plan);
        if command && as_you {
            // said as you in the chat, not as a reply to your own command
            target.reply_to = None;
        }
        let d = out
            .deliver(
                self,
                &OutRequest {
                    target,
                    text,
                    purpose,
                    approve,
                    generation,
                    auth,
                },
            )
            .await?;
        self.mark_out(&m, &d)?;
        Ok(Some(d))
    }

    /// `meta.chan_out` on the conversation message: what became of it in the space.
    fn mark_out(&self, m: &steve_store::Message, d: &Delivered) -> ChannelResult<()> {
        let status = match d {
            Delivered::Sent { .. } => "sent",
            Delivered::Drafted { .. } => "draft",
            Delivered::Blocked { .. } => "blocked",
            Delivered::Failed { .. } => "failed",
            Delivered::Queued { .. } => "pending",
            Delivered::Skipped => "skipped",
        };
        let mut meta: Map<String, Value> = m.meta.as_object().cloned().unwrap_or_default();
        meta.insert(
            "chan_out".into(),
            json!({"message": d.message_id(), "status": status, "decision": d.decision_id()}),
        );
        self.store()
            .messages()
            .patch_meta(&m.id, &Value::Object(meta))?;
        Ok(())
    }

    /// `links(item, from, chan_message)` and `work_item_id`, like a rule's `create_item`.
    fn link_item(&self, ws: &str, plan: &Plan, item: &str) -> ChannelResult<()> {
        self.store().transaction(|tx| -> ChannelResult<()> {
            let it = || ObjRef::new("item", item);
            links::add(
                tx,
                ws,
                &NewLink::new(
                    it(),
                    "from",
                    ObjRef::new("chan_message", &plan.message_id),
                    crate::replies::LINK_SOURCE,
                ),
            )?;
            if let Some(p) = &plan.sender_party_id {
                links::add(
                    tx,
                    ws,
                    &NewLink::new(
                        it(),
                        "raised_by",
                        ObjRef::new("party", p),
                        crate::replies::LINK_SOURCE,
                    ),
                )?;
            }
            tx.execute(
                "UPDATE chan_messages SET work_item_id = ? WHERE id = ?",
                params![item, plan.message_id],
            )?;
            messages::event(tx, ws, &plan.space_id, &plan.message_id);
            Ok(())
        })
    }

    /// Records a turn the C4 responder handled itself (a rule's `create_item`, your `/bot` without a
    /// conversation port's routing) in the space's conversation, so the conversation still has it:
    /// the inbound turn and what Steve said. An item made from it flows back there like one made in
    /// web chat. Best-effort.
    pub(crate) fn conv_note(&self, plan: &Plan, said: &str, item: Option<&str>) {
        if let Err(e) = self.conv_note_inner(plan, said, item) {
            tracing::warn!(message = plan.message_id, "conversation note: {e}");
        }
    }

    fn conv_note_inner(&self, plan: &Plan, said: &str, item: Option<&str>) -> ChannelResult<()> {
        let conv = conv_space(&plan.space_id);
        let meta = json!({ "chan": chan_meta(self, plan, plan.trigger.is_some())? });
        let msgs = self.store().messages();
        let origin = msgs.add(&plan.ws, "me", &plan.text, None, &meta, &conv)?;
        if said.trim().is_empty() {
            return Ok(());
        }
        let reply = msgs.add(
            &plan.ws,
            "steve",
            said,
            item,
            &json!({"origin": origin, "chan_out": {"status": "sent"}}),
            &conv,
        )?;
        if let Some(i) = item {
            let th = self.store().thread();
            th.item_add(i, &plan.ws, &origin, Some(&reply), false)?;
        }
        Ok(())
    }
}

/// The provider's name for a space's system: the bot account's own system (`feishu_app`) is shown
/// as its platform (`feishu` → 飞书).
fn provider_name(c: &rusqlite::Connection, system: &str) -> String {
    let name = |id: &str| -> Option<String> {
        c.query_row(
            "SELECT nullif(name, '') FROM systems WHERE id = ?",
            [id],
            |r| r.get::<_, Option<String>>(0),
        )
        .ok()
        .flatten()
    };
    system
        .strip_suffix("_app")
        .and_then(name)
        .or_else(|| name(system))
        .unwrap_or_else(|| system.to_string())
}

/// The badge of a conversation: where it lives.
fn badge(c: &rusqlite::Connection, space: &crate::spaces::SpaceRow) -> ChannelResult<Value> {
    let provider = provider_name(c, &space.system_id);
    let mode = crate::arbiter::mode_of(c, &space.workspace_id, &space.connection_id)?;
    Ok(json!({
        "space": conv_space(&space.id),
        "space_id": space.id,
        "name": crate::respond::space_label(space),
        "kind": space.kind,
        "dm": space.kind == "dm",
        "system": space.system_id,
        "provider": provider,
        "has_external": space.has_external,
        "member_count": space.member_count,
        "mode": mode.code(),
    }))
}

/// Optional server-side search, state filter and pagination. No limit preserves the legacy list.
#[derive(Debug, Default, serde::Deserialize)]
pub struct ListQuery {
    pub q: Option<String>,
    pub state: Option<String>,
    pub offset: Option<i64>,
    pub limit: Option<i64>,
}

#[derive(Debug, serde::Serialize)]
pub struct ListPage {
    pub conversations: Vec<Value>,
    pub total: i64,
    pub has_more: bool,
    pub next_offset: Option<i64>,
}

fn member(
    c: &rusqlite::Connection,
    ws: &str,
    viewer: &steve_store::base::Viewer,
) -> ChannelResult<()> {
    let allowed: bool = ws == viewer.ws && c.query_row(
        "SELECT EXISTS (SELECT 1 FROM workspace_members WHERE workspace_id = ? AND user_id = ?)",
        params![ws, viewer.user], |r| r.get(0),
    )?;
    if !allowed {
        return Err(steve_modules::ApiError::not_found("对话"));
    }
    Ok(())
}

/// The IM conversations the viewer may see, newest first; no silently omitted older conversations.
pub fn list(
    c: &rusqlite::Connection,
    ws: &str,
    viewer: &steve_store::base::Viewer,
) -> ChannelResult<Vec<Value>> {
    Ok(list_page(c, ws, viewer, &ListQuery::default())?.conversations)
}

pub fn list_page(
    c: &rusqlite::Connection,
    ws: &str,
    viewer: &steve_store::base::Viewer,
    query: &ListQuery,
) -> ChannelResult<ListPage> {
    member(c, ws, viewer)?;
    let state = match query.state.as_deref().unwrap_or("all") {
        "all" => -1,
        "open" => 0,
        "closed" => 1,
        _ => return Err(steve_modules::ApiError::invalid("state")),
    };
    let offset = query.offset.unwrap_or(0);
    if offset < 0 || query.limit.is_some_and(|n| !(1..=200).contains(&n)) {
        return Err(steve_modules::ApiError::invalid("pagination"));
    }
    let q = query.q.as_deref().unwrap_or("").trim();
    if q.chars().count() > 200 {
        return Err(steve_modules::ApiError::invalid("q"));
    }
    // Visibility and preferences are applied before pagination. Search is literal, not LIKE wildcards.
    let cte = format!(
        "WITH grouped AS (
          SELECT space, count(*) AS n, max(created) AS last FROM messages
          WHERE workspace_id = ?1 AND space LIKE 'chan:%' GROUP BY space
        ), visible AS (
          SELECT g.*, s.id, s.name, s.system_id, coalesce(p.closed, 0) AS closed,
            coalesce((SELECT nullif(name, '') FROM systems WHERE id = CASE WHEN s.system_id LIKE '%_app'
              THEN substr(s.system_id, 1, length(s.system_id)-4) ELSE s.system_id END),
              (SELECT nullif(name, '') FROM systems WHERE id = s.system_id), s.system_id) AS provider,
            (SELECT text FROM messages WHERE workspace_id = ?1 AND space = g.space
              ORDER BY created DESC, rowid DESC LIMIT 1) AS last_text
          FROM grouped g JOIN spaces s ON g.space = 'chan:' || s.id
          LEFT JOIN conversation_preferences p ON p.workspace_id = ?1 AND p.user_id = ?2 AND p.space_id = s.id
          WHERE s.workspace_id = ?1 AND {visibility}
        ), filtered AS (
          SELECT * FROM visible WHERE (?3 = -1 OR closed = ?3)
          AND (?4 = '' OR instr(lower(coalesce(name, '') || ' ' || system_id || ' ' || provider || ' ' || last_text), lower(?4)) > 0)
        )", visibility = spaces::visible_sql("?2"));
    let total: i64 = c.query_row(
        &format!("{cte} SELECT count(*) FROM filtered"),
        params![ws, viewer.user, state, q],
        |r| r.get(0),
    )?;
    let mut st = c.prepare(&format!("{cte} SELECT id, space, n, last, closed, last_text FROM filtered ORDER BY last DESC, space ASC LIMIT ?5 OFFSET ?6"))?;
    let rows = st
        .query_map(
            params![ws, viewer.user, state, q, query.limit.unwrap_or(-1), offset],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, bool>(4)?,
                    r.get::<_, String>(5)?,
                ))
            },
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut out = Vec::new();
    for (id, conv, n, last, closed, text) in rows {
        let Some(space) = spaces::get(c, &id)? else {
            continue;
        };
        let mut b = badge(c, &space)?;
        let author: String = c.query_row(
            "SELECT author FROM messages WHERE workspace_id = ? AND space = ? ORDER BY created DESC, rowid DESC LIMIT 1",
            params![ws, conv], |r| r.get(0),
        )?;
        b["count"] = json!(n);
        b["last"] = json!(last);
        b["last_text"] = json!(text.chars().take(80).collect::<String>());
        b["last_author"] = json!(author);
        b["closed"] = json!(closed);
        out.push(b);
    }
    let next = offset.saturating_add(out.len() as i64);
    let has_more = next < total;
    Ok(ListPage {
        conversations: out,
        total,
        has_more,
        next_offset: has_more.then_some(next),
    })
}

/// Personal list preference only: leaves message history and channel processing untouched.
/// The caller runs this inside a store transaction so visibility and mutation share one snapshot.
pub fn set_closed(
    c: &rusqlite::Connection,
    ws: &str,
    viewer: &steve_store::base::Viewer,
    conv: &str,
    closed: bool,
) -> ChannelResult<()> {
    member(c, ws, viewer)?;
    let id = space_of_conv(conv).ok_or_else(|| steve_modules::ApiError::not_found("对话"))?;
    spaces::own(c, viewer, id).map_err(|_| steve_modules::ApiError::not_found("对话"))?;
    let exists: bool = c.query_row(
        "SELECT EXISTS (SELECT 1 FROM messages WHERE workspace_id = ? AND space = ?)",
        params![ws, conv],
        |r| r.get(0),
    )?;
    if !exists {
        return Err(steve_modules::ApiError::not_found("对话"));
    }
    c.execute(
        "INSERT INTO conversation_preferences (workspace_id, user_id, space_id, closed) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(workspace_id, user_id, space_id) DO UPDATE SET closed = excluded.closed",
        params![ws, viewer.user, id, closed],
    )?;
    Ok(())
}

/// `GET /conversations/:space/messages`: the conversation (≤500, oldest first) and its badge.
/// Inbound turns carry `reaction` (the state of their chat message's reaction) and `channel_message`
/// (that chat message as `/spaces/:id/messages` shows it, `null` when it is gone or not of this
/// space), Steve's messages `chan_out.status` as it is now.
pub fn messages_of(
    ch: &Channels,
    ws: &str,
    viewer: &steve_store::base::Viewer,
    conv: &str,
) -> ChannelResult<Value> {
    let id = space_of_conv(conv).ok_or_else(|| steve_modules::ApiError::not_found("对话"))?;
    let b = ch.store().reader(|c| -> ChannelResult<Value> {
        let space =
            spaces::own(c, viewer, id).map_err(|_| steve_modules::ApiError::not_found("对话"))?;
        badge(c, &space)
    })?;
    let msgs = ch.store().messages().list(ws, conv, 500)?;
    let mut out = Vec::with_capacity(msgs.len());
    ch.store().reader(|c| -> ChannelResult<()> {
        for m in msgs {
            let mut v = serde_json::to_value(&m).map_err(steve_modules::ApiError::from)?;
            if let Some(cm) = m.meta["chan"]["message"].as_str() {
                if let Some(r) = crate::reactions::get(c, cm)? {
                    v["reaction"] =
                        serde_json::to_value(r).map_err(steve_modules::ApiError::from)?;
                }
                // the chat message itself (cards, images, files), only from this space
                let src = messages::get(c, cm)
                    .ok()
                    .flatten()
                    .filter(|x| x.space_id == id && in_ws(c, cm, ws));
                v["channel_message"] = match src {
                    Some(x) => serde_json::to_value(x).map_err(steve_modules::ApiError::from)?,
                    None => Value::Null,
                };
            }
            if let Some(out_id) = m.meta["chan_out"]["message"].as_str()
                && let Some((st, decision)) = out_state(c, ws, out_id)?
            {
                v["meta"]["chan_out"]["status"] = json!(st);
                v["meta"]["chan_out"]["decision"] = json!(decision);
            }
            out.push(v);
        }
        Ok(())
    })?;
    Ok(json!({"conversation": b, "messages": out}))
}

/// What became of Steve's message `id` in the space, and the open approval a draft waits on (the
/// conversation answers it in place). A draft whose approval is no longer open (dismissed before
/// that discarded it) never goes out as it stands: it reads `discarded`.
fn out_state(
    c: &rusqlite::Connection,
    ws: &str,
    id: &str,
) -> ChannelResult<Option<(String, Option<String>)>> {
    let row: Option<(String, Option<String>)> = c
        .query_row(
            "SELECT m.status, d.id FROM chan_messages m
               LEFT JOIN decisions d ON d.id = m.decision_id AND d.status = 'open'
              WHERE m.id = ? AND m.workspace_id = ?",
            params![id, ws],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(row.map(|(st, open)| match (st.as_str(), open) {
        ("draft", Some(d)) => (st, Some(d)),
        ("draft", None) => (crate::outbound::DISCARDED.to_string(), None),
        _ => (st, None),
    }))
}

/// Is chat message `id` in workspace `ws`?
fn in_ws(c: &rusqlite::Connection, id: &str, ws: &str) -> bool {
    c.query_row(
        "SELECT EXISTS (SELECT 1 FROM chan_messages WHERE id = ? AND workspace_id = ?)",
        params![id, ws],
        |r| r.get(0),
    )
    .unwrap_or(false)
}

/// What a conversation message says in the space: its text, with the choices of a suggestion card
/// or a question spelled out (chats have no buttons).
fn said_text(text: &str, meta: &Value) -> String {
    let mut t = text.trim().to_string();
    if let Some(cs) = meta["suggest"]["choices"].as_array() {
        let labels: Vec<&str> = cs.iter().filter_map(|c| c["label"].as_str()).collect();
        if !labels.is_empty() {
            t.push_str(&format!("\n（可以回复：{}）", labels.join(" / ")));
        }
    } else if let Some(os) = meta["ask"]["options"].as_array() {
        let opts: Vec<&str> = os.iter().filter_map(Value::as_str).collect();
        if !opts.is_empty() {
            t.push_str(&format!("\n（可以回复：{}）", opts.join(" / ")));
        }
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversation_spaces() {
        assert_eq!(conv_space("spc_1"), "chan:spc_1");
        assert_eq!(space_of_conv("chan:spc_1"), Some("spc_1"));
        assert_eq!(space_of_conv("steve"), None);
        assert_eq!(space_of_conv("chan:"), None);
    }

    #[test]
    fn cards_are_spelled_out() {
        let m = json!({"suggest": {"choices": [{"id": "here", "label": "在这里出方案"}, {"id": "item", "label": "建成事项：X"}]}});
        assert_eq!(
            said_text("要哪种？", &m),
            "要哪种？\n（可以回复：在这里出方案 / 建成事项：X）"
        );
        let m = json!({"ask": {"options": ["前端", "后端"]}});
        assert_eq!(said_text("哪个？", &m), "哪个？\n（可以回复：前端 / 后端）");
        assert_eq!(said_text(" 好 ", &json!({})), "好");
    }

    #[test]
    fn generations_expire_and_evict_the_oldest_not_all() {
        let g = Generations::default();
        let t0 = Instant::now();
        let flagged = Generation {
            needs_approval: true,
            ..Default::default()
        };
        g.put_at("old", flagged.clone(), t0);
        for i in 0..GENERATIONS_MAX {
            g.put_at(
                &format!("m{i}"),
                Generation::default(),
                t0 + Duration::from_millis(1 + i as u64),
            );
        }
        // Over the cap: only the oldest went, the pending ones stayed.
        assert_eq!(g.take("old"), None);
        assert_eq!(g.take("m0"), Some(Generation::default()));
        g.put_at("keep", flagged.clone(), t0 + Duration::from_secs(1));
        assert!(g.0.lock().unwrap().len() <= GENERATIONS_MAX);
        assert_eq!(g.take("keep"), Some(flagged.clone()));
        // After the TTL a new put forgets the stale ones.
        g.put_at(
            "late",
            flagged.clone(),
            t0 + GENERATION_TTL + Duration::from_secs(5),
        );
        assert_eq!(g.take("m1"), None);
        assert_eq!(g.take("late"), Some(flagged));
    }
}
