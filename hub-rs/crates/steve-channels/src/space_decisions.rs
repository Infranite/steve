//! The “新群” decision: `{kind:"ask", subject_type:"space",
//! options:["open","mute","quiet","block"]}`, opened when a bot
//! account finds itself in a new group.
//!
//! - `open` (去设置): the space counts as seen (`configured=1`); the result carries `space_id` so the
//!   card goes to `#/channels/space/<id>`.
//! - `mute` (静默 = Steve's role 只记录): `muted=1`; `quiet` (不提醒), `block` (屏蔽): that attention
//!   (migration 019). Each with `configured=1`, a revision and audit like `PATCH /spaces/:id`.
//! - Undo puts `muted`, the attention and `configured` back as they were.
//! - The bot leaving the group (`bot.deleted`, `chat.disbanded`) expires an open one
//!   ([`expire_for_space`]): there is nothing left to set up.
//!
//! Register with `steve_channels::space_decisions::register(&mut registry)`.

use serde_json::{Value, json};
use steve_modules::decisions::{DecisionCx, DecisionHandler};
use steve_modules::{ApiError, ApiResult, Registry};
use steve_store::base;
use steve_store::channel_spaces::{
    preferences::{self, Preference},
    registration, rows,
};
use steve_store::{Decision, Tx, W, audit_repo};

use crate::ChannelResult;
use crate::spaces::{self, Attention, SUBJECT_SPACE};

/// The handler of `subject_type = "space"`.
pub struct NewSpaceDecision;

/// Registers [`NewSpaceDecision`] for `space`.
pub fn register(reg: &mut Registry) {
    reg.decision(SUBJECT_SPACE, Box::new(NewSpaceDecision));
}

/// What the answer sets.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Flags {
    muted: bool,
    attention: Attention,
    configured: bool,
}

fn flags_of(s: &spaces::SpaceSettings) -> Flags {
    Flags {
        muted: s.muted,
        attention: s.attention,
        configured: s.configured,
    }
}

/// Writes `muted`, the attention and `configured` with a revision and audit; returns whether
/// anything changed.
fn set_flags(
    tx: &mut W<'_>,
    ws: &str,
    actor: &str,
    space: &str,
    to: Flags,
    why: &Value,
) -> ApiResult<bool> {
    let before = crate::spaces::settings(tx, space)?;
    let was = flags_of(&before);
    if was == to {
        return Ok(false);
    }
    let now = steve_store::now();
    let revision = before.revision + 1;
    preferences::set_flags(tx, space, to.muted, to.configured, revision, now)?;
    if was.attention != to.attention {
        preferences::set(
            tx,
            ws,
            space,
            Preference::Attention(to.attention.code()),
            now,
        )?;
    }
    let mut fields = Vec::new();
    if was.muted != to.muted {
        fields.push("muted");
    }
    if was.attention != to.attention {
        fields.push("attention");
    }
    if !fields.is_empty() {
        audit_repo::revise(
            tx,
            ws,
            "space",
            space,
            revision,
            &json!({"muted": was.muted, "attention": was.attention.code()}),
            &json!({"muted": to.muted, "attention": to.attention.code()}),
            actor,
        )?;
    }
    let mut detail = json!({ "fields": fields });
    if let (Some(o), Some(w)) = (detail.as_object_mut(), why.as_object()) {
        o.extend(w.clone());
    }
    audit_repo::record(tx, ws, actor, "space.patch", space, &detail)?;
    registration::emit(tx, ws, space);
    Ok(true)
}

/// What `choice` makes of the flags the space had.
fn answered(choice: &str, was: Flags) -> Option<Flags> {
    let mut to = was;
    to.configured = true;
    match choice {
        "open" => {}
        "mute" => to.muted = true,
        "quiet" => to.attention = Attention::Quiet,
        "block" => to.attention = Attention::Blocked,
        _ => return None,
    }
    Some(to)
}

impl DecisionHandler for NewSpaceDecision {
    fn resolve(&self, tx: &mut W<'_>, cx: &DecisionCx, d: &Decision) -> ApiResult<Value> {
        let Some(s) = rows::get_in_workspace(tx, cx.ws, &d.subject_id)? else {
            // the space went away meanwhile: nothing to do
            return Ok(json!({"choice": cx.choice, "gone": true}));
        };
        let before = crate::spaces::settings(tx, &s.id)?;
        let was = flags_of(&before);
        let to = answered(cx.choice, was).ok_or_else(|| ApiError::invalid("choice"))?;
        set_flags(tx, cx.ws, cx.actor, &s.id, to, &json!({"decision": d.id}))?;
        Ok(json!({
            "choice": cx.choice,
            "space_id": s.id,
            "was": {"muted": was.muted, "attention": was.attention.code(), "configured": was.configured},
        }))
    }

    /// Back to the flags the space had before the answer (only when nobody changed them since).
    fn before_undo(&self, tx: &mut W<'_>, d: &Decision) -> ApiResult<()> {
        let Some(s) = rows::get_in_workspace(tx, &d.workspace_id, &d.subject_id)? else {
            return Ok(());
        };
        let w = &d.result["was"];
        let (Some(muted), Some(configured), Some(attention)) = (
            w["muted"].as_bool(),
            w["configured"].as_bool(),
            w["attention"].as_str().and_then(Attention::parse),
        ) else {
            return Ok(());
        };
        let was = Flags {
            muted,
            attention,
            configured,
        };
        let Some(to) = answered(d.result["choice"].as_str().unwrap_or(""), was) else {
            return Ok(());
        };
        let now = flags_of(&crate::spaces::settings(tx, &s.id)?);
        if (now.muted, now.attention) != (to.muted, to.attention) {
            // changed on the space page after the answer: keep what the user set there
            return Ok(());
        }
        set_flags(
            tx,
            &d.workspace_id,
            "steve",
            &s.id,
            was,
            &json!({"decision": d.id, "undo": true}),
        )?;
        Ok(())
    }
}

/// Expires the open “新群” decision of a space (the bot left or the group was disbanded). Returns
/// whether one was open.
pub fn expire_for_space(tx: &Tx, ws: &str, space: &str, reason: &str) -> ChannelResult<bool> {
    let Some(d) = base::decisions::open_for(tx, ws, SUBJECT_SPACE, space)? else {
        return Ok(false);
    };
    Ok(base::decisions::settle(
        tx,
        &d.id,
        &json!({"expired": true, "reason": reason}),
        "expired",
        &Value::Null,
    )?)
}
