//! Flow-back, as one living card per item: an item made from the conversation
//! has exactly one card in it (`meta.card` on one message), updated in place as the item moves —
//! running, waiting for you, delivered, released, failed, done — with the latest delivery summary and
//! the latest reply to something forwarded from the conversation. Lifecycle events never post a message
//! of their own. The PR follower updates this same card for milestones, not routine snapshots.
//!
//! Where the card is: on Steve's announcement of the item, or on Steve's acknowledgement of the user's
//! latest message to it ([`adopt`]). Only when the user's input is needed (a question, an approval, a
//! delivery to accept, a failure) — or a question's answer arrives — while the card is far up the
//! conversation does it move to a new message at the bottom, quoting the origin once; that message is
//! the item's only card-only message (a later move hides it and posts the next one), so the
//! conversation never fills with one message per event.
//!
//! Release results belong to the notifications; the card only shows “已发布”. A failed Release build
//! after the item is done is a line of its own on the card (`build`), never a version.
//!
//! Called from the orchestrator's state-changing paths (inside its critical section): only the store is
//! touched, never the core lock.

use super::hidden;
use crate::error::Result;
use crate::repo::{Db, Message};
use crate::util::{js_head, now};
use serde_json::{Map, Value, json};

/// Kinds that go back even when the user turned following off.
const ALWAYS: [&str; 3] = ["ask", "deliver", "fail"];
/// The card counts as far up once this many messages are below it.
pub const FAR: usize = 3;

/// What happened to the item.
#[derive(Debug, Clone, Default)]
pub struct Update<'a> {
    /// ask | reply | deliver | done | drop | fail | release | build
    pub kind: &'a str,
    pub text: String,
    /// the decision to answer here (ask, approval, fail, acceptance)
    pub decision: Option<String>,
    /// the forwarded message a reply answers
    pub reference: Option<String>,
    /// the delivery summary in full (items that came from a question)
    pub full: bool,
}

impl Update<'_> {
    /// The user has to do something: answer, approve, accept, or decide what to do about a failure.
    fn needs_you(&self) -> bool {
        matches!(self.kind, "ask" | "fail") || (self.kind == "deliver" && self.decision.is_some())
    }
}

/// The first paragraph of a summary, ≤600 chars.
pub fn first_paragraph(s: &str) -> String {
    let p = s
        .trim()
        .split("\n\n")
        .find(|p| !p.trim().is_empty())
        .unwrap_or("")
        .trim();
    js_head(p, 600).to_string()
}

fn card_of(m: &Message) -> Option<&Value> {
    m.meta.get("card").filter(|c| c.is_object())
}

fn card_only(m: &Message) -> bool {
    m.meta.get("card_only").is_some_and(crate::gateway::truthy)
}

/// `u` applied to the card `c`.
fn merged(c: Option<&Value>, item: &str, u: &Update<'_>, at: i64) -> Value {
    let mut c = c.cloned().unwrap_or_else(|| json!({}));
    c["item"] = json!(item);
    c["at"] = json!(at);
    c["state"] = json!(u.kind);
    match u.kind {
        "deliver" => {
            let mut d = json!({"text": u.text, "full": u.full, "at": at});
            if let Some(x) = &u.decision {
                d["decision"] = json!(x);
            }
            c["delivery"] = d;
        }
        "reply" => c["reply"] = json!({"text": u.text, "ref": u.reference, "at": at}),
        "release" => c["released"] = json!({"version": u.text, "at": at}),
        "build" => c["build"] = json!({"text": u.text, "at": at}),
        _ => {}
    }
    /* the line of what is going on now (a question, a failure); a later event clears it */
    if matches!(u.kind, "ask" | "fail") {
        c["text"] = json!(u.text);
    } else if let Some(o) = c.as_object_mut() {
        o.remove("text");
    }
    match &u.decision {
        Some(d) => c["decision"] = json!(d),
        None => {
            if let Some(o) = c.as_object_mut() {
                o.remove("decision");
            }
        }
    }
    c
}

/// The message carrying the item's card: the latest visible one with `meta.card` for it, else the
/// announcement. Index into `visible`.
fn host(visible: &[Message], item: &str, announce: Option<&str>) -> Option<usize> {
    visible
        .iter()
        .rposition(|m| card_of(m).is_some_and(|c| c["item"] == item))
        .or_else(|| announce.and_then(|a| visible.iter().position(|m| m.id == a)))
}

fn with_meta(m: &Message, f: impl FnOnce(&mut Map<String, Value>)) -> Value {
    let mut meta = m.meta.as_object().cloned().unwrap_or_default();
    f(&mut meta);
    Value::Object(meta)
}

/// The card leaves `m`: a card-only message is hidden, any other keeps its text (and the item
/// reference the conversation shows for it).
fn release_host(db: &Db, m: &Message) -> Result<()> {
    let meta = with_meta(m, |o| {
        if card_only(m) {
            o.insert("hidden".into(), json!(true));
        } else if let Some(c) = o.remove("card") {
            o.insert("was_card".into(), c);
        }
    });
    db.message_patch_meta(&m.id, &meta)
}

/// The text of a card-only message: what the card is about now (shown when the card cannot be drawn).
fn headline(item: &str, card: &Value) -> String {
    let s = |k: &str| card.get(k).and_then(Value::as_str).unwrap_or("");
    match s("state") {
        "deliver" => format!(
            "{item} 交付了：{}",
            card["delivery"]["text"].as_str().unwrap_or("")
        ),
        "ask" | "fail" => s("text").to_string(),
        "build" => card["build"]["text"].as_str().unwrap_or(item).to_string(),
        _ => item.to_string(),
    }
}

/// The conversation an item's origin message is in (`steve` for the web's; `chan:…` for an IM
/// space's): its flow-back goes there.
fn conv_of(db: &Db, origin: &str) -> String {
    db.message(origin)
        .ok()
        .flatten()
        .map(|m| m.space)
        .unwrap_or_else(|| super::MAIN.into())
}

/// Post `u` for `item` if it came from the conversation; the message whose card shows it.
pub fn flow(db: &Db, item: &str, u: Update<'_>) -> Result<Option<String>> {
    let Some(ti) = db
        .store()
        .read(|r| steve_store::thread::dispatch_by_item(r, item))?
    else {
        return Ok(None);
    };
    if !ti.follow && !ALWAYS.contains(&u.kind) {
        return Ok(None);
    }
    let at = now();
    let space = conv_of(db, &ti.origin_message_id);
    let visible: Vec<Message> = db
        .messages_in(&ti.workspace_id, &space, 200)?
        .into_iter()
        .filter(|m| !hidden(m))
        .collect();
    let h = host(&visible, item, ti.reply_message_id.as_deref());
    let below = h.map_or(usize::MAX, |i| visible.len() - 1 - i);
    let card = merged(h.and_then(|i| card_of(&visible[i])), item, &u, at);
    /* an answer to a question is worth reading where the user is now, like the user's input */
    let urgent = u.needs_you() || (u.kind == "deliver" && u.full);
    let text = headline(item, &card);
    if let Some(i) = h
        && !(urgent && below >= FAR)
    {
        let m = &visible[i];
        let meta = with_meta(m, |o| {
            o.insert("card".into(), card.clone());
        });
        if card_only(m) {
            db.store().messages().patch_text_meta(&m.id, &text, &meta)?;
        } else {
            db.message_patch_meta(&m.id, &meta)?;
        }
        db.store()
            .write(|w| steve_store::thread::set_dispatch_last_post(w, item, at))?;
        return Ok(Some(m.id.clone()));
    }
    if let Some(i) = h {
        release_host(db, &visible[i])?;
    }
    /* a question's answer is a normal message (the summary is its text); otherwise the card is the message */
    let full = u.kind == "deliver" && u.full;
    let text = if full { u.text } else { text };
    let meta = json!({"card": card, "card_only": !full, "reply_to": ti.origin_message_id});
    let id = db.message_add_in(&ti.workspace_id, &space, "steve", &text, Some(item), &meta)?;
    db.store()
        .write(|w| steve_store::thread::set_dispatch_last_post(w, item, at))?;
    Ok(Some(id))
}

/// [`flow`] that only logs a failure: flow-back never breaks the item operation it hangs off.
pub fn flow_quiet(db: &Db, item: &str, u: Update<'_>) {
    if let Err(e) = flow(db, item, u) {
        tracing::warn!(item, "thread flow-back: {e}");
    }
}

/// The item is working again (it went on, was resumed, or handed to Steve): a failure its card still
/// shows is over. The card goes back to running in place, never moves, and is left alone in any other
/// state; the failure itself stays in the item's timeline. No-op for items not made from the conversation.
pub fn recover(db: &Db, item: &str) -> Result<()> {
    let Some(ti) = db
        .store()
        .read(|r| steve_store::thread::dispatch_by_item(r, item))?
    else {
        return Ok(());
    };
    let visible: Vec<Message> = db
        .messages_in(&ti.workspace_id, &conv_of(db, &ti.origin_message_id), 200)?
        .into_iter()
        .filter(|m| !hidden(m))
        .collect();
    let Some(m) = host(&visible, item, ti.reply_message_id.as_deref()).map(|i| &visible[i]) else {
        return Ok(());
    };
    let Some(c) = card_of(m).filter(|c| c["state"] == "fail") else {
        return Ok(());
    };
    let card = merged(
        Some(c),
        item,
        &Update {
            kind: "run",
            ..Default::default()
        },
        now(),
    );
    let meta = with_meta(m, |o| {
        o.insert("card".into(), card.clone());
    });
    if card_only(m) {
        db.store()
            .messages()
            .patch_text_meta(&m.id, &headline(item, &card), &meta)?;
        Ok(())
    } else {
        db.message_patch_meta(&m.id, &meta)
    }
}

/// [`recover`] that only logs a failure.
pub fn recover_quiet(db: &Db, item: &str) {
    if let Err(e) = recover(db, item) {
        tracing::warn!(item, "thread card: {e}");
    }
}

/// The card goes on `msg` (Steve's announcement of the item, or its acknowledgement of the user's latest
/// message to it): the card it had elsewhere comes along, that message lets it go. No-op for items not
/// made from the conversation.
pub fn adopt(db: &Db, item: &str, msg: &str) -> Result<()> {
    let Some(ti) = db
        .store()
        .read(|r| steve_store::thread::dispatch_by_item(r, item))?
    else {
        return Ok(());
    };
    let visible: Vec<Message> = db
        .messages_in(&ti.workspace_id, &conv_of(db, &ti.origin_message_id), 200)?
        .into_iter()
        .filter(|m| !hidden(m) && m.id != msg)
        .collect();
    let mut card = json!({"item": item, "state": "run", "at": now()});
    if let Some(i) = host(
        &visible,
        item,
        ti.reply_message_id.as_deref().filter(|r| *r != msg),
    ) {
        let m = &visible[i];
        if let Some(c) = card_of(m) {
            card = c.clone();
        }
        release_host(db, m)?;
    }
    if let Some(m) = db.message(msg)? {
        let meta = with_meta(&m, |o| {
            o.insert("card".into(), card);
        });
        db.message_patch_meta(msg, &meta)?;
    }
    Ok(())
}

/// [`adopt`] that only logs a failure.
pub fn adopt_quiet(db: &Db, item: &str, msg: &str) {
    if let Err(e) = adopt(db, item, msg) {
        tracing::warn!(item, "thread card: {e}");
    }
}

/// The delivery summary as it goes back: the first paragraph, or all of it (≤4000) when the item came
/// from a question.
pub fn delivery(db: &Db, item: &str, summary: &str) -> Result<(String, bool)> {
    let q = db
        .store()
        .read(|r| steve_store::thread::dispatch_by_item(r, item))?
        .is_some_and(|t| t.from_question);
    Ok(if q {
        (js_head(summary.trim(), 4000).to_string(), true)
    } else {
        (first_paragraph(summary), false)
    })
}
