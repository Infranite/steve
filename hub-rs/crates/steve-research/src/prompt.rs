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
    /// The run acts with the user's full access on the Hub's machine; otherwise it only reads `dirs`.
    pub full: bool,
}

/// What a read-only run may do, and the heading of its directories.
const READ_ONLY: (&str, &str) = (
    "这是一次只读研究：你只能读取下面列出的目录、知识库和事项。通过命令读取时，只有当前执行器已提供只读保障才可使用，并受同样的可读目录和敏感路径限制。不能修改文件、执行开发任务或访问网络。不要声称做了任何改动。",
    "可读目录：",
);

/// What the owner's own assistant may do on the Hub's machine, and the heading of the projects.
const FULL: (&str, &str) = (
    "你运行在 Hub 所在的这台机器上，以当前系统用户的身份拥有这台机器的完整权限：可以读写任意目录的文件、执行命令、访问网络；工作目录默认是用户的主目录。用户让你查看、整理、安装、排查这台机器上的东西时，直接动手做，然后如实说明做了什么、结果如何；没做成的照实说，不要声称做了没做的事。这是对话，回答以快为先：先用几秒内出结果的办法（如 ls 当前一层），不要一上来对整个主目录或整块磁盘做递归扫描（du、find、grep -r 这类可能跑几分钟的命令）；确实需要时先缩小范围，并在回答里说明这一步耗时较长。删除、覆盖、强制推送这类难以恢复的操作，先说清楚要动什么，得到用户明确同意后再做。Steve 自己的密钥、数据库和程序文件不在你可触及的范围内。项目里需要评审、留痕或耗时较长的开发任务不要在这里直接改代码，按下面第 4 条用 OFFER 交给 Bot 作为事项去做。",
    "已登记的项目目录：",
);

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
    let (access, dirs_title) = if p.full { FULL } else { READ_ONLY };
    TEMPLATE
        .replace("{{bot}}", bot)
        .replace("{{style}}", p.style.trim())
        .replace("{{access}}", access)
        .replace("{{dirs_title}}", dirs_title)
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
