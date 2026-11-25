//! Capability-preflight messages for requests that require machine tools.
//! Tool-less answer models are never probed first to trigger a handoff.

/// `thread_reply_receipts.route_outcome` of a question preflighted to an item because it needs tools.
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
pub(crate) fn handed_off(id: &str, bot: &str, proposal: bool) -> String {
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
