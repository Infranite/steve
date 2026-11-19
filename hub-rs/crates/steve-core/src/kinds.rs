//! Item categories: every new task gets exactly one category.
//!
//! The store enforces the category invariant. The router can name a category; otherwise
//! [`guess`] reads the title and goal for flows, mail rules and channel bots.

use steve_store::kinds::KINDS;

use crate::Result;
use crate::repo::{Db, Tag};

/// What [`guess`] read from a title and goal.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Guess {
    pub kind: &'static str,
}

/// Words that point to a category, strongest category first (it wins a tie).
const WORDS: [(&str, &[&str]); 6] = [
    (
        "bug",
        &[
            "修复",
            "修正",
            "修一下",
            "bug",
            "报错",
            "出错",
            "错误",
            "故障",
            "失败",
            "崩溃",
            "异常",
            "无法",
            "不能用",
            "不生效",
            "没生效",
            "没反应",
            "不显示",
            "丢失",
            "卡住",
            "卡死",
            "闪退",
            "白屏",
            "乱码",
            "回归",
            "fix",
            "不对",
            "不正确",
            "有问题",
        ],
    ),
    (
        "ops",
        &[
            "发布",
            "部署",
            "上线",
            "运维",
            "重启",
            "备份",
            "证书",
            "域名",
            "服务器",
            "launchd",
            "systemd",
            "deploy",
            "回滚",
            "监控",
            "磁盘",
            "环境变量",
            "升级 steve",
            "升级版本",
            "配置环境",
        ],
    ),
    (
        "doc",
        &[
            "文档",
            "方案",
            "说明",
            "readme",
            "手册",
            "报告",
            "总结",
            "调研",
            "讲一下",
            "讲讲",
            "解释",
            "介绍一下",
            "梳理",
            "写一份",
            "出一份",
        ],
    ),
    (
        "refactor",
        &[
            "重构",
            "refactor",
            "拆分模块",
            "解耦",
            "抽取",
            "整理代码",
            "清理代码",
            "简化代码",
            "代码结构",
        ],
    ),
    (
        "chore",
        &[
            "依赖",
            "lint",
            "格式化",
            "工具配置",
            "杂项",
            "版本号",
            "bump",
            "gitignore",
            "清理日志",
            "清理缓存",
            "升级依赖",
        ],
    ),
    (
        "feat",
        &[
            "新增",
            "增加",
            "添加",
            "支持",
            "实现",
            "功能",
            "优化",
            "改进",
            "允许",
            "能够",
            "接入",
            "引入",
            "页面",
            "交互",
            "界面",
            "体验",
            "设计并实现",
        ],
    ),
];

/// The category a title and goal read as: each word found counts its length, twice in the title.
/// Nothing found reads as `feat` (most work adds or improves something).
pub fn guess(title: &str, goal: &str) -> Guess {
    let (t, g) = (title.to_lowercase(), goal.to_lowercase());
    let mut scores: Vec<(&'static str, usize)> = WORDS
        .iter()
        .map(|(k, ws)| {
            let s = ws
                .iter()
                .map(|w| {
                    let n = w.chars().count();
                    t.matches(w).count() * n * 2 + g.matches(w).count() * n
                })
                .sum();
            (*k, s)
        })
        .collect();
    /* stable: on a tie the earlier (stronger) category stays first */
    scores.sort_by_key(|s| std::cmp::Reverse(s.1));
    let top = scores[0];
    if top.1 == 0 {
        return Guess { kind: "feat" };
    }
    Guess { kind: top.0 }
}

/// A new item's tags made whole: tags of other workspaces dropped, duplicates once, exactly one
/// category, first. The first category given wins; with none, [`guess`] picks it (returned).
pub(crate) fn settle(
    db: &Db,
    ws: &str,
    ids: &[String],
    title: &str,
    goal: &str,
) -> Result<(Vec<String>, Option<Guess>)> {
    let kinds = db.kinds(ws)?;
    let mut kind: Option<String> = None;
    let mut rest: Vec<String> = vec![];
    for id in ids {
        if kinds.iter().any(|k| &k.id == id) {
            kind.get_or_insert_with(|| id.clone());
        } else if !rest.contains(id) && db.tag(id)?.is_some_and(|t| t.workspace_id == ws) {
            rest.push(id.clone());
        }
    }
    let (kind, guessed) = match kind {
        Some(k) => (k, None),
        None => {
            let g = guess(title, goal);
            (id_of(&kinds, g.kind), Some(g))
        }
    };
    Ok((std::iter::once(kind).chain(rest).collect(), guessed))
}

fn id_of(kinds: &[Tag], name: &str) -> String {
    kinds
        .iter()
        .find(|k| k.name == name)
        .map(|k| k.id.clone())
        .unwrap_or_default()
}

/// The category lines for a prompt: `  bug: 修复错误…`.
pub fn prompt_lines() -> Vec<String> {
    KINDS.iter().map(|(k, d)| format!("  {k}: {d}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guess_reads_the_main_goal() {
        let g = |t: &str, goal: &str| guess(t, goal).kind;
        assert_eq!(g("修复消息无法排队", "思考时发送的消息丢了"), "bug");
        assert_eq!(
            g("新增排队消息编辑能力", "排队中的消息可以撤回修改"),
            "feat"
        );
        assert_eq!(g("设计交付队列方案", "先出方案，不改代码"), "doc");
        assert_eq!(g("重构标签模块", "不改变外部行为"), "refactor");
        assert_eq!(g("发布 Hub 新版本", "部署到生产并检查健康"), "ops");
        assert_eq!(g("升级依赖", "把 npm 依赖升到最新"), "chore");
        assert_eq!(g("优化事项页面信息密度与布局", ""), "feat");
    }

    #[test]
    fn nothing_to_go_on_is_feat_never_chore() {
        let x = guess("ST-9", "看看");
        assert_eq!(x, Guess { kind: "feat" });
    }
}
