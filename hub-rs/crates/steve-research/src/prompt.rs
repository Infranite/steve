//! The research prompt (`research_prompt.txt`), filled from data the caller collected (bot, knowledge
//! context, readable directories, recent messages, question).

/// A readable directory as the prompt lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirInfo {
    pub name: String,
    pub path: String,
    pub description: String,
}

/// Everything the prompt shows.
#[derive(Debug, Clone, Default)]
pub struct PromptInput {
    /// The main bot's name.
    pub bot: String,
    /// The bot's way of speaking and its rules, verbatim.
    pub style: String,
    /// `chat_context`: knowledge snippets numbered `[n]`.
    pub context: String,
    pub dirs: Vec<DirInfo>,
    /// The last messages of the thread, oldest first: (who, text).
    pub recent: Vec<(String, String)>,
    pub question: String,
    /// how to draw an interface in the answer (the web's main conversation only), or empty
    pub ui: String,
}

const TEMPLATE: &str = include_str!("research_prompt.txt");

/// The filled prompt. A follow-up that cannot `--resume` passes the previous answer in `recent`.
pub fn render(p: &PromptInput) -> String {
    let or_none = |s: String| {
        if s.trim().is_empty() {
            "（无）".to_string()
        } else {
            s
        }
    };
    let dirs = p
        .dirs
        .iter()
        .map(|d| {
            if d.description.trim().is_empty() {
                format!("- {}：{}", d.name, d.path)
            } else {
                format!("- {}：{}（{}）", d.name, d.path, d.description.trim())
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let recent = p
        .recent
        .iter()
        .map(|(w, t)| format!("{w}：{t}"))
        .collect::<Vec<_>>()
        .join("\n");
    let bot = if p.bot.trim().is_empty() {
        "Steve"
    } else {
        p.bot.trim()
    };
    TEMPLATE
        .replace("{{bot}}", bot)
        .replace("{{style}}", p.style.trim())
        .replace("{{dirs}}", &or_none(dirs))
        .replace("{{context}}", &or_none(p.context.clone()))
        .replace("{{recent}}", &or_none(recent))
        .replace("{{question}}", p.question.trim())
        .replace("{{ui}}", p.ui.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fills_every_slot() {
        let s = render(&PromptInput {
            bot: "Steve".into(),
            style: "简洁。".into(),
            context: "[1] 知识库按来源切块".into(),
            dirs: vec![DirInfo {
                name: "steve".into(),
                path: "/w/steve".into(),
                description: "Hub".into(),
            }],
            recent: vec![("用户".into(), "你好".into())],
            question: "知识系统怎么运作？".into(),
            ui: "界面：```a2ui".into(),
        });
        assert!(!s.contains("{{"));
        assert!(s.trim_end().ends_with("界面：```a2ui"));
        assert!(!render(&PromptInput::default()).contains("{{ui}}"));
        assert!(s.contains("- steve：/w/steve（Hub）"));
        assert!(s.contains("用户：你好"));
        assert!(s.contains("OFFER:"));
        assert!(render(&PromptInput::default()).contains("（无）"));
    }
}
