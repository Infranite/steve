//! A git project's change lands the way any contributor's does (GitHub flow): the agent works on the
//! item's own branch, which starts from the remote's latest default branch, pushes it and opens a pull
//! request (an issue first for a bug). The host's CI and merge do the rest, not this machine.
//!
//! Once the item is accepted, Steve follows the pull request of its branch ([`Core::check_prs`], every
//! stall tick, off the core's lock):
//!
//! - it turns on the host's auto-merge for the head that was accepted (where the repository does not
//!   allow auto-merge, it merges once every check passed);
//! - merged → the completion rule ([`crate::upstream::probe`]) is looked at again and the item closes on
//!   what the remote shows;
//! - a conflict with the default branch, a branch behind it that the host requires up to date, or a
//!   failed check → the agent is woken with what is wrong, once per pushed head, to fix it on the same
//!   branch;
//! - no pull request for the branch → the agent is asked once to push it and open one;
//! - closed without merging → a card for you.
//!
//! Only GitHub remotes are followed (`gh`); on another host the agent opens the host's merge request
//! itself and the completion rule still decides from what the remote's default branch holds.

use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};

use crate::cycle::{CLOSING, OP_RELEASE};
use crate::error::Result;
use crate::orchestrator::To;
use crate::util::{js_head, now};
use crate::{Core, Tx};

/// How long one `gh` call may take.
const GH_DEADLINE: Duration = Duration::from_secs(30);
/// How often a followed pull request is looked at.
pub const PR_EVERY_MS: i64 = 60_000;

/// A pull request as the host shows it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Pr {
    pub number: u64,
    pub url: String,
    /// `OPEN` | `MERGED` | `CLOSED`
    pub state: String,
    /// The commit its branch points at.
    pub head: String,
    pub draft: bool,
    /// The host's auto-merge is on.
    pub auto: bool,
    /// It cannot merge into the default branch without resolving conflicts.
    pub conflict: bool,
    /// The host requires the branch up to date with the default branch, and it is not.
    pub behind: bool,
    /// The checks that failed (their names).
    pub failing: Vec<String>,
    /// Some check has not finished.
    pub pending: bool,
}

/// What a look for the branch's pull request found.
#[derive(Debug, Clone, PartialEq)]
pub enum Look {
    /// The branch has none.
    None,
    Found(Pr),
    /// The host could not be asked (no `gh`, signed out, offline): nothing is decided on it.
    Unknown(String),
}

/// What the repository allows for merging.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    /// The host's auto-merge may be turned on.
    pub auto: bool,
    /// The merge method to ask for: rebase, squash or merge, the first one allowed.
    pub method: Option<&'static str>,
}

/// The pull request of a branch from `gh pr list --json …` output: an open one first, else the
/// newest merged one, else the newest closed one.
pub fn read(list: &Value) -> Look {
    let Some(prs) = list.as_array() else {
        return Look::Unknown("gh 的输出看不懂".into());
    };
    let newest = |state: &str| {
        prs.iter()
            .filter(|p| p["state"].as_str() == Some(state))
            .max_by_key(|p| p["number"].as_u64().unwrap_or(0))
    };
    let Some(p) = newest("OPEN")
        .or_else(|| newest("MERGED"))
        .or_else(|| newest("CLOSED"))
    else {
        return Look::None;
    };
    let mut failing = vec![];
    let mut pending = false;
    for c in p["statusCheckRollup"].as_array().into_iter().flatten() {
        let name = c["name"]
            .as_str()
            .or_else(|| c["context"].as_str())
            .unwrap_or("check")
            .to_string();
        if let Some(state) = c["state"].as_str() {
            /* a commit status */
            match state {
                "FAILURE" | "ERROR" => failing.push(name),
                "PENDING" | "EXPECTED" => pending = true,
                _ => {}
            }
            continue;
        }
        /* a check run */
        if c["status"].as_str().is_some_and(|s| s != "COMPLETED") {
            pending = true;
            continue;
        }
        if matches!(
            c["conclusion"].as_str(),
            Some("FAILURE" | "TIMED_OUT" | "CANCELLED" | "ACTION_REQUIRED" | "STARTUP_FAILURE")
        ) {
            failing.push(name);
        }
    }
    failing.sort();
    failing.dedup();
    let merge_state = p["mergeStateStatus"].as_str().unwrap_or("");
    Look::Found(Pr {
        number: p["number"].as_u64().unwrap_or(0),
        url: p["url"].as_str().unwrap_or("").to_string(),
        state: p["state"].as_str().unwrap_or("").to_string(),
        head: p["headRefOid"].as_str().unwrap_or("").to_string(),
        draft: p["isDraft"].as_bool().unwrap_or(false),
        auto: p["autoMergeRequest"].is_object(),
        conflict: p["mergeable"].as_str() == Some("CONFLICTING") || merge_state == "DIRTY",
        behind: merge_state == "BEHIND",
        failing,
        pending,
    })
}

/// What the repository allows, from `gh repo view --json …` output.
pub fn read_settings(v: &Value) -> Settings {
    let on = |k: &str| v[k].as_bool().unwrap_or(false);
    let method = [
        ("rebaseMergeAllowed", "--rebase"),
        ("squashMergeAllowed", "--squash"),
        ("mergeCommitAllowed", "--merge"),
    ]
    .into_iter()
    .find(|(k, _)| on(k))
    .map(|(_, m)| m);
    Settings {
        auto: on("autoMergeAllowed"),
        method,
    }
}

/// `gh args…` with a deadline: its stdout, or why it failed.
fn gh(gh: &Path, args: &[&str]) -> std::result::Result<String, String> {
    let mut cmd = std::process::Command::new(gh);
    cmd.args(args).env("GH_PROMPT_DISABLED", "1");
    match crate::util::run_deadline(cmd, GH_DEADLINE) {
        Ok((true, out, _)) => Ok(out),
        Ok((false, _, e)) => Err(js_head(e.trim(), 300).to_string()),
        Err(crate::util::RunError::Spawn(e)) => Err(format!("gh 不可用：{e}")),
        Err(_) => Err("gh 超时".into()),
    }
}

/// The pull request of `branch` in `slug`.
pub fn look(gh_cli: &Path, slug: &str, branch: &str) -> Look {
    match gh(
        gh_cli,
        &[
            "pr",
            "list",
            "--repo",
            slug,
            "--head",
            branch,
            "--state",
            "all",
            "--limit",
            "10",
            "--json",
            "number,state,url,headRefOid,isDraft,mergeable,mergeStateStatus,autoMergeRequest,statusCheckRollup",
        ],
    ) {
        Ok(out) => match serde_json::from_str::<Value>(&out) {
            Ok(v) => read(&v),
            Err(e) => Look::Unknown(e.to_string()),
        },
        Err(e) => Look::Unknown(e),
    }
}

/// What the repository `slug` allows.
pub fn settings(gh_cli: &Path, slug: &str) -> std::result::Result<Settings, String> {
    let out = gh(
        gh_cli,
        &[
            "repo",
            "view",
            slug,
            "--json",
            "autoMergeAllowed,rebaseMergeAllowed,squashMergeAllowed,mergeCommitAllowed",
        ],
    )?;
    serde_json::from_str::<Value>(&out)
        .map(|v| read_settings(&v))
        .map_err(|e| e.to_string())
}

/// What Steve did to get an open pull request merged, for the head it did it for.
#[derive(Debug, Clone, PartialEq)]
pub enum Merging {
    /// The host's auto-merge is on now.
    Auto,
    /// Merged right away (the repository does not allow auto-merge; every check passed).
    Merged,
    /// Nothing yet: the checks still run (no auto-merge to leave it to).
    Waiting,
    Failed(String),
}

/// Asks the host to merge `pr` once its checks pass: auto-merge where the repository allows it, else
/// a merge right now when every check passed. Only the head that was accepted is ever merged
/// (`--match-head-commit`).
pub fn merge(gh_cli: &Path, slug: &str, pr: &Pr) -> Merging {
    let s = match settings(gh_cli, slug) {
        Ok(s) => s,
        Err(e) => return Merging::Failed(e),
    };
    let Some(method) = s.method else {
        return Merging::Failed("仓库没有允许任何合并方式".into());
    };
    let n = pr.number.to_string();
    let mut args = vec![
        "pr",
        "merge",
        n.as_str(),
        "--repo",
        slug,
        method,
        "--match-head-commit",
        pr.head.as_str(),
    ];
    if s.auto {
        args.push("--auto");
    } else if pr.pending || !pr.failing.is_empty() || pr.conflict || pr.behind {
        return Merging::Waiting;
    }
    match gh(gh_cli, &args) {
        Ok(_) if s.auto => Merging::Auto,
        Ok(_) => Merging::Merged,
        Err(e) => Merging::Failed(e),
    }
}

/// A followed pull request due for a look.
#[derive(Debug, Clone)]
pub(crate) struct Due {
    op: String,
    item: String,
    slug: String,
    branch: String,
    /// The head Steve already asked the host to merge.
    merge_head: String,
    /// Looked at only for whether it merged: the branch has nothing of its own (#218).
    only_merged: bool,
}

impl Core {
    /// One look at every followed pull request that is due (network calls outside the lock). Returns
    /// how many were looked at.
    pub fn check_prs(&self) -> Result<usize> {
        let due = self.tx().prs_due()?;
        let gh_cli = self.config().gh.clone();
        for d in &due {
            let look = look(&gh_cli, &d.slug, &d.branch);
            let merging = match &look {
                Look::Found(pr)
                    if !d.only_merged
                        && pr.state == "OPEN"
                        && !pr.draft
                        && !pr.auto
                        && !pr.head.is_empty()
                        && pr.head != d.merge_head =>
                {
                    Some((pr.head.clone(), merge(&gh_cli, &d.slug, pr)))
                }
                _ => None,
            };
            self.tx().pr_seen(d, look, merging)?;
        }
        Ok(due.len())
    }
}

/// The wording of what the agent hears when Steve follows its pull request.
pub(crate) fn follow_text(branch: &str) -> String {
    format!(
        "Steve 会给分支 {branch} 的 PR 打开自动合并，并跟踪它的 CI 和冲突：确认这个分支已经推送到 origin、PR 已经开好（还没有就现在推送，用 gh pr create 开 PR；修 bug 先建 issue，在 PR 里写 Closes #编号）。不要自己合并、不要推送默认分支、不要手工打 tag（发版按仓库自己的流程）；CI 失败或冲突时 Steve 会叫你在同一个分支上修。"
    )
}

impl Tx<'_> {
    /// The accepted item's pull request is followed from now on (`slug` on GitHub, `branch` its
    /// branch). Idempotent.
    pub(crate) fn follow_pr(&mut self, op: &str, slug: &str, branch: &str) -> Result<()> {
        self.db().store().transaction(|tx| {
            steve_store::operation_set_in(
                tx,
                op,
                "pending",
                &json!({"pr_slug": slug, "pr_branch": branch}),
            )
        })?;
        Ok(())
    }

    /// The pull requests due for a look: required releases still pending, followed, of a closing item.
    pub(crate) fn prs_due(&mut self) -> Result<Vec<Due>> {
        let t = now();
        let mut out = vec![];
        for o in self
            .db()
            .store()
            .items()
            .operations_with(OP_RELEASE, "pending")?
        {
            let s = |k: &str| o.evidence[k].as_str().unwrap_or("").to_string();
            let (slug, branch) = (s("pr_slug"), s("pr_branch"));
            /* merged: what is left (a tag) is the completion rule's to see, not the pull request's */
            if slug.is_empty() || branch.is_empty() || o.evidence["pr_merged"] == json!(true) {
                continue;
            }
            if self
                .st
                .pr_checked
                .get(&o.id)
                .is_some_and(|at| t - at < PR_EVERY_MS)
            {
                continue;
            }
            let Some(i) = self.db().item(&o.item_id)? else {
                continue;
            };
            if i.phase != CLOSING || o.revision != i.revision {
                continue;
            }
            self.st.pr_checked.insert(o.id.clone(), t);
            out.push(Due {
                op: o.id.clone(),
                item: o.item_id.clone(),
                slug,
                branch,
                merge_head: s("pr_merge_head"),
                only_merged: o.evidence["own"] == json!(false),
            });
        }
        Ok(out)
    }

    /// What a look at the item's pull request found, and what Steve did about it.
    pub(crate) fn pr_seen(
        &mut self,
        d: &Due,
        look: Look,
        merging: Option<(String, Merging)>,
    ) -> Result<()> {
        let Some(o) = self.db().store().items().operation(&d.op)? else {
            return Ok(());
        };
        let Some(i) = self.db().item(&d.item)? else {
            return Ok(());
        };
        if o.status != "pending" || i.phase != CLOSING || o.revision != i.revision {
            return Ok(());
        }
        /* a branch with nothing of its own (#218): only a merged pull request counts; anything else
         * (none, open, closed, or the host can't be asked) leaves it to the acceptance alone */
        if o.evidence["own"] == json!(false) {
            let merged = matches!(&look, Look::Found(pr) if pr.state == "MERGED");
            let ev = if merged {
                json!({"pr_merged": true})
            } else {
                json!({"pr_none": true})
            };
            self.db()
                .store()
                .transaction(|tx| steve_store::operation_set_in(tx, &d.op, "pending", &ev))?;
            self.forget_probe(&i.id);
            self.st.pr_checked.remove(&d.op);
            self.lc_try_close(&i.id)?;
            return Ok(());
        }
        let told: Vec<String> = o.evidence["pr_told"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        /* what the agent is told (once per key: a pushed head, a set of failed checks) */
        let mut tell: Option<(String, String)> = None;
        let mut ev = json!({});
        let mut close = false;
        let mut card: Option<String> = None;
        match look {
            Look::Unknown(_) => return Ok(()),
            Look::None => {
                tell = Some((
                    "no-pr".into(),
                    format!(
                        "（Steve）验收通过了，但分支 {b} 还没有 PR。推送这个分支（git push -u origin {b}），用 gh pr create 开到默认分支的 PR（修 bug 先建 issue，在 PR 里写 Closes #编号），然后结束这一回合；Steve 会打开自动合并并跟踪。不要自己合并，也不要推送默认分支。",
                        b = d.branch
                    ),
                ));
            }
            Look::Found(pr) => {
                ev["pr"] = json!(pr.number);
                ev["pr_url"] = json!(pr.url);
                if let Some((head, m)) = &merging {
                    ev["pr_merge_head"] = json!(head);
                    let line = match m {
                        Merging::Auto => {
                            Some(format!("已为 PR #{} 打开自动合并：{}", pr.number, pr.url))
                        }
                        Merging::Merged => {
                            Some(format!("PR #{} 的检查都通过了，Steve 已合并它", pr.number))
                        }
                        Merging::Waiting => {
                            /* asked again once the checks are done */
                            ev["pr_merge_head"] = json!("");
                            None
                        }
                        Merging::Failed(e) => Some(format!(
                            "Steve 没能让 PR #{} 自动合并：{}",
                            pr.number,
                            js_head(e, 200)
                        )),
                    };
                    if let Some(line) = line {
                        self.db().timeline_add(&i.id, "steve", "sys", &line, None)?;
                    }
                }
                match pr.state.as_str() {
                    "MERGED" => {
                        if o.evidence["pr_merged"] != json!(true) {
                            ev["pr_merged"] = json!(true);
                            self.db().timeline_add(
                                &i.id,
                                "steve",
                                "sys",
                                &format!("PR #{} 已合并：{}", pr.number, pr.url),
                                None,
                            )?;
                        }
                        close = true;
                    }
                    "CLOSED" => {
                        if !told.iter().any(|k| k == "closed") {
                            card = Some(format!(
                                "PR #{} 被关闭了，没有合并（{}）。要重新开 PR、放弃这个改动，还是你来看看？",
                                pr.number, pr.url
                            ));
                        }
                    }
                    _ if pr.conflict => {
                        ev["pr_problem"] = json!("conflict");
                        tell = Some((
                            format!("conflict:{}", pr.head),
                            format!(
                                "（Steve）PR #{} 和默认分支有冲突，合并不了：{}。在你的分支上取最新的默认分支（git fetch origin），rebase 到它上面，解决冲突、跑相关检查，然后 git push --force-with-lease 推回同一个分支。改动内容变了就重新用 steve_deliver 交付。不要合并 PR，也不要推送默认分支。",
                                pr.number, pr.url
                            ),
                        ));
                    }
                    _ if pr.behind => {
                        ev["pr_problem"] = json!("behind");
                        tell = Some((
                            format!("behind:{}", pr.head),
                            format!(
                                "（Steve）PR #{} 落后于默认分支，仓库要求合并前先跟上：{}。git fetch origin 后 rebase 到最新的默认分支，跑相关检查，然后 git push --force-with-lease 推回同一个分支。不要合并 PR，也不要推送默认分支。",
                                pr.number, pr.url
                            ),
                        ));
                    }
                    _ if !pr.failing.is_empty() => {
                        ev["pr_problem"] = json!("ci");
                        ev["pr_failing"] = json!(pr.failing);
                        tell = Some((
                            format!("ci:{}:{}", pr.head, pr.failing.join(",")),
                            format!(
                                "（Steve）PR #{} 的检查没通过：{}（{}）。用 gh pr checks {} 看失败的日志，在同一个分支上修好、跑过相关检查后推送；只是偶发失败就用 gh run rerun --failed 重跑。改动内容变了就重新用 steve_deliver 交付。不要合并 PR，也不要推送默认分支。",
                                pr.number,
                                pr.failing.join("、"),
                                pr.url,
                                pr.number
                            ),
                        ));
                    }
                    _ => {
                        ev["pr_problem"] = Value::Null;
                    }
                }
            }
        }
        let mut all = told.clone();
        let mut wake: Option<String> = None;
        if let Some((key, text)) = tell.filter(|(k, _)| !told.contains(k)) {
            all.push(key);
            wake = Some(text);
        }
        if card.is_some() {
            all.push("closed".into());
        }
        if all.len() != told.len() {
            ev["pr_told"] = json!(all);
        }
        self.db()
            .store()
            .transaction(|tx| steve_store::operation_set_in(tx, &d.op, "pending", &ev))?;
        if let Some(text) = wake {
            self.pr_wake(&i.id, &text)?;
        }
        if let Some(why) = card {
            self.fail(&i.id, &why)?;
        }
        if close {
            self.forget_probe(&i.id);
            self.st.pr_checked.remove(&d.op);
            self.lc_try_close(&i.id)?;
        }
        Ok(())
    }

    /// The agent hears it now: its session gets the prompt (queued behind a running turn); without a
    /// session, it starts one.
    fn pr_wake(&mut self, item: &str, text: &str) -> Result<()> {
        let to = match self.active_session(item)? {
            Some(s) => To::Session(s.id),
            None => To::Decision {
                live: None,
                item: Some(item.into()),
            },
        };
        self.db().timeline_add(
            item,
            "steve",
            "sys",
            &format!(
                "Steve 跟踪 PR：{}",
                js_head(text.trim_start_matches("（Steve）"), 120)
            ),
            None,
        )?;
        self.send(&to, text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pr(extra: Value) -> Value {
        let mut p = json!({"number": 7, "state": "OPEN", "url": "https://github.com/acme/app/pull/7",
            "headRefOid": "abc", "isDraft": false, "mergeable": "MERGEABLE", "mergeStateStatus": "CLEAN",
            "autoMergeRequest": null, "statusCheckRollup": []});
        for (k, v) in extra.as_object().unwrap() {
            p[k] = v.clone();
        }
        p
    }

    #[test]
    fn the_open_pull_request_comes_first_then_the_newest_merged_one() {
        assert_eq!(read(&json!([])), Look::None);
        let l = read(&json!([
            pr(json!({"number": 3, "state": "CLOSED"})),
            pr(json!({"number": 5, "state": "MERGED"})),
            pr(json!({"number": 4, "state": "MERGED"})),
        ]));
        assert!(matches!(l, Look::Found(p) if p.number == 5 && p.state == "MERGED"));
        let l = read(&json!([
            pr(json!({"number": 5, "state": "MERGED"})),
            pr(json!({"number": 6})),
        ]));
        assert!(matches!(l, Look::Found(p) if p.number == 6 && p.state == "OPEN"));
        assert!(matches!(read(&json!({})), Look::Unknown(_)));
    }

    #[test]
    fn checks_conflicts_and_auto_merge_are_read() {
        let Look::Found(p) = read(&json!([pr(json!({
            "mergeable": "CONFLICTING", "mergeStateStatus": "DIRTY",
            "autoMergeRequest": {"mergeMethod": "REBASE"},
            "statusCheckRollup": [
                {"__typename": "CheckRun", "name": "rust", "status": "COMPLETED", "conclusion": "FAILURE"},
                {"__typename": "CheckRun", "name": "web", "status": "IN_PROGRESS", "conclusion": ""},
                {"__typename": "CheckRun", "name": "lint", "status": "COMPLETED", "conclusion": "SUCCESS"},
                {"__typename": "StatusContext", "context": "ci/legacy", "state": "ERROR"},
            ]
        }))])) else {
            panic!()
        };
        assert!(p.conflict && p.auto && p.pending && !p.behind);
        assert_eq!(p.failing, ["ci/legacy", "rust"]);
        let Look::Found(p) = read(&json!([pr(json!({"mergeStateStatus": "BEHIND"}))])) else {
            panic!()
        };
        assert!(p.behind && !p.conflict && !p.auto && !p.pending && p.failing.is_empty());
    }

    #[test]
    fn the_first_allowed_merge_method_is_asked_for() {
        let s = read_settings(
            &json!({"autoMergeAllowed": true, "rebaseMergeAllowed": false,
            "squashMergeAllowed": true, "mergeCommitAllowed": true}),
        );
        assert_eq!(
            s,
            Settings {
                auto: true,
                method: Some("--squash")
            }
        );
        assert_eq!(read_settings(&json!({})).method, None);
    }
}
