//! The “新群” decision (§8 待处理): `{kind:"ask", subject_type:"space",
//! options:["open","mute","quiet","block"]}` (older ones have only the first two), opened when a bot
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

use rusqlite::params;
use serde_json::{Value, json};
use steve_modules::decisions::{DecisionCx, DecisionHandler};
use steve_modules::{ApiError, ApiResult, Registry};
use steve_store::base;
use steve_store::{Decision, Tx};

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
    tx: &Tx,
    ws: &str,
    actor: &str,
    space: &str,
    to: Flags,
    why: &Value,
) -> ApiResult<bool> {
    let before = spaces::settings(tx, space)?;
    let was = flags_of(&before);
    if was == to {
        return Ok(false);
    }
    let now = steve_store::now();
    let revision = before.revision + 1;
    tx.execute(
        "UPDATE space_settings SET muted = ?, configured = ?, revision = ?, updated = ? WHERE space_id = ?",
        params![to.muted as i64, to.configured as i64, revision, now, space],
    )?;
    if was.attention != to.attention {
        spaces::write_attention(tx, ws, space, to.attention, now)?;
    }
    let mut fields = Vec::new();
    if was.muted != to.muted {
        fields.push("muted");
    }
    if was.attention != to.attention {
        fields.push("attention");
    }
    if !fields.is_empty() {
        base::revision(
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
    base::audit(tx, ws, actor, "space.patch", space, &detail)?;
    spaces::event(tx, ws, space);
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
    fn resolve(&self, tx: &Tx, cx: &DecisionCx, d: &Decision) -> ApiResult<Value> {
        let Some(s) = spaces::get(tx, &d.subject_id)?.filter(|s| s.workspace_id == cx.ws) else {
            // the space went away meanwhile: nothing to do
            return Ok(json!({"choice": cx.choice, "gone": true}));
        };
        let before = spaces::settings(tx, &s.id)?;
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
    fn before_undo(&self, tx: &Tx, d: &Decision) -> ApiResult<()> {
        let Some(s) = spaces::get(tx, &d.subject_id)?.filter(|s| s.workspace_id == d.workspace_id)
        else {
            return Ok(());
        };
        let w = &d.result["was"];
        let (Some(muted), Some(configured)) = (w["muted"].as_bool(), w["configured"].as_bool())
        else {
            return Ok(());
        };
        let was = Flags {
            muted,
            // answers recorded before 019 carry no attention
            attention: w["attention"]
                .as_str()
                .and_then(Attention::parse)
                .unwrap_or(Attention::Realtime),
            configured,
        };
        let Some(to) = answered(d.result["choice"].as_str().unwrap_or(""), was) else {
            return Ok(());
        };
        let now = flags_of(&spaces::settings(tx, &s.id)?);
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
