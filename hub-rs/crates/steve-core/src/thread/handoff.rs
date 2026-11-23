//! An answer that needs tools: the answer is a model call without them, so a question that has to
//! run a command, read a file or look at the machine (“看下磁盘还剩多少”) makes the model reach for
//! one, and the call refuses it ([`CoreError::NeedsTools`]). That is decided by the refusal, not by
//! how the question reads: for the owner's own trigger word on their account ([`Conv::hands_on`]) the
//! question becomes an item a Bot works on, once, where the conversation lets a turn become one.
//! Anywhere else (the web conversation, a Bot's chat, a chat with outside members) the
//! conversation says why in plain words and what to do instead; “做成事项” still makes one.
//!
//! [`Conv::hands_on`]: super::Conv::hands_on
//!
//! [`CoreError::NeedsTools`]: crate::error::CoreError::NeedsTools

use super::offer::ThreadItem;
use super::research::InlineJob;
use super::{MAIN, Steps, lock};
use crate::Core;
use crate::error::Result;
use crate::util::now;
use serde_json::{Map, json};

/// `route_log.outcome` of a question handed to an item because its answer needed tools.
pub const NEEDS_TOOLS_OUTCOME: &str = "needs_tools";

/// What a chat's conversation says when a question needs tools and may not become an item there.
pub const NEEDS_TOOLS_HERE: &str = "（这件事要在机器上动手：跑命令、读文件或查看系统状态，这里的直接回答做不到。要做的话，在 Steve 的对话里交代给我，我会交给 Bot 去做。）";
/// … and your own conversation, where the answer was asked for here (“在这里回答”).
pub const NEEDS_TOOLS_ANSWER: &str = "（这件事要在机器上动手：跑命令、读文件或查看系统状态，直接回答做不到。点“做成事项”交给 Bot 去做。）";

/// What the conversation says when a question that needs tools could not be handed on: why.
pub fn not_handed_off(error: &str) -> String {
    format!(
        "（这件事要在机器上动手：跑命令、读文件或查看系统状态，直接回答做不到；转成事项也没成：{}。可以在 Steve 的对话里点“做成事项”再试。）",
        crate::util::js_head(error, 120)
    )
}

/// The reply that says a question became item `id` because it needs tools; `proposal`: it waits for
/// the owner's yes first.
fn handed_off(id: &str, bot: &str, proposal: bool) -> String {
    if proposal {
        format!(
            "这件事要在机器上动手（跑命令、读文件或查看系统状态），直接回答做不到。已做成事项 {id}，等主人确认后交给 {bot} 去做。"
        )
    } else {
        format!(
            "这件事要在机器上动手（跑命令、读文件或查看系统状态），直接回答做不到，已做成事项 {id} 交给 {bot} 去做。"
        )
    }
}

impl Core {
    /// The answer to `j` reached for a tool it may not use: the question becomes an item, once.
    /// `Ok(None)`: it may not become one here ([`InlineJob::to_item`]) or already did; the caller
    /// says why there is no answer.
    pub(crate) async fn hand_off(&self, ws: &str, j: &InlineJob) -> Result<Option<String>> {
        let Some(start) = j.to_item else {
            return Ok(None);
        };
        let th = self.store().store().thread();
        if th.route_of(&j.origin)?.is_some_and(|r| r.item_id.is_some()) {
            return Ok(None);
        }
        // title, goal, Bot and project as for any item made from the conversation
        let history = self.history_before(ws, &j.origin)?;
        let r = self
            .route_in(ws, &j.question, &history, true, j.space != MAIN)
            .await?;
        let goal = if r.goal.trim().is_empty() || r.goal.trim() == j.question.trim() {
            j.question.clone()
        } else {
            format!("{}\n\n（用户原话：{}）", r.goal.trim(), j.question)
        };
        let _one = lock(&self.0.thread.acting);
        // made meanwhile (a click on “做成事项”, a second failure of the same question): that one
        if th.route_of(&j.origin)?.is_some_and(|r| r.item_id.is_some()) {
            return Ok(None);
        }
        let item = self.thread_item(ThreadItem {
            ws,
            origin: &j.origin,
            title: r.title.clone(),
            goal,
            bot: self.named_bot(ws, &j.question)?.map(|b| b.id),
            project: r.project.clone().or_else(|| j.project.clone()),
            tags: r.tag_ids.clone(),
            files: j.files.clone(),
            from_question: super::decide::pure_question(&j.question),
            answer: None,
            start,
            by: j.user.clone(),
        })?;
        th.route_outcome(&j.origin, NEEDS_TOOLS_OUTCOME, Some(&item.id))?;
        let text = handed_off(&item.id, &self.bot_name(&item), item.status == "propose");
        let steps = Steps {
            route: j.route.clone(),
            ..Default::default()
        };
        let mut meta = Map::new();
        meta.insert("steps".into(), steps.json(now() - j.t0));
        meta.insert(
            "handoff".into(),
            json!({"why": NEEDS_TOOLS_OUTCOME, "item": item.id}),
        );
        let id = self.announce(ws, &item, &j.origin, &text, meta)?;
        Ok(Some(id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_words_say_why_and_what_to_do() {
        let t = handed_off("ST-9", "运维 Bot", false);
        assert!(t.contains("ST-9") && t.contains("运维 Bot") && t.contains("跑命令"));
        assert!(handed_off("ST-9", "运维 Bot", true).contains("等主人确认"));
        assert!(NEEDS_TOOLS_HERE.contains("Steve 的对话"));
        let n = not_handed_off("没有可用的 Bot");
        assert!(n.contains("没有可用的 Bot") && n.contains("做成事项"));
    }
}
