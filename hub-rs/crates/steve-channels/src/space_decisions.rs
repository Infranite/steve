//! Space rule-suggestion decisions (`subject_type = "space"`, `kind = "propose"`):
//! opened when similar messages in a space are ignored repeatedly
//! ([`crate::rule_suggestions`]).
//!
//! - `record_only` (以后只记录): switches the space to record-only (`muted = true`, and
//!   `reply_as_you = record` when your own account speaks there), sets `configured = true`,
//!   supersedes remaining open drafts, and records revision + audit.
//! - `keep` (保持现状): leaves the space settings unchanged and records the decision timestamp so
//!   the suggestion does not immediately re-trigger.
//! - Undo puts `muted`, `reply_as_you`, and `configured` back as they were (when unchanged since).
//! - Leaving/disbanding the group or manually switching the space to quiet/blocked/muted expires
//!   any open suggestion ([`expire_for_space`]).
//!
//! Register with `steve_channels::space_decisions::register(&mut registry)`.

use serde_json::{Value, json};
use steve_modules::decisions::{DecisionCx, DecisionHandler, DecisionOutcome};
use steve_modules::{ApiError, ApiResult, Registry};
use steve_store::{Decision, W};

use crate::ChannelResult;
use crate::spaces::{self, Mode, ReplyAsYou, SUBJECT_SPACE};

/// The handler of `subject_type = "space"`.
pub struct SpaceRuleSuggestionDecision;

/// Registers [`SpaceRuleSuggestionDecision`] for `space`.
pub fn register(reg: &mut Registry) {
    reg.decision(SUBJECT_SPACE, Box::new(SpaceRuleSuggestionDecision));
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Flags {
    muted: bool,
    reply_as_you: ReplyAsYou,
    configured: bool,
}

fn flags_of(s: &spaces::SpaceSettings) -> Flags {
    Flags {
        muted: s.muted,
        reply_as_you: s.reply_as_you,
        configured: s.configured,
    }
}

fn set_flags(
    w: &mut W<'_>,
    ws: &str,
    actor: &str,
    space: &str,
    to: Flags,
    why: &Value,
) -> ApiResult<bool> {
    let before = spaces::settings(w, space)?;
    let was = flags_of(&before);
    if was == to {
        return Ok(false);
    }
    let now = steve_store::now();
    let revision = before.revision + 1;
    use steve_store::channel_spaces::preferences::{self, Preference};
    preferences::set_flags(w, space, to.muted, to.configured, revision, now)?;
    if was.reply_as_you != to.reply_as_you {
        preferences::set(
            w,
            ws,
            space,
            Preference::ReplyAsYou(to.reply_as_you.code()),
            now,
        )?;
        if was.reply_as_you == ReplyAsYou::Draft
            && to.reply_as_you != ReplyAsYou::Draft
            && spaces::speaks_as_you(w, ws, space, None)?
        {
            crate::presence::supersede(
                w,
                ws,
                space,
                crate::presence::Answering::All,
                crate::presence::BY_SETTING,
                to.reply_as_you.code(),
            )?;
        }
    }

    let mut fields = Vec::new();
    if was.muted != to.muted {
        fields.push("muted");
    }
    if was.reply_as_you != to.reply_as_you {
        fields.push("reply_as_you");
    }
    if !fields.is_empty() {
        steve_store::audit_repo::revise(
            w,
            ws,
            "space",
            space,
            revision,
            &json!({"muted": was.muted, "reply_as_you": was.reply_as_you.code()}),
            &json!({"muted": to.muted, "reply_as_you": to.reply_as_you.code()}),
            actor,
        )?;
    }
    let mut detail = json!({ "fields": fields });
    if let (Some(o), Some(w)) = (detail.as_object_mut(), why.as_object()) {
        o.extend(w.clone());
    }
    steve_store::audit_repo::record(w, ws, actor, "space.patch", space, &detail)?;
    steve_store::channel_spaces::registration::emit(w, ws, space);
    Ok(true)
}

impl DecisionHandler for SpaceRuleSuggestionDecision {
    fn resolve(&self, w: &mut W<'_>, cx: &DecisionCx, d: &Decision) -> ApiResult<DecisionOutcome> {
        let Some(s) = steve_store::channel_spaces::rows::get_in_workspace(w, cx.ws, &d.subject_id)?
        else {
            return Ok(json!({"choice": cx.choice, "gone": true}).into());
        };
        let before = spaces::settings(w, &s.id)?;
        let was = flags_of(&before);
        let voice_mode = (!s.connection_id.is_empty())
            .then(|| crate::bindings::connection_account(w, cx.ws, &s.connection_id).ok())
            .flatten()
            .map(|(_, m)| m);
        let resolved_at = steve_store::now().max(
            d.result
                .get("suggested_at")
                .and_then(Value::as_i64)
                .unwrap_or(0),
        );
        match cx.choice {
            "record_only" => {
                let to = Flags {
                    muted: true,
                    reply_as_you: if voice_mode == Some(Mode::Own) {
                        ReplyAsYou::Record
                    } else {
                        was.reply_as_you
                    },
                    configured: true,
                };
                set_flags(w, cx.ws, cx.actor, &s.id, to, &json!({"decision": d.id}))?;
                Ok(json!({
                    "choice": cx.choice,
                    "space_id": s.id,
                    "resolved_at": resolved_at,
                    "was": {
                        "muted": was.muted,
                        "reply_as_you": was.reply_as_you.code(),
                        "configured": was.configured,
                    },
                })
                .into())
            }
            "keep" => Ok(json!({
                "choice": cx.choice,
                "space_id": s.id,
                "resolved_at": resolved_at,
            })
            .into()),
            _ => Err(ApiError::invalid("choice")),
        }
    }

    fn before_undo(&self, w: &mut W<'_>, d: &Decision) -> ApiResult<()> {
        if d.result["choice"].as_str() != Some("record_only") {
            return Ok(());
        }
        let Some(s) =
            steve_store::channel_spaces::rows::get_in_workspace(w, &d.workspace_id, &d.subject_id)?
        else {
            return Ok(());
        };
        let prior = &d.result["was"];
        let (Some(muted), Some(configured), Some(reply_as_you)) = (
            prior["muted"].as_bool(),
            prior["configured"].as_bool(),
            prior["reply_as_you"].as_str().and_then(ReplyAsYou::parse),
        ) else {
            return Ok(());
        };
        let was = Flags {
            muted,
            reply_as_you,
            configured,
        };
        let now = flags_of(&spaces::settings(w, &s.id)?);
        if !now.muted {
            return Ok(());
        }
        set_flags(
            w,
            &d.workspace_id,
            "steve",
            &s.id,
            was,
            &json!({"decision": d.id, "undo": true}),
        )?;
        Ok(())
    }
}

/// Expires any open rule-suggestion decision on `space` (e.g. bot left, group disbanded, or space
/// switched to record-only / quiet / blocked). Returns whether one was open.
pub fn expire_for_space(w: &mut W<'_>, ws: &str, space: &str, reason: &str) -> ChannelResult<bool> {
    let Some(d) = steve_store::decision_repo::open_for(w, ws, SUBJECT_SPACE, space)? else {
        return Ok(false);
    };
    Ok(steve_store::DecisionRows(w).settle(
        &d.id,
        &json!({"expired": true, "reason": reason}),
        steve_store::decision_repo::DecisionChange::Expire,
        None,
    )?)
}
