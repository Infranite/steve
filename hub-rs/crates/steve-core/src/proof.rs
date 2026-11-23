//! Checks supplied with delivery for a goal that asks for a build or an actual operation.

use crate::Tx;
use crate::error::Result;
use crate::repo::Item;
use crate::runtime::Check;

const BUILD: &[&str] = &[
    "构建通过",
    "构建成功",
    "构建失败",
    "确认构建",
    "release 构建",
    "流水线",
    "ci 通过",
    "ci 成功",
    "build pass",
];
const DEPLOY: &[&str] = &[
    "当前实例",
    "本机实例",
    "运行实例",
    "线上实例",
    "实例上",
    "实例已升级",
    "实例升级",
    "本机生效",
    "本机运行",
    "实例生效",
    "线上生效",
    "部署到",
    "部署后",
    "完成部署",
    "已部署",
    "发布上线",
    "上线后",
    "已上线",
];
const OPERATE: &[&str] = &[
    "实际操作",
    "实操",
    "实测",
    "亲手",
    "手动验证",
    "手工验证",
    "截图",
    "端到端",
    "e2e",
    "验证",
];

/// What the goal asks to be shown before the item may close.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Needs {
    pub build: bool,
    pub deploy: bool,
    pub operate: bool,
}

impl Needs {
    pub fn any(self) -> bool {
        self.build || self.deploy || self.operate
    }

    /// In words, for the agent.
    pub fn words(self) -> Vec<&'static str> {
        [
            (self.build, "构建通过"),
            (self.deploy, "部署生效"),
            (self.operate, "实际操作验证"),
        ]
        .into_iter()
        .filter_map(|(on, w)| on.then_some(w))
        .collect()
    }
}

/// What `text` (an item's title and goals) asks to be shown.
pub fn needs(text: &str) -> Needs {
    /* "验证码" is a thing, not a verification */
    let t = text.to_lowercase().replace("验证码", "");
    let has = |words: &[&str]| words.iter().any(|w| t.contains(w));
    Needs {
        build: has(BUILD),
        deploy: has(DEPLOY),
        operate: has(OPERATE),
    }
}

/// A check as the delivery and its evidence show it; an actual operation says so.
pub fn check_line(c: &Check) -> String {
    let scope_tag = match c.effective_scope() {
        "baseline" => "[基线] ",
        "external" => "[外部] ",
        _ => "",
    };
    format!(
        "{} {}{}{}",
        if c.passed { "✓" } else { "✗" },
        scope_tag,
        if c.operated { "实际操作：" } else { "" },
        c.name
    )
}

/// A check that shows a build: its name says so (`构建`, `编译`, `build`, `compile`, `CI`).
pub fn is_build_check(c: &Check) -> bool {
    let n = c.name.to_lowercase();
    ["构建", "编译", "流水线", "build", "compile"]
        .iter()
        .any(|w| n.contains(w))
        || n.split(|ch: char| !ch.is_ascii_alphanumeric())
            .any(|w| w == "ci")
}

impl Tx<'_> {
    /// What the item's goal asks to be shown (its title and every goal it had).
    pub(crate) fn needs_of(&self, i: &Item) -> Result<Needs> {
        let mut text = i.title.clone();
        for g in self.db().goals(&i.id)? {
            text.push('\n');
            text.push_str(&g.text);
        }
        Ok(needs(&text))
    }

    /// The agent's protocol line for the goal's evidence; `None` when the goal asks for none.
    pub(crate) fn proof_line(&self, i: &Item) -> Result<Option<String>> {
        let n = self.needs_of(i)?;
        if !n.any() {
            return Ok(None);
        }
        let mut parts = vec![];
        if n.build {
            parts.push("the build passes: list a passed build check");
        }
        if n.deploy {
            parts.push("it runs where it is deployed: check it there and report that check with `operated: true`");
        }
        if n.operate {
            parts.push(
                "it was actually used: list at least one passed check with `operated: true` for something you did on the running thing yourself (opened the page and clicked through, ran the upgrade, called the live API), named with what you saw; builds and tests don't count",
            );
        }
        Ok(Some(format!(
            "- This item's goal asks to show, before it can close: {}.",
            parts.join("; ")
        )))
    }

    /// Why a delivery lacks the evidence the goal asks for (an actual operation, the build it ran);
    /// `None` when it has it.
    pub(crate) fn proof_missing(&self, i: &Item, checks: &[Check]) -> Result<Option<String>> {
        let n = self.needs_of(i)?;
        let wanted = n.operate || n.deploy;
        if wanted && !checks.iter().any(|c| c.operated && c.passed) {
            return Ok(Some(format!(
                "目标要求{}，交付的检查里没有一条是实际操作（operated）。",
                n.words().join("、")
            )));
        }
        if n.build && !checks.iter().any(|c| c.passed && is_build_check(c)) {
            return Ok(Some(
                "目标要求构建通过，交付的检查里没有通过的构建。".into(),
            ));
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_goal_says_what_has_to_be_shown() {
        // ST-53: the Release pipeline, confirmed by a passing build
        let n = needs("修复 Release 流水线\n修复偶发失败的测试，并确认构建通过");
        assert!(n.build && !n.deploy);
        // ST-55: the entry on this instance, and what to verify on it
        let n = needs(
            "排查并恢复版本检查与升级重启入口\n核对当前版本、安装方式识别、升级能力接口和设置·关于的显示条件，查明入口消失原因并修复；验证检查更新、升级备份与重启",
        );
        assert!(!n.build && n.operate);
        assert!(needs("补核 ST-55 当前实例的升级入口").deploy);
        // a plain change asks for nothing beyond the project's rule
        assert_eq!(needs("统计页“今天”按小时统计"), Needs::default());
        assert!(!needs("登录页加验证码").operate);
        assert!(needs("CI 通过后再合并").build);
        assert_eq!(
            needs("构建通过、实际操作").words(),
            vec!["构建通过", "实际操作验证"]
        );
    }

    #[test]
    fn an_actual_operation_is_named_in_the_evidence() {
        let c = |operated| Check {
            name: "设置·关于点检查更新".into(),
            passed: true,
            operated,
            scope: None,
        };
        assert_eq!(check_line(&c(false)), "✓ 设置·关于点检查更新");
        assert_eq!(check_line(&c(true)), "✓ 实际操作：设置·关于点检查更新");
    }

    #[test]
    fn a_build_check_says_it_is_one() {
        let c = |name: &str| Check {
            name: name.into(),
            passed: true,
            operated: false,
            scope: None,
        };
        for yes in [
            "构建通过",
            "cargo build --release",
            "npm run build",
            "CI 全绿",
            "编译",
        ] {
            assert!(is_build_check(&c(yes)), "{yes}");
        }
        for no in ["cargo test", "decision 页面", "precision"] {
            assert!(!is_build_check(&c(no)), "{no}");
        }
    }
}
