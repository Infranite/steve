//! The item's own conditions of done (ST-57): what its goal asks to be shown, on top of the project's
//! completion rule. ST-53 asked to confirm that the build passes and ST-55 to restore an entry on this
//! instance, yet both closed on "merged and tagged" with the build and the running version left open.
//!
//! The title and every goal of the item are read for three kinds of evidence ([`needs`]):
//! - **build** — the build passes. A delivery shows the build the agent ran: a passed check that names
//!   one ([`is_build_check`]). With a tag rule on GitHub the Release build the tag starts also becomes
//!   a required operation ([`OP_BUILD`](crate::cycle::OP_BUILD)): the close waits for it to succeed; a
//!   failure, or no result within the follow-up window, goes to the agent to fix and to you. Where Steve
//!   can't follow that build (a merge rule, a remote not on GitHub), an agent that stopped without
//!   delivering is continued to deliver its build check instead of closed on the remote.
//! - **deploy** — the result runs where it is used. For Steve's own project (on the Hub's device) with a
//!   completion rule, the required operation [`OP_DEPLOY`] holds the close until this Hub runs a version
//!   that contains the result; it is looked at again before every look at the gate, so the close follows
//!   the upgrade and the restart. Anywhere else Steve can't look, and the agent shows it like an
//!   operation (below).
//! - **operate** — somebody actually used it. `steve_deliver` needs a passed check marked `operated` (the
//!   page clicked through, the upgrade run, the live API called); builds and tests don't count. An agent
//!   that stopped without delivering is continued instead of closed on the remote's evidence alone.
//!
//! Marking the item done yourself is the evidence: none of this applies to it ([`MARKED`]).
//!
//! Nothing here depends on where the agent runs: the item's own facts (its remote, gathered on its
//! Node or here) give the tag and the commit, the agent hears through [`Tx::send`], and the running
//! version is this Hub's.

use std::path::Path;

use serde_json::json;

use crate::Tx;
use crate::cycle::{COMPLETE, Closing, OP_RELEASE, closing_of};
use crate::error::Result;
use crate::orchestrator::To;
use crate::patch;
use crate::repo::Item;
use crate::runtime::Check;
use crate::thread::flowback::{Update, flow_quiet};
use crate::upstream::DONE_TAG;

/// The required operation "this Hub runs a version that contains the result".
pub const OP_DEPLOY: &str = "deploy";

/// The reason of a completion you marked yourself.
pub const MARKED: &str = "你标记了完成";

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
    format!(
        "{} {}{}",
        if c.passed { "✓" } else { "✗" },
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

/// Steve follows the Release build a probe's tag starts: a tag rule, and a remote on GitHub.
pub(crate) fn build_followed(rule: &str, p: &crate::upstream::Probe) -> bool {
    rule == DONE_TAG && p.slug.is_some()
}

/// The numbers of a release version or tag (`v0.2.45`, `0.2.45`): exactly three.
fn numbers(v: &str) -> Option<[u64; 3]> {
    let core = v.trim().trim_start_matches('v');
    let core = core.split(['-', '+']).next()?;
    let n: Vec<u64> = core
        .split('.')
        .map(|n| n.parse().ok())
        .collect::<Option<_>>()?;
    n.try_into().ok()
}

/// The commit a version names at its end: a local build (`0.2.17-local-51826fc`) or a dated one
/// (`20261105-120301-4d419d6a`).
fn named_commit(v: &str) -> Option<&str> {
    v.rsplit_once('-')
        .map(|(_, c)| c)
        .filter(|c| c.len() >= 7 && c.chars().all(|x| x.is_ascii_hexdigit()))
}

/// The running Hub (`version`, and the commit it was built from when known) contains `commit`, which
/// `tag` points at (or brought the result in). By ancestry when `repo` knows both commits; otherwise
/// a release at or above the tag's. A build named by a commit nobody can place is not taken on its
/// number.
pub(crate) fn runs(
    repo: Option<&Path>,
    version: &str,
    running: Option<&str>,
    tag: Option<&str>,
    commit: Option<&str>,
) -> bool {
    let built = running.or(named_commit(version));
    if let (Some(dir), Some(built), Some(commit)) = (repo, built, commit) {
        let o = crate::git::command()
            .arg("-C")
            .arg(dir)
            .args(["merge-base", "--is-ancestor", commit, built])
            .output();
        if let Ok(o) = o
            && matches!(o.status.code(), Some(0 | 1))
        {
            return o.status.success();
        }
    }
    if named_commit(version).is_some() {
        return false;
    }
    match (numbers(version), tag.and_then(numbers)) {
        (Some(a), Some(b)) => a >= b,
        _ => false,
    }
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

    /// The item works on Steve's own repository, on the Hub's device: this Hub is where it runs.
    fn own_project(&self, i: &Item) -> Result<bool> {
        let Some(p) = self.db().project(i.project_id.as_deref())? else {
            return Ok(false);
        };
        if p.device_id != crate::release::HUB_DEVICE {
            return Ok(false);
        }
        let canon = |s: &Path| std::fs::canonicalize(s).unwrap_or_else(|_| s.to_path_buf());
        Ok(canon(Path::new(&p.path)) == canon(&self.cfg().steve_repo))
    }

    /// Steve checks that the result runs here: Steve's own project with a completion rule.
    fn deploy_checked(&self, i: &Item) -> Result<bool> {
        Ok(self.done_rule(i)?.is_some() && self.own_project(i)?)
    }

    /// The closing asks for the goal's evidence (not one you marked done yourself).
    pub(crate) fn proof_needed(&self, i: &Item, c: &Closing) -> Result<Needs> {
        if c.kind != COMPLETE || c.reason == MARKED {
            return Ok(Needs::default());
        }
        self.needs_of(i)
    }

    /// The agent's protocol line for the goal's evidence; `None` when the goal asks for none.
    pub(crate) fn proof_line(&self, i: &Item) -> Result<Option<String>> {
        let n = self.needs_of(i)?;
        if !n.any() {
            return Ok(None);
        }
        let tagged = self.done_rule(i)?.as_deref() == Some(DONE_TAG);
        let mut parts = vec![];
        if n.build {
            parts.push(if tagged {
                "the build passes: steve_deliver needs a passed check for the build you ran, named as one (构建 or build; a delivery without it is refused); when the remote is on GitHub, the item also closes only after the Release build the tag starts succeeded (Steve follows it; don't wait for it in a turn, and fix it when Steve tells you it failed)"
            } else {
                "the build passes: steve_deliver needs a passed check for the build you ran, named as one (构建 or build; a delivery without it is refused)"
            });
        }
        if n.deploy {
            parts.push(if self.deploy_checked(i)? {
                "it runs on this Steve instance: after the tag, the item closes only once this Hub runs a version that contains it (Steve checks the running version; the user upgrades)"
            } else {
                "it runs where it is deployed: check it there and report that check with `operated: true`"
            });
        }
        if n.operate {
            parts.push(
                "it was actually used: steve_deliver needs at least one passed check with `operated: true` for something you did on the running thing yourself (opened the page and clicked through, ran the upgrade, called the live API), named with what you saw; builds and tests don't count",
            );
        }
        Ok(Some(format!(
            "- This item's goal asks to show, before it can close: {}.",
            parts.join("; ")
        )))
    }

    /// Why a delivery lacks the evidence the goal asks for (an actual operation, the build it ran);
    /// `None` when it has it.
    pub(crate) fn proof_refused(&self, i: &Item, checks: &[Check]) -> Result<Option<String>> {
        let n = self.needs_of(i)?;
        let wanted = n.operate || (n.deploy && !self.deploy_checked(i)?);
        if wanted && !checks.iter().any(|c| c.operated && c.passed) {
            return Ok(Some(format!(
                "Not delivered: this item's goal asks for {} to be shown, and none of the checks is an actual operation. Do it on the running thing yourself (open the page and click through, run the upgrade, call the live API), then deliver again with that check marked `operated: true` and named with what you saw. Builds and tests don't count. If you can't do it here, say why with steve_ask instead.",
                n.words().join("、")
            )));
        }
        if n.build && !checks.iter().any(|c| c.passed && is_build_check(c)) {
            return Ok(Some(
                "Not delivered: this item's goal asks for the build to pass, and none of the checks is a passed build. Run the build, then deliver again with it among the checks, named as one (for example \"构建：cargo build --release 通过\"). If it can't be built here, say why with steve_ask instead.".into(),
            ));
        }
        Ok(None)
    }

    /// The remote alone is not the evidence the goal asks for: an agent that stopped after merging (and
    /// tagging) without delivering is continued to show it (`Some` with what it hears). A build Steve
    /// follows itself ([`build_followed`]) needs no delivery.
    pub(crate) fn proof_continue(
        &self,
        i: &Item,
        rule: &str,
        p: &crate::upstream::Probe,
    ) -> Result<Option<String>> {
        let n = self.needs_of(i)?;
        let mut more = vec![];
        if n.operate || (n.deploy && !self.deploy_checked(i)?) {
            more.push("这个事项的目标还要求实际操作验证：在运行中的环境里亲手操作一遍，用 steve_deliver 交付，并把那条检查标成 `operated: true`。".to_string());
        }
        if n.build && !build_followed(rule, p) {
            let why = if rule == DONE_TAG {
                "远端不在 GitHub 上，Steve 跟踪不了 tag 触发的 Release 构建"
            } else {
                "这个项目合并就算完成，Steve 不跟踪构建"
            };
            more.push(format!("这个事项的目标还要求构建通过，{why}：跑一遍构建，用 steve_deliver 交付，把它作为一条通过的检查列进去（名字写明“构建”或 build）。"));
        }
        Ok((!more.is_empty()).then(|| more.concat()))
    }

    /// The deploy requirement of an accepted result, once the remote showed it: met when this Hub runs
    /// a version that contains it. Called again before each look at the gate; idempotent.
    pub(crate) fn deploy_requirement(&mut self, i: &Item, c: &Closing) -> Result<()> {
        if !self.proof_needed(i, c)?.deploy || !self.deploy_checked(i)? {
            return Ok(());
        }
        let rel = self
            .db()
            .store()
            .items()
            .operations(&i.id, i.cycle)?
            .into_iter()
            .find(|o| o.kind == OP_RELEASE && o.revision == i.revision && o.status == "succeeded");
        let Some(rel) = rel else {
            return Ok(());
        };
        let s = |k: &str| rel.evidence[k].as_str().map(String::from);
        let (tag, commit) = (s("tag"), s("landed").or(s("commit")));
        let what = match &tag {
            Some(t) => format!("本机运行包含 {t} 的版本"),
            None => "本机运行包含这个结果的版本".into(),
        };
        let key = format!("{}:{}:{}", OP_DEPLOY, i.cycle, i.revision);
        let (op, new) = self.lc_op_add(
            i,
            OP_DEPLOY,
            Some(&key),
            true,
            "pending",
            &what,
            json!({"tag": tag, "commit": commit}),
        )?;
        if op.status != "pending" {
            return Ok(());
        }
        let version = self.cfg().version.clone();
        let running = self.cfg().commit.clone();
        let repo = self.cfg().steve_repo.clone();
        if runs(
            Some(&repo),
            &version,
            running.as_deref(),
            tag.as_deref(),
            commit.as_deref(),
        ) {
            let ev = json!({"tag": tag, "commit": commit, "version": version});
            self.db()
                .store()
                .transaction(|tx| steve_store::operation_set_in(tx, &op.id, "succeeded", &ev))?;
            self.db().timeline_add(
                &i.id,
                "steve",
                "sys",
                &format!(
                    "Steve 核对：本机已运行 {version}，包含这个结果；事项目标要求的部署证据已取得"
                ),
                None,
            )?;
            return Ok(());
        }
        if new {
            let line = format!(
                "事项目标要求在本机生效：本机现在运行 {version}，还不包含{}。升级后 Steve 自动核对并结束事项；确认不需要可以直接标记完成",
                tag.as_deref().map(|t| format!(" {t}")).unwrap_or_default()
            );
            self.db().timeline_add(&i.id, "steve", "sys", &line, None)?;
            self.db()
                .item_patch(&i.id, patch![stage: "收尾", now: format!("等{what}")])?;
            flow_quiet(
                self.db(),
                &i.id,
                Update {
                    kind: "build",
                    text: format!("{} {line}。", i.id),
                    ..Default::default()
                },
            );
        }
        Ok(())
    }

    /// Whether the build the tag started holds the close (the goal asks for it to pass).
    pub(crate) fn build_required(&self, i: &Item) -> Result<bool> {
        let Some(c) = closing_of(i) else {
            return Ok(false);
        };
        Ok(self.proof_needed(i, &c)?.build)
    }

    /// A required build came out: success may close the item; a failure (or no result in time) goes
    /// to the agent to fix and to you.
    pub(crate) fn build_settled(&mut self, item: &str, line: &str, failed: bool) -> Result<()> {
        if !failed {
            self.lc_try_close(item)?;
            return Ok(());
        }
        self.db().item_patch(
            item,
            patch![now: "Release 构建没有通过，事项目标要求构建通过"],
        )?;
        let to = match self.active_session(item)? {
            Some(s) => To::Session(s.id),
            None => To::Decision {
                live: None,
                item: Some(item.into()),
            },
        };
        self.send(
            &to,
            &format!("（Steve）{line}。这个事项的目标要求构建通过，所以还没有结束。查明原因并修好，重新交付；新的 tag 推上去后 Steve 会再跟踪它的构建。"),
        )?;
        flow_quiet(
            self.db(),
            item,
            Update {
                kind: "build",
                text: format!(
                    "{item} {line}。事项目标要求构建通过，所以还没结束：已让 Agent 去修，修好重新交付；你确认不需要也可以直接标记完成。"
                ),
                ..Default::default()
            },
        );
        Ok(())
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

    #[test]
    fn the_running_version_contains_the_tag() {
        assert!(runs(None, "0.2.46", None, Some("v0.2.45"), None));
        assert!(runs(None, "0.2.45", None, Some("v0.2.45"), None));
        assert!(!runs(None, "0.2.35", None, Some("v0.2.45"), None));
        assert!(!runs(None, "0.2.9", None, Some("v0.2.10"), None));
        assert!(!runs(None, "dev", None, Some("v0.2.45"), None));
        assert!(!runs(None, "0.2.45", None, None, None));
        // a build named by its commit is judged by the commit, never by its number
        assert!(!runs(
            None,
            "0.2.99-local-51826fc",
            None,
            Some("v0.2.45"),
            None
        ));
        assert!(!runs(
            None,
            "20261105-120301-4d419d6a",
            None,
            Some("v0.2.45"),
            None
        ));
    }

    #[test]
    fn a_build_contains_what_its_commit_contains() {
        let d = tempfile::tempdir().unwrap();
        let r = crate::git::tests::repo(d.path());
        let first = crate::git::tests::g(&r, &["rev-parse", "HEAD"]);
        std::fs::write(r.join("n.txt"), "n\n").unwrap();
        crate::git::tests::g(&r, &["add", "."]);
        crate::git::tests::g(&r, &["commit", "-q", "-m", "n"]);
        let second = crate::git::tests::g(&r, &["rev-parse", "HEAD"]);
        let local = |c: &str| format!("0.2.1-local-{}", &c[..7]);
        assert!(runs(Some(&r), &local(&second), None, None, Some(&first)));
        assert!(!runs(Some(&r), &local(&first), None, None, Some(&second)));
        // the commit the release was built from decides over its number
        assert!(runs(
            Some(&r),
            "0.2.1",
            Some(&second),
            Some("v9.0.0"),
            Some(&first)
        ));
        assert!(!runs(
            Some(&r),
            "9.9.9",
            Some(&first),
            Some("v0.2.1"),
            Some(&second)
        ));
        // a commit this repository doesn't know: by number
        let unknown = "0123456789abcdef0123456789abcdef01234567";
        assert!(runs(
            Some(&r),
            "0.2.5",
            Some(unknown),
            Some("v0.2.4"),
            Some(&first)
        ));
    }
}
