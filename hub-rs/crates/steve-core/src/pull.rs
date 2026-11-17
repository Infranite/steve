//! A git project's change lands the way any contributor's does (GitHub flow): the agent works on the
//! item's own branch, which starts from the remote's latest default branch, pushes it and opens a pull
//! request (an issue first for a bug). The host's CI does the checking, not this machine.
//!
//! Once the item is accepted, Steve follows the pull request of its branch ([`Core::check_prs`], every
//! stall tick, off the core's lock):
//!
//! - Steve merges it itself, never through the host's auto-merge (#239): on a look where the item is
//!   still closing on the same revision, every check of the pull request passed and its head is the
//!   accepted result, it runs `gh pr merge --match-head-commit <that head>` with the first merge method
//!   the repository allows (`gh api repos/{slug}`). A result that was invalidated is never merged, and
//!   nothing armed on the host has to be taken back;
//! - only a change an independent review passed is merged that way: the item's work graph, as accepted,
//!   has its required reviews done. Any other change (no work graph, or a graph accepted as a single
//!   session) waits on a "PR awaiting merge" card that only you answer, never YOLO; your yes lets Steve
//!   merge it as above, and merging it on the host yourself completes the item too;
//! - a merge that fails is tried again later, waiting longer each time ([`MERGE_RETRY_MS`]); after
//!   [`MERGE_FAILS_CARD`] failures in a row, or [`UNKNOWN_LOOKS_CARD`] looks in a row the host could
//!   not answer, a card for you;
//! - merged → the completion rule ([`crate::upstream::probe`]) is looked at again and the item closes on
//!   what the remote shows;
//! - a conflict with the default branch, a branch behind it that the host requires up to date, a failed
//!   check, a draft, or a head other than the accepted result → the agent is woken with what is wrong,
//!   once per pushed head and at most [`PR_TELLS_MAX`] times a cycle (then a card for you), to fix it on
//!   the same branch;
//! - no pull request for the branch → the agent is asked once to push it and open one;
//! - closed without merging → a card for you.
//!
//! Every merge, and every change left to you, is marked for the statistics (`pr`, with what review it
//! had), so how often an unreviewed change is reverted or fixed later can be counted.
//!
//! Only GitHub remotes are followed (`gh`); on another host the agent opens the host's merge request
//! itself and the completion rule still decides from what the remote's default branch holds.

use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};
use steve_modules::outbound::YOLO_ACTOR;

use crate::cycle::{CLOSING, COMPLETE, OP_RELEASE, closing_of, conflict};
use crate::error::Result;
use crate::orchestrator::To;
use crate::repo::{Decision, Item, NewDecision};
use crate::thread::flowback::{Update, flow_quiet};
use crate::util::{js_head, now};
use crate::{Core, Tx};

/// How long one `gh` call may take.
const GH_DEADLINE: Duration = Duration::from_secs(30);
/// How often a followed pull request is looked at.
pub const PR_EVERY_MS: i64 = 60_000;
/// The wait before a failed merge is tried again; each failure in a row doubles it, up to
/// [`MERGE_RETRY_MAX_MS`].
pub const MERGE_RETRY_MS: i64 = 2 * PR_EVERY_MS;
/// The longest wait between two tries of a merge that keeps failing.
pub const MERGE_RETRY_MAX_MS: i64 = 60 * 60_000;
/// Merge failures in a row that make a card for you (Steve keeps trying after it).
pub const MERGE_FAILS_CARD: i64 = 3;
/// Looks in a row the host could not answer that make a card for you.
pub const UNKNOWN_LOOKS_CARD: i64 = 5;
/// How often the agent may be woken about its pull request in one cycle; what is wrong after that is
/// a card for you.
pub const PR_TELLS_MAX: i64 = 8;
/// `subject_type` of the card asking you whether to merge a change no independent review passed
/// (an `approve`; the subject is the release operation). Only you answer it.
pub const PR_AWAITING_MERGE: &str = "pr_awaiting_merge";
/// The `pr_told` key of the "PR awaiting merge" card: asked once per result.
pub(crate) const TOLD_AWAITING: &str = "awaiting";
/// The statistics mark of a pull request: `awaiting` (left to you), `merged`.
pub const MARK_PR: &str = "pr";

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
    /// The host's auto-merge is on (someone turned it on; Steve never does).
    pub auto: bool,
    /// It cannot merge into the default branch without resolving conflicts.
    pub conflict: bool,
    /// The host requires the branch up to date with the default branch, and it is not.
    pub behind: bool,
    /// The checks that failed (their names).
    pub failing: Vec<String>,
    /// Some check has not finished.
    pub pending: bool,
    /// The host is still working out whether it can merge (`UNKNOWN`).
    pub settling: bool,
}

impl Pr {
    /// Open, ready for review, every check passed and nothing in the way: it may be merged now.
    pub fn mergeable_now(&self) -> bool {
        self.state == "OPEN"
            && !self.draft
            && !self.head.is_empty()
            && !self.pending
            && self.failing.is_empty()
            && !self.conflict
            && !self.behind
            && !self.settling
    }
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
    let mergeable = p["mergeable"].as_str().unwrap_or("");
    Look::Found(Pr {
        number: p["number"].as_u64().unwrap_or(0),
        url: p["url"].as_str().unwrap_or("").to_string(),
        state: p["state"].as_str().unwrap_or("").to_string(),
        head: p["headRefOid"].as_str().unwrap_or("").to_string(),
        draft: p["isDraft"].as_bool().unwrap_or(false),
        auto: p["autoMergeRequest"].is_object(),
        conflict: mergeable == "CONFLICTING" || merge_state == "DIRTY",
        behind: merge_state == "BEHIND",
        failing,
        pending,
        settling: mergeable == "UNKNOWN" || merge_state == "UNKNOWN",
    })
}

/// The merge method to ask for, from `gh api repos/{slug}` output: rebase, squash or merge, the first
/// one the repository allows. The `allow_*` fields are only there for an account that may write to
/// the repository.
pub fn merge_method(repo: &Value) -> std::result::Result<&'static str, String> {
    const METHODS: [(&str, &str); 3] = [
        ("allow_rebase_merge", "--rebase"),
        ("allow_squash_merge", "--squash"),
        ("allow_merge_commit", "--merge"),
    ];
    if METHODS.iter().all(|(k, _)| repo.get(k).is_none()) {
        return Err("读不到仓库允许的合并方式：gh 登录的账号对这个仓库没有写权限".into());
    }
    METHODS
        .into_iter()
        .find(|(k, _)| repo[k].as_bool() == Some(true))
        .map(|(_, m)| m)
        .ok_or_else(|| "仓库没有允许任何合并方式".into())
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

/// The fields `look` asks `gh pr list` for.
pub const PR_FIELDS: &str = "number,state,url,headRefOid,isDraft,mergeable,mergeStateStatus,autoMergeRequest,statusCheckRollup";

/// The pull request of `branch` in `slug`.
pub fn look(gh_cli: &Path, slug: &str, branch: &str) -> Look {
    match gh(
        gh_cli,
        &[
            "pr", "list", "--repo", slug, "--head", branch, "--state", "all", "--limit", "10",
            "--json", PR_FIELDS,
        ],
    ) {
        Ok(out) => match serde_json::from_str::<Value>(&out) {
            Ok(v) => read(&v),
            Err(e) => Look::Unknown(e.to_string()),
        },
        Err(e) => Look::Unknown(e),
    }
}

/// What a merge Steve asked for came to.
#[derive(Debug, Clone, PartialEq)]
pub enum Merging {
    Merged,
    Failed(String),
}

/// Merges pull request `number` of `slug` at `head` and nowhere else (`--match-head-commit`: a head
/// pushed since is refused by the host), with the first method the repository allows.
pub fn merge(gh_cli: &Path, slug: &str, number: u64, head: &str) -> Merging {
    let repo = gh(gh_cli, &["api", &format!("repos/{slug}")]).and_then(|out| {
        serde_json::from_str::<Value>(&out).map_err(|e| format!("gh api 的输出看不懂：{e}"))
    });
    let method = match repo.and_then(|r| merge_method(&r)) {
        Ok(m) => m,
        Err(e) => return Merging::Failed(e),
    };
    let n = number.to_string();
    match gh(
        gh_cli,
        &[
            "pr",
            "merge",
            &n,
            "--repo",
            slug,
            method,
            "--match-head-commit",
            head,
        ],
    ) {
        Ok(_) => Merging::Merged,
        Err(e) => Merging::Failed(e),
    }
}

/// How long to wait after `failures` merge failures in a row.
pub fn merge_retry_ms(failures: i64) -> i64 {
    let doublings = (failures - 1).clamp(0, 30) as u32;
    MERGE_RETRY_MS
        .saturating_mul(1_i64 << doublings)
        .min(MERGE_RETRY_MAX_MS)
}

/// A followed pull request due for a look.
#[derive(Debug, Clone)]
pub(crate) struct Due {
    op: String,
    item: String,
    slug: String,
    branch: String,
    /// Looked at only for whether it merged: the branch has nothing of its own (#218).
    only_merged: bool,
    /// Steve may merge this result: an independent review passed it, or you said yes.
    may_merge: bool,
    /// When a merge that failed may be tried again.
    merge_after: i64,
    /// The accepted result's fingerprint.
    frozen: Option<String>,
    /// The worktree, when it is on this machine.
    cwd: Option<String>,
    /// On a Node: the fingerprint its last facts said.
    node_print: Option<String>,
    on_node: bool,
}

/// Where the accepted result is now, for its pull request.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Accepted {
    /// At this commit of the worktree: it still holds what was accepted (the same content, or the
    /// same change on a newer default branch).
    At(String),
    /// The worktree holds something else now: the closing gate voids the result.
    Moved,
    /// Nothing tells: no fingerprint was frozen with the result, or the worktree can't be read.
    Unknown,
}

impl Due {
    /// [`Accepted`], from the worktree as it is now. Git runs here, off the core's lock.
    fn accepted(&self) -> Accepted {
        let Some(frozen) = self.frozen.as_deref() else {
            return Accepted::Unknown;
        };
        let now = if self.on_node {
            self.node_print.clone()
        } else {
            crate::cycle::fingerprint(self.cwd.as_deref())
        };
        match now {
            None => Accepted::Unknown,
            Some(n) if crate::cycle::same_result(frozen, &n) => {
                Accepted::At(crate::cycle::fingerprint_head(&n).to_string())
            }
            Some(_) => Accepted::Moved,
        }
    }
}

impl Core {
    /// One look at every followed pull request that is due (network calls outside the lock). Returns
    /// how many were looked at.
    pub fn check_prs(&self) -> Result<usize> {
        let due = self.tx().prs_due()?;
        let gh_cli = self.config().gh.clone();
        for d in &due {
            let look = look(&gh_cli, &d.slug, &d.branch);
            let open = matches!(&look, Look::Found(pr) if !d.only_merged && pr.state == "OPEN" && !pr.draft);
            /* looked at only for an open pull request Steve may merge or tell about: the worktree's
             * fingerprint is not free */
            let accepted = if open { d.accepted() } else { Accepted::Moved };
            let merging = match (&look, &accepted) {
                (Look::Found(pr), Accepted::At(h))
                    if d.may_merge
                        && now() >= d.merge_after
                        && pr.mergeable_now()
                        && &pr.head == h
                        /* asked again under the lock right before: a result invalidated since is
                         * never merged, and the host refuses any other head */
                        && self.tx().pr_may_merge(&d.op)? =>
                {
                    Some(merge(&gh_cli, &d.slug, pr.number, h))
                }
                _ => None,
            };
            self.tx().pr_seen(d, look, accepted, merging)?;
        }
        Ok(due.len())
    }
}

/// The wording of what the agent hears when Steve follows its pull request.
pub(crate) fn follow_text(branch: &str) -> String {
    format!(
        "Steve 会跟踪分支 {branch} 的 PR 的 CI 和冲突，检查都通过后由 Steve 按验收的版本合并它（没有经过独立评审的改动先等主人批准）：确认这个分支已经推送到 origin、PR 已经开好（还没有就现在推送，用 gh pr create 开 PR；修 bug 先建 issue，在 PR 里写 Closes #编号）。不要自己合并、不要推送默认分支、不要手工打 tag（发版按仓库自己的流程）；CI 失败或冲突时 Steve 会叫你在同一个分支上修。"
    )
}

/// The strings of a JSON array (`pr_told`).
fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
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

    /// The release of an earlier result of this cycle (not `op`) that found the pull request of
    /// `branch`: the one Steve followed last.
    pub(crate) fn pr_followed(
        &self,
        i: &Item,
        op: &str,
        branch: &str,
    ) -> Result<Option<steve_store::ItemOperation>> {
        Ok(self
            .db()
            .store()
            .items()
            .operations(&i.id, i.cycle)?
            .into_iter()
            .filter(|o| {
                o.kind == OP_RELEASE
                    && o.id != op
                    && o.evidence["pr_branch"].as_str() == Some(branch)
                    && o.evidence["pr"].as_u64().is_some()
            })
            .max_by_key(|o| o.revision))
    }

    /// The accepted result passed an independent review: the item's work graph, as it was accepted,
    /// has every required review done and was not accepted as a single session.
    pub(crate) fn pr_reviewed(&self, i: &Item) -> Result<bool> {
        let Some(c) = closing_of(i) else {
            return Ok(false);
        };
        let Some(g) = self.db().store().workflows().get(&i.id)? else {
            return Ok(false);
        };
        let reviews: Vec<_> = g
            .units
            .iter()
            .filter(|u| u.spec.kind == "review" && u.spec.required)
            .collect();
        Ok(c.workflow_revision == Some(g.revision)
            && !g.single_session()
            && !reviews.is_empty()
            && reviews.iter().all(|u| u.status == "succeeded"))
    }

    /// How the change was reviewed, for the statistics: `graph` (an independent review passed),
    /// `you` (you said yes to merging it) or `none`.
    fn pr_review(&self, i: &Item, o: &steve_store::ItemOperation) -> Result<&'static str> {
        Ok(if self.pr_reviewed(i)? {
            "graph"
        } else if o.evidence["pr_merge_ok"] == json!(true) {
            "you"
        } else {
            "none"
        })
    }

    /// Steve may merge the item's pull request right now: its release is still open, the item still
    /// closing on the revision that was accepted, and the change reviewed or yours to have said yes to.
    pub(crate) fn pr_may_merge(&mut self, op: &str) -> Result<bool> {
        let Some(o) = self.db().store().items().operation(op)? else {
            return Ok(false);
        };
        let Some(i) = self.db().item(&o.item_id)? else {
            return Ok(false);
        };
        let accepted = closing_of(&i).is_some_and(|c| c.kind == COMPLETE && c.accepted);
        Ok(o.status == "pending"
            && i.phase == CLOSING
            && o.revision == i.revision
            && accepted
            && o.evidence["pr_merge_declined"] != json!(true)
            && self.pr_review(&i, &o)? != "none")
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
            let on_node = self.git_on_node(&i.id);
            out.push(Due {
                op: o.id.clone(),
                item: o.item_id.clone(),
                slug,
                branch,
                only_merged: o.evidence["own"] == json!(false),
                may_merge: self.pr_may_merge(&o.id)?,
                merge_after: o.evidence["pr_merge_after"].as_i64().unwrap_or(0),
                frozen: closing_of(&i).and_then(|c| c.fingerprint),
                cwd: i.cwd.clone(),
                node_print: on_node
                    .then(|| self.git_seen(&i.id).map(|s| s.print.clone()))
                    .flatten(),
                on_node,
            });
        }
        Ok(out)
    }

    /// What a look at the item's pull request found (`accepted`: where the accepted result is now,
    /// looked at while the pull request is open), and what came of a merge Steve asked for.
    pub(crate) fn pr_seen(
        &mut self,
        d: &Due,
        look: Look,
        accepted: Accepted,
        merging: Option<Merging>,
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
        let told = strings(&o.evidence["pr_told"]);
        let review = self.pr_review(&i, &o)?;
        /* what the agent is told (once per key: a pushed head, a set of failed checks) */
        let mut tell: Option<(String, String)> = None;
        let mut ev = json!({});
        let mut close = false;
        /* a card for you, once per key */
        let mut card: Option<(String, String)> = None;
        let mut ask_merge: Option<(u64, String)> = None;
        let unknown = o.evidence["pr_unknown"].as_i64().unwrap_or(0);
        if unknown > 0 && !matches!(look, Look::Unknown(_)) {
            ev["pr_unknown"] = json!(0);
        }
        match look {
            Look::Unknown(e) => {
                let n = unknown + 1;
                ev["pr_unknown"] = json!(n);
                if n >= UNKNOWN_LOOKS_CARD {
                    ev["pr_problem"] = json!("unknown");
                    card = Some((
                        "unknown".into(),
                        format!(
                            "Steve 连续 {n} 次没能读到分支 {} 的 PR，合并在等：{}。看看 gh 能不能用、有没有登录。",
                            d.branch,
                            js_head(&e, 200)
                        ),
                    ));
                }
            }
            Look::None => {
                tell = Some((
                    "no-pr".into(),
                    format!(
                        "（Steve）验收通过了，但分支 {b} 还没有 PR。推送这个分支（git push -u origin {b}），用 gh pr create 开到默认分支的 PR（修 bug 先建 issue，在 PR 里写 Closes #编号），然后结束这一回合；检查通过后 Steve 会合并它。不要自己合并，也不要推送默认分支。",
                        b = d.branch
                    ),
                ));
            }
            Look::Found(pr) => {
                ev["pr"] = json!(pr.number);
                ev["pr_url"] = json!(pr.url);
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
                            self.mark_merged(&i, &pr, review, "elsewhere");
                        }
                        close = true;
                    }
                    "CLOSED" => {
                        card = Some((
                            "closed".into(),
                            format!(
                                "PR #{} 被关闭了，没有合并（{}）。要重新开 PR、放弃这个改动，还是你来看看？",
                                pr.number, pr.url
                            ),
                        ));
                    }
                    _ if pr.conflict => {
                        ev["pr_problem"] = json!("conflict");
                        tell = Some((
                            format!("conflict:{}", pr.head),
                            format!(
                                "（Steve）PR #{} 和默认分支有冲突，合并不了：{}。在你的分支上取最新的默认分支（git fetch origin），把它合并进来（git merge origin/默认分支），解决冲突、跑相关检查，然后 git push 推回同一个分支：推自己的分支不用批准。仓库自己的说明要求 rebase 时，rebase 之后的强推（git push --force-with-lease）仍要先用 steve_request_approval 请求批准。改动内容变了就重新用 steve_deliver 交付。不要合并 PR，也不要推送默认分支。",
                                pr.number, pr.url
                            ),
                        ));
                    }
                    _ if pr.behind => {
                        ev["pr_problem"] = json!("behind");
                        tell = Some((
                            format!("behind:{}", pr.head),
                            format!(
                                "（Steve）PR #{} 落后于默认分支，仓库要求合并前先跟上：{}。git fetch origin 后把最新的默认分支合并进来（git merge origin/默认分支），跑相关检查，然后 git push 推回同一个分支：推自己的分支不用批准。仓库自己的说明要求 rebase 时，rebase 之后的强推（git push --force-with-lease）仍要先用 steve_request_approval 请求批准。不要合并 PR，也不要推送默认分支。",
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
                    _ if pr.draft => {
                        ev["pr_problem"] = json!("draft");
                        tell = Some((
                            format!("draft:{}", pr.head),
                            format!(
                                "（Steve）验收通过了，但 PR #{n} 还是草稿，合并不了：{}。用 gh pr ready {n} 把它转为可合并，然后结束这一回合。不要合并 PR，也不要推送默认分支。",
                                pr.url,
                                n = pr.number
                            ),
                        ));
                    }
                    _ => {
                        ev["pr_problem"] = Value::Null;
                        /* the branch on the host is not what was accepted: the accepted commit was
                         * not pushed, or something else was pushed over it */
                        if let Accepted::At(h) = &accepted
                            && *h != pr.head
                            && !d.on_node
                        {
                            ev["pr_problem"] = json!("head");
                            tell = Some((
                                format!("head:{}:{h}", pr.head),
                                format!(
                                    "（Steve）PR #{} 的分支头是 {}，不是验收通过的版本 {}，Steve 只合并验收的版本：{}。把验收的提交推送到分支 {}（git push origin HEAD:{}），然后结束这一回合。改动内容变了就重新用 steve_deliver 交付。不要合并 PR，也不要推送默认分支。",
                                    pr.number,
                                    js_head(&pr.head, 12),
                                    js_head(h, 12),
                                    pr.url,
                                    d.branch,
                                    d.branch
                                ),
                            ));
                        }
                        if review == "none" && o.evidence["pr_merge_declined"] != json!(true) {
                            ask_merge = Some((pr.number, pr.url.clone()));
                        }
                        /* ready to merge, but nothing says which commit was accepted: Steve does
                         * not merge a head it can't bind to the result */
                        if accepted == Accepted::Unknown && d.may_merge && pr.mergeable_now() {
                            ev["pr_problem"] = json!("accepted_unknown");
                            card = Some((
                                "accepted-unknown".into(),
                                format!(
                                    "PR #{} 的检查都通过了，但 Steve 看不出它是不是验收通过的版本（验收时没有记下工作区，或现在读不到工作区），所以不会合并：{}。你看过后可以在 GitHub 上自己合并，事项照常完成。",
                                    pr.number, pr.url
                                ),
                            ));
                        }
                    }
                }
                match &merging {
                    Some(Merging::Merged) => {
                        ev["pr_merged"] = json!(true);
                        ev["pr_merged_by"] = json!("steve");
                        ev["pr_merge_failures"] = json!(0);
                        ev["pr_problem"] = Value::Null;
                        self.db().timeline_add(
                            &i.id,
                            "steve",
                            "sys",
                            &format!(
                                "PR #{} 的检查都通过了，Steve 已按验收的版本（{}）合并它：{}",
                                pr.number,
                                js_head(&pr.head, 12),
                                pr.url
                            ),
                            None,
                        )?;
                        self.mark_merged(&i, &pr, review, "steve");
                        close = true;
                    }
                    Some(Merging::Failed(e)) => {
                        let n = o.evidence["pr_merge_failures"].as_i64().unwrap_or(0) + 1;
                        let wait = merge_retry_ms(n);
                        ev["pr_merge_failures"] = json!(n);
                        ev["pr_merge_after"] = json!(now() + wait);
                        ev["pr_merge_error"] = json!(js_head(e, 300));
                        ev["pr_problem"] = json!("merge");
                        self.db().timeline_add(
                            &i.id,
                            "steve",
                            "sys",
                            &format!(
                                "Steve 没能合并 PR #{}（第 {n} 次）：{}；{} 分钟后再试",
                                pr.number,
                                js_head(e, 200),
                                wait / 60_000
                            ),
                            None,
                        )?;
                        if n >= MERGE_FAILS_CARD {
                            card = Some((
                                format!("merge:{}", pr.head),
                                format!(
                                    "Steve 连续 {n} 次没能合并 PR #{}（{}）：{}。Steve 会继续隔一段时间再试；你也可以在 GitHub 上看看，自己合并后事项照常完成。",
                                    pr.number,
                                    pr.url,
                                    js_head(e, 200)
                                ),
                            ));
                        }
                    }
                    None => {}
                }
            }
        }
        let mut all = told.clone();
        let mut wake: Option<String> = None;
        if let Some((key, text)) = tell.filter(|(k, _)| !told.contains(k)) {
            all.push(key);
            let tells = o.evidence["pr_tells"].as_i64().unwrap_or(0);
            if tells < PR_TELLS_MAX {
                ev["pr_tells"] = json!(tells + 1);
                wake = Some(text);
            } else if card.is_none() {
                /* the agent was told often enough this cycle: what is wrong now is yours to look at */
                card = Some((
                    "tells".into(),
                    format!(
                        "Steve 已经为分支 {} 的 PR 叫了 Bot {tells} 次，还是合并不了。最新的情况：{}",
                        d.branch,
                        js_head(text.trim_start_matches("（Steve）"), 300)
                    ),
                ));
            }
        }
        let card = card.filter(|(k, _)| !told.contains(k));
        if let Some((k, _)) = &card {
            all.push(k.clone());
        }
        let ask_merge = ask_merge.filter(|_| !told.iter().any(|k| k == TOLD_AWAITING));
        if ask_merge.is_some() {
            all.push(TOLD_AWAITING.into());
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
        if let Some((_, why)) = card {
            self.fail(&i.id, &why)?;
        }
        if let Some((n, url)) = ask_merge {
            self.pr_ask_merge(&i, &d.op, n, &url)?;
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

    /// The "PR awaiting merge" card: a change no independent review passed is merged only once you
    /// said yes. Only you answer it ([`Self::pr_merge_answer`]).
    fn pr_ask_merge(&mut self, i: &Item, op: &str, number: u64, url: &str) -> Result<()> {
        let d = self.db().decision_create(NewDecision {
            workspace_id: i.workspace_id.clone(),
            kind: "approve".into(),
            subject_type: PR_AWAITING_MERGE.into(),
            subject_id: op.into(),
            item_id: Some(i.id.clone()),
            session_id: None,
            question: format!(
                "PR #{number} 待合并：这个事项改了代码，但没有经过独立评审（没有带评审的工作图），Steve 不会自己合并它。看过改动后批准，Steve 会在检查都通过后按验收的版本合并；你也可以在 GitHub 上自己合并。"
            ),
            draft: Some(url.into()),
            options: Some(vec!["yes".into(), "no".into()]),
            requested_by: "steve".into(),
        })?;
        self.db().timeline_add(
            &i.id,
            "steve",
            "sys",
            &format!("PR #{number} 没有经过独立评审，等你批准合并：{url}"),
            None,
        )?;
        flow_quiet(
            self.db(),
            &i.id,
            Update {
                kind: "approve",
                text: format!("{} 的 PR #{number} 等你批准合并", i.id),
                decision: Some(d.id),
                ..Default::default()
            },
        );
        self.mark_pr(
            &i.workspace_id,
            &i.id,
            "awaiting",
            json!({"pr": number, "url": url, "review": "none"}),
        );
        crate::lifecycle::settle(self.db(), &i.id)
    }

    /// The `pr` mark of a pull request that merged (`by`: `steve`, or `elsewhere` when it was merged on
    /// the host), with the review the change had.
    fn mark_merged(&self, i: &Item, pr: &Pr, review: &str, by: &str) {
        self.mark_pr(
            &i.workspace_id,
            &i.id,
            "merged",
            json!({"pr": pr.number, "url": pr.url, "head": pr.head, "review": review, "by": by}),
        );
    }

    /// Your answer to a "PR awaiting merge" card: yes lets Steve merge the accepted result once its
    /// checks pass; no leaves the pull request to you. YOLO never answers it.
    pub(crate) fn pr_merge_answer(
        &mut self,
        d: &Decision,
        choice: &str,
        text: &str,
        actor: &str,
    ) -> Result<()> {
        if actor == YOLO_ACTOR {
            return Err(conflict("合并没有经过独立评审的改动，必须由你决定"));
        }
        let yes = match choice {
            "yes" => true,
            "no" => false,
            _ => return Err(conflict("请选择批准或不批准合并")),
        };
        let o = self
            .db()
            .store()
            .items()
            .operation(&d.subject_id)?
            .filter(|o| o.status == "pending");
        let i = match (&o, d.item_id.as_deref()) {
            (Some(_), Some(item)) => self.db().item(item)?,
            _ => None,
        };
        let (Some(o), Some(i)) = (o, i) else {
            return Err(conflict("这个结果已经完成或作废，不再合并"));
        };
        if i.phase != CLOSING || o.revision != i.revision {
            return Err(conflict("这个结果已经作废，不再合并"));
        }
        self.db().decision_resolve(
            &d.id,
            json!({"approved": yes, "text": text}),
            "resolved",
            None,
        )?;
        let pr = o.evidence["pr"].as_u64().unwrap_or(0);
        let ev = if yes {
            json!({"pr_merge_ok": true, "pr_merge_by": actor})
        } else {
            json!({"pr_merge_declined": true, "pr_merge_by": actor})
        };
        self.db()
            .store()
            .transaction(|tx| steve_store::operation_set_in(tx, &o.id, "pending", &ev))?;
        let reason = if text.trim().is_empty() {
            String::new()
        } else {
            format!("：{}", text.trim())
        };
        let line = if yes {
            format!("批准合并 PR #{pr}{reason}")
        } else {
            format!("不合并 PR #{pr}{reason}")
        };
        self.answer_line(&i.id, actor, &line, None)?;
        let after = if yes {
            "检查都通过后 Steve 按验收的版本合并它"
        } else {
            "Steve 不会合并它；你在 GitHub 上自己合并后事项照常完成，要改动就给事项发消息"
        };
        self.db()
            .timeline_add(&i.id, "steve", "sys", &format!("PR #{pr}：{after}"), None)?;
        /* looked at again on the next tick */
        self.st.pr_checked.remove(&o.id);
        crate::lifecycle::settle(self.db(), &i.id)
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
        let m = merge_method(
            &json!({"allow_rebase_merge": false, "allow_squash_merge": true,
            "allow_merge_commit": true}),
        );
        assert_eq!(m, Ok("--squash"));
        assert_eq!(
            merge_method(
                &json!({"allow_rebase_merge": false, "allow_squash_merge": false,
                "allow_merge_commit": false})
            ),
            Err("仓库没有允许任何合并方式".into())
        );
        assert!(merge_method(&json!({})).unwrap_err().contains("没有写权限"));
    }

    /* ── the contract with the real `gh` (2.103): its own output, recorded ── */

    /// `gh api repos/{slug}` as an account that may write to the repository answers it: the merge
    /// methods are `allow_*` fields there. (`gh repo view --json` has no auto-merge field at all, #239.)
    #[test]
    fn the_merge_method_is_read_from_what_gh_api_really_answers() {
        let repo: Value =
            serde_json::from_str(include_str!("tests/fixtures/gh-api-repo.json")).unwrap();
        assert_eq!(merge_method(&repo), Ok("--rebase"));
        /* an account that may only read the repository gets no `allow_*` fields */
        let mut read_only = repo.clone();
        for k in [
            "allow_rebase_merge",
            "allow_squash_merge",
            "allow_merge_commit",
            "allow_auto_merge",
        ] {
            read_only.as_object_mut().unwrap().remove(k);
        }
        assert!(merge_method(&read_only).unwrap_err().contains("没有写权限"));
        let view = include_str!("tests/fixtures/gh-repo-view-fields.txt");
        assert!(view.contains("Unknown JSON field: \"autoMergeAllowed\""));
    }

    /// Every field `look` asks `gh pr list` for is one the real `gh` offers.
    #[test]
    fn every_pull_request_field_asked_for_is_one_gh_offers() {
        let offered: Vec<&str> = include_str!("tests/fixtures/gh-pr-list-fields.txt")
            .lines()
            .skip_while(|l| !l.starts_with("Available fields"))
            .skip(1)
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        assert!(offered.len() > 10, "{offered:?}");
        for f in PR_FIELDS.split(',') {
            assert!(offered.contains(&f), "gh pr list has no field {f}");
        }
    }

    /// A real open pull request whose checks all passed reads as one that may be merged now.
    #[test]
    fn a_real_clean_pull_request_reads_as_mergeable() {
        let list: Value =
            serde_json::from_str(include_str!("tests/fixtures/gh-pr-list-clean.json")).unwrap();
        let Look::Found(p) = read(&list) else {
            panic!()
        };
        assert_eq!(p.state, "OPEN");
        assert_eq!(p.head.len(), 40);
        assert!(!p.auto && !p.pending && p.failing.is_empty() && !p.settling);
        assert!(p.mergeable_now(), "{p:?}");
    }

    #[test]
    fn a_pull_request_is_not_mergeable_while_anything_is_in_the_way() {
        let Look::Found(clean) = read(&json!([pr(json!({}))])) else {
            panic!()
        };
        assert!(clean.mergeable_now());
        for extra in [
            json!({"isDraft": true}),
            json!({"mergeable": "UNKNOWN", "mergeStateStatus": "UNKNOWN"}),
            json!({"mergeStateStatus": "BEHIND"}),
            json!({"mergeable": "CONFLICTING"}),
            json!({"statusCheckRollup": [{"__typename": "CheckRun", "name": "rust", "status": "QUEUED", "conclusion": ""}]}),
            json!({"statusCheckRollup": [{"__typename": "CheckRun", "name": "rust", "status": "COMPLETED", "conclusion": "FAILURE"}]}),
            json!({"state": "MERGED"}),
        ] {
            let Look::Found(p) = read(&json!([pr(extra.clone())])) else {
                panic!()
            };
            assert!(!p.mergeable_now(), "{extra}");
        }
    }

    #[test]
    fn a_failed_merge_waits_longer_each_time_up_to_the_most() {
        assert_eq!(merge_retry_ms(1), MERGE_RETRY_MS);
        assert_eq!(merge_retry_ms(2), 2 * MERGE_RETRY_MS);
        assert_eq!(merge_retry_ms(3), 4 * MERGE_RETRY_MS);
        assert_eq!(merge_retry_ms(100), MERGE_RETRY_MAX_MS);
    }
}
